//! Room creation, replacement and Host ownership checks.

use piik_protocol::CodeEntryPolicy;

use crate::error::{Error, ErrorCode, invalid};
use crate::password::DerivedMaterial;
use crate::store::{
    ClosedRoom, CreatedRoom, ReplacedRoom, RoomRef, Store, check_identity_and_gate,
    connected_session_ids, room_ref, split_derived, stored_room_authority, verify_digest,
};

impl Store {
    /// Checks room capacity before password derivation.
    pub fn begin_create_room(&self) -> Result<(), Error> {
        self.ensure_initialized()?;
        if self.rooms.len() >= self.max_rooms {
            return Err(ErrorCode::RoomLimit.into());
        }
        Ok(())
    }

    /// Commits room creation with the unchanged derivation result.
    pub fn create_room(
        &mut self,
        policy: CodeEntryPolicy,
        derived: DerivedMaterial,
        preferred_room_id: Option<&str>,
    ) -> Result<CreatedRoom, Error> {
        // Derivation failures precede admission checks; gate outcomes are
        // resolved below.
        let (material, gate) = split_derived(derived)?;
        self.ensure_initialized()?;
        if self.rooms.len() >= self.max_rooms {
            return Err(ErrorCode::RoomLimit.into());
        }
        match gate {
            Some(Error::Room(ErrorCode::RoomBusy)) => return Err(ErrorCode::RoomBusy.into()),
            Some(_) => return Err(invalid("Room creation password derivation was cancelled")),
            None => {}
        }
        let room_id = self.take_room_code(preferred_room_id)?;
        let result = self
            .new_room(&room_id, policy, material)
            .and_then(|(room, created)| {
                if let Some(database) = &self.database {
                    database.insert_room(&stored_room_authority(&room))?;
                }
                Ok((room, created))
            });
        match result {
            Ok((room, created)) => {
                self.rooms.set(room_id, room);
                Ok(created)
            }
            Err(error) => {
                self.release_room_code(room_id);
                Err(error)
            }
        }
    }

    /// Returns the room owned by the exact Host token. The caller retains its
    /// identity while deriving password material outside the lock.
    pub fn host_managed_room(&self, room_id: &str, host_token: &str) -> Result<RoomRef, Error> {
        self.ensure_initialized()?;
        self.host_managed(room_id, host_token).map(room_ref)
    }

    /// The `may_start` predicate for replacement and password changes: the
    /// room must still be the same incarnation and the token must still own
    /// it. Call it with the lock held.
    pub fn host_still_owns_room(&self, room_id: &str, host_token: &str, current: &RoomRef) -> bool {
        self.rooms.get(&room_id.to_owned()).is_some_and(|room| {
            room.incarnation == current.incarnation
                && verify_digest(host_token, &room.host_token_digest)
        })
    }

    /// Commits a replacement. `current` is the reference
    /// [`Store::host_managed_room`] returned before the derivation.
    pub fn replace_room(
        &mut self,
        room_id: &str,
        host_token: &str,
        policy: CodeEntryPolicy,
        derived: DerivedMaterial,
        current: &RoomRef,
    ) -> Result<ReplacedRoom, Error> {
        // Derivation failures return before rechecking authority; gate
        // outcomes do not.
        let (material, gate) = split_derived(derived)?;
        self.ensure_initialized()?;
        let owned = self.host_managed(room_id, host_token)?;
        check_identity_and_gate(owned, current, gate)?;

        let replacement_room_id = self.take_room_code(None)?;
        match self.commit_replacement(room_id, &replacement_room_id, policy, material) {
            Ok(replaced) => Ok(replaced),
            Err(error) => {
                self.release_room_code(replacement_room_id);
                Err(error)
            }
        }
    }

    fn commit_replacement(
        &mut self,
        room_id: &str,
        replacement_room_id: &str,
        policy: CodeEntryPolicy,
        material: Option<Vec<u8>>,
    ) -> Result<ReplacedRoom, Error> {
        let (room, created) = self.new_room(replacement_room_id, policy, material)?;
        let key = room_id.to_owned();
        let current = self
            .rooms
            .get(&key)
            .ok_or_else(|| invalid("Replaced room is missing"))?;
        if let Some(database) = &self.database {
            database.replace_room(
                room_id,
                &current.host_token_digest,
                &stored_room_authority(&room),
            )?;
        }
        let closed = ClosedRoom {
            room_id: key.clone(),
            session_ids: connected_session_ids(current),
        };
        self.rooms.delete(&key);
        self.rooms.set(replacement_room_id.to_owned(), room);
        self.release_room_code(key);
        Ok(ReplacedRoom { created, closed })
    }
}
