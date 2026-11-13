use rusqlite::Connection;
use sha2::{Digest, Sha256};

use super::*;
use crate::{StoredRoomAuthority, ViewerGrantAction};

fn raw_database(path: &std::path::Path) -> Connection {
    Connection::open(path).expect("raw open")
}

fn raw_exec(path: &std::path::Path, statement: &str) {
    raw_database(path)
        .execute_batch(statement)
        .unwrap_or_else(|error| panic!("raw exec {statement:?}: {error}"));
}

/// The primary or extended SQLite result code of a database failure.
fn sqlite_code(error: &Error) -> Option<i32> {
    match error {
        Error::Database(rusqlite::Error::SqliteFailure(failure, _)) => Some(failure.extended_code),
        _ => None,
    }
}

const SQLITE_FULL: i32 = 13;
const SQLITE_READONLY: i32 = 8;
const SQLITE_CONSTRAINT_FOREIGNKEY: i32 = 787;

#[test]
fn does_not_persist_a_room_when_password_derivation_is_busy() {
    let (_dir, path) = database_path();
    let mut first = stable_store(
        &path,
        Options {
            max_rooms: 1,
            ..Options::default()
        },
    );
    with_saturated_gate(&mut first, |store| {
        expect_code(
            try_create_room(store, PRIVATE, "room-password", "4321"),
            ErrorCode::RoomBusy,
        );
    });
    assert_eq!(first.size(), 0);
    first.close().unwrap();

    let mut second = stable_store(
        &path,
        Options {
            max_rooms: 1,
            ..Options::default()
        },
    );
    assert_eq!(second.size(), 0);
    assert_eq!(
        create_room(&mut second, PRIVATE, "", "4321").room_id,
        "4321"
    );
}

#[test]
fn room_creation_storage_failures_preserve_authority() {
    for (name, install, remove, code) in [
        (
            "read-only connection",
            "PRAGMA query_only = ON",
            "PRAGMA query_only = OFF",
            SQLITE_READONLY,
        ),
        (
            // The room INSERT succeeds; a deferred constraint rejects COMMIT.
            // Temporary objects leave the production schema unchanged.
            "commit rejection",
            "PRAGMA foreign_keys = ON;
             CREATE TEMP TABLE fault_parent (id INTEGER PRIMARY KEY);
             CREATE TEMP TABLE fault_child (
               id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED
             );
             CREATE TEMP TRIGGER reject_commit AFTER INSERT ON main.rooms
             BEGIN INSERT INTO fault_child VALUES (1); END;",
            "DROP TRIGGER temp.reject_commit",
            SQLITE_CONSTRAINT_FOREIGNKEY,
        ),
    ] {
        let (_dir, path) = database_path();
        let mut store = stable_store(&path, Options::default());
        create_room(&mut store, OPEN, "", "4321");
        let before = database(&store).read_stored_rooms().unwrap();
        let free = store.free_room_codes.len();
        database(&store).exec_batch(install).unwrap();
        let error = try_create_room(&mut store, OPEN, "", "5678").unwrap_err();
        assert_eq!(sqlite_code(&error), Some(code), "{name}: {error:?}");
        let after = database(&store).read_stored_rooms().unwrap();
        assert_eq!(before, after, "{name}");
        assert_eq!(store.size(), 1, "{name}");
        assert_eq!(store.free_room_codes.len(), free, "{name}");
        database(&store).exec_batch(remove).unwrap();
        let created = create_room(&mut store, OPEN, "", "5678");
        assert_eq!(
            created.room_id, "5678",
            "{name}: failed write did not release the code"
        );
        store.close().unwrap();

        let restored = stable_store(&path, Options::default());
        assert_eq!(restored.size(), 2, "{name}");
        restored
            .host_managed_room(&created.room_id, &created.host_token)
            .unwrap_or_else(|error| panic!("{name}: restored Host authority: {error}"));
    }
}

#[test]
fn database_full_does_not_consume_room_authority_or_prevent_later_writes() {
    let (_dir, path) = database_path();
    let mut store = stable_store(
        &path,
        Options {
            max_rooms: 1000,
            ..Options::default()
        },
    );
    let initial_free = store.free_room_codes.len();
    let pages = database(&store).read_pragma_integer("page_count").unwrap();
    database(&store)
        .exec_batch(&format!("PRAGMA max_page_count = {pages}"))
        .unwrap();
    for n in 0..1000 {
        let room_id = format!("{:04}", 1000 + n);
        let Err(error) = try_create_room(&mut store, OPEN, "", &room_id) else {
            continue;
        };
        assert_eq!(sqlite_code(&error), Some(SQLITE_FULL), "{error:?}");
        let rows = database(&store).read_stored_rooms().unwrap();
        assert_eq!(rows.len(), n);
        assert_eq!(store.size(), n);
        assert_eq!(store.free_room_codes.len(), initial_free - n);
        database(&store)
            .exec_batch("PRAGMA max_page_count = 1000")
            .unwrap();
        let created = create_room(&mut store, OPEN, "", &room_id);
        assert_eq!(created.room_id, room_id);
        store.close().unwrap();

        let restored = stable_store(
            &path,
            Options {
                max_rooms: 1000,
                ..Options::default()
            },
        );
        assert_eq!(restored.size(), n + 1);
        restored
            .host_managed_room(&created.room_id, &created.host_token)
            .unwrap();
        return;
    }
    panic!("bounded database did not reach SQLITE_FULL");
}

#[test]
fn restores_the_exact_authority_aggregate_without_participants() {
    let (_dir, path) = database_path();
    let mut first = stable_store(&path, Options::default());
    let room = create_room(&mut first, OPEN, "", "4321");
    first
        .set_code_entry_policy(&room.room_id, PRIVATE, &room.host_token)
        .unwrap();
    try_set_viewer_password(&mut first, &room.room_id, "room-password", &room.host_token).unwrap();
    let rotated = first
        .set_viewer_grant(&room.room_id, ViewerGrantAction::Rotate, &room.host_token)
        .unwrap();
    let rotated_grant = rotated.viewer_grant.clone().unwrap();
    let host = must_connect(
        &mut first,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );
    let old_viewer = must_connect(
        &mut first,
        &db_viewer_input(&room.room_id, "old-viewer", &rotated_grant),
    );
    assert_eq!(
        first.get_connected_host(&room.room_id).unwrap().peer_id,
        host.peer_id
    );
    assert!(
        first
            .get_connected_viewer(&room.room_id, &old_viewer.peer_id)
            .is_some()
    );
    first.close().unwrap();

    let mut second = stable_store(&path, Options::default());
    assert_eq!(second.size(), 1);
    assert!(second.get_connected_host(&room.room_id).is_none());
    assert!(second.get_connected_viewers(&room.room_id).is_empty());
    expect_code(
        second.connect_participant(&db_viewer_input(
            &room.room_id,
            "revoked-grant",
            &room.viewer_grant,
        )),
        ErrorCode::InvalidToken,
    );
    let granted = must_connect(
        &mut second,
        &db_viewer_input(&room.room_id, "current-grant", &rotated_grant),
    );
    assert_eq!(
        granted.viewer_authorization_generation,
        rotated.viewer_authorization_generation
    );
    let connected = try_connect_viewer_with_password(
        &mut second,
        &password_input(
            &room.room_id,
            "room-password",
            "password-client",
            "password-session",
        ),
    )
    .unwrap();
    assert_eq!(connected.code_entry_policy, PRIVATE);
    expect_code(
        second.connect_participant(&host_input(&room.room_id, "wrong-token", "host-session")),
        ErrorCode::InvalidToken,
    );
    must_connect(
        &mut second,
        &host_input(&room.room_id, &room.host_token, "recovered-host"),
    );
    second.close().unwrap();

    let raw = raw_database(&path);
    let mut statement = raw.prepare("SELECT * FROM rooms").unwrap();
    let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
    assert_eq!(
        columns,
        [
            "room_id",
            "host_token_digest",
            "viewer_grant_digest",
            "viewer_authorization_generation",
            "code_entry_policy",
            "viewer_password_material",
        ]
    );
    let (room_id, host_digest, grant_digest, generation, policy, material) = statement
        .query_row([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Vec<u8>>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Vec<u8>>(5)?,
            ))
        })
        .unwrap();
    assert_eq!(
        host_digest,
        Sha256::digest(room.host_token.as_bytes()).to_vec()
    );
    assert_eq!(
        grant_digest,
        Sha256::digest(rotated_grant.as_bytes()).to_vec()
    );
    assert_eq!(material.len(), 48);
    // Nothing reversible is stored: no token, grant or password appears.
    let mut stored = format!("{room_id}{generation}{policy}").into_bytes();
    stored.extend_from_slice(&host_digest);
    stored.extend_from_slice(&grant_digest);
    stored.extend_from_slice(&material);
    for secret in [
        room.host_token.as_str(),
        rotated_grant.as_str(),
        "room-password",
    ] {
        assert!(
            !stored
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "a credential was stored verbatim"
        );
    }
}

#[test]
fn preserves_disconnected_authority_across_repeated_restarts() {
    let (_dir, path) = database_path();
    let mut first = stable_store(&path, Options::default());
    let room = create_room(&mut first, OPEN, "", "5890");
    let host = must_connect(
        &mut first,
        &host_input(&room.room_id, &room.host_token, "host-session"),
    );
    first
        .disconnect_participant(&room.room_id, &host.peer_id, "host-session")
        .unwrap();
    first.close().unwrap();

    for _ in 0..3 {
        let mut restored = stable_store(&path, Options::default());
        assert_eq!(restored.size(), 1);
        let viewer = must_connect(
            &mut restored,
            &db_viewer_input(&room.room_id, "dormant-viewer", &room.viewer_grant),
        );
        assert!(!viewer.host_online, "restored room reported an online host");
        must_connect(
            &mut restored,
            &host_input(&room.room_id, &room.host_token, "host-session"),
        );
        restored.close().unwrap();
    }
}

#[test]
fn persists_explicit_room_deletion_before_recycling_its_code() {
    let (_dir, path) = database_path();
    let mut first = stable_store(&path, Options::default());
    let room = create_room(&mut first, OPEN, "", "6789");
    assert_eq!(
        first.abandon_room(&room.room_id).unwrap().unwrap().room_id,
        room.room_id
    );
    first.close().unwrap();

    let mut second = stable_store(&path, Options::default());
    assert_eq!(second.size(), 0);
    let replacement = create_room(&mut second, OPEN, "", &room.room_id);
    assert_eq!(replacement.room_id, room.room_id);
    assert_ne!(replacement.host_token, room.host_token);
    assert_ne!(replacement.viewer_grant, room.viewer_grant);
}

#[test]
fn persists_room_replacement_as_one_authority_transition() {
    let (_dir, path) = database_path();
    let mut first = stable_store(&path, Options::default());
    let original = create_room(&mut first, OPEN, "", "4321");
    let replacement = try_replace_room(
        &mut first,
        &original.room_id,
        &original.host_token,
        PRIVATE,
        "new-password",
    )
    .unwrap();
    first.close().unwrap();

    let mut second = stable_store(&path, Options::default());
    assert_eq!(second.size(), 1);
    expect_code(
        second.connect_participant(&host_input(
            &original.room_id,
            &original.host_token,
            "host-session",
        )),
        ErrorCode::InvalidToken,
    );
    let connected = must_connect(
        &mut second,
        &db_viewer_input(
            &replacement.created.room_id,
            "replacement-viewer",
            &replacement.created.viewer_grant,
        ),
    );
    assert_eq!(connected.room_id, replacement.created.room_id);
    let with_password = try_connect_viewer_with_password(
        &mut second,
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
fn rolls_back_replacement_when_old_authority_is_stale() {
    let (_dir, path) = database_path();
    let mut first = stable_store(&path, Options::default());
    create_room(&mut first, PRIVATE, "room-password", "4321");
    first.close().unwrap();

    let mut database = Database::new(&path).unwrap();
    let before = database.initialize().unwrap();
    assert_eq!(before.len(), 1);
    let mut replacement = before[0].clone();
    replacement.room_id = "5678".to_owned();
    expect_message(
        database.replace_room("4321", &[0; 32], &replacement),
        "Room database replacement delete did not match current authority",
    );
    database.close().unwrap();
    assert_eq!(database.initialize().unwrap(), before);
    database.close().unwrap();
}

/// The configured path is opened literally: `%41` is not decoded and `#`
/// does not end the file name.
#[test]
fn opens_the_path_with_uri_reserved_characters_literally() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rooms%41#1.sqlite");
    let mut first = stable_store(&path, Options::default());
    create_room(&mut first, OPEN, "", "4321");
    first.close().unwrap();
    assert!(path.exists(), "the configured file was not the one opened");
    let mut second = stable_store(&path, Options::default());
    assert_eq!(second.size(), 1);
    second.close().unwrap();
}

#[test]
fn rejects_another_owner_of_the_same_database() {
    let (_dir, path) = database_path();
    let mut first = Database::new(&path).unwrap();
    first.initialize().unwrap();
    let mut contender = Database::new(&path).unwrap();
    let error = contender.initialize().unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("locked"),
        "{error}"
    );
    contender.close().unwrap();
    first.close().unwrap();

    let mut successor = Database::new(&path).unwrap();
    assert!(successor.initialize().unwrap().is_empty());
    successor.close().unwrap();
}

#[test]
fn rejects_an_unknown_database_identity() {
    for mutation in ["PRAGMA application_id = 1", "PRAGMA user_version = 99"] {
        let (_dir, path) = database_path();
        let mut initial = Database::new(&path).unwrap();
        initial.initialize().unwrap();
        initial.close().unwrap();
        raw_exec(&path, mutation);

        let mut reopened = Database::new(&path).unwrap();
        let error = reopened.initialize().unwrap_err();
        assert!(
            error.to_string().contains("does not match"),
            "{mutation}: {error}"
        );
        reopened.close().unwrap();
    }
}

#[test]
fn rejects_an_altered_current_version_schema() {
    let (_dir, path) = database_path();
    let mut initial = Database::new(&path).unwrap();
    initial.initialize().unwrap();
    initial.close().unwrap();
    raw_exec(&path, "ALTER TABLE rooms ADD COLUMN unexpected TEXT");

    let mut reopened = Database::new(&path).unwrap();
    expect_message(
        reopened.initialize(),
        "Room database columns do not match the current schema",
    );
    reopened.close().unwrap();
}

#[test]
fn rejects_corrupt_authority_rows() {
    let (_dir, path) = database_path();
    let mut initial = stable_store(&path, Options::default());
    create_room(&mut initial, OPEN, "", "7890");
    initial.close().unwrap();

    let raw = raw_database(&path);
    raw.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    raw.execute_batch("UPDATE rooms SET host_token_digest = zeroblob(31)")
        .unwrap();
    raw.close().map_err(|(_, error)| error).unwrap();

    let mut reopened = Store::new(Options {
        max_rooms: 8,
        max_viewers_per_room: 8,
        database: Some(Database::new(&path).unwrap()),
        ..Options::default()
    })
    .unwrap();
    // Either rejection is acceptable: SQLite may report the broken row from
    // PRAGMA quick_check before the digest length assertion runs.
    let error = reopened.initialize().unwrap_err().to_string();
    assert!(
        error.contains("Host token") || error.contains("integrity"),
        "{error}"
    );
    reopened.close().unwrap();
}

/// Invalid authority must be rejected before mutating SQLite.
#[test]
fn rejects_invalid_authority_arguments() {
    let valid = StoredRoomAuthority {
        room_id: "1234".to_owned(),
        host_token_digest: vec![0; 32],
        viewer_grant_digest: vec![0; 32],
        viewer_authorization_generation: "MzMzMzMzMzMzMzMzMzMzMw".to_owned(),
        code_entry_policy: OPEN,
        viewer_password_material: None,
    };
    type Case = Box<dyn Fn(&Database, StoredRoomAuthority) -> Result<(), Error>>;
    let cases: Vec<(&str, Case, &str)> = vec![
        (
            "room ID",
            Box::new(|d, mut room| {
                room.room_id = "0123".to_owned();
                d.insert_room(&room)
            }),
            "Room database contains an invalid room ID",
        ),
        (
            "host digest",
            Box::new(|d, mut room| {
                room.host_token_digest = vec![0; 31];
                d.insert_room(&room)
            }),
            "Host token digest must contain 32 bytes",
        ),
        (
            "grant digest",
            Box::new(|d, mut room| {
                room.viewer_grant_digest = vec![0; 31];
                d.insert_room(&room)
            }),
            "Viewer grant digest must contain 32 bytes",
        ),
        (
            "generation",
            Box::new(|d, mut room| {
                room.viewer_authorization_generation = "MzMzMzMzMzMzMzMzMzMzMw==".to_owned();
                d.insert_room(&room)
            }),
            "Viewer authorization generation is invalid",
        ),
        (
            "password material",
            Box::new(|d, mut room| {
                room.viewer_password_material = Some(vec![0; 47]);
                d.insert_room(&room)
            }),
            "Viewer password material must contain 48 bytes",
        ),
        (
            "identity",
            Box::new(|d, _| d.delete_rooms(&[("123".to_owned(), vec![0; 32])])),
            "Room ID is invalid",
        ),
        (
            "replacement identity",
            Box::new(|d, room| d.replace_room("1234", &[0; 32], &room)),
            "Replacement room ID must be different",
        ),
    ];
    for (name, run, want) in cases {
        let (_dir, path) = database_path();
        let mut database = Database::new(&path).unwrap();
        database.initialize().unwrap();
        let result = run(&database, valid.clone());
        assert_eq!(
            result.map_err(|error| error.to_string()),
            Err(want.to_owned()),
            "{name}"
        );
        database.close().unwrap();
    }
}

#[test]
fn rejects_a_memory_database_path() {
    for path in ["", ":memory:"] {
        expect_message(
            Database::new(path).map(|_| ()),
            "Room database path must identify a file",
        );
    }
}

#[test]
fn rejects_a_second_initialize() {
    let (_dir, path) = database_path();
    let mut database = Database::new(&path).unwrap();
    database.initialize().unwrap();
    expect_message(
        database.initialize(),
        "Room database is already initialized",
    );
    database.close().unwrap();
    expect_message(
        database.insert_room(&StoredRoomAuthority {
            room_id: "1234".to_owned(),
            host_token_digest: vec![0; 32],
            viewer_grant_digest: vec![0; 32],
            viewer_authorization_generation: "MzMzMzMzMzMzMzMzMzMzMw".to_owned(),
            code_entry_policy: OPEN,
            viewer_password_material: None,
        }),
        "Room database is not initialized",
    );
}
