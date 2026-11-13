//! Source-scoped grants. One manager covers every native share: they operate
//! the same physical desktop, so input belongs to one process-wide source
//! owner at a time.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak, mpsc};
use std::time::Instant;

use piik_input_route::Packet;
use piik_native_capture::CaptureTarget;
use piik_protocol::RemoteInputPeer;
use serde::Serialize;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::cipher::{InputCipher, random_hex};
use crate::diagnostics::DiagnosticCounters;
use crate::event::{Bounds, InputEvent};
use crate::gamepad::{GamepadBackend, GamepadDevice};
use crate::policy::Mode;
use crate::shortcut::DEFAULT_STOP_KEY;
use crate::text::{MAX_INPUT_BYTES, TextSink, apply_text};
use crate::{Error, PLATFORM};

mod receive;

pub(crate) type SessionId = u64;
/// A connection belongs to one session and one authenticated endpoint.
pub(crate) type Key = (SessionId, String);
pub(crate) type StopHandle = Box<dyn FnOnce() + Send>;
pub(crate) type RegisterStop = Box<dyn FnMut(i32) -> Result<StopHandle, Error> + Send>;
pub(crate) type OpenGamepads = Box<dyn Fn() -> Option<Arc<dyn GamepadBackend>> + Send>;
type Reply = Box<dyn Fn(&Packet) + Send + Sync>;

/// The platform input owner: target observation and injection.
pub(crate) trait InputSink: Send {
    /// Current source geometry and whether it may receive input. `Ok` with
    /// empty bounds means observation is temporarily unavailable; `Err` means
    /// the source identity is gone.
    fn target(&mut self, target: &CaptureTarget) -> Result<(Bounds, bool), Error>;
    /// Brings the shared source forward so a new grant can receive input.
    /// Called once per grant, never per event.
    fn raise(&mut self, target: &CaptureTarget);
    fn apply(&mut self, target: Bounds, event: &InputEvent) -> Result<(), Error>;
    /// Releases every physical key and button this backend holds.
    fn reset(&mut self) -> Result<(), Error>;
}

/// One Viewer's request in the Host snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct View {
    pub id: String,
    pub peer_id: String,
    pub state: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub endpoint_id: String,
}

/// The owning Host's current permission and grants for one share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct State {
    pub platform: String,
    pub mode: String,
    pub requests: Vec<View>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub co_play: bool,
    /// Whether a compatible ViGEmBus is installed. Reported regardless of
    /// whether co-play is on, so the Host page can mention controllers as an
    /// option without making the Host feel told off for skipping them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gamepad_driver: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub simultaneous: Option<bool>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            platform: PLATFORM.to_owned(),
            mode: Mode::Off.as_str().to_owned(),
            requests: Vec::new(),
            co_play: false,
            gamepad_driver: None,
            simultaneous: None,
        }
    }
}

pub(crate) struct Connection {
    /// Distinguishes a replacement under the same key.
    pub serial: u64,
    pub endpoint_id: String,
    pub edge_id: String,
    pub epoch: String,
    pub requested: bool,
    pub request_id: String,
    pub intent: String,
    pub grant: String,
    pub suspended: bool,
    pub input_failed: bool,
    pub peer_id: String,
    pub secret: String,
    pub cipher: InputCipher,
    pub keys: BTreeSet<String>,
    pub buttons: BTreeSet<i64>,
    /// One virtual controller per Browser gamepad index, keyed by that index.
    /// A Viewer may bring more than one, and each owns its own XInput slot.
    pub pads: BTreeMap<u8, Box<dyn GamepadDevice>>,
    /// Indices refused for lack of room, so a report for one of them retries
    /// only when the Viewer asks again rather than spinning on every frame.
    pub pad_failed: BTreeSet<u8>,
    /// Per-index intent counters: a pending enumeration is only accepted while
    /// the count for its index is unchanged.
    pub pad_intent: BTreeMap<u8, u64>,
}

pub(crate) struct SessionState {
    pub target: CaptureTarget,
    pub paused: bool,
    pub closed: bool,
    pub mode: Mode,
    pub co_play: bool,
    pub simultaneous: bool,
    pub policy_change: Option<(u64, CancellationToken)>,
    pub password: [u8; 32],
    pub password_retry: Option<Instant>,
    changed: Arc<Notify>,
    reply: Reply,
}

impl SessionState {
    pub fn new(changed: Arc<Notify>, reply: Reply) -> Self {
        Self {
            target: CaptureTarget::default(),
            paused: false,
            closed: false,
            mode: Mode::Off,
            co_play: false,
            simultaneous: false,
            policy_change: None,
            password: [0; 32],
            password_retry: None,
            changed,
            reply,
        }
    }

    pub fn send_reply(&self, packet: &Packet) {
        (self.reply)(packet);
    }
}

impl Connection {
    pub fn new(serial: u64, peer: &RemoteInputPeer, cipher: InputCipher) -> Self {
        Self {
            serial,
            endpoint_id: peer.endpoint_id.clone(),
            edge_id: String::new(),
            epoch: String::new(),
            requested: false,
            request_id: String::new(),
            intent: String::new(),
            grant: String::new(),
            suspended: false,
            input_failed: false,
            peer_id: peer.peer_id.clone(),
            secret: peer.secret.clone(),
            cipher,
            keys: BTreeSet::new(),
            buttons: BTreeSet::new(),
            pads: BTreeMap::new(),
            pad_failed: BTreeSet::new(),
            pad_intent: BTreeMap::new(),
        }
    }
}

pub(crate) struct Inner {
    pub sink: Box<dyn InputSink>,
    pub pending_keys: BTreeSet<String>,
    pub pending_buttons: BTreeSet<i64>,
    pub gamepads: Option<Arc<dyn GamepadBackend>>,
    pub open_gamepads: OpenGamepads,
    /// The Browser index whose target is being enumerated, if any. One at a
    /// time: the driver picks the free XInput slot itself.
    pub gamepad_opening: Option<u8>,
    /// Cached answer to "is ViGEmBus installed". Probing touches the device
    /// interface list, so it happens once, on the first state read that needs
    /// it, and never again for this Host.
    pub gamepad_driver: OnceLock<bool>,
    pub connections: BTreeMap<Key, Connection>,
    pub diagnostics: DiagnosticCounters,
    pub sessions: BTreeMap<SessionId, SessionState>,
    /// The one source allowed to inject into the desktop.
    pub owner: Option<SessionId>,
    pub closed: bool,
    pub stop_key: i32,
    pub register_stop: Option<RegisterStop>,
    pub stop_shortcut: Option<StopHandle>,
    pub next_id: u64,
    pub this: Weak<Shared>,
}

pub(crate) enum Signal {
    StopPressed,
    Stop,
}

pub(crate) struct Shared {
    inner: Mutex<Inner>,
    signals: Option<mpsc::Sender<Signal>>,
}

impl Shared {
    pub fn new(inner: Inner, signals: Option<mpsc::Sender<Signal>>) -> Self {
        Self {
            inner: Mutex::new(inner),
            signals,
        }
    }

    pub fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|error| error.into_inner())
    }

    pub fn close(&self) {
        let stop_shortcut = {
            let mut inner = self.lock();
            if inner.closed {
                return;
            }
            inner.closed = true;
            inner.release();
            let _ = inner.retry_releases();
            if let Some(gamepads) = inner.gamepads.take() {
                let _ = gamepads.close();
            }
            inner.stop_shortcut.take()
        };
        if let Some(stop) = stop_shortcut {
            stop();
        }
        if let Some(signals) = &self.signals {
            let _ = signals.send(Signal::Stop);
        }
    }

    pub fn revoke(&self) {
        let mut inner = self.lock();
        if inner.closed {
            return;
        }
        let ids: Vec<SessionId> = inner.sessions.keys().copied().collect();
        for id in ids {
            if inner.sessions[&id].mode != Mode::Off {
                inner.set_policy(id, Mode::Ask, [0; 32]);
            } else {
                inner.cancel_policy(id);
            }
        }
    }

    pub(crate) fn receive_packet(
        &self,
        session: SessionId,
        edge_id: &str,
        packet: Packet,
        current: Option<&dyn Fn() -> bool>,
    ) {
        let mut inner = self.lock();
        let key = (session, packet.id.clone());
        if inner.closed || inner.session_closed(session) || !inner.connections.contains_key(&key) {
            return;
        }
        // Retirement may already have removed its relay record before the
        // channel closes. It still ends the matching epoch; only new input
        // needs a live binding.
        if packet.closed {
            let c = &inner.connections[&key];
            if c.edge_id == edge_id && c.epoch == packet.epoch {
                inner.revoke(&key);
                if let Some(c) = inner.connections.get_mut(&key) {
                    c.edge_id.clear();
                }
            }
            return;
        }
        if current.is_some_and(|current| !current()) {
            return;
        }
        inner.diagnostics.received_packets += 1;
        let Some(plain) = inner
            .connections
            .get_mut(&key)
            .and_then(|c| c.cipher.open(&packet.data, &packet.epoch))
        else {
            return;
        };
        inner.diagnostics.decrypted_packets += 1;
        let c = &inner.connections[&key];
        if !c.edge_id.is_empty() && (c.edge_id != edge_id || c.epoch != packet.epoch) {
            inner.revoke(&key);
        }
        let Some(c) = inner.connections.get_mut(&key) else {
            return;
        };
        c.edge_id = edge_id.to_owned();
        c.epoch = packet.epoch;
        inner.receive_locked(&key, &plain);
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.close();
    }
}

fn empty(value: &&str) -> bool {
    value.is_empty()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReplyBody<'a> {
    platform: &'static str,
    state: &'a str,
    #[serde(skip_serializing_if = "empty")]
    grant: &'a str,
    #[serde(skip_serializing_if = "empty")]
    intent: &'a str,
    mode: &'static str,
    #[serde(skip_serializing_if = "empty")]
    reason: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    input_failed: bool,
    /// Facts for the current source and grant. Every peer of this build
    /// supports all of them, so they are always present.
    capabilities: Vec<&'static str>,
    co_play: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    gamepad_room: Option<usize>,
    /// Browser gamepad indices this Viewer could not use for lack of a slot,
    /// addressed the same way reports are. A Viewer with several controllers
    /// keeps the rest working and stops sending only these.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    gamepad_failed: Vec<u8>,
}

/// Committed text from one controller: reset and apply follow its grant.
struct ControllerText<'a> {
    inner: &'a mut Inner,
    key: &'a Key,
}

impl TextSink for ControllerText<'_> {
    fn reset(&mut self) -> Result<(), Error> {
        self.inner.reset_held(self.key.0)
    }

    fn apply(&mut self, target: Bounds, event: InputEvent) -> Result<(), Error> {
        self.inner.apply_held(self.key, target, &event)
    }
}

impl Inner {
    pub fn new(
        this: Weak<Shared>,
        sink: Box<dyn InputSink>,
        register_stop: Option<RegisterStop>,
        open_gamepads: OpenGamepads,
    ) -> Self {
        Self {
            sink,
            pending_keys: BTreeSet::new(),
            pending_buttons: BTreeSet::new(),
            gamepads: None,
            open_gamepads,
            gamepad_opening: None,
            gamepad_driver: OnceLock::new(),
            connections: BTreeMap::new(),
            diagnostics: DiagnosticCounters::default(),
            sessions: BTreeMap::new(),
            owner: None,
            closed: false,
            stop_key: DEFAULT_STOP_KEY,
            register_stop,
            stop_shortcut: None,
            next_id: 0,
            this,
        }
    }

    pub fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    pub fn session_closed(&self, session: SessionId) -> bool {
        self.sessions.get(&session).is_none_or(|s| s.closed)
    }

    pub fn session_keys(&self, session: SessionId) -> Vec<Key> {
        self.connections
            .keys()
            .filter(|key| key.0 == session)
            .cloned()
            .collect()
    }

    pub fn notify(&self, session: SessionId) {
        if let Some(s) = self.sessions.get(&session) {
            s.changed.notify_one();
        }
    }

    pub fn available(&self, session: SessionId) -> bool {
        self.sessions
            .get(&session)
            .is_some_and(|s| !s.closed && !s.paused && !s.target.source_id.is_empty())
    }

    fn mode(&self, session: SessionId) -> Mode {
        self.sessions.get(&session).map_or(Mode::Off, |s| s.mode)
    }

    pub fn co_play(&self, session: SessionId) -> bool {
        self.sessions.get(&session).is_some_and(|s| s.co_play)
    }

    pub fn state(&self, key: &Key) -> &'static str {
        let Some(c) = self.connections.get(key) else {
            return "ready";
        };
        if !c.grant.is_empty() {
            if c.suspended { "suspended" } else { "active" }
        } else if c.requested {
            "pending"
        } else {
            "ready"
        }
    }

    pub fn send(&mut self, key: &Key, state: &str) {
        self.reply(key, state, "");
    }

    pub fn send_state(&mut self, key: &Key) {
        let state = self.state(key);
        self.send(key, state);
    }

    pub fn reply(&mut self, key: &Key, state: &str, reason: &str) {
        let Some(c) = self.connections.get(key) else {
            return;
        };
        if c.epoch.is_empty() {
            return;
        }
        let session = key.0;
        let mode = self.mode(session);
        let state = if state == "ready" && (!self.available(session) || mode == Mode::Off) {
            "unavailable"
        } else {
            state
        };
        let shared = self.co_play(session);
        // Text resets the sink, so it stays with one keyboard holder. Every
        // other capability belongs to this build and this source.
        let mut capabilities = vec!["coPlay", "inputFeedback"];
        if !shared || self.single_controller(session).as_ref() == Some(key) {
            capabilities.push("text");
        }
        // The capability survives a failed attempt: the refusal is about room,
        // not about this build, so the Viewer keeps the option to retry.
        if shared && self.gamepads.is_some() {
            capabilities.push("gamepad");
        }
        // The Host's own controllers share these slots, so the count is read
        // from the machine rather than from how many virtual pads we made.
        let gamepad_room = self
            .gamepads
            .as_ref()
            .map(|_| crate::platform::xinput::free_count());
        let body = ReplyBody {
            platform: PLATFORM,
            state,
            grant: &c.grant,
            intent: &c.intent,
            mode: mode.as_str(),
            reason,
            input_failed: c.input_failed,
            capabilities,
            co_play: shared,
            gamepad_room,
            gamepad_failed: c.pad_failed.iter().copied().collect(),
        };
        let data = serde_json::to_vec(&body).expect("reply serializes");
        let Some(c) = self.connections.get_mut(key) else {
            return;
        };
        let sealed = c.cipher.seal(&data, &c.epoch);
        let packet = Packet {
            id: c.id().to_owned(),
            epoch: c.epoch.clone(),
            data: sealed,
            ..Packet::default()
        };
        if let Some(s) = self.sessions.get(&session) {
            (s.reply)(&packet);
        }
    }

    /// Source identity, geometry and focus are checked at the input owner,
    /// never inferred from a Browser's last displayed video frame.
    pub fn refresh_active(&mut self) -> (Bounds, bool) {
        let Some(owner) = self.owner else {
            return (Bounds::default(), false);
        };
        let Some(target) = self.sessions.get(&owner).map(|s| s.target.clone()) else {
            return (Bounds::default(), false);
        };
        let (bounds, focused) = match self.sink.target(&target) {
            Ok(observed) => observed,
            Err(error) => {
                let failed: Vec<Key> = self
                    .session_keys(owner)
                    .into_iter()
                    .filter(|key| !self.connections[key].grant.is_empty())
                    .collect();
                for key in &failed {
                    let c = &self.connections[key];
                    tracing::debug!(
                        event = "target-failed",
                        sourceKind = %target.kind,
                        connectionId = %piik_diagnostics::id(&c.edge_id),
                        inputGeneration = %piik_diagnostics::id(&c.epoch),
                        error = %error,
                        "remote-input"
                    );
                }
                self.release();
                for key in &failed {
                    self.send(key, "failed");
                }
                return (Bounds::default(), false);
            }
        };
        let suspended = !focused;
        let mut changed = false;
        for key in self.session_keys(owner) {
            let c = self.connections.get_mut(&key).expect("listed connection");
            if !c.grant.is_empty() && c.suspended != suspended {
                c.suspended = suspended;
                changed = true;
                self.send_state(&key);
            }
        }
        if changed {
            if !focused {
                let failed = self.reset_held(owner).is_err();
                for key in self.session_keys(owner) {
                    if failed {
                        self.note_input_result(&key, true);
                    }
                    let failed_pads: Vec<u8> = {
                        let c = self.connections.get_mut(&key).expect("listed connection");
                        let mut failed = Vec::new();
                        for (index, pad) in c.pads.iter_mut() {
                            if pad.update(Default::default()).is_err() {
                                failed.push(*index);
                            }
                        }
                        failed
                    };
                    for index in failed_pads {
                        self.fail_gamepad(&key, index);
                    }
                }
            }
            self.notify(owner);
        }
        (bounds, focused)
    }

    /// Retires every input grant belonging to the desktop's source owner.
    pub fn release(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        let _ = self.reset_held(owner);
        let mut freed = false;
        for key in self.session_keys(owner) {
            if self.connections[&key].grant.is_empty() {
                continue;
            }
            freed |= self.close_pads(&key);
            let c = self.connections.get_mut(&key).expect("listed connection");
            c.pad_failed.clear();
            c.grant.clear();
            c.suspended = false;
            c.input_failed = false;
            self.send(&key, "ready");
        }
        // The owner's own sessions have just been told; other sessions sharing
        // this source only learn the budget moved from this announcement.
        if freed {
            self.announce_gamepad_room();
        }
        self.notify(owner);
    }

    /// Releases only this player's contribution. Other keyboard holders keep a
    /// shared key down until its final holder releases it.
    pub fn clear_held(&mut self, key: &Key) -> Result<(), Error> {
        let Some(c) = self.connections.get_mut(key) else {
            return Ok(());
        };
        let keys = std::mem::take(&mut c.keys);
        let buttons = std::mem::take(&mut c.buttons);
        let mut result = None;
        for code in keys {
            if !self.key_held(&code)
                && let Err(error) = self.release_key(&code)
            {
                result = crate::join(result, error);
            }
        }
        for button in buttons {
            if !self.button_held(button)
                && let Err(error) = self.release_button(button)
            {
                result = crate::join(result, error);
            }
        }
        result.map_or(Ok(()), Err)
    }

    pub fn key_held(&self, code: &str) -> bool {
        self.connections.values().any(|c| c.keys.contains(code))
    }

    pub fn button_held(&self, button: i64) -> bool {
        self.connections
            .values()
            .any(|c| c.buttons.contains(&button))
    }

    pub fn single_controller(&self, session: SessionId) -> Option<Key> {
        let mut granted = self
            .connections
            .iter()
            .filter(|(key, c)| key.0 == session && !c.grant.is_empty())
            .map(|(key, _)| key);
        let single = granted.next()?;
        granted.next().is_none().then(|| single.clone())
    }

    pub fn release_connection(&mut self, key: &Key) {
        if self.connections.get(key).is_none_or(|c| c.grant.is_empty()) {
            return;
        }
        let session = key.0;
        if !self.co_play(session) {
            self.release();
            return;
        }
        let _ = self.clear_held(key);
        let freed = self.close_pads(key);
        let c = self.connections.get_mut(key).expect("granted connection");
        c.pad_failed.clear();
        c.grant.clear();
        c.suspended = false;
        c.input_failed = false;
        if let Some(remaining) = self.single_controller(session) {
            self.send_state(&remaining);
        }
        // A departing Viewer hands its controllers back; anyone refused earlier
        // needs the new count to know the retry is worth attempting.
        if freed {
            self.announce_gamepad_room();
        }
        if self
            .connections
            .iter()
            .any(|(other, c)| other.0 == session && !c.grant.is_empty())
        {
            return;
        }
        self.owner = None;
    }

    pub fn revoke(&mut self, key: &Key) {
        let Some(c) = self.connections.get_mut(key) else {
            return;
        };
        c.requested = false;
        self.release_connection(key);
        self.send(key, "ready");
        self.notify(key.0);
    }

    pub fn revoke_edge(&mut self, session: SessionId, edge_id: &str) {
        for key in self.session_keys(session) {
            if self.connections[&key].edge_id == edge_id {
                self.revoke(&key);
            }
        }
    }

    /// Automatic permission never takes another source. Concurrent control
    /// requires the source to allow simultaneous input and leaves the one
    /// desktop owner in place until explicit approval hands it over.
    pub fn can_join(&self, key: &Key) -> bool {
        let Some(owner) = self.owner else {
            return true;
        };
        let session = &self.sessions[&key.0];
        owner == key.0 && session.co_play && session.simultaneous
    }

    pub fn grant(&mut self, key: &Key) -> Result<(), Error> {
        if !self.connections[key].grant.is_empty() {
            return Ok(());
        }
        let session = key.0;
        if self.co_play(session) {
            if !self.can_join(key) {
                let state = self.state(key);
                self.reply(key, state, "busy");
                return Err(Error::Busy);
            }
        } else {
            self.release();
        }
        let previous = self.single_controller(session);
        let c = self.connections.get_mut(key).expect("known connection");
        c.grant = random_hex(32);
        c.requested = false;
        c.pad_failed.clear();
        self.owner = Some(session);
        if let Some(previous) = previous {
            self.send_state(&previous);
        }
        // A grant for a background window could not receive input until the Host
        // brought it forward by hand. Raise it now, once, while the person is
        // asking for control. Whether Windows lets this window take the
        // foreground is decided by the observation below, not assumed here.
        if let Some(target) = self.sessions.get(&session).map(|s| s.target.clone()) {
            self.sink.raise(&target);
        }
        self.refresh_active();
        if !self.connections[key].grant.is_empty() {
            self.send_state(key);
        }
        self.notify(session);
        Ok(())
    }
}

impl Connection {
    fn id(&self) -> &str {
        &self.endpoint_id
    }
}
