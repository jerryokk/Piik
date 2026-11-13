//! Engine-level tests on 127.0.0.1 UDP. Omitted IPv6 cases are documented
//! where they apply.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use piik_encoded::Frame;
use tokio::sync::mpsc;

use crate::*;

mod edge;
mod gathering;
mod publication;
mod receiver;
mod source;

/// An 8x8 VP8 keyframe fixture: one complete keyframe.
pub(crate) const VP8_KEY_8X8: &[u8] = &[
    0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x08, 0x00, 0x08, 0x00, 0x00, 0x47, 0x08, 0x85, 0x85, 0x88,
    0x85, 0x84, 0x88, 0x02, 0x02, 0x00, 0x0c, 0x0d, 0x60, 0x00, 0xfe, 0xff, 0xab, 0x50, 0x80,
];

pub(crate) fn loopback_engine() -> Engine {
    Engine::new(EngineOptions {
        bind_address: "127.0.0.1:0".into(),
        include_loopback: true,
        ..EngineOptions::default()
    })
    .unwrap()
}

/// Collects one owner's events.
pub(crate) struct Events {
    pub sink: EventSink,
    pub receiver: mpsc::UnboundedReceiver<EdgeEvent>,
}

pub(crate) fn events() -> Events {
    let (sender, receiver) = mpsc::unbounded_channel();
    Events {
        sink: Arc::new(move |event| {
            let _ = sender.send(event);
        }),
        receiver,
    }
}

/// Every event currently queued.
pub(crate) fn drain(receiver: &mut mpsc::UnboundedReceiver<EdgeEvent>) -> Vec<EdgeEvent> {
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
}

/// Waits for an event matching `predicate`, returning every event seen.
pub(crate) async fn wait_event(
    receiver: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    timeout: Duration,
    mut predicate: impl FnMut(&EdgeEvent) -> bool,
) -> Option<EdgeEvent> {
    tokio::time::timeout(timeout, async {
        while let Some(event) = receiver.recv().await {
            if predicate(&event) {
                return Some(event);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

/// A recovery callback recording requested layers.
pub(crate) fn recorder() -> (RecoveryCallback, Arc<Mutex<Vec<Vec<i32>>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let sink = requests.clone();
    (
        Arc::new(move |layers| sink.lock().unwrap().push(layers)),
        requests,
    )
}

pub(crate) fn frame(data: &[u8], pts: Duration, recovery: bool) -> Frame {
    Frame {
        data: data.to_vec(),
        pts,
        duration: Duration::from_millis(33),
        recovery,
    }
}

/// One Host edge connected to a Viewer receiver on its own engine over
/// loopback, with every trickled candidate exchanged.
pub(crate) struct Peer {
    pub viewer: Engine,
    pub edge: Edge,
    pub receiver: Receiver,
    pub edge_events: Events,
    /// Kept alive: the receiver's sink sends into it.
    #[allow(dead_code)]
    pub receiver_events: Events,
}

/// Exchanges trickled candidates until both ends connect.
pub(crate) async fn exchange(
    edge: &Edge,
    edge_events: &mut Events,
    receiver: &Receiver,
    receiver_events: &mut Events,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        for event in drain(&mut edge_events.receiver) {
            if let EdgeEvent::LocalCandidate { candidate, current } = event
                && current()
            {
                receiver.add_remote_candidate(candidate).await.unwrap();
            }
        }
        for event in drain(&mut receiver_events.receiver) {
            if let EdgeEvent::LocalCandidate { candidate, current } = event
                && current()
            {
                edge.add_remote_candidate(candidate).await.unwrap();
            }
        }
        if edge.state() == PcState::Connected && receiver.state() == PcState::Connected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!(
        "loopback pair did not connect: edge {:?}, receiver {:?}",
        edge.state(),
        receiver.state()
    );
}

impl Peer {
    /// A new edge on `source` with a connected native receiver.
    pub async fn connect(host: &Engine, source: &VideoSource, id: &str) -> Self {
        Self::connect_with(host, source, id, None).await
    }

    pub async fn connect_with(
        host: &Engine,
        source: &VideoSource,
        id: &str,
        audio: Option<AudioSource>,
    ) -> Self {
        let viewer = loopback_engine();
        let mut edge_events = events();
        let edge = host
            .new_edge(
                source,
                EdgeOptions {
                    connection_id: id.into(),
                    audio,
                    events: Some(edge_events.sink.clone()),
                    ..EdgeOptions::default()
                },
            )
            .await
            .unwrap();
        let offer = edge.create_offer().await.unwrap();
        assert!(!offer.contains("a=candidate:"), "offer is trickled");
        let mut receiver_events = events();
        let (receiver, answer) = viewer
            .new_receiver(ReceiverOptions {
                offer,
                edge_capacity: 1,
                events: Some(receiver_events.sink.clone()),
                ..ReceiverOptions::default()
            })
            .await
            .unwrap();
        edge.set_answer(&answer, "").await.unwrap();
        exchange(&edge, &mut edge_events, &receiver, &mut receiver_events).await;
        Self {
            viewer,
            edge,
            receiver,
            edge_events,
            receiver_events,
        }
    }

    /// Received `(timestamp, payload)` video packets so far.
    pub async fn received(&self) -> Vec<(u32, Arc<[u8]>)> {
        let key = self.receiver.key;
        self.viewer
            .call(move |app, _, _| {
                app.receivers
                    .get(&key)
                    .map(|receiver| receiver.received.clone())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default()
    }

    /// Waits until at least `count` video packets arrived.
    pub async fn wait_packets(&self, count: usize) -> Vec<(u32, Arc<[u8]>)> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let received = self.received().await;
            if received.len() >= count {
                return received;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "only {} of {count} packets arrived",
                received.len()
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn close(&self) {
        self.viewer.close().await.unwrap();
    }
}

/// One input with a recovery frame on layer 0.
pub(crate) async fn write_source_frame(
    source: &VideoSource,
    data: &[u8],
    pts: Duration,
) -> Result<()> {
    source.begin_frame(pts).await?;
    source.write_video(0, frame(data, pts, true)).await
}

/// Waits until `predicate` holds, polling.
pub(crate) async fn eventually(label: &str, mut predicate: impl AsyncFnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !predicate().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {label}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
