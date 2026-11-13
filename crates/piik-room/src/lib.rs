//! Room codes, Host/Viewer credentials, participant membership and persistent
//! room authority. Durable mutations write SQLite before memory; participants
//! and live connections remain process-only.
//!
//! # Locking And Password Work
//!
//! [`Store`] has no internal lock. The caller keeps it behind the signaling
//! server's single lock (for example `Mutex<Store>`) for every store
//! operation, and never awaits while holding that lock. Password derivation is
//! the exception: it runs on a [`PasswordDeriver`] obtained from
//! [`Store::password_deriver`] without the store lock. Derivation is CPU-bound
//! and blocks in the admission gate, so async callers run it on a blocking
//! thread (`tokio::task::spawn_blocking`).
//!
//! Password changes use a locked begin, unlocked derive, then locked commit.
//! Pass the derivation result to [`Store::create_room`], [`Store::replace_room`]
//! or [`Store::set_viewer_password`] unchanged; the commit revalidates room
//! ownership/capacity before mapping gate outcomes. Viewer login instead
//! returns derivation errors before committing credentials.
//!
//! A derivation admission predicate runs outside the signaling lock and must
//! acquire that lock when consulting room/session state.
//! [`Store::connect_viewer_with_password`] also calls its predicate during the
//! locked commit, so that call needs the already-locked form. Both must express
//! the same current-session condition.
//!
//! [`Store::host_managed_room`] and [`Store::viewer_password_challenge`] return
//! a [`RoomRef`] for identity checks after unlocked work. A removed room is
//! never reused as its replacement; a stale reference cannot authorize
//! mutation of the new room.
//!
//! [`Options::random`] is called under the caller's lock and by derivation
//! threads, so it must be thread-safe; the default operating-system source is.

#![forbid(unsafe_code)]

mod database;
mod error;
mod password;
mod store;

#[cfg(test)]
mod tests;

pub use database::{Database, StoredRoomAuthority};
pub use error::{Error, ErrorCode};
pub use password::{DerivedMaterial, PasswordChallenge, PasswordDeriver};
pub use store::{
    CAPACITY, Clock, ClosedRoom, CodeEntryUpdate, ConnectParticipantInput,
    ConnectViewerWithPasswordInput, ConnectedParticipant, ConnectedPeer, CreatedRoom,
    DisconnectedParticipant, Options, RandomSource, ReplacedRoom, RevokedViewer, RoomRef, Store,
    ViewerGrantAction, ViewerGrantUpdate,
};
