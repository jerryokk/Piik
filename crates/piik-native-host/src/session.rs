//! One native Host share: the capture generation and its single video reader,
//! source audio and the optional mix, P2P edges, the local App edge, SFU
//! publications and the share's remote-input session.
//!
//! Submodules group the share's responsibilities: [`control`] holds the
//! control-facing API, [`capture`] the capture generation lifecycle,
//! [`video`] the video reader, and [`tasks`] the share's background loops.

mod capture;
mod control;
mod tasks;
mod video;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::Duration;

use futures_util::FutureExt;
use piik_media_edge::{AudioSource, Edge, EdgeEvent, Engine, EngineOptions, VideoSource};
use piik_media_transport::DataChannel;
use piik_native_capture::{CaptureTarget, VideoOptions};
use piik_native_control::media::{
    HostEnded, MediaError, MediaEventKind, MediaResult, QualityProfile,
};
use piik_remote_input::Manager;
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::audio_mix::AudioMix;
use crate::bridge::{self, Current, Emitter};
use crate::capture::{CaptureProvider, SharedStream};
use crate::publication::HostPublication;

#[cfg(test)]
pub(crate) use capture::{apply_output_plan, wait_for_capture_profile};
pub(crate) use tasks::owns_publication;

pub(crate) const START_TIMEOUT: Duration = Duration::from_secs(5);
const QUALITY_SAMPLE_PERIOD: Duration = Duration::from_secs(2);
const MAX_SHARE_ID_BYTES: usize = 256;

pub(crate) type InputSession = piik_remote_input::Session<DataChannel>;

/// `nativehost.Options`.
pub struct Options {
    pub share_id: String,
    pub capture: Arc<dyn CaptureProvider>,
    pub video: VideoOptions,
    pub profile: QualityProfile,
    pub audio_enabled: bool,
    pub exclude_audio: Option<CaptureTarget>,
    pub microphone_mixing: bool,
    pub edge_capacity: usize,
    /// IPv4 `address:port`; empty binds every interface.
    pub bind_address: String,
    pub port_mapping: bool,
    pub remote_input: Option<Arc<Manager>>,
    pub events: Arc<dyn bridge::EventSink>,
}

pub(crate) type PublicationKey = (String, String);

/// One owned edge. The token is the identity a retired edge's late events
/// are fenced against, compared by pointer.
#[derive(Clone)]
pub(crate) struct OwnedEdge {
    pub token: Arc<()>,
    pub edge: Edge,
}

#[derive(Clone)]
pub(crate) struct OwnedPublication {
    pub token: Arc<()>,
    pub publication: Arc<HostPublication>,
}

pub(crate) struct State {
    pub stream: SharedStream,
    pub audio_stream: Option<SharedStream>,
    pub source: Option<VideoSource>,
    pub audio_source: Option<AudioSource>,
    pub video_options: VideoOptions,
    pub profile: QualityProfile,
    pub edges: HashMap<String, OwnedEdge>,
    pub publications: HashMap<PublicationKey, OwnedPublication>,
    /// Closed (cancelled) once the video reader installed the committed stream.
    pub capture_applied: Option<CancellationToken>,
    pub capture_unavailable: Vec<usize>,
    pub paused: bool,
    pub closed: bool,
}

pub(crate) struct Inner {
    pub share_id: String,
    pub capture: Arc<dyn CaptureProvider>,
    pub edge_capacity: usize,
    pub engine: Engine,
    pub emitter: Emitter,
    pub ctx: CancellationToken,
    pub state: Mutex<State>,
    /// Serializes capture operations across their awaits.
    pub update: tokio::sync::Mutex<()>,
    /// Cancelled once video first became active.
    pub video_active: CancellationToken,
    /// Single-permit wakes: repeated notifications coalesce into one.
    pub audio_changed: Notify,
    pub capture_changed: Notify,
    pub mixer: Mutex<Option<Arc<AudioMix>>>,
    pub remote_input: Option<InputSession>,
    pub volume: watch::Sender<f64>,
    /// Set once `run` finished retirement.
    pub ended: watch::Sender<bool>,
    pub ended_result: Mutex<Option<Option<HostEnded>>>,
}

/// `nativehost.Session`. Cheap to clone.
#[derive(Clone)]
pub struct Session {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("share_id", &self.inner.share_id)
            .finish_non_exhaustive()
    }
}

pub(crate) fn canceled() -> MediaError {
    MediaError::failed("context canceled")
}

fn unavailable() -> MediaError {
    MediaError::failed("native share is unavailable")
}

impl Session {
    /// Starts the share. `parent` owns the share: cancelling it ends the share.
    pub async fn start(parent: &CancellationToken, mut options: Options) -> MediaResult<Self> {
        if options.share_id.is_empty() || options.share_id.len() > MAX_SHARE_ID_BYTES {
            return Err(MediaError::failed("native share identity is invalid"));
        }
        if !(1..=piik_media_edge::MAX_RELAY_CHILDREN).contains(&options.edge_capacity) {
            return Err(MediaError::failed("native share edge capacity is invalid"));
        }
        if !options.profile.valid() || options.video.profile != options.profile.video {
            return Err(MediaError::failed("native share profile is invalid"));
        }
        let engine = Engine::new(EngineOptions {
            bind_address: options.bind_address.clone(),
            include_loopback: true,
            port_mapping: options.port_mapping,
            initial_bitrate: i64::from(options.profile.video.bitrate),
        })
        .map_err(bridge::media_error)?;
        options.video.output_groups = options
            .edge_capacity
            .min(piik_native_capture::MAX_OUTPUT_GROUPS);
        let stream = match options.capture.video(parent, options.video.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                let _ = engine.close().await;
                return Err(MediaError::Capture(error));
            }
        };
        let ctx = parent.child_token();
        let (emitter, _forwarder) = Emitter::new(&options.share_id, options.events, ctx.clone());
        let session = Self {
            inner: Arc::new(Inner {
                share_id: options.share_id.clone(),
                capture: options.capture.clone(),
                edge_capacity: options.edge_capacity,
                engine,
                emitter,
                ctx: ctx.clone(),
                state: Mutex::new(State {
                    stream: stream.clone(),
                    audio_stream: None,
                    source: None,
                    audio_source: None,
                    video_options: options.video.clone(),
                    profile: options.profile.clone(),
                    edges: HashMap::new(),
                    publications: HashMap::new(),
                    capture_applied: None,
                    capture_unavailable: Vec::new(),
                    paused: false,
                    closed: false,
                }),
                update: tokio::sync::Mutex::new(()),
                video_active: CancellationToken::new(),
                audio_changed: Notify::new(),
                capture_changed: Notify::new(),
                mixer: Mutex::new(None),
                remote_input: options
                    .remote_input
                    .as_ref()
                    .and_then(|manager| manager.new_session()),
                volume: watch::Sender::new(1.0),
                ended: watch::Sender::new(false),
                ended_result: Mutex::new(None),
            }),
        };
        let inner = &session.inner;
        inner.sync_remote_input_source();
        // Drain video while the optional audio process initializes. Otherwise
        // a full video pipe can stall the platform capture service audio needs.
        let (ready_tx, ready_rx) = oneshot::channel();
        let (video_done_tx, mut video_done) = oneshot::channel();
        // Source audio may still await platform permission when video ends;
        // its context lives with video, including after a successful start.
        let audio_ctx = ctx.child_token();
        {
            let inner = inner.clone();
            let audio_ctx = audio_ctx.clone();
            tokio::spawn(async move {
                let result = inner.run_video(ready_tx).await;
                audio_ctx.cancel();
                let _ = video_done_tx.send(result);
            });
        }
        let timeout = async {
            if options.video.target.kind == "picker" {
                std::future::pending::<()>().await;
            }
            tokio::time::sleep(START_TIMEOUT).await;
        };
        let ready = tokio::select! {
            ready = ready_rx => ready.unwrap_or_else(|_| Err(MediaError::failed("native capture stopped unexpectedly"))),
            () = timeout => Err(MediaError::failed("native capture did not start in time")),
            () = parent.cancelled() => Err(canceled()),
        };
        if let Err(error) = ready {
            ctx.cancel();
            let error = bridge::join_close(error, stream.close().await);
            let _ = video_done.await;
            inner.retire().await;
            return Err(error);
        }
        // A fixed audio shape returns in the start response. Mixed audio has a
        // stable output, so its optional input attaches later.
        let mut audio_stream = None;
        if options.audio_enabled && !options.microphone_mixing {
            match inner
                .start_source_audio(
                    &audio_ctx,
                    &options.video.target,
                    options.exclude_audio.clone(),
                )
                .await
            {
                Ok(stream) => audio_stream = Some(stream),
                Err(error) => tracing::debug!(event = "capture-audio-unavailable",
                    share = %piik_diagnostics::id(&options.share_id), error = %error, "piik-client"),
            }
            inner.lock().audio_stream = audio_stream.clone();
        }
        if let Ok(video_result) = video_done.try_recv() {
            ctx.cancel();
            let _ = stream.close().await;
            if let Some(audio) = &audio_stream {
                let _ = audio.close().await;
            }
            inner.retire().await;
            return Err(video_result
                .err()
                .unwrap_or_else(|| MediaError::failed("native capture stopped unexpectedly")));
        }
        if audio_stream.is_some() || options.microphone_mixing {
            let capacity = piik_media_edge::physical_source_capacity(options.edge_capacity);
            match inner
                .engine
                .new_audio_source(capacity, options.profile.audio_bitrate)
                .await
            {
                Ok(source) => inner.lock().audio_source = Some(source),
                Err(error) => {
                    tracing::debug!(event = "capture-audio-source-failed",
                        share = %piik_diagnostics::id(&options.share_id), error = %error, "piik-client");
                    if let Some(audio) = audio_stream.take() {
                        let _ = audio.close().await;
                    }
                    inner.lock().audio_stream = None;
                }
            }
        }
        if options.microphone_mixing {
            let Some(output) = inner.lock().audio_source.clone() else {
                ctx.cancel();
                let _ = stream.close().await;
                let _ = video_done.await;
                inner.retire().await;
                return Err(MediaError::failed("native mixed audio could not start"));
            };
            let mixer = AudioMix::new(ctx.clone(), output, inner.emitter.clone());
            mixer.set_source(audio_stream.clone());
            *inner.mixer_lock() = Some(mixer);
        }
        let audio_starting = (options.audio_enabled && inner.mixer().is_some()).then(|| {
            let inner = inner.clone();
            let target = options.video.target.clone();
            let exclude = options.exclude_audio.clone();
            tokio::spawn(async move { inner.start_mixed_source(audio_ctx, target, exclude).await })
        });
        tokio::spawn(inner.clone().run(video_done, audio_starting));
        if ctx.is_cancelled() {
            let _ = session.close_session().await;
            return Err(canceled());
        }
        Ok(session)
    }
}

impl Inner {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn mixer_lock(&self) -> MutexGuard<'_, Option<Arc<AudioMix>>> {
        self.mixer.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn mixer(&self) -> Option<Arc<AudioMix>> {
        self.mixer_lock().clone()
    }

    pub(crate) fn emit(&self, kind: MediaEventKind, current: Option<Current>) {
        self.emitter.emit(kind, current);
    }

    pub(crate) fn current_stream(&self) -> Option<SharedStream> {
        let state = self.lock();
        (!state.closed).then(|| state.stream.clone())
    }

    pub(crate) fn current_audio_stream(&self) -> Option<SharedStream> {
        let state = self.lock();
        if state.closed {
            None
        } else {
            state.audio_stream.clone()
        }
    }

    fn edge(&self, connection_id: &str) -> Option<Edge> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        state
            .edges
            .get(connection_id)
            .map(|owned| owned.edge.clone())
    }

    /// Binds the share's current source to its input permission.
    pub(crate) fn sync_remote_input_source(&self) {
        if let Some(input) = &self.remote_input {
            let target = self.lock().video_options.target.clone();
            input.set_target(target);
        }
    }

    pub(crate) fn clear_remote_input_source(&self) {
        if let Some(input) = &self.remote_input {
            input.set_target(CaptureTarget::default());
        }
    }

    /// The edge event sink: media facts become control events fenced by
    /// `token`; the input channel joins the share's remote-input route.
    fn edge_sink(
        self: &Arc<Self>,
        connection_id: &str,
        token: &Arc<()>,
    ) -> piik_media_edge::EventSink {
        let weak = Arc::downgrade(self);
        let connection_id = connection_id.to_owned();
        let owns = owner_check(&weak, &connection_id, token);
        let input = InputLeg::default();
        Arc::new(move |event: EdgeEvent| {
            let Some(inner) = weak.upgrade() else { return };
            if let Some(fact) = bridge::edge_fact(&event) {
                bridge::emit_edge_fact(&inner.emitter, &connection_id, fact, Some(owns.clone()));
                return;
            }
            if let Some(session) = &inner.remote_input {
                input.handle(session, &connection_id, event);
            }
        })
    }

    async fn prepare_edge(
        self: &Arc<Self>,
        connection_id: &str,
        ice_servers: Vec<piik_media_edge::IceServerUrls>,
        local_offer_id: &str,
        punched: bool,
    ) -> MediaResult<String> {
        let unavailable = || MediaError::failed("native media edge is unavailable");
        let (source, audio) = {
            let state = self.lock();
            if state.closed || state.edges.contains_key(connection_id) {
                return Err(unavailable());
            }
            (state.source.clone(), state.audio_source.clone())
        };
        let source = source.ok_or_else(unavailable)?;
        let token = Arc::new(());
        let edge = self
            .engine
            .new_edge(
                &source,
                piik_media_edge::EdgeOptions {
                    connection_id: connection_id.to_owned(),
                    ice_servers,
                    audio,
                    local: !local_offer_id.is_empty(),
                    punched,
                    events: Some(self.edge_sink(connection_id, &token)),
                },
            )
            .await
            .map_err(bridge::media_error)?;
        let installed = {
            let mut state = self.lock();
            let free = !state.closed && !state.edges.contains_key(connection_id);
            if free {
                state.edges.insert(
                    connection_id.to_owned(),
                    OwnedEdge {
                        token,
                        edge: edge.clone(),
                    },
                );
            }
            free
        };
        if !installed {
            edge.close().await;
            return Err(unavailable());
        }
        if self.remote_input.is_some() {
            // The channel's handle arrives with its open event.
            let _ = edge
                .open_data_channel(piik_remote_input::CHANNEL_LABEL)
                .await;
        }
        let offer = if local_offer_id.is_empty() {
            edge.create_offer().await
        } else {
            edge.create_local_offer(local_offer_id, false).await
        };
        match offer {
            Ok(offer) => Ok(offer),
            Err(error) => {
                self.close_edge(connection_id).await;
                Err(bridge::media_error(error))
            }
        }
    }

    pub(crate) async fn close_edge(&self, connection_id: &str) {
        self.engine.close_punch(connection_id);
        let owned = self.lock().edges.remove(connection_id);
        if let Some(owned) = owned {
            owned.edge.close().await;
        }
    }

    /// Waits for active video before starting source audio. A starting status
    /// admits control and progress; only active proves that platform video
    /// initialization finished. The video reader keeps draining meanwhile.
    pub(crate) async fn start_source_audio(
        &self,
        ctx: &CancellationToken,
        target: &CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) -> MediaResult<SharedStream> {
        tokio::select! {
            () = self.video_active.cancelled() => {}
            () = ctx.cancelled() => return Err(canceled()),
        }
        if ctx.is_cancelled() {
            return Err(canceled());
        }
        Ok(self.capture.audio(ctx, target.clone(), exclude).await?)
    }

    /// The initial mixed source input: it waits for active video without
    /// holding up a source or quality change, and a later source-audio
    /// commit supersedes it.
    async fn start_mixed_source(
        self: Arc<Self>,
        audio_ctx: CancellationToken,
        target: CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) {
        tokio::select! {
            () = self.video_active.cancelled() => {}
            () = self.audio_changed.notified() => return,
            () = audio_ctx.cancelled() => return,
        }
        let _update = self.update.lock().await;
        if self.audio_changed.notified().now_or_never().is_some() {
            return;
        }
        let input = match self.start_source_audio(&audio_ctx, &target, exclude).await {
            Ok(input) => input,
            Err(error) => {
                tracing::debug!(event = "capture-audio-unavailable",
                    share = %piik_diagnostics::id(&self.share_id), error = %error, "piik-client");
                return;
            }
        };
        let installed = {
            let mut state = self.lock();
            let open = !audio_ctx.is_cancelled() && !state.closed;
            if open {
                state.audio_stream = Some(input.clone());
            }
            open
        };
        if !installed {
            let _ = input.close().await;
            return;
        }
        if let Some(mixer) = self.mixer() {
            mixer.set_source(Some(input));
        }
    }

    /// Both explicit close and source termination retire these idempotent
    /// owners. It never waits for `run`: close uses it to unblock readers.
    pub(crate) async fn retire(&self) {
        if let Some(input) = &self.remote_input {
            input.close();
        }
        let (source, stream, audio_source, audio_stream) = {
            let state = self.lock();
            (
                state.source.clone(),
                state.stream.clone(),
                state.audio_source.clone(),
                state.audio_stream.clone(),
            )
        };
        if let Some(source) = source {
            source.close().await;
        }
        let _ = stream.close().await;
        if let Some(audio) = audio_stream {
            let _ = audio.close().await;
        }
        if let Some(audio) = audio_source {
            audio.close().await;
        }
        let _ = self.engine.close().await;
    }

    async fn wait_ended(&self) {
        let mut ended = self.ended.subscribe();
        let _ = ended.wait_for(|ended| *ended).await;
    }
}

/// Whether `token` still owns `connection_id`'s edge.
fn owner_check(weak: &Weak<Inner>, connection_id: &str, token: &Arc<()>) -> Current {
    let weak = weak.clone();
    let connection_id = connection_id.to_owned();
    let token = Arc::downgrade(token);
    Arc::new(move || {
        let (Some(inner), Some(token)) = (weak.upgrade(), token.upgrade()) else {
            return false;
        };
        let state = inner.lock();
        !state.closed
            && state
                .edges
                .get(&connection_id)
                .is_some_and(|owned| Arc::ptr_eq(&owned.token, &token))
    })
}

type Lost = Box<dyn Fn() + Send + Sync>;
/// The bound input channel and its transport-loss action.
type Bound = Option<(Arc<DataChannel>, Lost)>;

/// One edge's input channel binding and its transport-loss handling.
#[derive(Clone, Default)]
pub(crate) struct InputLeg(Arc<Mutex<Bound>>);

impl InputLeg {
    pub(crate) fn handle(&self, session: &InputSession, edge_id: &str, event: EdgeEvent) {
        use piik_input_route::InputChannel;
        use piik_media_edge::ChannelEvent;
        let mut leg = self.0.lock().unwrap_or_else(|error| error.into_inner());
        match event {
            EdgeEvent::Channel(ChannelEvent::Open(channel)) => {
                if channel.label() != piik_remote_input::CHANNEL_LABEL || leg.is_some() {
                    drop(leg);
                    channel.close();
                    return;
                }
                let lost = session.attach(edge_id, channel.clone());
                *leg = Some((channel.clone(), lost));
                drop(leg);
                session.route().down_opened(edge_id, &channel);
            }
            EdgeEvent::Channel(ChannelEvent::Data {
                channel,
                data,
                binary,
            }) => {
                drop(leg);
                session
                    .route()
                    .down_message(edge_id, &channel, &data, !binary);
            }
            EdgeEvent::Channel(ChannelEvent::Close(channel)) => {
                if leg
                    .as_ref()
                    .is_some_and(|(current, _)| Arc::ptr_eq(current, &channel))
                {
                    *leg = None;
                }
                drop(leg);
                session.route().down_closed(edge_id, &channel);
            }
            // Pion's ordered ICE observer: loss retires queued deliveries and
            // grants together while SCTP is retained for recovery.
            EdgeEvent::InputTransport(false) => {
                if let Some((_, lost)) = leg.as_ref() {
                    lost();
                }
            }
            _ => {}
        }
    }
}
