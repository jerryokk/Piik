use super::*;
use crate::ViewerGrantAction;

#[test]
fn empty_room_retention_uses_authenticated_presence() {
    let clock = TestClock::default();
    let mut store = new_store(Options {
        now: clock.source(),
        ..Options::default()
    });
    let created = create_room(&mut store, OPEN, "", "4321");
    assert_eq!(
        store.empty_room_ids(0),
        std::slice::from_ref(&created.room_id),
        "never-joined room"
    );
    let host = must_connect(
        &mut store,
        &host_input(&created.room_id, &created.host_token, "host-session"),
    );
    let viewer = must_connect(
        &mut store,
        &viewer_input(&created.room_id, "viewer-session", &created.viewer_grant),
    );
    clock.set(3_600_000);
    assert!(
        store.expire_empty_rooms(clock.get()).unwrap().is_empty(),
        "occupied room retired"
    );
    store
        .disconnect_participant(&created.room_id, &host.peer_id, "host-session")
        .unwrap();
    assert!(
        store.empty_room_ids(clock.get()).is_empty(),
        "a Viewer without a Host must protect the room"
    );
    store
        .disconnect_participant(&created.room_id, &viewer.peer_id, "viewer-session")
        .unwrap();
    assert!(
        store.empty_room_ids(clock.get() - 1).is_empty(),
        "empty interval began before the last departure"
    );

    // Reconnect/replacement and a late old-session close must not retire the
    // current participant, or let failed admission renew an empty room.
    clock.set(clock.get() + 10);
    must_connect(
        &mut store,
        &host_input(&created.room_id, &created.host_token, "new-session"),
    );
    store
        .disconnect_participant(&created.room_id, &host.peer_id, "host-session")
        .unwrap();
    assert!(
        store.empty_room_ids(clock.get()).is_empty(),
        "stale disconnect made the new session empty"
    );
    store
        .disconnect_participant(&created.room_id, &host.peer_id, "new-session")
        .unwrap();
    let last_departure = clock.get();
    clock.set(clock.get() + 100);
    expect_code(
        store.connect_participant(&host_input(&created.room_id, "wrong-token", "host-session")),
        ErrorCode::InvalidToken,
    );
    store.remove_disconnected_viewer(&created.room_id, &viewer.peer_id);
    let closed = store.expire_empty_rooms(last_departure).unwrap();
    assert_eq!(closed.len(), 1);
    assert!(closed[0].session_ids.is_empty());
    assert_eq!(store.size(), 0);
    let reused = create_room(&mut store, OPEN, "", &created.room_id);
    expect_code(
        store.connect_participant(&viewer_input(
            &reused.room_id,
            "old-invite",
            &created.viewer_grant,
        )),
        ErrorCode::InvalidToken,
    );
}

#[test]
fn revoking_last_connected_viewer_starts_empty_interval() {
    let clock = TestClock::default();
    let mut store = new_store(Options {
        now: clock.source(),
        ..Options::default()
    });
    let created = create_room(&mut store, OPEN, "", "4321");
    must_connect(
        &mut store,
        &viewer_input(&created.room_id, "viewer-session", &created.viewer_grant),
    );
    clock.set(100);
    store
        .set_viewer_grant(
            &created.room_id,
            ViewerGrantAction::Revoke,
            &created.host_token,
        )
        .unwrap();
    assert!(
        store.empty_room_ids(99).is_empty(),
        "revocation did not start the empty interval"
    );
    assert_eq!(store.empty_room_ids(100).len(), 1);
    clock.set(200);
    store
        .set_viewer_grant(
            &created.room_id,
            ViewerGrantAction::Rotate,
            &created.host_token,
        )
        .unwrap();
    assert_eq!(
        store.empty_room_ids(100).len(),
        1,
        "changing credentials on an already empty room renewed it"
    );
}

#[test]
fn restore_preserves_creation_order_and_restarts_empty_interval() {
    let (_dir, path) = database_path();
    let clock = TestClock::default();
    let options = || Options {
        now: clock.source(),
        ..Options::default()
    };
    let mut first = stable_store(&path, options());
    let older = create_room(&mut first, OPEN, "", "9000");
    clock.set(clock.get() + 1);
    create_room(&mut first, OPEN, "", "1000");
    first
        .set_code_entry_policy(&older.room_id, PRIVATE, &older.host_token)
        .unwrap();
    first.close().unwrap();

    clock.set(7_200_000);
    let mut restored = stable_store(&path, options());
    assert!(
        restored.empty_room_ids(clock.get() - 1).is_empty(),
        "restart must allow participants to reconnect"
    );
    assert_eq!(restored.empty_room_ids(clock.get()), ["9000", "1000"]);
    assert_eq!(restored.expire_empty_rooms(clock.get()).unwrap().len(), 2);
    restored.close().unwrap();
    assert_eq!(
        stable_store(&path, options()).size(),
        0,
        "expired authority survived restart"
    );
}

#[test]
fn expiry_storage_failure_does_not_partially_retire_rooms() {
    let (_dir, path) = database_path();
    let mut store = stable_store(
        &path,
        Options {
            now: Some(Arc::new(|| 0)),
            ..Options::default()
        },
    );
    create_room(&mut store, OPEN, "", "9000");
    create_room(&mut store, OPEN, "", "1000");
    // Fail the second deletion, after the first executed inside the transaction.
    database(&store)
        .exec_batch(
            "CREATE TEMP TRIGGER fail_retirement BEFORE DELETE ON rooms
             WHEN OLD.room_id = '1000' BEGIN SELECT RAISE(ABORT, 'injected write failure'); END",
        )
        .unwrap();
    let free = store.free_room_codes.len();
    assert!(
        store.expire_empty_rooms(0).is_err(),
        "expiry unexpectedly succeeded"
    );
    let rows = database(&store).read_stored_rooms().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(store.size(), 2);
    assert_eq!(store.free_room_codes.len(), free);
}
