//! Fake media, sources and outbox for control tests.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::BoxFuture;
use piik_loopback::{CancellationToken, ControlEvent, ControlSession, ProtocolViolation};
use piik_native_capture::{
    Capabilities, CaptureTarget, Error, Microphone, VideoOptions, VideoProfile,
};
use piik_protocol::{PunchPrediction, RemoteInputPeer};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::capture::SourceProvider;
use crate::events::Outbox;
use crate::media::*;
use crate::session::{Config, Session};
use crate::wire::IceCandidateInit;

pub(crate) struct TestOutbox(mpsc::Sender<ControlEvent>);

impl Outbox for TestOutbox {
    fn send(&self, event: ControlEvent) -> BoxFuture<'_, bool> {
        Box::pin(async move { self.0.send(event).await.is_ok() })
    }
}

pub(crate) struct FakeSources;

impl SourceProvider for FakeSources {
    fn list_sources(&self) -> BoxFuture<'_, Result<Vec<CaptureTarget>, Error>> {
        Box::pin(async { Err(Error::Invalid("native capture source list is unavailable")) })
    }

    fn preview_source<'a>(
        &'a self,
        _target: &'a CaptureTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, Error>> {
        Box::pin(async { Err(Error::Invalid("native capture preview is unavailable")) })
    }

    fn list_microphones(&self) -> BoxFuture<'_, Result<Vec<Microphone>, Error>> {
        Box::pin(async {
            Ok(vec![Microphone {
                id: "device".into(),
                label: "Microphone".into(),
            }])
        })
    }
}

#[derive(Default)]
pub(crate) struct FakePunch;

impl PunchPreparation for FakePunch {
    fn prepare(&self) -> BoxFuture<'_, MediaResult<Option<PunchPrediction>>> {
        Box::pin(async { Ok(None) })
    }

    fn run(&self, _remote: PunchPrediction) -> BoxFuture<'_, bool> {
        Box::pin(async { false })
    }
}

#[derive(Default)]
pub(crate) struct Punches(Mutex<HashMap<String, Arc<FakePunch>>>);

impl Punches {
    fn begin(&self, connection_id: &str) -> Arc<dyn PunchPreparation> {
        let punch = Arc::new(FakePunch);
        self.0
            .lock()
            .unwrap()
            .insert(connection_id.to_owned(), punch.clone());
        punch
    }

    fn get(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>> {
        let punch = self.0.lock().unwrap().get(connection_id).cloned()?;
        Some(punch)
    }

    fn close(&self, connection_id: &str) {
        self.0.lock().unwrap().remove(connection_id);
    }
}

/// How a fake capture mutation completes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Outcome {
    Succeed,
    SourceCanceled,
    /// Waits for [`FakeHost::release`] or close.
    Block,
}

pub(crate) struct FakeHost {
    pub share_id: String,
    pub options: HostOptions,
    pub closed: CancellationToken,
    ended: CancellationToken,
    end_result: Mutex<Option<HostEnded>>,
    release: CancellationToken,
    pub outcome: Mutex<Outcome>,
    edges: Mutex<HashSet<String>>,
    punches: Punches,
    pub calls: Mutex<Vec<String>>,
}

impl FakeHost {
    fn new(options: HostOptions) -> Self {
        Self {
            share_id: options.share_id.clone(),
            options,
            closed: CancellationToken::new(),
            ended: CancellationToken::new(),
            end_result: Mutex::new(None),
            release: CancellationToken::new(),
            outcome: Mutex::new(Outcome::Succeed),
            edges: Mutex::default(),
            punches: Punches::default(),
            calls: Mutex::default(),
        }
    }

    /// The underlying owner ends the share.
    pub fn end(&self, result: HostEnded) {
        *self.end_result.lock().unwrap() = Some(result);
        self.ended.cancel();
    }

    pub fn release(&self) {
        self.release.cancel();
    }

    fn record(&self, call: &str) {
        self.calls.lock().unwrap().push(call.to_owned());
    }

    async fn mutate(&self, call: &str) -> MediaResult<()> {
        self.record(call);
        let outcome = *self.outcome.lock().unwrap();
        match outcome {
            Outcome::Succeed => Ok(()),
            Outcome::SourceCanceled => Err(MediaError::SourceCanceled),
            Outcome::Block => tokio::select! {
                () = self.release.cancelled() => Ok(()),
                () = self.closed.cancelled() => Err(MediaError::failed("native share is closed")),
            },
        }
    }

    fn offer(&self, connection_id: &str) -> MediaResult<String> {
        if self.closed.is_cancelled()
            || !self.edges.lock().unwrap().insert(connection_id.to_owned())
        {
            return Err(MediaError::failed("native media edge already exists"));
        }
        Ok("v=0 offer".into())
    }
}

impl OutboundMedia for FakeHost {
    fn share_id(&self) -> &str {
        &self.share_id
    }

    fn set_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn add_candidate<'a>(
        &'a self,
        _: &'a str,
        _: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn close_edge<'a>(&'a self, connection_id: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.edges.lock().unwrap().remove(connection_id);
        })
    }

    fn punch(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>> {
        self.punches.get(connection_id)
    }

    fn close_punch(&self, connection_id: &str) {
        self.punches.close(connection_id);
    }
}

impl HostMedia for FakeHost {
    fn codec(&self) -> String {
        self.options.video.codec.clone()
    }

    fn has_audio(&self) -> bool {
        self.options.audio_enabled
    }

    fn source_audio(&self) -> Option<bool> {
        None
    }

    fn prepare_edge<'a>(
        &'a self,
        connection_id: &'a str,
        _: Vec<IceServerUrls>,
        _: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(async move { self.offer(connection_id) })
    }

    fn prepare_local_edge<'a>(
        &'a self,
        connection_id: &'a str,
        _: &'a str,
        _: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(async move { self.offer(connection_id) })
    }

    fn begin_punch(
        &self,
        connection_id: &str,
        _: Vec<IceServerUrls>,
        _: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>> {
        Ok(self.punches.begin(connection_id))
    }

    fn set_paused(&self, paused: bool) {
        self.record(&format!("paused:{paused}"));
    }

    fn update_profile(&self, _: QualityProfile) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(self.mutate("update"))
    }

    fn replace_source(
        &self,
        options: VideoOptions,
        _: bool,
        _: Option<CaptureTarget>,
    ) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async move {
            self.record(&format!("border:{}", options.show_capture_border));
            self.mutate("replace").await
        })
    }

    fn set_microphone(
        &self,
        enabled: Option<bool>,
        gain: Option<f64>,
        device_id: Option<String>,
    ) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async move {
            if gain.is_some() {
                self.record("gain");
                return Ok(());
            }
            let _ = (enabled, device_id);
            self.mutate("microphone").await
        })
    }

    fn set_volume(&self, _: f64) -> MediaResult<()> {
        Ok(())
    }

    fn remote_input_changed(&self) -> BoxFuture<'_, ()> {
        Box::pin(std::future::pending())
    }

    fn remote_input_state(&self) -> piik_remote_input::State {
        piik_remote_input::State::default()
    }

    fn prepare_remote_input_policy(&self, _: &str, _: &str, _: bool) -> MediaResult<PolicyApply> {
        Err(piik_remote_input::Error::Unavailable.into())
    }

    fn set_remote_input_peers(&self, _: &[RemoteInputPeer]) -> MediaResult<()> {
        Err(piik_remote_input::Error::Unavailable.into())
    }

    fn decide_remote_input(&self, _: &str, _: bool) -> MediaResult<()> {
        Err(piik_remote_input::Error::Unavailable.into())
    }

    fn set_remote_input_simultaneous(&self, _: bool) -> MediaResult<()> {
        Err(piik_remote_input::Error::Unavailable.into())
    }

    fn prepare_publication<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Vec<IceServerUrls>,
    ) -> BoxFuture<'a, MediaResult<(String, PublicationMedia)>> {
        Box::pin(async { Err(MediaError::failed("publication is unavailable")) })
    }

    fn publication(&self, _: &str, _: &str) -> MediaResult<Arc<dyn Publication>> {
        Err(MediaError::failed("native publication does not exist"))
    }

    fn close_publication(&self, _: &str, _: &str) {}

    fn done(&self) -> BoxFuture<'_, Option<HostEnded>> {
        Box::pin(async move {
            tokio::select! {
                () = self.closed.cancelled() => None,
                () = self.ended.cancelled() => self.end_result.lock().unwrap().take(),
            }
        })
    }

    fn close(&self) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async move {
            self.closed.cancel();
            Ok(())
        })
    }
}

/// How a fake Host start completes.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum StartMode {
    Succeed,
    SourceCanceled,
    /// Waits for its cancellation.
    Block,
}

pub(crate) struct FakeHostFactory {
    pub mode: Mutex<StartMode>,
    pub hosts: Mutex<Vec<Arc<FakeHost>>>,
}

impl HostMediaFactory for FakeHostFactory {
    fn start(
        &self,
        cancel: CancellationToken,
        options: HostOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn HostMedia>>> {
        Box::pin(async move {
            let mode = *self.mode.lock().unwrap();
            match mode {
                StartMode::SourceCanceled => return Err(MediaError::SourceCanceled),
                StartMode::Block => {
                    cancel.cancelled().await;
                    return Err(MediaError::failed("native capture start was canceled"));
                }
                StartMode::Succeed => {}
            }
            let host = Arc::new(FakeHost::new(options));
            self.hosts.lock().unwrap().push(host.clone());
            Ok(host as Arc<dyn HostMedia>)
        })
    }
}

pub(crate) struct FakeViewer {
    share_id: String,
    punches: Punches,
}

impl OutboundMedia for FakeViewer {
    fn share_id(&self) -> &str {
        &self.share_id
    }

    fn set_answer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async { Err(MediaError::failed("native relay edge does not exist")) })
    }

    fn add_candidate<'a>(
        &'a self,
        _: &'a str,
        _: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn close_edge<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn punch(&self, connection_id: &str) -> Option<Arc<dyn PunchPreparation>> {
        self.punches.get(connection_id)
    }

    fn close_punch(&self, connection_id: &str) {
        self.punches.close(connection_id);
    }
}

impl ViewerMedia for FakeViewer {
    fn update_profile(&self, _: VideoProfile) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn accept_offer<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Vec<IceServerUrls>,
        reuse: bool,
        _: bool,
    ) -> BoxFuture<'a, MediaResult<OfferResult>> {
        Box::pin(async move {
            Ok(OfferResult {
                answer_sdp: "v=0 answer".into(),
                audio: false,
                codec: "vp8".into(),
                reused: reuse,
            })
        })
    }

    fn add_receiver_candidate<'a>(
        &'a self,
        _: &'a str,
        _: Option<IceCandidateInit>,
    ) -> BoxFuture<'a, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }

    fn close_receiver<'a>(&'a self, _: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async {})
    }

    fn prepare_edge<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: Vec<IceServerUrls>,
        _: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(async { Err(MediaError::failed("native relay source does not exist")) })
    }

    fn prepare_local_edge<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: bool,
    ) -> BoxFuture<'a, MediaResult<String>> {
        Box::pin(async { Err(MediaError::failed("native relay source does not exist")) })
    }

    fn begin_punch(
        &self,
        connection_id: &str,
        _: &str,
        _: Vec<IceServerUrls>,
        _: &str,
    ) -> MediaResult<Arc<dyn PunchPreparation>> {
        Ok(self.punches.begin(connection_id))
    }

    fn close(&self) -> BoxFuture<'_, MediaResult<()>> {
        Box::pin(async { Ok(()) })
    }
}

pub(crate) struct FakeViewerFactory;

impl ViewerMediaFactory for FakeViewerFactory {
    fn start(
        &self,
        _: CancellationToken,
        options: ViewerOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn ViewerMedia>>> {
        Box::pin(async move {
            Ok(Arc::new(FakeViewer {
                share_id: options.share_id,
                punches: Punches::default(),
            }) as Arc<dyn ViewerMedia>)
        })
    }
}

pub(crate) struct Harness {
    pub session: Session,
    pub events: mpsc::Receiver<ControlEvent>,
    /// Fills the outbox to model socket backpressure.
    pub filler: mpsc::Sender<ControlEvent>,
    pub hosts: Arc<FakeHostFactory>,
    cancel: CancellationToken,
}

pub(crate) fn harness(capabilities: Capabilities) -> Harness {
    harness_with_queue(capabilities, piik_loopback::EVENT_QUEUE_SIZE)
}

pub(crate) fn harness_with_queue(capabilities: Capabilities, queue: usize) -> Harness {
    let (sender, events) = mpsc::channel(queue);
    let hosts = Arc::new(FakeHostFactory {
        mode: Mutex::new(StartMode::Succeed),
        hosts: Mutex::default(),
    });
    let config = Config {
        capabilities,
        port_mapping: false,
        remote_input: None,
        sources: Arc::new(FakeSources),
        host: hosts.clone(),
        viewer: Arc::new(FakeViewerFactory),
    };
    Harness {
        filler: sender.clone(),
        session: Session::new(config, Arc::new(TestOutbox(sender))),
        events,
        hosts,
        cancel: CancellationToken::new(),
    }
}

/// Applies the socket writer's staleness and admission hooks.
pub(crate) fn deliver(event: ControlEvent) -> Option<Value> {
    if event.current.as_ref().is_some_and(|current| !current()) {
        return None;
    }
    if let Some(before_write) = event.before_write {
        before_write();
    }
    Some(event.value)
}

impl Harness {
    pub async fn handle(&self, payload: &str) -> Result<Option<Value>, ProtocolViolation> {
        let event = self
            .session
            .handle(&self.cancel, payload.as_bytes())
            .await?;
        Ok(event.and_then(deliver))
    }

    /// The synchronous response; panics on a violation or async completion.
    pub async fn respond(&self, payload: &str) -> Value {
        match self.handle(payload).await {
            Ok(Some(value)) => value,
            other => panic!("{payload}: {other:?}"),
        }
    }

    pub async fn violation(&self, payload: &str) -> String {
        match self.handle(payload).await {
            Err(violation) => violation.0,
            other => panic!("accepted {payload}: {other:?}"),
        }
    }

    pub async fn assert_failed(&self, payload: &str) {
        let value = self.respond(payload).await;
        assert_eq!(value["type"], "request-failed", "{payload}: {value}");
        assert_eq!(value["code"], "operation-failed", "{payload}: {value}");
    }

    /// The next delivered event.
    pub async fn next_event(&mut self) -> Value {
        loop {
            let event = tokio::time::timeout(Duration::from_secs(5), self.events.recv())
                .await
                .expect("event")
                .expect("open outbox");
            if let Some(value) = deliver(event) {
                return value;
            }
        }
    }

    /// The response to `payload`, synchronous or completed through events.
    pub async fn await_response(&mut self, payload: &str) -> Value {
        if let Some(value) = self.handle(payload).await.expect("valid request") {
            return value;
        }
        let id: Value = serde_json::from_str(payload).unwrap();
        loop {
            let value = self.next_event().await;
            if value["id"] == id["id"] {
                return value;
            }
        }
    }

    pub fn host(&self, index: usize) -> Arc<FakeHost> {
        self.hosts.hosts.lock().unwrap()[index].clone()
    }

    pub async fn close(&self) {
        self.session.close().await;
    }
}
