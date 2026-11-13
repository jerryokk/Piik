//! One control connection's native session: role ownership, admission slots
//! and asynchronous completion.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

use futures_util::future::BoxFuture;
use piik_loopback::{
    CancellationToken, ControlEvent, ControlSession, EventSink, ProtocolViolation,
};
use piik_native_capture::Capabilities;
use piik_remote_input::Manager;
use serde_json::Value;

use crate::capture::SourceProvider;
use crate::events::{Outbox, to_value};
use crate::media::{
    HostMedia, HostMediaFactory, MediaError, MediaEvents, OutboundMedia, ViewerMedia,
    ViewerMediaFactory, ViewerOptions,
};
use crate::wire::{
    EventEnvelope, RemoteControlStateEvent, RequestFailedResponse, ResponseEnvelope,
    ShareEndedEvent,
};

/// What every control connection's session shares.
#[derive(Clone)]
pub struct Config {
    /// The probed platform capture support.
    pub capabilities: Capabilities,
    pub port_mapping: bool,
    /// The App's remote input owner; `None` leaves the stop shortcut unavailable.
    pub remote_input: Option<Arc<Manager>>,
    pub sources: Arc<dyn SourceProvider>,
    pub host: Arc<dyn HostMediaFactory>,
    pub viewer: Arc<dyn ViewerMediaFactory>,
}

impl Config {
    /// The loopback factory: one session per accepted connection.
    pub fn factory(self) -> piik_loopback::ControlFactory {
        Arc::new(move |sink: EventSink| {
            Some(Box::new(Session::new(self.clone(), Arc::new(sink))) as Box<dyn ControlSession>)
        })
    }
}

/// The native control session behind one `/control` connection.
pub struct Session {
    pub(crate) inner: Arc<Inner>,
}

impl Session {
    pub fn new(config: Config, outbox: Arc<dyn Outbox>) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                outbox,
                cancel: CancellationToken::new(),
                state: Mutex::new(State::default()),
            }),
        }
    }
}

impl ControlSession for Session {
    fn handle<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<Option<ControlEvent>, ProtocolViolation>> {
        Box::pin(self.inner.handle(cancel, payload))
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(self.inner.close())
    }
}

pub(crate) struct Inner {
    pub(crate) config: Config,
    outbox: Arc<dyn Outbox>,
    /// Fires when the session closes.
    pub(crate) cancel: CancellationToken,
    state: Mutex<State>,
}

/// One admitted operation; its token completes when the worker finishes.
#[derive(Clone)]
pub(crate) struct Slot(Arc<CancellationToken>);

impl Slot {
    fn new() -> Self {
        Self(Arc::new(CancellationToken::new()))
    }

    fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }

    pub(crate) async fn finished(&self) {
        self.0.cancelled().await;
    }
}

#[derive(Clone, Copy)]
pub(crate) enum SlotKind {
    /// Capture mutations, including start-share.
    Update,
    /// Connection preparation, independent of capture mutations.
    Punch,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) closed: bool,
    pub(crate) host: Option<Arc<dyn HostMedia>>,
    pub(crate) viewer: Option<Arc<dyn ViewerMedia>>,
    pub(crate) update: Option<Slot>,
    pub(crate) punch: Option<Slot>,
    start_id: String,
    pub(crate) start_cancel: Option<CancellationToken>,
}

impl State {
    fn slot(&mut self, kind: SlotKind) -> &mut Option<Slot> {
        match kind {
            SlotKind::Update => &mut self.update,
            SlotKind::Punch => &mut self.punch,
        }
    }
}

/// A valid request's failure path. Violations close the connection; every
/// other failure is that request's `request-failed`.
pub(crate) enum Fail {
    Violation(&'static str),
    Operation(MediaError),
}

impl From<MediaError> for Fail {
    fn from(error: MediaError) -> Self {
        Self::Operation(error)
    }
}

impl From<piik_native_capture::Error> for Fail {
    fn from(error: piik_native_capture::Error) -> Self {
        Self::Operation(error.into())
    }
}

impl From<piik_remote_input::Error> for Fail {
    fn from(error: piik_remote_input::Error) -> Self {
        Self::Operation(error.into())
    }
}

pub(crate) fn fail(message: &'static str) -> Fail {
    Fail::Operation(MediaError::failed(message))
}

/// The admitted request's identity for its response.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    pub(crate) id: String,
    pub(crate) kind: String,
    pub(crate) report_source_cancellation: bool,
}

impl Request {
    pub(crate) fn ack(&self, kind: &str) -> ControlEvent {
        ControlEvent::new(self.response(kind))
    }

    pub(crate) fn response(&self, kind: &str) -> Value {
        to_value(&ResponseEnvelope::new(&self.id, kind))
    }

    pub(crate) fn envelope(&self, kind: &str) -> ResponseEnvelope {
        ResponseEnvelope::new(&self.id, kind)
    }

    pub(crate) fn failure(&self, error: &MediaError) -> Value {
        tracing::debug!(
            target: "piik_native_control",
            event = "native-operation-failed",
            request_id = %self.id,
            request_type = %piik_diagnostics::safe_text(&self.kind),
            error = %piik_diagnostics::safe_text(&error.to_string()),
            "piik-client"
        );
        let code = if self.report_source_cancellation && error.source_canceled() {
            "source-canceled"
        } else {
            "operation-failed"
        };
        to_value(&RequestFailedResponse {
            envelope: self.envelope("request-failed"),
            code,
        })
    }

    pub(crate) fn complete(&self, result: Result<Value, MediaError>) -> Value {
        result.unwrap_or_else(|error| self.failure(&error))
    }
}

/// An operation admitted under the state lock, then run by a worker.
pub(crate) type Operation = BoxFuture<'static, Result<Value, MediaError>>;

impl Inner {
    pub(crate) fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn media_events(&self) -> MediaEvents {
        MediaEvents::new(self.outbox.clone(), self.cancel.clone())
    }

    /// Queues one event unless the session has closed.
    pub(crate) async fn emit(&self, event: ControlEvent) {
        tokio::select! {
            _ = self.outbox.send(event) => {}
            () = self.cancel.cancelled() => {}
        }
    }

    pub(crate) fn current(&self, share_id: &str) -> Option<Arc<dyn HostMedia>> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        state
            .host
            .as_ref()
            .filter(|host| host.share_id() == share_id)
            .cloned()
    }

    pub(crate) fn current_viewer(&self, share_id: &str) -> Option<Arc<dyn ViewerMedia>> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        state
            .viewer
            .as_ref()
            .filter(|viewer| viewer.share_id() == share_id)
            .cloned()
    }

    pub(crate) fn current_media(&self, share_id: &str) -> Option<Arc<dyn OutboundMedia>> {
        let state = self.lock();
        if state.closed {
            return None;
        }
        if let Some(host) = state
            .host
            .as_ref()
            .filter(|host| host.share_id() == share_id)
        {
            return Some(host.clone());
        }
        let viewer = state.viewer.as_ref()?;
        (viewer.share_id() == share_id).then(|| viewer.clone() as Arc<dyn OutboundMedia>)
    }

    /// Reserves one admission slot; `None` when closed or already active.
    pub(crate) fn begin(&self, state: &mut State, kind: SlotKind) -> Option<Slot> {
        if state.closed || state.slot(kind).is_some() {
            return None;
        }
        let slot = Slot::new();
        *state.slot(kind) = Some(slot.clone());
        Some(slot)
    }

    /// Releases a slot whose worker never started.
    pub(crate) fn abandon(&self, kind: SlotKind, slot: &Slot) {
        let mut state = self.lock();
        let field = state.slot(kind);
        if field.as_ref().is_some_and(|current| current.same(slot)) {
            *field = None;
        }
        drop(state);
        slot.0.cancel();
    }

    /// Completes a worker: its result's queue capacity and the slot share
    /// the response lifetime, and the writer releases admission before the
    /// ACK can trigger a successor.
    pub(crate) async fn finish(self: &Arc<Self>, kind: SlotKind, slot: Slot, value: Value) {
        slot.0.cancel();
        {
            let mut state = self.lock();
            if state.closed {
                let field = state.slot(kind);
                if field.as_ref().is_some_and(|current| current.same(&slot)) {
                    *field = None;
                }
                return;
            }
        }
        let weak = Arc::downgrade(self);
        let event = ControlEvent::new(value).with_before_write(move || {
            if let Some(inner) = weak.upgrade() {
                let mut state = inner.lock();
                let field = state.slot(kind);
                if field.as_ref().is_some_and(|current| current.same(&slot)) {
                    *field = None;
                }
            }
        });
        self.emit(event).await;
    }

    /// Capture mutations share one bounded operation slot. Stop remains
    /// synchronous and cancels the owning share before joining this
    /// completion. `prepare` runs under the state lock, so an
    /// authority-changing intent registers before off/pause can overtake it;
    /// it never waits for an OS permission dialog.
    pub(crate) fn run_host_operation(
        self: &Arc<Self>,
        host: &Arc<dyn HostMedia>,
        request: &Request,
        prepare: impl FnOnce() -> Result<Operation, MediaError>,
    ) -> Option<ControlEvent> {
        let mut state = self.lock();
        if state.closed
            || !state
                .host
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, host))
            || state.update.is_some()
            || state.start_cancel.is_some()
        {
            drop(state);
            return Some(ControlEvent::new(request.failure(&MediaError::failed(
                "native share operation is unavailable or already active",
            ))));
        }
        let operation = match prepare() {
            Ok(operation) => operation,
            Err(error) => {
                drop(state);
                return Some(ControlEvent::new(request.failure(&error)));
            }
        };
        let slot = Slot::new();
        state.update = Some(slot.clone());
        drop(state);
        let inner = self.clone();
        let host = host.clone();
        let request = request.clone();
        tokio::spawn(async move {
            let mut result = operation.await;
            let current = {
                let state = inner.lock();
                !state.closed
                    && state
                        .host
                        .as_ref()
                        .is_some_and(|current| Arc::ptr_eq(current, &host))
            };
            if !current && result.is_ok() {
                result = Err(MediaError::failed("native share stopped during operation"));
            }
            let value = request.complete(result);
            inner.finish(SlotKind::Update, slot, value).await;
        });
        None
    }

    pub(crate) async fn ensure_viewer(
        &self,
        share_id: &str,
        edge_capacity: usize,
    ) -> Result<Arc<dyn ViewerMedia>, MediaError> {
        let unavailable = || MediaError::failed("native media role is unavailable");
        {
            let state = self.lock();
            if state.closed || state.host.is_some() || state.start_cancel.is_some() {
                return Err(unavailable());
            }
            if let Some(viewer) = &state.viewer {
                if viewer.share_id() != share_id {
                    return Err(MediaError::failed("native Viewer session identity changed"));
                }
                return Ok(viewer.clone());
            }
        }
        let viewer = self
            .config
            .viewer
            .start(
                self.cancel.child_token(),
                ViewerOptions {
                    share_id: share_id.to_owned(),
                    edge_capacity,
                    port_mapping: self.config.port_mapping,
                    events: self.media_events(),
                },
            )
            .await?;
        let installed = {
            let mut state = self.lock();
            if state.closed
                || state.host.is_some()
                || state.viewer.is_some()
                || state.start_cancel.is_some()
            {
                false
            } else {
                state.viewer = Some(viewer.clone());
                true
            }
        };
        if !installed {
            let _ = viewer.close().await;
            return Err(unavailable());
        }
        Ok(viewer)
    }

    pub(crate) async fn stop_viewer(&self, share_id: &str) {
        let viewer = self
            .lock()
            .viewer
            .take_if(|viewer| viewer.share_id() == share_id);
        if let Some(viewer) = viewer {
            let _ = viewer.close().await;
        }
    }

    /// Stops the share, first cancelling and joining its start when the stop
    /// overtakes it. An already-retired share is not a failure.
    pub(crate) async fn stop_share(&self, share_id: &str) -> Result<(), MediaError> {
        loop {
            let starting = {
                let state = self.lock();
                match &state.start_cancel {
                    Some(cancel) if state.start_id == share_id => {
                        cancel.cancel();
                        Some(state.update.clone())
                    }
                    _ => None,
                }
            };
            if let Some(start) = starting {
                if let Some(start) = start {
                    start.finished().await;
                }
                continue;
            }
            let (host, update) = {
                let mut state = self.lock();
                let Some(host) = state.host.take_if(|host| host.share_id() == share_id) else {
                    return Ok(());
                };
                (Some(host), state.update.clone())
            };
            let result = match host {
                Some(host) => host.close().await,
                None => Ok(()),
            };
            if let Some(update) = update {
                update.finished().await;
            }
            return result;
        }
    }

    /// Installs a started Host share; `false` when it was overtaken.
    pub(crate) fn install_host(
        &self,
        host: &Arc<dyn HostMedia>,
        cancel: &CancellationToken,
    ) -> bool {
        let mut state = self.lock();
        if state.closed || cancel.is_cancelled() || state.host.is_some() || state.viewer.is_some() {
            return false;
        }
        state.host = Some(host.clone());
        true
    }

    pub(crate) fn begin_start(&self, share_id: &str, cancel: &CancellationToken) -> Option<Slot> {
        let mut state = self.lock();
        if state.closed
            || state.host.is_some()
            || state.viewer.is_some()
            || state.update.is_some()
            || state.start_cancel.is_some()
        {
            return None;
        }
        let slot = Slot::new();
        state.update = Some(slot.clone());
        state.start_id = share_id.to_owned();
        state.start_cancel = Some(cancel.clone());
        Some(slot)
    }

    pub(crate) fn end_start(&self) {
        let mut state = self.lock();
        state.start_id.clear();
        state.start_cancel = None;
    }

    /// Relays remote-input changes and the share's end until it ends.
    /// `lifetime` is the share's start token, cancelled when this returns.
    pub(crate) async fn watch_host(
        self: Arc<Self>,
        host: Arc<dyn HostMedia>,
        report_failure: bool,
        lifetime: CancellationToken,
    ) {
        let _lifetime = lifetime.drop_guard();
        loop {
            tokio::select! {
                () = host.remote_input_changed() => {
                    let event = RemoteControlStateEvent {
                        envelope: EventEnvelope::new("remote-control-state", host.share_id()),
                        state: host.remote_input_state(),
                    };
                    let weak: Weak<Self> = Arc::downgrade(&self);
                    let owner = host.clone();
                    let event = ControlEvent::new(to_value(&event)).with_current(move || {
                        weak.upgrade().is_some_and(|inner| {
                            inner
                                .current(owner.share_id())
                                .is_some_and(|current| Arc::ptr_eq(&current, &owner))
                        })
                    });
                    self.emit(event).await;
                }
                ended = host.done() => {
                    {
                        let mut state = self.lock();
                        if state.host.as_ref().is_some_and(|current| Arc::ptr_eq(current, &host)) {
                            state.host = None;
                        }
                    }
                    if let Some(ended) = ended {
                        let failure = match &ended {
                            Err(error) if report_failure => error.failure_details().cloned(),
                            _ => None,
                        };
                        let event = ShareEndedEvent {
                            version: piik_loopback::PROTOCOL_VERSION,
                            kind: "share-ended",
                            share_id: host.share_id().to_owned(),
                            failed: ended.is_err(),
                            failure,
                        };
                        self.emit(ControlEvent::new(to_value(&event))).await;
                    }
                    return;
                }
                () = self.cancel.cancelled() => return,
            }
        }
    }

    async fn close(&self) {
        let (host, viewer, update) = {
            let mut state = self.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            if let Some(cancel) = &state.start_cancel {
                cancel.cancel();
            }
            (state.host.take(), state.viewer.take(), state.update.clone())
        };
        self.cancel.cancel();
        if let Some(host) = host {
            let _ = host.close().await;
        }
        if let Some(viewer) = viewer {
            let _ = viewer.close().await;
        }
        if let Some(update) = update {
            update.finished().await;
        }
    }
}
