//! Room store, database and retention tests.

mod database;
mod retention;
mod store;

use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use piik_protocol::{CodeEntryPolicy, Role};
use tempfile::TempDir;

use crate::password::VIEWER_PASSWORD_SALT_BYTES;
use crate::{
    ConnectParticipantInput, ConnectViewerWithPasswordInput, ConnectedParticipant, CreatedRoom,
    Database, Error, ErrorCode, Options, PasswordDeriver, RandomSource, ReplacedRoom, Store,
};

const OPEN: CodeEntryPolicy = CodeEntryPolicy::Open;
const PRIVATE: CodeEntryPolicy = CodeEntryPolicy::Private;

fn new_store(mut options: Options) -> Store {
    if options.max_rooms == 0 {
        options.max_rooms = 2;
    }
    if options.max_viewers_per_room == 0 {
        options.max_viewers_per_room = 2;
    }
    let mut store = Store::new(options).expect("Store::new");
    store.initialize().expect("initialize");
    store
}

fn constant_random(value: impl Fn(usize) -> u8 + Send + Sync + 'static) -> Option<RandomSource> {
    Some(Arc::new(move |size| vec![value(size); size]))
}

/// A settable clock shared with the store.
#[derive(Clone, Default)]
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn set(&self, value: i64) {
        self.0.store(value, Ordering::SeqCst);
    }

    fn get(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }

    fn source(&self) -> Option<crate::Clock> {
        let clock = self.0.clone();
        Some(Arc::new(move || clock.load(Ordering::SeqCst)))
    }
}

/// The locked/unlocked/locked creation sequence the crate documents.
fn try_create_room(
    store: &mut Store,
    policy: CodeEntryPolicy,
    password: &str,
    preferred_room_id: &str,
) -> Result<CreatedRoom, Error> {
    store.begin_create_room()?;
    let derived = if password.is_empty() {
        Ok(None)
    } else {
        store
            .password_deriver()
            .derive_viewer_password_material(password, None)
            .map(Some)
    };
    store.create_room(policy, derived, Some(preferred_room_id))
}

fn create_room(
    store: &mut Store,
    policy: CodeEntryPolicy,
    password: &str,
    preferred_room_id: &str,
) -> CreatedRoom {
    try_create_room(store, policy, password, preferred_room_id).expect("create_room")
}

fn try_replace_room(
    store: &mut Store,
    room_id: &str,
    host_token: &str,
    policy: CodeEntryPolicy,
    password: &str,
) -> Result<ReplacedRoom, Error> {
    let current = store.host_managed_room(room_id, host_token)?;
    let derived = if password.is_empty() {
        Ok(None)
    } else {
        let owns = || store.host_still_owns_room(room_id, host_token, &current);
        store
            .password_deriver()
            .derive_viewer_password_material(password, Some(&owns))
            .map(Some)
    };
    store.replace_room(room_id, host_token, policy, derived, &current)
}

fn try_set_viewer_password(
    store: &mut Store,
    room_id: &str,
    password: &str,
    host_token: &str,
) -> Result<bool, Error> {
    let current = store.host_managed_room(room_id, host_token)?;
    let derived = if password.is_empty() {
        Ok(None)
    } else {
        let owns = || store.host_still_owns_room(room_id, host_token, &current);
        store
            .password_deriver()
            .derive_viewer_password_material(password, Some(&owns))
            .map(Some)
    };
    store.set_viewer_password(room_id, host_token, derived, &current)
}

fn password_input(
    room_id: &str,
    password: &str,
    client_id: &str,
    session_id: &str,
) -> ConnectViewerWithPasswordInput {
    ConnectViewerWithPasswordInput {
        room_id: room_id.to_owned(),
        password: password.to_owned(),
        client_id: client_id.to_owned(),
        session_id: session_id.to_owned(),
    }
}

fn try_connect_viewer_with_password(
    store: &mut Store,
    input: &ConnectViewerWithPasswordInput,
) -> Result<ConnectedParticipant, Error> {
    let always = || true;
    let challenge = store.viewer_password_challenge(&input.room_id)?;
    let derived = store.password_deriver().derive_viewer_password(
        &input.password,
        challenge.salt(),
        Some(&always),
    )?;
    store.connect_viewer_with_password(input, &derived, &challenge, &always)
}

fn host_input(room_id: &str, host_token: &str, session_id: &str) -> ConnectParticipantInput {
    ConnectParticipantInput {
        room_id: room_id.to_owned(),
        role: Role::Host,
        token: host_token.to_owned(),
        viewer_grant: String::new(),
        client_id: "host-client".to_owned(),
        session_id: session_id.to_owned(),
    }
}

fn viewer_input(room_id: &str, session_id: &str, viewer_grant: &str) -> ConnectParticipantInput {
    ConnectParticipantInput {
        room_id: room_id.to_owned(),
        role: Role::Viewer,
        token: String::new(),
        viewer_grant: viewer_grant.to_owned(),
        client_id: format!("viewer-{session_id}"),
        session_id: session_id.to_owned(),
    }
}

#[track_caller]
fn expect_code<T: std::fmt::Debug>(result: Result<T, Error>, code: ErrorCode) {
    match result {
        Err(Error::Room(actual)) if actual == code => {}
        other => panic!("expected {code}, got {other:?}"),
    }
}

#[track_caller]
fn expect_message<T: std::fmt::Debug>(result: Result<T, Error>, message: &str) {
    match result {
        Err(error) if error.to_string() == message => {}
        other => panic!("expected {message:?}, got {other:?}"),
    }
}

#[track_caller]
fn must_connect(store: &mut Store, input: &ConnectParticipantInput) -> ConnectedParticipant {
    store
        .connect_participant(input)
        .expect("connect_participant")
}

/// A one-shot latch that blocked derivation predicates wait on.
#[derive(Default)]
struct Release {
    open: Mutex<bool>,
    wake: Condvar,
}

impl Release {
    fn wait(&self) {
        let mut open = self.open.lock().unwrap();
        while !*open {
            open = self.wake.wait(open).unwrap();
        }
    }

    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.wake.notify_all();
    }
}

/// Opens the latch even when the operation under test panics, so scoped
/// threads can finish.
struct OpenOnDrop<'a>(&'a Release);

impl Drop for OpenOnDrop<'_> {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// Holds the derivation gate at 2 running and 16 queued derivations for the
/// duration of `operation`.
fn with_saturated_gate(store: &mut Store, operation: impl FnOnce(&mut Store)) {
    let deriver = store.password_deriver();
    let release = Release::default();
    thread::scope(|scope| {
        let _open = OpenOnDrop(&release);
        for _ in 0..18 {
            scope.spawn(|| {
                let blocked = || {
                    release.wait();
                    false
                };
                let _ = deriver.derive_viewer_password(
                    "gate-password",
                    &[0; VIEWER_PASSWORD_SALT_BYTES],
                    Some(&blocked),
                );
            });
        }
        wait_for_saturated_gate(&deriver);
        operation(store);
    });
}

fn wait_for_saturated_gate(deriver: &PasswordDeriver) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let (active, pending) = deriver.gate_state();
        if active == 2 && pending == 16 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "gate did not saturate: active={active} pending={pending}"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

/// A temporary database path. Keep the directory alive (declared before the
/// store) so the store closes first; Windows will not remove an open file.
fn database_path() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("rooms.sqlite");
    (dir, path)
}

fn stable_store(path: &std::path::Path, mut options: Options) -> Store {
    if options.max_rooms == 0 {
        options.max_rooms = 8;
    }
    options.max_viewers_per_room = 8;
    options.database = Some(Database::new(path).expect("Database::new"));
    let mut store = Store::new(options).expect("Store::new");
    store.initialize().expect("initialize");
    store
}

fn db_viewer_input(room_id: &str, session_id: &str, viewer_grant: &str) -> ConnectParticipantInput {
    let mut input = viewer_input(room_id, session_id, viewer_grant);
    input.client_id = format!("client-{session_id}");
    input
}

fn database(store: &Store) -> &Database {
    store.database.as_ref().expect("stable store")
}
