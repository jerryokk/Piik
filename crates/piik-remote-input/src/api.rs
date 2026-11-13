//! The public Manager and per-share Session handles.

use std::pin::pin;
use std::sync::{Arc, Weak, mpsc};
use std::time::Duration;

use piik_input_route::{Delivery, InputChannel, Packet, Receive, Relay};
use piik_native_capture::CaptureTarget;
use piik_protocol::{RemoteInputPeer, valid_input_key, valid_opaque_id};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::cipher::InputCipher;
use crate::diagnostics::DiagnosticSnapshot;
use crate::manager::{
    Connection, Inner, InputSink, OpenGamepads, RegisterStop, SessionId, SessionState, Shared,
    Signal, State, View,
};
use crate::policy::{Mode, PolicyChange, begin_policy};
use crate::shortcut::StopShortcut;
use crate::{Error, PLATFORM};

/// Held input polling: foreground changes must release held input even when
/// the remote player sends nothing. Only an active grant observes the target.
const REFRESH_INTERVAL: Duration = Duration::from_millis(100);

/// Owns desktop input for every native share of the App. Input stays
/// disabled until a share's Host enables it; no service, driver or startup
/// entry is installed. Dropping the manager closes it.
pub struct Manager {
    shared: Arc<Shared>,
}

impl Manager {
    /// Prepares the Windows backend and its stop-shortcut and focus watcher.
    pub fn new() -> Result<Self, Error> {
        let sink = crate::platform::input::WindowsSink::new()?;
        let (signals, receiver) = mpsc::channel();
        let pressed = signals.clone();
        let register: RegisterStop = Box::new(move |key| {
            let pressed = pressed.clone();
            crate::platform::hotkey::watch_stop_shortcut(
                key,
                Box::new(move || {
                    let _ = pressed.send(Signal::StopPressed);
                }),
            )
        });
        let open_gamepads: OpenGamepads = Box::new(crate::platform::vigem::open_backend);
        let manager = Self::build(Box::new(sink), Some(register), open_gamepads, Some(signals));
        let weak = Arc::downgrade(&manager.shared);
        std::thread::Builder::new()
            .name("piik-remote-input".into())
            .spawn(move || watch(&weak, &receiver))
            .map_err(|error| Error::input(format!("start remote input watcher: {error}")))?;
        Ok(manager)
    }

    pub(crate) fn build(
        sink: Box<dyn InputSink>,
        register_stop: Option<RegisterStop>,
        open_gamepads: OpenGamepads,
        signals: Option<mpsc::Sender<Signal>>,
    ) -> Self {
        let shared = Arc::new_cyclic(|this: &Weak<Shared>| {
            Shared::new(
                Inner::new(this.clone(), sink, register_stop, open_gamepads),
                signals,
            )
        });
        Self { shared }
    }

    #[cfg(test)]
    pub(crate) fn shared(&self) -> &Arc<Shared> {
        &self.shared
    }

    /// A new share's input session, or `None` once the manager is closed.
    pub fn new_session<C: InputChannel + 'static>(&self) -> Option<Session<C>> {
        let mut inner = self.shared.lock();
        if inner.closed {
            return None;
        }
        let id = inner.next_id();
        let weak = Arc::downgrade(&self.shared);
        let receive: Receive<C> = Box::new(
            move |edge_id: &str, packet: Packet, delivery: Option<Delivery<C>>| {
                if let Some(shared) = weak.upgrade() {
                    let current = delivery.map(|delivery| move || delivery.is_current());
                    shared.receive_packet(
                        id,
                        edge_id,
                        packet,
                        current.as_ref().map(|f| f as &dyn Fn() -> bool),
                    );
                }
            },
        );
        let route = Arc::new(Relay::new(Some(receive)));
        let changed = Arc::new(Notify::new());
        let reply_route = route.clone();
        inner.sessions.insert(
            id,
            SessionState::new(
                changed.clone(),
                Box::new(move |packet| reply_route.reply(packet)),
            ),
        );
        Some(Session {
            shared: self.shared.clone(),
            id,
            route,
            changed,
        })
    }

    /// The local stop: every enabled share returns to Ask each time.
    pub fn revoke(&self) {
        self.shared.revoke();
    }

    pub fn close(&self) {
        self.shared.close();
    }

    /// Counts only; it returns before a report writer takes any of its locks.
    pub fn diagnostics(&self) -> DiagnosticSnapshot {
        self.shared.lock().snapshot()
    }

    pub fn supports_stop_shortcut(&self) -> bool {
        self.shared.lock().register_stop.is_some()
    }

    pub fn read_stop_shortcut(&self) -> Result<StopShortcut, Error> {
        self.shared.lock().read_stop_shortcut()
    }

    pub fn set_stop_shortcut(&self, key: i32) -> Result<StopShortcut, Error> {
        self.shared.lock().set_stop_shortcut(key)
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.shared.close();
    }
}

fn watch(shared: &Weak<Shared>, signals: &mpsc::Receiver<Signal>) {
    loop {
        let signal = match signals.recv_timeout(REFRESH_INTERVAL) {
            Ok(signal) => Some(signal),
            Err(mpsc::RecvTimeoutError::Timeout) => None,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let Some(shared) = shared.upgrade() else {
            return;
        };
        match signal {
            Some(Signal::Stop) => return,
            Some(Signal::StopPressed) => shared.revoke(),
            None => {
                let mut inner = shared.lock();
                if inner.closed {
                    return;
                }
                let _ = inner.retry_releases();
                inner.refresh_active();
            }
        }
    }
}

/// One share's input permission. The data channel belongs to the share; the
/// permission belongs to its current source. Dropping the session closes it.
pub struct Session<C: InputChannel + 'static> {
    shared: Arc<Shared>,
    id: SessionId,
    route: Arc<Relay<C>>,
    changed: Arc<Notify>,
}

impl<C: InputChannel + 'static> Session<C> {
    /// The input relay root. The transport owner forwards this share's input
    /// channel notifications to its `down_*` methods.
    pub fn route(&self) -> &Relay<C> {
        &self.route
    }

    /// Resolves after the permission or grant owner changed. Notifications
    /// coalesce; input packets never notify, and no callback runs under the
    /// input lock.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Consumes a pending change notification without waiting.
    pub fn take_change(&self) -> bool {
        let notified = pin!(self.changed.notified());
        notified.enable()
    }

    /// Binds the share's current source. Replacing a picture revokes the grant
    /// and turns control off without losing the channel needed to request
    /// control of the next source.
    pub fn set_target(&self, target: CaptureTarget) {
        self.shared.lock().set_target(self.id, target, None);
    }

    /// Binds a physical input channel and returns the transport-loss action
    /// for it. Loss invalidates queued deliveries and grants together while
    /// retaining SCTP for recovery.
    pub fn attach(&self, edge_id: &str, channel: Arc<C>) -> Box<dyn Fn() + Send + Sync> {
        let _ = self.route.attach_down(edge_id, channel.clone());
        let shared = Arc::downgrade(&self.shared);
        let route = Arc::downgrade(&self.route);
        let (id, edge_id) = (self.id, edge_id.to_owned());
        Box::new(move || {
            let (Some(shared), Some(route)) = (shared.upgrade(), route.upgrade()) else {
                return;
            };
            let mut inner = shared.lock();
            if !inner.closed && !inner.session_closed(id) && route.reset_down(&edge_id, &channel) {
                inner.revoke_edge(id, &edge_id);
            }
        })
    }

    /// Installs the input endpoints published by the owning Host's
    /// authenticated room connection. A relay never names an input identity.
    pub fn set_peers(&self, peers: &[RemoteInputPeer]) -> Result<(), Error> {
        self.shared.lock().set_peers(self.id, peers)
    }

    pub fn pause(&self, paused: bool) {
        self.shared.lock().pause(self.id, paused);
    }

    pub fn state(&self) -> State {
        self.shared.lock().view(self.id)
    }

    pub fn decide(&self, request_id: &str, allow: bool) -> Result<(), Error> {
        self.shared.lock().decide(self.id, request_id, allow)
    }

    /// Reserves one Host policy intent before asynchronous preparation.
    pub fn begin_policy(
        &self,
        parent: &CancellationToken,
        mode: &str,
        password: &str,
        co_play: bool,
    ) -> Result<PolicyChange, Error> {
        begin_policy(&self.shared, self.id, parent, mode, password, co_play)
    }

    pub fn set_simultaneous(&self, enabled: bool) -> Result<(), Error> {
        self.shared.lock().set_simultaneous(self.id, enabled)
    }

    pub fn close(&self) {
        {
            let mut inner = self.shared.lock();
            if inner.session_closed(self.id) {
                return;
            }
            inner.close_session(self.id);
        }
        self.changed.notify_one();
        self.route.close();
    }

    #[cfg(test)]
    pub(crate) fn id(&self) -> SessionId {
        self.id
    }
}

impl<C: InputChannel + 'static> Drop for Session<C> {
    fn drop(&mut self) {
        self.close();
    }
}

impl PolicyChange {
    /// A capture replacement made for this same intent binds its installed
    /// picture; only a still-current change survives the binding.
    pub fn set_target(&self, target: CaptureTarget) {
        let (shared, session, id) = self.parts();
        shared.lock().set_target(session, target, Some(id));
    }
}

impl Inner {
    pub fn set_target(&mut self, session: SessionId, target: CaptureTarget, change: Option<u64>) {
        if self.closed || self.session_closed(session) {
            return;
        }
        let target = if matches!(target.kind.as_str(), "display" | "window") {
            target
        } else {
            CaptureTarget::default()
        };
        let s = self.sessions.get_mut(&session).expect("open session");
        s.target = target;
        s.simultaneous = false;
        let current = s.policy_change.as_ref().map(|(id, _)| *id);
        if change.is_none() || current != change {
            self.cancel_policy(session);
        }
        if self.owner == Some(session) {
            self.release();
        }
        self.apply_policy(session, Mode::Off, [0; 32], false);
    }

    pub fn set_peers(
        &mut self,
        session: SessionId,
        peers: &[RemoteInputPeer],
    ) -> Result<(), Error> {
        if self.closed || self.session_closed(session) {
            return Err(Error::Closed);
        }
        let mut retained = std::collections::BTreeSet::new();
        for peer in peers {
            if !valid_input_key(&peer.endpoint_id, &peer.secret)
                || !valid_opaque_id(&peer.peer_id)
                || !retained.insert(peer.endpoint_id.clone())
            {
                return Err(Error::InvalidEndpoint);
            }
        }
        for peer in peers {
            let key = (session, peer.endpoint_id.clone());
            if let Some(previous) = self.connections.get(&key) {
                if previous.secret == peer.secret && previous.peer_id == peer.peer_id {
                    continue;
                }
                self.revoke(&key);
            }
            let cipher = InputCipher::new(&peer.secret).ok_or(Error::InvalidEndpoint)?;
            let serial = self.next_id();
            self.connections
                .insert(key, Connection::new(serial, peer, cipher));
            // Signaling and the media return path arrive in either order. Ask
            // this endpoint to sync only once its authenticated key is installed.
            let ready = Packet {
                id: peer.endpoint_id.clone(),
                ready: Some(true),
                ..Packet::default()
            };
            self.sessions[&session].send_reply(&ready);
        }
        for key in self.session_keys(session) {
            if !retained.contains(&key.1) {
                self.revoke(&key);
                self.connections.remove(&key);
            }
        }
        self.notify(session);
        Ok(())
    }

    pub fn pause(&mut self, session: SessionId, paused: bool) {
        let Some(s) = self.sessions.get_mut(&session) else {
            return;
        };
        s.paused = paused;
        if paused {
            self.cancel_policy(session);
            if self.owner == Some(session) {
                self.release();
            }
        }
        for key in self.session_keys(session) {
            if paused {
                self.connections
                    .get_mut(&key)
                    .expect("listed connection")
                    .requested = false;
            }
            self.send_state(&key);
        }
        self.notify(session);
    }

    pub fn close_session(&mut self, session: SessionId) {
        if let Some(s) = self.sessions.get_mut(&session) {
            s.closed = true;
        }
        self.cancel_policy(session);
        if self.owner == Some(session) {
            self.release();
        }
        for key in self.session_keys(session) {
            self.connections.remove(&key);
        }
        self.sessions.remove(&session);
        self.release_idle_shortcut();
    }

    pub fn view(&self, session: SessionId) -> State {
        let mut state = State::default();
        let Some(s) = self.sessions.get(&session) else {
            return state;
        };
        state.platform = PLATFORM.to_owned();
        state.mode = s.mode.as_str().to_owned();
        state.co_play = s.co_play;
        if s.co_play {
            state.simultaneous = Some(s.simultaneous);
        }
        // Only a Host who has turned remote control on is offered controllers.
        // Probing reads the device interface list; it is answered once per Host
        // and cached, so a state read stays cheap.
        if s.mode != Mode::Off {
            state.gamepad_driver = Some(*self.gamepad_driver.get_or_init(|| {
                self.gamepads.is_some() || crate::platform::vigem::driver_present()
            }));
        }
        for key in self.session_keys(session) {
            let c = &self.connections[&key];
            if c.requested || !c.grant.is_empty() {
                state.requests.push(View {
                    id: c.request_id.clone(),
                    peer_id: c.peer_id.clone(),
                    state: self.state(&key).to_owned(),
                    endpoint_id: if s.co_play {
                        key.1.clone()
                    } else {
                        String::new()
                    },
                });
            }
        }
        state.requests.sort_by(|a, b| a.id.cmp(&b.id));
        state
    }

    pub fn decide(
        &mut self,
        session: SessionId,
        request_id: &str,
        allow: bool,
    ) -> Result<(), Error> {
        let key = self
            .session_keys(session)
            .into_iter()
            .find(|key| self.connections[key].request_id == request_id);
        let Some(key) = key.filter(|key| {
            let c = &self.connections[key];
            !self.closed
                && self.available(session)
                && !c.edge_id.is_empty()
                && (c.requested || !c.grant.is_empty())
        }) else {
            return Err(Error::RequestUnavailable);
        };
        let result = self.decide_request(session, &key, allow);
        self.notify(session);
        result
    }

    fn decide_request(
        &mut self,
        session: SessionId,
        key: &crate::manager::Key,
        allow: bool,
    ) -> Result<(), Error> {
        let s = &self.sessions[&session];
        if !allow {
            if s.mode != Mode::Ask && !s.co_play {
                self.set_policy(session, Mode::Ask, [0; 32]);
                return Ok(());
            }
            self.revoke(key);
            return Ok(());
        }
        // Explicit approval hands over this source's exclusive control.
        // Automatic admission still uses can_join and never displaces.
        if self.connections[key].requested
            && s.co_play
            && !s.simultaneous
            && self.owner == Some(session)
        {
            self.release();
        }
        self.grant(key)
    }
}
