// Manager tests: authorization policy, prepared changes, co-play, gamepads,
// simultaneous control and diagnostics. The shared fixture lives here.

mod authority;
mod co_play;
mod diagnostics;
mod gamepads;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;

use super::*;

/// Sends a request through the full encrypted packet path.
fn request(fixture: &Fixture, session: &TestSession, key: &Key, password: &str) {
    let event = InputEvent {
        intent: "f".repeat(32),
        password: password.into(),
        ..InputEvent::of("request")
    };
    let packet = encrypted(fixture, key, &serde_json::to_vec(&event).unwrap());
    let edge = fixture.conn(key, |c| c.edge_id.clone());
    fixture
        .manager
        .shared()
        .receive_packet(session.id(), &edge, packet, None);
}

fn bind(fixture: &Fixture, key: &Key, edge: &str, epoch: &str) {
    fixture.inner(|inner| {
        let c = inner.connections.get_mut(key).unwrap();
        c.edge_id = edge.into();
        c.epoch = epoch.into();
    });
}

fn key_down(grant: &str) -> Vec<u8> {
    format!(r#"{{"type":"key","code":"KeyA","down":true,"grant":"{grant}"}}"#).into_bytes()
}

/// Blocks device creation until the test releases it.
struct BlockedCreation {
    started: Mutex<mpsc::Sender<()>>,
    finish: Mutex<mpsc::Receiver<()>>,
    pad: RecordedPad,
    opens: AtomicUsize,
}

impl GamepadBackend for Arc<BlockedCreation> {
    fn open(&self) -> Result<Box<dyn GamepadDevice>, Error> {
        self.opens.fetch_add(1, Ordering::SeqCst);
        let _ = lock(&self.started).send(());
        let _ = lock(&self.finish).recv();
        Ok(Box::new(PadDevice(self.pad.clone())))
    }
    fn close(&self) -> Result<(), Error> {
        Ok(())
    }
}

fn blocked() -> (Arc<BlockedCreation>, mpsc::Receiver<()>, mpsc::Sender<()>) {
    let (started, on_start) = mpsc::channel();
    let (finish, on_finish) = mpsc::channel();
    let backend = Arc::new(BlockedCreation {
        started: Mutex::new(started),
        finish: Mutex::new(on_finish),
        pad: RecordedPad::default(),
        opens: AtomicUsize::new(0),
    });
    (backend, on_start, finish)
}
