//! Native Viewer tests. Upstream offers come from a loopback `piik-media-edge`
//! Host edge, as the native Host sends them.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;
use piik_input_route::InputChannel;
use piik_media_edge::{
    AudioSource, ChannelEvent, Edge, EdgeEvent, EdgeOptions, Engine, EngineOptions, VideoSource,
};
use piik_native_capture::VideoProfile;
use piik_native_control::media::{MediaEvent, MediaEventKind};
use piik_native_host::bridge::EventSink;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{Options, Session};

struct Events(mpsc::Sender<MediaEvent>);

impl EventSink for Events {
    fn send(&self, event: MediaEvent) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.0.send(event).await.is_ok() })
    }
}

fn start(edge_capacity: usize) -> (Session, mpsc::Receiver<MediaEvent>) {
    let (tx, rx) = mpsc::channel(256);
    let session = Session::start(
        &CancellationToken::new(),
        Options {
            share_id: "viewer-session".into(),
            edge_capacity,
            port_mapping: false,
            events: Arc::new(Events(tx)),
            relay: None,
            bind_address: "127.0.0.1:0".into(),
        },
    )
    .unwrap();
    (session, rx)
}

fn loopback_engine() -> Engine {
    Engine::new(EngineOptions {
        bind_address: "127.0.0.1:0".into(),
        include_loopback: true,
        ..EngineOptions::default()
    })
    .unwrap()
}

/// A Host-side sender whose complete local offers feed the Viewer.
struct Upstream {
    engine: Engine,
    source: VideoSource,
    audio: Option<AudioSource>,
}

impl Upstream {
    async fn new(audio: bool) -> Self {
        let engine = loopback_engine();
        let source = engine.new_source("vp8", 4, 1, None).await.unwrap();
        source.set_format(0, 320, 180).await.unwrap();
        source.configure_outputs(vec![300_000]).await.unwrap();
        let audio = if audio {
            Some(engine.new_audio_source(4, 64_000).await.unwrap())
        } else {
            None
        };
        Self {
            engine,
            source,
            audio,
        }
    }

    async fn edge(
        &self,
        id: &str,
        channel: bool,
        events: Option<piik_media_edge::EventSink>,
    ) -> Edge {
        let edge = self
            .engine
            .new_edge(
                &self.source,
                EdgeOptions {
                    connection_id: id.into(),
                    audio: self.audio.clone(),
                    local: true,
                    events,
                    ..EdgeOptions::default()
                },
            )
            .await
            .unwrap();
        if channel {
            edge.open_data_channel(piik_input_route::LABEL)
                .await
                .unwrap();
        }
        edge
    }

    async fn offer(&self, id: &str) -> String {
        self.edge(id, false, None)
            .await
            .create_local_offer("offer", false)
            .await
            .unwrap()
    }
}

#[tokio::test]
async fn offer_reuse_keeps_source_and_child_ownership() {
    for reuse in [false, true] {
        let (session, _rx) = start(2);
        let plain = Upstream::new(false).await.offer("upstream-edge").await;
        let with_audio = Upstream::new(true).await.offer("upstream-edge").await;
        for audio in [false, true, false] {
            let offer = if audio {
                with_audio.clone()
            } else {
                plain.clone()
            };
            let result = session
                .accept_offer("upstream", &offer, Vec::new(), reuse, false)
                .await
                .unwrap();
            assert!(
                !result.reused && result.audio == audio,
                "new media shape: {result:?}"
            );
            assert!(
                session.edge_source("local-playback").is_none(),
                "incompatible source retained its edge"
            );
            let receiver = session.receiver_source("upstream").unwrap();
            session
                .prepare_local_edge("upstream", "local-playback", "local_offer_1", false)
                .await
                .unwrap();
            let result = session
                .accept_offer("upstream", &offer, Vec::new(), reuse, false)
                .await
                .unwrap();
            assert_eq!(result.reused, reuse, "repeat offer: {result:?}");
            let current = session.receiver_source("upstream").unwrap();
            if reuse {
                assert_eq!(
                    current, receiver,
                    "compatible offer retired the existing source"
                );
                assert_eq!(
                    session.edge_source("local-playback").as_deref(),
                    Some("upstream")
                );
            } else {
                assert_ne!(
                    current, receiver,
                    "legacy request did not replace its receiver"
                );
                assert!(session.edge_source("local-playback").is_none());
            }
        }
        session.close_session().await.unwrap();
    }
}

#[tokio::test]
async fn viewer_profile_has_no_implicit_capture_default() {
    let (session, _rx) = start(2);
    assert!(session.inner.lock().profile.is_none());
    assert!(
        session
            .update_profile(VideoProfile::default())
            .await
            .is_err()
    );
    let profile = VideoProfile {
        width: 1920,
        height: 1080,
        framerate: 30,
        bitrate: 5_000_000,
        preference: "balanced".into(),
    };
    session.update_profile(profile.clone()).await.unwrap();
    assert_eq!(session.inner.lock().profile.as_ref(), Some(&profile));
    session.update_profile(profile.clone()).await.unwrap();
    // A relay edge needs the Host profile; a local edge does not.
    session.close_session().await.unwrap();
    assert!(
        session.update_profile(profile).await.is_err(),
        "closed Viewer accepted a profile"
    );
}

#[tokio::test]
async fn relay_requires_the_host_profile() {
    let (session, _rx) = start(2);
    let upstream = Upstream::new(false).await;
    let offer = upstream.offer("upstream-edge").await;
    session
        .accept_offer("upstream", &offer, Vec::new(), false, false)
        .await
        .unwrap();
    assert!(
        session
            .prepare_edge("upstream", "relay-child", Vec::new(), false)
            .await
            .is_err(),
        "relay started without the Host profile"
    );
    assert!(
        session
            .prepare_edge("missing", "relay-child", Vec::new(), false)
            .await
            .is_err()
    );
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn receiver_capacity_preserves_current_and_prepared_sources() {
    let (session, _rx) = start(2);
    let upstream = Upstream::new(false).await;
    let offer = upstream.offer("upstream-edge").await;
    for id in ["active", "candidate"] {
        session
            .accept_offer(id, &offer, Vec::new(), false, false)
            .await
            .unwrap();
    }
    let active = session.receiver_source("active").unwrap();
    let candidate = session.receiver_source("candidate").unwrap();
    assert!(
        session
            .accept_offer("excess", &offer, Vec::new(), false, false)
            .await
            .is_err(),
        "a third receiver bypassed the bound"
    );
    assert_eq!(session.receiver_source("active").as_ref(), Some(&active));
    assert_eq!(
        session.receiver_source("candidate").as_ref(),
        Some(&candidate)
    );
    assert!(session.receiver_source("excess").is_none());
    let result = session
        .accept_offer("active", &offer, Vec::new(), true, false)
        .await
        .unwrap();
    assert!(
        result.reused,
        "existing receiver could not renegotiate at capacity"
    );
    session
        .prepare_local_edge("active", "local-preview", "preview-offer", false)
        .await
        .unwrap();
    session
        .accept_offer("candidate", &offer, Vec::new(), false, false)
        .await
        .unwrap();
    assert_eq!(session.receiver_source("active").as_ref(), Some(&active));
    assert_ne!(
        session.receiver_source("candidate").as_ref(),
        Some(&candidate)
    );
    assert!(session.edge_source("local-preview").is_some());
    session.close_receiver("candidate").await;
    assert_eq!(session.receiver_count(), 1);
    session
        .accept_offer("replacement", &offer, Vec::new(), false, false)
        .await
        .expect("retired receiver did not release its slot");
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn closing_a_receiver_retires_its_child_edges() {
    let (session, _rx) = start(2);
    let upstream = Upstream::new(false).await;
    let offer = upstream.offer("upstream-edge").await;
    session
        .accept_offer("upstream", &offer, Vec::new(), false, false)
        .await
        .unwrap();
    session
        .prepare_local_edge("upstream", "playback", "local-offer", false)
        .await
        .unwrap();
    assert!(
        session
            .prepare_local_edge("upstream", "playback", "other-offer", false)
            .await
            .is_err(),
        "duplicate edge identity was admitted"
    );
    assert!(
        session
            .prepare_local_edge("other", "playback", "offer", true)
            .await
            .is_err(),
        "restart crossed its source"
    );
    session.close_receiver("upstream").await;
    assert!(
        session.edge_source("playback").is_none(),
        "child edge outlived its source"
    );
    // Late candidates are disposable.
    session.add_candidate("playback", None).await.unwrap();
    session
        .add_receiver_candidate("upstream", None)
        .await
        .unwrap();
    assert!(
        session
            .set_answer("playback", "v=0", "local-offer")
            .await
            .is_err()
    );
    session.close_session().await.unwrap();
    assert!(
        session
            .accept_offer("upstream", &offer, Vec::new(), false, false)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn punch_preparation_binds_its_source() {
    let (session, _rx) = start(2);
    let secret = "00".repeat(32);
    assert!(
        session
            .begin_punch("child", "missing", Vec::new(), &secret)
            .is_err()
    );
    let preparation = session
        .begin_punch("receiver", "", Vec::new(), &secret)
        .unwrap();
    drop(preparation);
    session.inner.engine.close_punch("receiver");
    let upstream = Upstream::new(false).await;
    let offer = upstream.offer("upstream-edge").await;
    session
        .accept_offer("upstream", &offer, Vec::new(), false, false)
        .await
        .unwrap();
    assert!(
        session
            .begin_punch("upstream", "", Vec::new(), &secret)
            .is_err(),
        "an existing receiver identity was prepared again"
    );
    eventually_free(&session, &secret).await;
    session.close_session().await.unwrap();
}

/// Retries until the engine's single preparation slot is free.
pub(crate) async fn eventually_free(session: &Session, secret: &str) {
    for _ in 0..200 {
        if let Ok(preparation) = session.begin_punch("child", "upstream", Vec::new(), secret) {
            drop(preparation);
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("a source-bound preparation was never admitted");
}

fn candidate_for(
    event: &MediaEvent,
    id: &str,
) -> Option<Option<piik_native_control::wire::IceCandidateInit>> {
    match &event.kind {
        MediaEventKind::EdgeCandidate {
            connection_id,
            candidate,
        } if connection_id == id => Some(candidate.clone()),
        _ => None,
    }
}

/// Input crosses the upstream receiver and a local playback edge, and a relay
/// child carries the return path.
#[tokio::test]
async fn input_channel_uses_playback_and_relay() {
    let (session, mut rx) = start(2);
    let upstream = Upstream::new(false).await;
    let (host_tx, mut host_rx) = mpsc::unbounded_channel::<EdgeEvent>();
    let host_sink: piik_media_edge::EventSink = Arc::new(move |event| {
        let _ = host_tx.send(event);
    });
    let host = upstream.edge("upstream-edge", true, Some(host_sink)).await;
    let offer = host.create_local_offer("offer", false).await.unwrap();
    assert!(offer.contains("\nm=application "));
    let result = session
        .accept_offer("upstream", &offer, Vec::new(), false, false)
        .await
        .unwrap();
    host.set_answer(&result.answer_sdp, "offer").await.unwrap();
    // The receiver trickles; the local Host edge sent complete SDP.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let mut host_channel = None;
    let mut texts: Vec<String> = Vec::new();
    while (host_channel.is_none() || texts.is_empty()) && tokio::time::Instant::now() < deadline {
        while let Ok(event) = rx.try_recv() {
            if let Some(candidate) = candidate_for(&event, "upstream") {
                let _ = host
                    .add_remote_candidate(candidate.map(piik_native_host::bridge::edge_candidate))
                    .await;
            }
        }
        while let Ok(event) = host_rx.try_recv() {
            match event {
                EdgeEvent::Channel(ChannelEvent::Open(channel)) => host_channel = Some(channel),
                EdgeEvent::Channel(ChannelEvent::Data {
                    data,
                    binary: false,
                    ..
                }) => {
                    texts.push(String::from_utf8_lossy(&data).into_owned());
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    // The Viewer relay announces readiness upstream once its channel opens.
    assert_eq!(texts.first().map(String::as_str), Some(r#"{"ready":true}"#));
    let host_channel = host_channel.expect("upstream input channel did not open");
    // The Host root answers ready; the Viewer relays it to its playback.
    host_channel.send_text(r#"{"ready":true}"#);
    // The App window plays the Viewer's local edge with its own input channel.
    let playback = loopback_engine();
    let (play_tx, mut play_rx) = mpsc::unbounded_channel::<EdgeEvent>();
    let play_sink: piik_media_edge::EventSink = Arc::new(move |event| {
        let _ = play_tx.send(event);
    });
    let local_offer = session
        .prepare_local_edge("upstream", "playback", "local-offer", false)
        .await
        .unwrap();
    let (window, answer) = playback
        .new_receiver(piik_media_edge::ReceiverOptions {
            offer: local_offer,
            edge_capacity: 1,
            events: Some(play_sink),
            ..piik_media_edge::ReceiverOptions::default()
        })
        .await
        .unwrap();
    session
        .set_answer("playback", &answer, "local-offer")
        .await
        .unwrap();
    let mut local = None;
    let mut relayed_ready = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !(local.is_some() && relayed_ready) && tokio::time::Instant::now() < deadline {
        while let Ok(event) = play_rx.try_recv() {
            match event {
                EdgeEvent::LocalCandidate { candidate, current } if current() => {
                    let _ = session
                        .add_candidate(
                            "playback",
                            candidate.map(piik_native_host::bridge::wire_candidate),
                        )
                        .await;
                }
                EdgeEvent::Channel(ChannelEvent::Open(channel)) => local = Some(channel),
                EdgeEvent::Channel(ChannelEvent::Data {
                    data,
                    binary: false,
                    ..
                }) => {
                    relayed_ready |= data == br#"{"ready":true}"#;
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let local = local.expect("local input channel did not open");
    assert!(relayed_ready, "Host readiness did not reach playback");
    const PACKET: &str = r#"{"id":"0123456789abcdef0123456789abcdef","epoch":"0123456789abcdef0123456789abcdef","data":"encrypted"}"#;
    local.send_text(PACKET);
    let forwarded = loop {
        match wait_text(&mut host_rx, Duration::from_secs(5)).await {
            Some(text) if text == PACKET => break true,
            Some(_) => continue,
            None => break false,
        }
    };
    assert!(forwarded, "playback input did not reach the Host");
    window.close().await;
    session
        .update_profile(VideoProfile {
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 2_000_000,
            preference: "balanced".into(),
        })
        .await
        .unwrap();
    let relay = session
        .prepare_edge("upstream", "relay-child", Vec::new(), false)
        .await
        .unwrap();
    assert!(
        relay.contains("\nm=application "),
        "relay child has no input return path"
    );
    session.close_session().await.unwrap();
}

async fn wait_text(
    events: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    limit: Duration,
) -> Option<String> {
    tokio::time::timeout(limit, async {
        while let Some(event) = events.recv().await {
            if let EdgeEvent::Channel(ChannelEvent::Data {
                data,
                binary: false,
                ..
            }) = event
            {
                return Some(String::from_utf8_lossy(&data).into_owned());
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}
