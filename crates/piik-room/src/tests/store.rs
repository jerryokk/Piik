use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use regex::Regex;

use super::*;
use crate::store::uniform_index;
use crate::{CAPACITY, ViewerGrantAction};

fn room_code_pattern() -> Regex {
    Regex::new(r"^[1-9][0-9]{3}$").unwrap()
}

fn viewer_grant_pattern() -> Regex {
    Regex::new(r"^[A-Za-z0-9_-]{21}[AQgw]$").unwrap()
}

#[test]
fn allocates_every_free_room_code_and_recycles_releases() {
    let mut store = new_store(Options {
        max_rooms: CAPACITY,
        random: constant_random(|_| 0),
        ..Options::default()
    });
    let pattern = room_code_pattern();
    let mut rooms = Vec::with_capacity(CAPACITY);
    let mut seen = HashSet::with_capacity(CAPACITY);
    for _ in 0..CAPACITY {
        let created = create_room(&mut store, OPEN, "", "");
        assert!(pattern.is_match(&created.room_id), "{}", created.room_id);
        seen.insert(created.room_id.clone());
        rooms.push(created);
    }
    assert_eq!(seen.len(), CAPACITY);
    expect_code(
        try_create_room(&mut store, OPEN, "", ""),
        ErrorCode::RoomLimit,
    );

    let released = rooms[4_321].room_id.clone();
    let closed = store.abandon_room(&released).unwrap().unwrap();
    assert_eq!(closed.room_id, released);
    assert_eq!(create_room(&mut store, OPEN, "", "").room_id, released);
}

#[test]
fn uses_a_free_preferred_code_and_falls_back() {
    let mut store = new_store(Options {
        max_rooms: 3,
        ..Options::default()
    });
    let preferred = create_room(&mut store, OPEN, "", "4321");
    let fallback = create_room(&mut store, OPEN, "", "4321");
    assert_eq!(preferred.room_id, "4321");
    assert_ne!(fallback.room_id, "4321");
    assert_eq!(store.abandon_room("4321").unwrap().unwrap().room_id, "4321");
    assert_eq!(create_room(&mut store, OPEN, "", "4321").room_id, "4321");
}

#[test]
fn abandons_every_current_room_as_one_shutdown() {
    let mut store = new_store(Options {
        max_rooms: 3,
        ..Options::default()
    });
    let first = create_room(&mut store, OPEN, "", "");
    let second = create_room(&mut store, OPEN, "", "");
    must_connect(
        &mut store,
        &host_input(&first.room_id, &first.host_token, "host-session"),
    );

    let closed = store.abandon_all_rooms().unwrap();
    assert_eq!(closed.len(), 2);
    // Room insertion order is contract.
    assert_eq!(closed[0].room_id, first.room_id);
    assert_eq!(closed[1].room_id, second.room_id);
    assert_eq!(closed[0].session_ids, ["host-session"]);
    assert!(closed[1].session_ids.is_empty());
    assert_eq!(store.size(), 0);
}

#[test]
fn atomically_replaces_a_room_with_different_authority() {
    let mut store = new_store(Options::default());
    let original = create_room(&mut store, PRIVATE, "old-password", "4321");
    let host = must_connect(
        &mut store,
        &host_input(&original.room_id, &original.host_token, "host-session"),
    );
    must_connect(
        &mut store,
        &viewer_input(&original.room_id, "viewer-session", &original.viewer_grant),
    );

    expect_code(
        try_replace_room(
            &mut store,
            &original.room_id,
            &original.host_token,
            PRIVATE,
            "bad password",
        ),
        ErrorCode::InvalidToken,
    );
    assert_eq!(
        store.get_connected_host(&original.room_id).unwrap().peer_id,
        host.peer_id
    );

    let replacement = try_replace_room(
        &mut store,
        &original.room_id,
        &original.host_token,
        PRIVATE,
        "new-password",
    )
    .unwrap();
    assert_ne!(replacement.created.room_id, original.room_id);
    assert_eq!(replacement.closed.room_id, original.room_id);
    assert_eq!(
        replacement.closed.session_ids,
        ["host-session", "viewer-session"]
    );
    expect_code(
        store.connect_participant(&host_input(
            &original.room_id,
            &original.host_token,
            "host-session",
        )),
        ErrorCode::InvalidToken,
    );
    let connected = must_connect(
        &mut store,
        &viewer_input(
            &replacement.created.room_id,
            "new-viewer-session",
            &replacement.created.viewer_grant,
        ),
    );
    assert_eq!(connected.room_id, replacement.created.room_id);
    let with_password = try_connect_viewer_with_password(
        &mut store,
        &password_input(
            &replacement.created.room_id,
            "new-password",
            "password-viewer",
            "password-session",
        ),
    )
    .unwrap();
    assert_eq!(with_password.room_id, replacement.created.room_id);
}

#[test]
fn rejects_room_limits_beyond_the_code_space() {
    let result = Store::new(Options {
        max_rooms: CAPACITY + 1,
        max_viewers_per_room: 2,
        ..Options::default()
    });
    expect_message(
        result.map(|_| ()),
        "Room limit must be an integer between 1 and 9000",
    );
}

#[test]
fn resumes_disconnected_room_only_with_the_exact_host_token() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, OPEN, "", "");
    let host = must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );

    store
        .disconnect_participant(&room.room_id, &host.peer_id, "host-session")
        .unwrap();
    expect_code(
        store.connect_participant(&host_input(&room.room_id, "wrong-token", "host-session")),
        ErrorCode::InvalidToken,
    );
    let resumed = must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session-2"),
    );
    store
        .disconnect_participant(&room.room_id, &resumed.peer_id, "host-session-2")
        .unwrap();
    must_connect(
        &mut store,
        &viewer_input(&room.room_id, "viewer-session", &room.viewer_grant),
    );
    must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "late-host"),
    );
}

#[test]
fn manages_dormant_access_with_the_exact_host_token() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, OPEN, "", "");
    let other_room = create_room(&mut store, OPEN, "", "");

    expect_code(
        try_set_viewer_password(&mut store, &room.room_id, "room-password", "wrong-token"),
        ErrorCode::InvalidToken,
    );
    expect_code(
        store.set_code_entry_policy(&room.room_id, PRIVATE, &other_room.host_token),
        ErrorCode::InvalidToken,
    );
    let update = store
        .set_code_entry_policy(&room.room_id, PRIVATE, &room.host_token)
        .unwrap();
    assert_eq!(update.code_entry_policy, PRIVATE);
    assert!(!update.viewer_password_enabled);
    assert!(
        try_set_viewer_password(&mut store, &room.room_id, "room-password", &room.host_token)
            .unwrap()
    );
    assert!(!try_set_viewer_password(&mut store, &room.room_id, "", &room.host_token).unwrap());
    let grant = store
        .set_viewer_grant(&room.room_id, ViewerGrantAction::Rotate, &room.host_token)
        .unwrap();
    assert!(viewer_grant_pattern().is_match(grant.viewer_grant.as_deref().unwrap()));
    assert!(store.get_connected_host(&room.room_id).is_none());
}

#[test]
fn keeps_viewer_grants_independent_from_code_entry_policy() {
    let mut store = new_store(Options {
        max_rooms: 3,
        ..Options::default()
    });
    let room = create_room(&mut store, PRIVATE, "", "");
    expect_code(
        store.connect_participant(&viewer_input(&room.room_id, "code-only", "")),
        ErrorCode::InvalidToken,
    );
    expect_code(
        try_connect_viewer_with_password(
            &mut store,
            &password_input(
                &room.room_id,
                "any-password",
                "private-code-viewer",
                "private-code-session",
            ),
        ),
        ErrorCode::InvalidToken,
    );
    let granted = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "granted", &room.viewer_grant),
    );
    assert_eq!(granted.role, Role::Viewer);
}

#[test]
fn keeps_the_exact_viewer_grant_valid_for_the_room_incarnation() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, OPEN, "", "");
    must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );
    assert!(viewer_grant_pattern().is_match(&room.viewer_grant));
    let connected = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "room-lived-grant", &room.viewer_grant),
    );
    assert_eq!(connected.role, Role::Viewer);
}

#[test]
fn supports_open_and_password_enabled_private_code_entry() {
    let mut store = new_store(Options {
        max_rooms: 3,
        ..Options::default()
    });
    let open = create_room(&mut store, OPEN, "", "");
    assert_eq!(
        must_connect(&mut store, &viewer_input(&open.room_id, "open", "")).role,
        Role::Viewer
    );

    let password = create_room(&mut store, PRIVATE, "room-password", "");
    expect_code(
        store.connect_participant(&viewer_input(&password.room_id, "wrong", "")),
        ErrorCode::InvalidToken,
    );
    let connected = try_connect_viewer_with_password(
        &mut store,
        &password_input(
            &password.room_id,
            "room-password",
            "password-viewer",
            "password-session",
        ),
    )
    .unwrap();
    assert_eq!(connected.role, Role::Viewer);
}

#[test]
fn does_not_create_a_passwordless_room_when_the_gate_is_busy() {
    let mut store = new_store(Options {
        max_rooms: 1,
        ..Options::default()
    });
    with_saturated_gate(&mut store, |store| {
        expect_code(
            try_create_room(store, PRIVATE, "room-password", "4321"),
            ErrorCode::RoomBusy,
        );
    });
    assert_eq!(store.size(), 0);
    assert_eq!(create_room(&mut store, PRIVATE, "", "4321").room_id, "4321");
}

#[test]
fn keeps_gate_saturation_distinct_from_invalid_credentials() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, PRIVATE, "room-password", "");
    with_saturated_gate(&mut store, |store| {
        expect_code(
            try_connect_viewer_with_password(
                store,
                &password_input(
                    &room.room_id,
                    "room-password",
                    "busy-password-client",
                    "busy-password-session",
                ),
            ),
            ErrorCode::RoomBusy,
        );
    });
}

#[test]
fn keeps_room_replacement_and_password_updates_unchanged_when_busy() {
    let mut store = new_store(Options {
        max_rooms: 3,
        ..Options::default()
    });
    let room = create_room(&mut store, PRIVATE, "old-password", "4321");

    with_saturated_gate(&mut store, |store| {
        expect_code(
            try_replace_room(
                store,
                &room.room_id,
                &room.host_token,
                PRIVATE,
                "replacement-password",
            ),
            ErrorCode::RoomBusy,
        );
    });
    assert_eq!(store.size(), 1);
    try_connect_viewer_with_password(
        &mut store,
        &password_input(
            &room.room_id,
            "old-password",
            "after-replacement-client",
            "after-replacement-session",
        ),
    )
    .expect("password login after a busy replacement");

    with_saturated_gate(&mut store, |store| {
        expect_code(
            try_set_viewer_password(store, &room.room_id, "updated-password", &room.host_token),
            ErrorCode::RoomBusy,
        );
    });
    try_connect_viewer_with_password(
        &mut store,
        &password_input(
            &room.room_id,
            "old-password",
            "after-update-client",
            "after-update-session",
        ),
    )
    .expect("password login after a busy update");
}

#[test]
fn bounds_password_derivations_and_uses_the_same_path_for_unknown_rooms() {
    let store = Mutex::new(new_store(Options::default()));
    let deriver = store.lock().unwrap().password_deriver();
    let may_start_calls = AtomicI64::new(0);
    let release = Release::default();
    let results = Mutex::new(Vec::new());

    thread::scope(|scope| {
        let _open = OpenOnDrop(&release);
        for index in 0..40 {
            let (store, deriver, calls, release, results) =
                (&store, &deriver, &may_start_calls, &release, &results);
            scope.spawn(move || {
                // The first two admitted hold their slot until every attempt
                // has been accepted or rejected.
                let may_connect = || {
                    if calls.fetch_add(1, Ordering::SeqCst) < 2 {
                        release.wait();
                        return true;
                    }
                    false
                };
                let input = password_input(
                    "9999",
                    "bounded-password",
                    &format!("bounded-client-{index}"),
                    &format!("bounded-session-{index}"),
                );
                let challenge = store
                    .lock()
                    .unwrap()
                    .viewer_password_challenge(&input.room_id);
                let result = challenge.and_then(|challenge| {
                    let derived = deriver.derive_viewer_password(
                        &input.password,
                        challenge.salt(),
                        Some(&may_connect),
                    )?;
                    store.lock().unwrap().connect_viewer_with_password(
                        &input,
                        &derived,
                        &challenge,
                        &|| true,
                    )
                });
                results.lock().unwrap().push(result.map(|_| ()));
            });
        }
        wait_for_saturated_gate(&deriver);
        release.open();
    });

    let mut counts: HashMap<ErrorCode, usize> = HashMap::new();
    for result in results.into_inner().unwrap() {
        match result {
            Err(Error::Room(code)) => *counts.entry(code).or_default() += 1,
            other => panic!("unexpected result {other:?}"),
        }
    }
    assert_eq!(may_start_calls.load(Ordering::SeqCst), 18);
    assert_eq!(counts.get(&ErrorCode::RoomNotFound), Some(&2), "{counts:?}");
    assert_eq!(
        counts.get(&ErrorCode::InvalidToken),
        Some(&16),
        "{counts:?}"
    );
    assert_eq!(counts.get(&ErrorCode::RoomBusy), Some(&22), "{counts:?}");
}

#[test]
fn rotates_and_revokes_viewer_grants_without_changing_code_entry() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, OPEN, "", "");
    must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );

    let rotated = store
        .set_viewer_grant(&room.room_id, ViewerGrantAction::Rotate, &room.host_token)
        .unwrap();
    let rotated_grant = rotated.viewer_grant.clone().unwrap();
    expect_code(
        store.connect_participant(&viewer_input(
            &room.room_id,
            "old-grant",
            &room.viewer_grant,
        )),
        ErrorCode::InvalidToken,
    );
    let granted_viewer = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "new-grant", &rotated_grant),
    );
    let code_viewer = must_connect(&mut store, &viewer_input(&room.room_id, "still-open", ""));

    let revoked = store
        .set_viewer_grant(&room.room_id, ViewerGrantAction::Revoke, &room.host_token)
        .unwrap();
    assert!(revoked.viewer_grant.is_none());
    assert_eq!(revoked.revoked_viewers.len(), 1);
    assert_eq!(revoked.revoked_viewers[0].peer_id, granted_viewer.peer_id);
    assert_eq!(
        store
            .get_connected_viewer(&room.room_id, &code_viewer.peer_id)
            .unwrap()
            .session_id,
        "still-open"
    );
    expect_code(
        store.connect_participant(&viewer_input(&room.room_id, "revoked", &rotated_grant)),
        ErrorCode::InvalidToken,
    );
}

#[test]
fn keeps_the_old_grant_when_rotation_cannot_create_a_generation() {
    let failed_random_call = Arc::new(AtomicUsize::new(0));
    let random_calls = Arc::new(AtomicUsize::new(0));
    let random: RandomSource = {
        let (failed, calls) = (failed_random_call.clone(), random_calls.clone());
        Arc::new(move |size| {
            let call = calls.fetch_add(1, Ordering::SeqCst) + 1;
            let size = if call == failed.load(Ordering::SeqCst) {
                size - 1
            } else {
                size
            };
            vec![call as u8; size]
        })
    };
    let mut store = new_store(Options {
        random: Some(random),
        ..Options::default()
    });
    let room = create_room(&mut store, OPEN, "", "");
    must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );
    let viewer = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "existing", &room.viewer_grant),
    );

    failed_random_call.store(random_calls.load(Ordering::SeqCst) + 2, Ordering::SeqCst);
    expect_message(
        store.set_viewer_grant(&room.room_id, ViewerGrantAction::Rotate, &room.host_token),
        "Authorization generation random source must return 16 bytes",
    );
    failed_random_call.store(0, Ordering::SeqCst);

    assert_eq!(
        store
            .get_connected_viewer(&room.room_id, &viewer.peer_id)
            .unwrap()
            .session_id,
        "existing"
    );
    let connected = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "old-grant-still-valid", &room.viewer_grant),
    );
    assert_eq!(connected.role, Role::Viewer);
}

#[test]
fn clears_every_credential_on_process_restart() {
    let mut first_store = new_store(Options::default());
    let room = create_room(&mut first_store, PRIVATE, "room-password", "");
    let mut second_store = new_store(Options::default());

    let replacement = create_room(&mut second_store, OPEN, "", "");
    assert!(room_code_pattern().is_match(&replacement.room_id));
    expect_code(
        second_store.connect_participant(&host_input(
            &room.room_id,
            &room.host_token,
            "host-session",
        )),
        ErrorCode::InvalidToken,
    );
    expect_code(
        second_store.connect_participant(&viewer_input(
            &room.room_id,
            "old-grant",
            &room.viewer_grant,
        )),
        ErrorCode::InvalidToken,
    );
    expect_code(
        try_connect_viewer_with_password(
            &mut second_store,
            &password_input(&room.room_id, "room-password", "viewer", "session"),
        ),
        ErrorCode::RoomNotFound,
    );
}

#[test]
fn rejects_an_old_grant_when_a_new_room_reuses_the_same_code() {
    let mut first_store = new_store(Options {
        random: constant_random(|size| if size == 8 { 0 } else { 1 }),
        ..Options::default()
    });
    let first_room = create_room(&mut first_store, OPEN, "", "");
    first_store.abandon_room(&first_room.room_id).unwrap();

    let mut second_store = new_store(Options {
        random: constant_random(|size| if size == 8 { 0 } else { 2 }),
        ..Options::default()
    });
    let second_room = create_room(&mut second_store, OPEN, "", "");
    assert_eq!(second_room.room_id, first_room.room_id);
    assert_ne!(second_room.viewer_grant, first_room.viewer_grant);
    expect_code(
        second_store.connect_participant(&viewer_input(
            &second_room.room_id,
            "old-incarnation",
            &first_room.viewer_grant,
        )),
        ErrorCode::InvalidToken,
    );
}

#[test]
fn participant_replacement_rules() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, OPEN, "", "");
    let host = must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );

    let resumed = must_connect(
        &mut store,
        &host_input(&room.room_id, &room.host_token, "host-session-2"),
    );
    assert_eq!(resumed.peer_id, host.peer_id);
    assert_eq!(resumed.replaced_session_id.as_deref(), Some("host-session"));
    let mut second_host = host_input(&room.room_id, &room.host_token, "second-host-session");
    second_host.client_id = "second-host-client".to_owned();
    expect_code(
        store.connect_participant(&second_host),
        ErrorCode::HostAlreadyConnected,
    );

    let first = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "first", &room.viewer_grant),
    );
    let second = must_connect(
        &mut store,
        &viewer_input(&room.room_id, "second", &room.viewer_grant),
    );
    expect_code(
        store.connect_participant(&viewer_input(&room.room_id, "third", &room.viewer_grant)),
        ErrorCode::RoomFull,
    );
    assert_eq!(
        store.get_viewer_peer_ids(&room.room_id),
        [first.peer_id.clone(), second.peer_id.clone()]
    );

    // A viewer returning under the same client ID keeps its peer ID and its
    // place in the insertion order, which viewer presence is ordered by.
    let mut returning_input = viewer_input(&room.room_id, "first-again", &room.viewer_grant);
    returning_input.client_id = "viewer-first".to_owned();
    let returning = must_connect(&mut store, &returning_input);
    assert_eq!(returning.peer_id, first.peer_id);
    assert_eq!(returning.replaced_session_id.as_deref(), Some("first"));
    assert_eq!(
        store.get_viewer_peer_ids(&room.room_id),
        [first.peer_id.clone(), second.peer_id.clone()]
    );

    store
        .disconnect_participant(&room.room_id, &second.peer_id, "second")
        .unwrap();
    let connected = store.get_connected_viewers(&room.room_id);
    assert_eq!(connected.len(), 1);
    assert_eq!(connected[0].peer_id, first.peer_id);
    assert_eq!(connected[0].session_id, "first-again");
    assert!(!store.remove_disconnected_viewer(&room.room_id, &first.peer_id));
    assert!(store.remove_disconnected_viewer(&room.room_id, &second.peer_id));
    assert_eq!(
        store.get_viewer_peer_ids(&room.room_id),
        std::slice::from_ref(&first.peer_id)
    );

    for (room_id, grant, want) in [
        (room.room_id.as_str(), room.viewer_grant.as_str(), true),
        (room.room_id.as_str(), "", false),
        (room.room_id.as_str(), "AAAAAAAAAAAAAAAAAAAAAA", false),
        ("9999", room.viewer_grant.as_str(), false),
    ] {
        assert_eq!(
            store.viewer_grant_may_enter(room_id, grant).unwrap(),
            want,
            "{room_id} {grant}"
        );
    }
}

/// Pins the order of the checks after the derivation: the second Host check
/// first, then the room identity together with a cancelled derivation, then
/// busy.
#[test]
fn host_change_checks_identity_after_the_derivation() {
    // A constant random source gives a recreated room the same Host token, so
    // the identity comparison is reachable instead of being shadowed by the
    // token check.
    let mut store = new_store(Options {
        max_rooms: 3,
        random: constant_random(|_| 7),
        ..Options::default()
    });
    let room = create_room(&mut store, OPEN, "", "4321");
    let current = store
        .host_managed_room(&room.room_id, &room.host_token)
        .unwrap();
    assert!(store.host_still_owns_room(&room.room_id, &room.host_token, &current));

    store.abandon_room(&room.room_id).unwrap();
    assert!(!store.host_still_owns_room(&room.room_id, &room.host_token, &current));
    expect_code(
        store.set_viewer_password(&room.room_id, &room.host_token, Ok(None), &current),
        ErrorCode::InvalidToken,
    );

    let again = create_room(&mut store, OPEN, "", "4321");
    assert_eq!(again.host_token, room.host_token);
    expect_code(
        store.set_viewer_password(&again.room_id, &again.host_token, Ok(None), &current),
        ErrorCode::RoomAccessDenied,
    );

    let incarnation = store
        .host_managed_room(&again.room_id, &again.host_token)
        .unwrap();
    expect_code(
        store.set_viewer_password(
            &again.room_id,
            &again.host_token,
            Err(Error::DerivationCancelled),
            &incarnation,
        ),
        ErrorCode::RoomAccessDenied,
    );
    expect_code(
        store.set_viewer_password(
            &again.room_id,
            &again.host_token,
            Err(ErrorCode::RoomBusy.into()),
            &incarnation,
        ),
        ErrorCode::RoomBusy,
    );
    expect_code(
        store.replace_room(
            &again.room_id,
            &again.host_token,
            OPEN,
            Err(Error::DerivationCancelled),
            &incarnation,
        ),
        ErrorCode::RoomAccessDenied,
    );
}

/// A vanished room answers ROOM_NOT_FOUND; a replaced room and a refusing
/// `may_connect` answer INVALID_TOKEN.
#[test]
fn password_login_rechecks_the_room_after_the_derivation() {
    // A constant random source repeats the salt, so a recreated room holds the
    // same material and only the room identity separates the incarnations.
    let mut store = new_store(Options {
        max_rooms: 3,
        random: constant_random(|_| 7),
        ..Options::default()
    });
    let room = create_room(&mut store, PRIVATE, "room-password", "4321");
    let input = password_input(
        &room.room_id,
        "room-password",
        "password-viewer",
        "password-session",
    );
    let always = || true;
    let deriver = store.password_deriver();

    let challenge = store.viewer_password_challenge(&input.room_id).unwrap();
    let derived = deriver
        .derive_viewer_password(&input.password, challenge.salt(), None)
        .unwrap();
    store.abandon_room(&room.room_id).unwrap();
    expect_code(
        store.connect_viewer_with_password(&input, &derived, &challenge, &always),
        ErrorCode::RoomNotFound,
    );

    create_room(&mut store, PRIVATE, "room-password", "4321");
    expect_code(
        store.connect_viewer_with_password(&input, &derived, &challenge, &always),
        ErrorCode::InvalidToken,
    );

    let challenge = store.viewer_password_challenge(&input.room_id).unwrap();
    let derived = deriver
        .derive_viewer_password(&input.password, challenge.salt(), None)
        .unwrap();
    expect_code(
        store.connect_viewer_with_password(&input, &derived, &challenge, &|| false),
        ErrorCode::InvalidToken,
    );
}

/// An uninitialized store answers the initialization error at the challenge,
/// not INVALID_TOKEN after a KDF.
#[test]
fn password_login_requires_an_initialized_store() {
    let (_dir, path) = database_path();
    let store = Store::new(Options {
        max_rooms: 2,
        max_viewers_per_room: 2,
        database: Some(Database::new(&path).unwrap()),
        ..Options::default()
    })
    .unwrap();
    expect_message(
        store.viewer_password_challenge("1234"),
        "RoomStore stable authority is not initialized",
    );
}

/// ROOM_FULL must not leak to a Viewer that authenticated with the password.
#[test]
fn password_login_hides_a_full_room() {
    let mut store = new_store(Options::default());
    let room = create_room(&mut store, PRIVATE, "room-password", "");
    must_connect(
        &mut store,
        &viewer_input(&room.room_id, "first", &room.viewer_grant),
    );
    must_connect(
        &mut store,
        &viewer_input(&room.room_id, "second", &room.viewer_grant),
    );
    expect_code(
        try_connect_viewer_with_password(
            &mut store,
            &password_input(
                &room.room_id,
                "room-password",
                "password-viewer",
                "password-session",
            ),
        ),
        ErrorCode::InvalidToken,
    );
}

#[test]
fn create_room_reports_a_cancelled_derivation() {
    let mut store = new_store(Options::default());
    store.begin_create_room().unwrap();
    let derived = store
        .password_deriver()
        .derive_viewer_password_material("room-password", Some(&|| false));
    assert!(
        matches!(derived, Err(Error::DerivationCancelled)),
        "{derived:?}"
    );
    expect_message(
        store.create_room(PRIVATE, derived.map(Some), None),
        "Room creation password derivation was cancelled",
    );
    assert_eq!(store.size(), 0);
}

#[test]
fn rejects_a_malformed_viewer_password() {
    let store = new_store(Options::default());
    let deriver = store.password_deriver();
    for password in [String::new(), "with space".to_owned(), "a".repeat(65)] {
        expect_code(
            deriver.derive_viewer_password_material(&password, None),
            ErrorCode::InvalidToken,
        );
        expect_code(
            deriver.derive_viewer_password(&password, &[0; VIEWER_PASSWORD_SALT_BYTES], None),
            ErrorCode::InvalidToken,
        );
    }
}

/// `(u64::MAX % n + 1) % n` must not wrap where 2^64 % n is 0, which is every
/// power-of-two free count.
#[test]
fn uniform_index_rejection_limit() {
    for (name, count, want_index, want_draws) in [
        ("power of two accepts the top value", 8_192, 8_191, 1),
        ("one free code accepts the top value", 1, 0, 1),
        ("three free codes reject the top value", 3, 0, 2),
    ] {
        let draws = AtomicUsize::new(0);
        let random = |_size: usize| {
            if draws.fetch_add(1, Ordering::SeqCst) == 0 {
                vec![0xff; 8]
            } else {
                vec![0; 8]
            }
        };
        assert_eq!(uniform_index(count, &random).unwrap(), want_index, "{name}");
        assert_eq!(draws.load(Ordering::SeqCst), want_draws, "{name}");
    }
}

#[test]
fn uniform_index_rejects_a_short_random_source() {
    let error = uniform_index(9_000, &|_| vec![0; 7]).unwrap_err();
    assert!(error.to_string().contains("must return 8 bytes"), "{error}");
}

/// RFC 7914 section 12 (N=16384, r=8, p=1): the verifier is the first 32
/// bytes of standard scrypt output with the same parameters, so stored
/// password material stays valid.
#[test]
fn derives_the_rfc_7914_scrypt_vector() {
    let store = new_store(Options::default());
    let verifier = store
        .password_deriver()
        .derive_viewer_password("pleaseletmein", b"SodiumChloride", None)
        .unwrap();
    let expected: [u8; 32] = [
        0x70, 0x23, 0xbd, 0xcb, 0x3a, 0xfd, 0x73, 0x48, 0x46, 0x1c, 0x06, 0xcd, 0x81, 0xfd, 0x38,
        0xeb, 0xfd, 0xa8, 0xfb, 0xba, 0x90, 0x4f, 0x8e, 0x3e, 0xa9, 0xb5, 0x43, 0xf6, 0x54, 0x5d,
        0xa1, 0xf2,
    ];
    assert_eq!(verifier, expected);
}

/// Stores move between the signaling task and blocking derivation threads.
#[test]
fn store_and_deriver_are_thread_safe() {
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send::<Store>();
    assert_send_sync::<PasswordDeriver>();
    assert_send_sync::<crate::RoomRef>();
}
