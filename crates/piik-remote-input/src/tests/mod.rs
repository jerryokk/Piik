// Fakes and helpers for the manager, held-input, cipher and shortcut tests.
// Nothing here injects real input.

mod cipher;
mod held;
mod manager;
mod packets;
mod shortcut;
mod transport;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use piik_input_route::{InputChannel, Packet};
use piik_native_capture::CaptureTarget;
use piik_protocol::RemoteInputPeer;
use tokio_util::sync::CancellationToken;

use crate::cipher::{FROM_VIEWER, InputCipher};
use crate::event::{Bounds, InputEvent};
use crate::gamepad::{GamepadBackend, GamepadDevice, GamepadReport};
use crate::manager::{InputSink, Key, OpenGamepads, RegisterStop};
use crate::{Error, Manager, Session};

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// Recorded sink state shared with the test body.
#[derive(Default)]
pub(crate) struct Recorded {
    pub events: VecDeque<InputEvent>,
    pub resets: usize,
    /// How many times a grant asked for the source to be brought forward.
    pub raises: usize,
    pub target: Bounds,
    pub geometry: Bounds,
    pub focused: bool,
    pub target_error: Option<Error>,
    pub apply_error: Option<Error>,
    pub reset_error: Option<Error>,
    /// Reject only key releases, as the text-release fixture.
    pub reject_key_release: bool,
}

#[derive(Clone, Default)]
pub(crate) struct RecordedInput(pub Arc<Mutex<Recorded>>);

impl RecordedInput {
    pub fn focused() -> Self {
        let input = Self::default();
        input.with(|r| r.focused = true);
        input
    }

    pub fn with<T>(&self, f: impl FnOnce(&mut Recorded) -> T) -> T {
        f(&mut lock(&self.0))
    }

    pub fn events(&self) -> usize {
        self.with(|r| r.events.len())
    }

    pub fn resets(&self) -> usize {
        self.with(|r| r.resets)
    }

    pub fn next(&self) -> InputEvent {
        self.with(|r| r.events.pop_front())
            .expect("expected an input event")
    }

    pub fn drain(&self) -> Vec<InputEvent> {
        self.with(|r| r.events.drain(..).collect())
    }

    pub fn set_apply_error(&self, message: Option<&str>) {
        self.with(|r| r.apply_error = message.map(Error::input));
    }

    pub fn set_reset_error(&self, message: Option<&str>) {
        self.with(|r| r.reset_error = message.map(Error::input));
    }

    pub fn set_focused(&self, focused: bool) {
        self.with(|r| r.focused = focused);
    }
}

impl InputSink for RecordedInput {
    fn target(&mut self, _: &CaptureTarget) -> Result<(Bounds, bool), Error> {
        self.with(|r| match &r.target_error {
            Some(error) => Err(error.clone()),
            None => Ok((r.geometry, r.focused)),
        })
    }

    fn raise(&mut self, _: &CaptureTarget) {
        self.with(|r| r.raises += 1);
    }

    fn apply(&mut self, target: Bounds, event: &InputEvent) -> Result<(), Error> {
        self.with(|r| {
            if let Some(error) = &r.apply_error {
                return Err(error.clone());
            }
            if r.reject_key_release && event.kind == "key" && !event.down {
                return Err(Error::input("text key release rejected"));
            }
            r.target = target;
            r.events.push_back(event.clone());
            Ok(())
        })
    }

    fn reset(&mut self) -> Result<(), Error> {
        self.with(|r| {
            r.resets += 1;
            r.reset_error.clone().map_or(Ok(()), Err)
        })
    }
}

/// Gamepad fakes. A device's state is shared so tests observe it after the
/// manager has taken ownership.
#[derive(Default)]
pub(crate) struct PadState {
    pub state: GamepadReport,
    pub update_error: Option<Error>,
    pub closed: bool,
}

#[derive(Clone, Default)]
pub(crate) struct RecordedPad(pub Arc<Mutex<PadState>>);

impl RecordedPad {
    pub fn state(&self) -> GamepadReport {
        lock(&self.0).state
    }

    pub fn closed(&self) -> bool {
        lock(&self.0).closed
    }

    pub fn fail_updates(&self) {
        lock(&self.0).update_error = Some(Error::input("driver write failed"));
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

struct PadDevice(RecordedPad);

impl GamepadDevice for PadDevice {
    fn update(&mut self, report: GamepadReport) -> Result<(), Error> {
        let mut pad = lock(&self.0.0);
        if pad.closed {
            return Err(Error::input("recorded gamepad is closed"));
        }
        if let Some(error) = &pad.update_error {
            return Err(error.clone());
        }
        pad.state = report;
        Ok(())
    }

    fn close(&mut self) -> Result<(), Error> {
        if self.0.closed() {
            return Ok(());
        }
        let result = self.update(GamepadReport::default());
        lock(&self.0.0).closed = true;
        result
    }
}

#[derive(Default)]
pub(crate) struct PadsState {
    pub devices: Vec<RecordedPad>,
    pub capacity: usize,
    pub open_error: Option<Error>,
    pub closed: bool,
}

#[derive(Clone, Default)]
pub(crate) struct RecordedPads(pub Arc<Mutex<PadsState>>);

impl RecordedPads {
    pub fn with_capacity(capacity: usize) -> Self {
        let pads = Self::default();
        lock(&pads.0).capacity = capacity;
        pads
    }

    pub fn devices(&self) -> Vec<RecordedPad> {
        lock(&self.0).devices.clone()
    }

    pub fn set_open_error(&self, message: Option<&str>) {
        lock(&self.0).open_error = message.map(Error::input);
    }

    pub fn closed(&self) -> bool {
        lock(&self.0).closed
    }
}

impl GamepadBackend for RecordedPads {
    fn open(&self) -> Result<Box<dyn GamepadDevice>, Error> {
        let mut pads = lock(&self.0);
        if pads.capacity > 0
            && pads.devices.iter().filter(|pad| !pad.closed()).count() >= pads.capacity
        {
            return Err(Error::input("backend device capacity reached"));
        }
        if let Some(error) = &pads.open_error {
            return Err(error.clone());
        }
        let pad = RecordedPad::default();
        pads.devices.push(pad.clone());
        Ok(Box::new(PadDevice(pad)))
    }

    fn close(&self) -> Result<(), Error> {
        let devices = {
            let mut pads = lock(&self.0);
            pads.closed = true;
            pads.devices.clone()
        };
        for pad in devices {
            let _ = PadDevice(pad).close();
        }
        Ok(())
    }
}

/// A fake data channel for the relay; sent replies are kept for the viewer.
#[derive(Default)]
pub(crate) struct FakeChannel {
    pub closed: AtomicBool,
    pub buffered: AtomicUsize,
    pub sent: Mutex<Vec<String>>,
}

impl FakeChannel {
    pub fn open() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn take(&self) -> Vec<Packet> {
        lock(&self.sent)
            .drain(..)
            .map(|text| serde_json::from_str(&text).unwrap())
            .collect()
    }
}

impl InputChannel for FakeChannel {
    fn label(&self) -> &str {
        piik_input_route::LABEL
    }
    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    fn buffered_amount(&self) -> usize {
        self.buffered.load(Ordering::SeqCst)
    }
    fn send_text(&self, text: &str) {
        lock(&self.sent).push(text.to_owned());
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

pub(crate) type TestSession = Session<FakeChannel>;

pub(crate) struct Fixture {
    pub manager: Manager,
    pub sink: RecordedInput,
}

#[derive(Default)]
pub(crate) struct Options {
    pub gamepads: Option<Arc<dyn GamepadBackend>>,
    pub register_stop: Option<RegisterStop>,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with(RecordedInput::focused(), Options::default())
    }

    pub fn with_gamepads(backend: impl GamepadBackend + 'static) -> Self {
        Self::with(
            RecordedInput::focused(),
            Options {
                gamepads: Some(Arc::new(backend)),
                register_stop: None,
            },
        )
    }

    pub fn with(sink: RecordedInput, options: Options) -> Self {
        let backend = options.gamepads;
        let open_gamepads: OpenGamepads = Box::new(move || backend.clone());
        let manager = Manager::build(
            Box::new(sink.clone()),
            options.register_stop,
            open_gamepads,
            None,
        );
        Self { manager, sink }
    }

    pub fn session(&self) -> TestSession {
        let session = self.manager.new_session().expect("open manager");
        session.set_target(display("1"));
        session
    }

    /// Runs `f` with the manager lock held, for tests that call internals.
    pub fn inner<T>(&self, f: impl FnOnce(&mut crate::manager::Inner) -> T) -> T {
        f(&mut self.manager.shared().lock())
    }

    pub fn conn<T>(&self, key: &Key, f: impl FnOnce(&crate::manager::Connection) -> T) -> T {
        self.inner(|inner| f(&inner.connections[key]))
    }

    pub fn grant(&self, key: &Key) -> String {
        self.conn(key, |c| c.grant.clone())
    }

    pub fn owner(&self) -> Option<u64> {
        self.inner(|inner| inner.owner)
    }

    /// Delivers a plaintext message as an already decrypted packet.
    pub fn receive(&self, key: &Key, plain: &[u8]) {
        self.inner(|inner| inner.receive_locked(key, plain));
    }

    /// Sends an event as a decrypted packet, filling the request intent and
    /// current grant; gamepad reports wait for lazy creation and resend, as
    /// the next periodic full report.
    pub fn send(&self, key: &Key, mut event: InputEvent) {
        if event.kind == "request" && event.intent.is_empty() {
            event.intent = "f".repeat(32);
        }
        if event.grant.is_empty() {
            event.grant = self.grant(key);
        }
        let data = serde_json::to_vec(&event).unwrap();
        self.receive(key, &data);
        if event.kind == "gamepad" {
            self.wait_gamepad_idle();
            self.receive(key, &data);
        }
    }

    pub fn wait_gamepad_idle(&self) {
        let deadline = Instant::now() + Duration::from_secs(1);
        while self.inner(|inner| inner.gamepad_opening.is_some()) {
            assert!(Instant::now() < deadline, "gamepad creation did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn refresh(&self) {
        self.inner(|inner| {
            inner.refresh_active();
        });
    }

    pub fn retry_releases(&self) -> Result<(), Error> {
        self.inner(crate::manager::Inner::retry_releases)
    }

    pub fn release(&self) {
        self.inner(crate::manager::Inner::release);
    }

    pub fn release_connection(&self, key: &Key) {
        self.inner(|inner| inner.release_connection(key));
    }

    pub fn apply_held(&self, key: &Key, event: InputEvent) -> Result<(), Error> {
        self.inner(|inner| inner.apply_held(key, Bounds::default(), &event))
    }

    pub fn pending_key(&self, code: &str) -> bool {
        self.inner(|inner| inner.pending_keys.contains(code))
    }

    pub fn pending_button(&self, button: i64) -> bool {
        self.inner(|inner| inner.pending_buttons.contains(&button))
    }

    /// Whether this connection owns any virtual controller.
    pub fn pad(&self, key: &Key) -> bool {
        self.conn(key, |c| !c.pads.is_empty())
    }

    /// Whether this connection was refused a controller for lack of room.
    pub fn pad_refused(&self, key: &Key) -> bool {
        self.conn(key, |c| !c.pad_failed.is_empty())
    }

    /// How many controllers this connection currently owns.
    pub fn pads(&self, key: &Key) -> usize {
        self.conn(key, |c| c.pads.len())
    }
}

pub(crate) fn display(id: &str) -> CaptureTarget {
    CaptureTarget {
        kind: "display".into(),
        source_id: id.into(),
        ..CaptureTarget::default()
    }
}

pub(crate) fn window(id: &str) -> CaptureTarget {
    CaptureTarget {
        kind: "window".into(),
        source_id: id.into(),
        ..CaptureTarget::default()
    }
}

pub(crate) fn peer(name: &str, endpoint: char, secret: char) -> RemoteInputPeer {
    RemoteInputPeer {
        peer_id: name.into(),
        endpoint_id: endpoint.to_string().repeat(32),
        secret: secret.to_string().repeat(64),
    }
}

pub(crate) fn set_policy(session: &TestSession, mode: &str, password: &str) -> Result<(), Error> {
    let co_play = session.state().co_play;
    set_policy_co_play(session, mode, password, co_play)
}

pub(crate) fn set_policy_co_play(
    session: &TestSession,
    mode: &str,
    password: &str,
    co_play: bool,
) -> Result<(), Error> {
    let change = session.begin_policy(&CancellationToken::new(), mode, password, co_play)?;
    change.apply()
}

pub(crate) fn key_of(session: &TestSession, peer: &RemoteInputPeer) -> Key {
    (session.id(), peer.endpoint_id.clone())
}

/// A co-play session with simultaneous control on; edges and epochs are bound
/// directly for each Viewer.
pub(crate) fn co_play_session(fixture: &Fixture, viewers: usize) -> (TestSession, Vec<Key>) {
    let session = fixture.session();
    set_policy_co_play(&session, "room", "", true).unwrap();
    assert_eq!(
        session.state().simultaneous,
        Some(false),
        "new source enabled concurrent control by default"
    );
    session.set_simultaneous(true).unwrap();
    let peers: Vec<RemoteInputPeer> = (0..viewers)
        .map(|i| {
            let digit = format!("{:02x}", i + 1);
            RemoteInputPeer {
                peer_id: format!("viewer-{}", (b'a' + i as u8) as char),
                endpoint_id: digit.repeat(16),
                secret: digit.repeat(32),
            }
        })
        .collect();
    session.set_peers(&peers).unwrap();
    let keys: Vec<Key> = peers.iter().map(|peer| key_of(&session, peer)).collect();
    for key in &keys {
        fixture.inner(|inner| {
            let c = inner.connections.get_mut(key).unwrap();
            c.edge_id = key.1.clone();
            c.epoch = "e".repeat(32);
        });
    }
    (session, keys)
}

/// Seals a Viewer packet with the endpoint key, as the Browser does.
pub(crate) fn viewer_packet(
    cipher: &InputCipher,
    sequence: u64,
    epoch: &str,
    id: &str,
    event: &InputEvent,
) -> Packet {
    let plain = serde_json::to_vec(event).unwrap();
    Packet {
        id: id.into(),
        epoch: epoch.into(),
        data: cipher.seal_with(FROM_VIEWER, sequence, &plain, epoch),
        ..Packet::default()
    }
}

/// Seals the next Viewer packet for a connection with its own cipher.
pub(crate) fn encrypted(fixture: &Fixture, key: &Key, plain: &[u8]) -> Packet {
    fixture.conn(key, |c| Packet {
        id: key.1.clone(),
        epoch: c.epoch.clone(),
        data: c
            .cipher
            .seal_with(FROM_VIEWER, c.cipher.received + 1, plain, &c.epoch),
        ..Packet::default()
    })
}
