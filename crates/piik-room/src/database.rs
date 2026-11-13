//! The SQLite stable authority. Durable authority only, never presence or
//! media state.

use std::path::PathBuf;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use piik_protocol::{CodeEntryPolicy, MAX_SAFE_INTEGER, valid_room_code};
use rusqlite::{Connection, OpenFlags, params};

use crate::error::{Error, invalid};
use crate::password::VIEWER_PASSWORD_MATERIAL_BYTES;

// Database identity and the single current schema are checked before adopting
// room authority. The application ID is ASCII "SCRN".
const ROOM_DATABASE_APPLICATION_ID: i64 = 0x5343_524e;
const ROOM_DATABASE_SCHEMA_VERSION: i64 = 2;

const ROOM_SCHEMA: &str = "
        CREATE TABLE rooms (
          room_id TEXT PRIMARY KEY NOT NULL
            CHECK (room_id GLOB '[1-9][0-9][0-9][0-9]'),
          host_token_digest BLOB NOT NULL
            CHECK (typeof(host_token_digest) = 'blob' AND length(host_token_digest) = 32),
          viewer_grant_digest BLOB NOT NULL
            CHECK (typeof(viewer_grant_digest) = 'blob' AND length(viewer_grant_digest) = 32),
          viewer_authorization_generation TEXT NOT NULL
            CHECK (
              length(viewer_authorization_generation) = 22 AND
              viewer_authorization_generation NOT GLOB '*[^A-Za-z0-9_-]*'
            ),
          code_entry_policy TEXT NOT NULL
            CHECK (code_entry_policy IN ('open', 'private')),
          viewer_password_material BLOB NULL
            CHECK (
              viewer_password_material IS NULL OR
              (typeof(viewer_password_material) = 'blob' AND length(viewer_password_material) = 48)
            )
        ) STRICT;
        PRAGMA application_id = 1396920910;
        PRAGMA user_version = 2;
      ";

// The current schema only inserts/deletes rooms; updates retain rowid. Reading
// its insertion order keeps oldest-empty allocation stable across restarts,
// without persisting presence or adding a creation-time schema migration.
const SELECT_STORED_ROOMS: &str = "SELECT
         room_id,
         host_token_digest,
         viewer_grant_digest,
         viewer_authorization_generation,
         code_entry_policy,
         viewer_password_material
       FROM rooms
       ORDER BY rowid";

const INSERT_STORED_ROOM: &str = "INSERT INTO rooms (
         room_id,
         host_token_digest,
         viewer_grant_digest,
         viewer_authorization_generation,
         code_entry_policy,
         viewer_password_material
       ) VALUES (?, ?, ?, ?, ?, ?)";

const DELETE_ROOM_STATEMENT: &str = "DELETE FROM rooms WHERE room_id = ? AND host_token_digest = ?";

/// One room's durable authority, excluding transient sessions and presence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRoomAuthority {
    pub room_id: String,
    pub host_token_digest: Vec<u8>,
    pub viewer_grant_digest: Vec<u8>,
    pub viewer_authorization_generation: String,
    pub code_entry_policy: CodeEntryPolicy,
    pub viewer_password_material: Option<Vec<u8>>,
}

/// The SQLite stable authority. It holds one connection for its lifetime,
/// because `PRAGMA locking_mode = EXCLUSIVE` is a connection property.
pub struct Database {
    path: PathBuf,
    conn: Option<Connection>,
}

impl Database {
    /// Names the database file; nothing is opened until [`Database::initialize`].
    pub fn new(path: impl Into<PathBuf>) -> Result<Self, Error> {
        let path = path.into();
        if path.as_os_str().is_empty() || path.as_os_str() == ":memory:" {
            return Err(invalid("Room database path must identify a file"));
        }
        Ok(Self { path, conn: None })
    }

    /// Opens the file, takes the exclusive lock, creates or verifies the
    /// schema and restores all stored room authority.
    pub fn initialize(&mut self) -> Result<Vec<StoredRoomAuthority>, Error> {
        if self.conn.is_some() {
            return Err(invalid("Room database is already initialized"));
        }
        // Without SQLITE_OPEN_URI the configured path is opened literally.
        let conn = Connection::open_with_flags(
            &self.path,
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        self.conn = Some(conn);
        let result = self.open_exclusive();
        if result.is_err() {
            let _ = self.close();
        }
        result
    }

    fn open_exclusive(&self) -> Result<Vec<StoredRoomAuthority>, Error> {
        let conn = self.conn()?;
        // A lock conflict must fail immediately instead of waiting.
        conn.busy_timeout(Duration::ZERO)?;
        // The pragma answers "exclusive" even when another process owns the
        // file; the BEGIN EXCLUSIVE below is what actually takes the lock.
        let locking_mode: String =
            conn.query_row("PRAGMA main.locking_mode = EXCLUSIVE", [], |row| row.get(0))?;
        if locking_mode != "exclusive" {
            return Err(invalid(
                "Room database could not acquire exclusive locking mode",
            ));
        }
        conn.execute_batch("BEGIN EXCLUSIVE")?;
        let restored = self.recover().and_then(|restored| {
            conn.execute_batch("COMMIT")?;
            Ok(restored)
        });
        if restored.is_err() {
            self.rollback();
        }
        restored
    }

    fn recover(&self) -> Result<Vec<StoredRoomAuthority>, Error> {
        self.ensure_exact_schema()?;
        self.assert_database_integrity()?;
        self.read_stored_rooms()
    }

    /// Persists one validated authority record atomically.
    pub fn insert_room(&self, room: &StoredRoomAuthority) -> Result<(), Error> {
        assert_stored_room(room)?;
        self.transaction(|conn| {
            let changes = insert(conn, room)?;
            assert_single_change(changes, "insert")
        })
    }

    /// Updates the exact Host's room; `None` clears the password.
    pub fn set_viewer_password(
        &self,
        room_id: &str,
        host_token_digest: &[u8],
        viewer_password_material: Option<&[u8]>,
    ) -> Result<(), Error> {
        assert_room_identity(room_id, host_token_digest)?;
        assert_password_material(viewer_password_material)?;
        self.transaction(|conn| {
            let changes = conn.execute(
                "UPDATE rooms
              SET viewer_password_material = ?
            WHERE room_id = ? AND host_token_digest = ?",
                params![viewer_password_material, room_id, host_token_digest],
            )?;
            assert_single_change(changes, "password update")
        })
    }

    /// Persists code admission for the exact Host's room.
    pub fn set_code_entry_policy(
        &self,
        room_id: &str,
        host_token_digest: &[u8],
        code_entry_policy: CodeEntryPolicy,
    ) -> Result<(), Error> {
        assert_room_identity(room_id, host_token_digest)?;
        self.transaction(|conn| {
            let changes = conn.execute(
                "UPDATE rooms
              SET code_entry_policy = ?
            WHERE room_id = ? AND host_token_digest = ?",
                params![code_entry_policy.as_str(), room_id, host_token_digest],
            )?;
            assert_single_change(changes, "code-entry policy update")
        })
    }

    /// Atomically replaces the grant digest and authorization generation.
    pub fn set_viewer_grant(
        &self,
        room_id: &str,
        host_token_digest: &[u8],
        viewer_grant_digest: &[u8],
        viewer_authorization_generation: &str,
    ) -> Result<(), Error> {
        assert_room_identity(room_id, host_token_digest)?;
        assert_digest(viewer_grant_digest, "Viewer grant")?;
        assert_authorization_generation(viewer_authorization_generation)?;
        self.transaction(|conn| {
            let changes = conn.execute(
                "UPDATE rooms
              SET viewer_grant_digest = ?,
                  viewer_authorization_generation = ?
            WHERE room_id = ? AND host_token_digest = ?",
                params![
                    viewer_grant_digest,
                    viewer_authorization_generation,
                    room_id,
                    host_token_digest
                ],
            )?;
            assert_single_change(changes, "Viewer grant update")
        })
    }

    /// Deletes each `(room_id, host_token_digest)` in one transaction.
    pub(crate) fn delete_rooms(&self, rooms: &[(String, Vec<u8>)]) -> Result<(), Error> {
        for (room_id, host_token_digest) in rooms {
            assert_room_identity(room_id, host_token_digest)?;
        }
        self.transaction(|conn| {
            for (room_id, host_token_digest) in rooms {
                let changes =
                    conn.execute(DELETE_ROOM_STATEMENT, params![room_id, host_token_digest])?;
                assert_single_change(changes, "delete")?;
            }
            Ok(())
        })
    }

    /// Inserts the replacement and removes the old room as one authority
    /// transition.
    pub fn replace_room(
        &self,
        old_room_id: &str,
        old_host_token_digest: &[u8],
        replacement: &StoredRoomAuthority,
    ) -> Result<(), Error> {
        assert_room_identity(old_room_id, old_host_token_digest)?;
        assert_stored_room(replacement)?;
        if replacement.room_id == old_room_id {
            return Err(invalid("Replacement room ID must be different"));
        }
        self.transaction(|conn| {
            let inserted = insert(conn, replacement)?;
            assert_single_change(inserted, "replacement insert")?;
            let deleted = conn.execute(
                DELETE_ROOM_STATEMENT,
                params![old_room_id, old_host_token_digest],
            )?;
            assert_single_change(deleted, "replacement delete")
        })
    }

    /// Closes the connection; safe on a database that never opened one.
    pub fn close(&mut self) -> Result<(), Error> {
        match self.conn.take() {
            Some(conn) => conn.close().map_err(|(_, error)| error.into()),
            None => Ok(()),
        }
    }

    fn conn(&self) -> Result<&Connection, Error> {
        self.conn
            .as_ref()
            .ok_or_else(|| invalid("Room database is not initialized"))
    }

    /// Runs a raw batch on the owned connection; tests inject faults with it.
    #[cfg(test)]
    pub(crate) fn exec_batch(&self, sql: &str) -> Result<(), Error> {
        Ok(self.conn()?.execute_batch(sql)?)
    }

    fn ensure_exact_schema(&self) -> Result<(), Error> {
        let application_id = self.read_pragma_integer("application_id")?;
        let schema_version = self.read_pragma_integer("user_version")?;
        let schema_objects = self.read_schema_objects()?;

        if application_id == 0 && schema_version == 0 && schema_objects.is_empty() {
            self.conn()?.execute_batch(ROOM_SCHEMA)?;
            return Ok(());
        }
        if application_id != ROOM_DATABASE_APPLICATION_ID {
            return Err(invalid("Room database application identity does not match"));
        }
        if schema_version != ROOM_DATABASE_SCHEMA_VERSION {
            return Err(invalid("Room database schema version does not match"));
        }
        if schema_objects.len() != 1
            || schema_objects[0].0 != "table"
            || schema_objects[0].1 != "rooms"
        {
            return Err(invalid("Room database schema objects do not match"));
        }
        self.assert_exact_columns()
    }

    fn read_schema_objects(&self) -> Result<Vec<(String, String)>, Error> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(
            "SELECT type, name
           FROM sqlite_schema
          WHERE name NOT LIKE 'sqlite_%'
          ORDER BY type, name",
        )?;
        let objects = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(objects)
    }

    fn assert_database_integrity(&self) -> Result<(), Error> {
        let conn = self.conn()?;
        let mut statement = conn.prepare("PRAGMA quick_check")?;
        let results: Vec<String> = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        if results.len() != 1 || results[0] != "ok" {
            return Err(invalid("Room database integrity check failed"));
        }
        Ok(())
    }

    fn assert_exact_columns(&self) -> Result<(), Error> {
        const EXPECTED: [(&str, &str, i64, i64); 6] = [
            ("room_id", "TEXT", 1, 1),
            ("host_token_digest", "BLOB", 1, 0),
            ("viewer_grant_digest", "BLOB", 1, 0),
            ("viewer_authorization_generation", "TEXT", 1, 0),
            ("code_entry_policy", "TEXT", 1, 0),
            ("viewer_password_material", "BLOB", 0, 0),
        ];
        let conn = self.conn()?;
        let mut statement = conn.prepare("PRAGMA table_info(rooms)")?;
        let columns: Vec<(String, String, i64, i64)> = statement
            .query_map([], |row| {
                Ok((row.get(1)?, row.get(2)?, row.get(3)?, row.get(5)?))
            })?
            .collect::<Result<_, _>>()?;
        let matches = columns.len() == EXPECTED.len()
            && columns.iter().zip(EXPECTED).all(
                |((name, column_type, not_null, primary_key), expected)| {
                    (name.as_str(), column_type.as_str(), *not_null, *primary_key) == expected
                },
            );
        if !matches {
            return Err(invalid(
                "Room database columns do not match the current schema",
            ));
        }
        Ok(())
    }

    pub(crate) fn read_stored_rooms(&self) -> Result<Vec<StoredRoomAuthority>, Error> {
        let conn = self.conn()?;
        let mut statement = conn.prepare(SELECT_STORED_ROOMS)?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<Vec<u8>>>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut stored = Vec::with_capacity(rows.len());
        for (room_id, host_token_digest, viewer_grant_digest, generation, policy, material) in rows
        {
            read_digest(&host_token_digest, "Host token")?;
            read_digest(&viewer_grant_digest, "Viewer grant")?;
            let code_entry_policy = CodeEntryPolicy::parse(&policy)
                .ok_or_else(|| invalid("Room database contains an invalid code-entry policy"))?;
            if material
                .as_ref()
                .is_some_and(|material| material.len() != VIEWER_PASSWORD_MATERIAL_BYTES)
            {
                return Err(invalid(
                    "Room database contains invalid Viewer password material",
                ));
            }
            let room = StoredRoomAuthority {
                room_id,
                host_token_digest,
                viewer_grant_digest,
                viewer_authorization_generation: generation,
                code_entry_policy,
                viewer_password_material: material,
            };
            assert_stored_room(&room)?;
            stored.push(room);
        }
        Ok(stored)
    }

    pub(crate) fn read_pragma_integer(&self, name: &str) -> Result<i64, Error> {
        let invalid_pragma = || invalid(format!("Room database {name} is invalid"));
        let value: i64 = self
            .conn()?
            .query_row(&format!("PRAGMA {name}"), [], |row| row.get(0))
            .map_err(|_| invalid_pragma())?;
        if !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
            return Err(invalid_pragma());
        }
        Ok(value)
    }

    fn transaction(
        &self,
        operation: impl FnOnce(&Connection) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let conn = self.conn()?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = operation(conn).and_then(|()| Ok(conn.execute_batch("COMMIT")?));
        if result.is_err() {
            self.rollback();
        }
        result
    }

    /// SQLite may already have rolled back after an I/O or capacity error. An
    /// explicit rollback is harmless then; SQLite owns that state. The
    /// original failure is preserved rather than replaced by "no transaction".
    fn rollback(&self) {
        if let Some(conn) = &self.conn {
            let _ = conn.execute_batch("ROLLBACK");
        }
    }
}

fn insert(conn: &Connection, room: &StoredRoomAuthority) -> Result<usize, Error> {
    Ok(conn.execute(
        INSERT_STORED_ROOM,
        params![
            room.room_id,
            room.host_token_digest,
            room.viewer_grant_digest,
            room.viewer_authorization_generation,
            room.code_entry_policy.as_str(),
            room.viewer_password_material,
        ],
    )?)
}

fn assert_stored_room(room: &StoredRoomAuthority) -> Result<(), Error> {
    if !valid_room_code(&room.room_id) {
        return Err(invalid("Room database contains an invalid room ID"));
    }
    assert_digest(&room.host_token_digest, "Host token")?;
    assert_digest(&room.viewer_grant_digest, "Viewer grant")?;
    assert_authorization_generation(&room.viewer_authorization_generation)?;
    assert_password_material(room.viewer_password_material.as_deref())
}

fn assert_room_identity(room_id: &str, host_token_digest: &[u8]) -> Result<(), Error> {
    if !valid_room_code(room_id) {
        return Err(invalid("Room ID is invalid"));
    }
    assert_digest(host_token_digest, "Host token")
}

fn read_digest(value: &[u8], name: &str) -> Result<(), Error> {
    if value.len() != 32 {
        return Err(invalid(format!(
            "Room database contains an invalid {name} digest"
        )));
    }
    Ok(())
}

fn assert_digest(value: &[u8], name: &str) -> Result<(), Error> {
    if value.len() != 32 {
        return Err(invalid(format!("{name} digest must contain 32 bytes")));
    }
    Ok(())
}

fn assert_password_material(value: Option<&[u8]>) -> Result<(), Error> {
    if value.is_some_and(|value| value.len() != VIEWER_PASSWORD_MATERIAL_BYTES) {
        return Err(invalid("Viewer password material must contain 48 bytes"));
    }
    Ok(())
}

fn assert_authorization_generation(value: &str) -> Result<(), Error> {
    let canonical = URL_SAFE_NO_PAD
        .decode(value)
        .is_ok_and(|decoded| decoded.len() == 16 && URL_SAFE_NO_PAD.encode(&decoded) == value);
    if !canonical {
        return Err(invalid("Viewer authorization generation is invalid"));
    }
    Ok(())
}

fn assert_single_change(changes: usize, operation: &str) -> Result<(), Error> {
    if changes != 1 {
        return Err(invalid(format!(
            "Room database {operation} did not match current authority"
        )));
    }
    Ok(())
}
