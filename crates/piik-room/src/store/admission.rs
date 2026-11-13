//! Participant admission, disconnection and connected-peer queries.

use piik_protocol::{CodeEntryPolicy, Role};
use subtle::ConstantTimeEq;

use crate::error::{Error, ErrorCode};
use crate::password::{
    PasswordChallenge, VIEWER_PASSWORD_MATERIAL_BYTES, VIEWER_PASSWORD_SALT_BYTES,
};
use crate::store::{
    AdmittedBy, ConnectParticipantInput, ConnectViewerWithPasswordInput, ConnectedParticipant,
    DisconnectedParticipant, Store, connect_host, connect_viewer, find_viewer_by_peer_id_mut,
    room_occupied, room_ref, verify_digest, viewer_grant_is_valid,
};

impl Store {
    /// `current` is the reference [`Store::host_managed_room`] returned before
    /// the derivation.
    pub fn connect_participant(
        &mut self,
        input: &ConnectParticipantInput,
    ) -> Result<ConnectedParticipant, Error> {
        self.ensure_initialized()?;
        let Some(room) = self.rooms.get_mut(&input.room_id) else {
            // A code-only Viewer learns that the code is unknown; a credential
            // holder learns only that the credential is invalid.
            if input.role == Role::Viewer && input.viewer_grant.is_empty() {
                return Err(ErrorCode::RoomNotFound.into());
            }
            return Err(ErrorCode::InvalidToken.into());
        };
        if input.role == Role::Host {
            if !verify_digest(&input.token, &room.host_token_digest) {
                return Err(ErrorCode::InvalidToken.into());
            }
            return connect_host(room, input, &self.random);
        }
        let admitted_by = if !input.viewer_grant.is_empty() {
            if !viewer_grant_is_valid(room, &input.viewer_grant) {
                return Err(ErrorCode::InvalidToken.into());
            }
            AdmittedBy::Grant
        } else if room.code_entry_policy != CodeEntryPolicy::Open {
            return Err(ErrorCode::InvalidToken.into());
        } else {
            AdmittedBy::Code
        };
        connect_viewer(
            room,
            &input.client_id,
            &input.session_id,
            admitted_by,
            self.max_viewers_per_room,
            &self.random,
        )
    }

    /// Validates an invitation without admitting a participant.
    pub fn viewer_grant_may_enter(&self, room_id: &str, grant: &str) -> Result<bool, Error> {
        self.ensure_initialized()?;
        Ok(self
            .rooms
            .get(&room_id.to_owned())
            .is_some_and(|room| viewer_grant_is_valid(room, grant)))
    }

    /// The locked first half of password admission. A missing room and a room
    /// without a private password both answer with 48 zero bytes, so the
    /// derivation cost does not reveal which. An uninitialized store fails
    /// before this check.
    pub fn viewer_password_challenge(&self, room_id: &str) -> Result<PasswordChallenge, Error> {
        self.ensure_initialized()?;
        let found = self.rooms.get(&room_id.to_owned());
        let mut expected_material = vec![0; VIEWER_PASSWORD_MATERIAL_BYTES];
        if let Some(room) = found
            && room.code_entry_policy == CodeEntryPolicy::Private
            && let Some(material) = &room.viewer_password_material
        {
            expected_material.copy_from_slice(material);
        }
        Ok(PasswordChallenge {
            expected_material,
            room: found.map(room_ref),
        })
    }

    /// Commits password admission after derivation. `derived` is the
    /// [`PasswordDeriver::derive_viewer_password`] result; `challenge` comes
    /// from [`Store::viewer_password_challenge`]; `may_connect` is evaluated
    /// with the lock held.
    pub fn connect_viewer_with_password(
        &mut self,
        input: &ConnectViewerWithPasswordInput,
        derived: &[u8],
        challenge: &PasswordChallenge,
        may_connect: &dyn Fn() -> bool,
    ) -> Result<ConnectedParticipant, Error> {
        self.ensure_initialized()?;
        let expected = &challenge.expected_material;
        let matches = bool::from(derived.ct_eq(&expected[VIEWER_PASSWORD_SALT_BYTES..]));

        let Some(room) = self.rooms.get_mut(&input.room_id) else {
            return Err(ErrorCode::RoomNotFound.into());
        };
        let admitted = challenge
            .room
            .as_ref()
            .is_some_and(|reference| reference.incarnation == room.incarnation)
            && room.code_entry_policy == CodeEntryPolicy::Private
            && room
                .viewer_password_material
                .as_ref()
                .is_some_and(|material| bool::from(material.ct_eq(expected)))
            && matches
            && may_connect();
        if !admitted {
            return Err(ErrorCode::InvalidToken.into());
        }
        match connect_viewer(
            room,
            &input.client_id,
            &input.session_id,
            AdmittedBy::Code,
            self.max_viewers_per_room,
            &self.random,
        ) {
            Err(Error::Room(ErrorCode::RoomFull)) => Err(ErrorCode::InvalidToken.into()),
            result => result,
        }
    }

    /// Clears only the matching session; `None` when the room, peer or current
    /// session no longer matches.
    pub fn disconnect_participant(
        &mut self,
        room_id: &str,
        peer_id: &str,
        session_id: &str,
    ) -> Result<Option<DisconnectedParticipant>, Error> {
        self.ensure_initialized()?;
        let now = self.now.clone();
        let Some(room) = self.rooms.get_mut(&room_id.to_owned()) else {
            return Ok(None);
        };
        let role = if let Some(host) = room.host.as_mut().filter(|host| host.peer_id == peer_id) {
            if host.session_id.as_deref() != Some(session_id) {
                return Ok(None);
            }
            host.session_id = None;
            Role::Host
        } else {
            let Some(viewer) = find_viewer_by_peer_id_mut(room, peer_id) else {
                return Ok(None);
            };
            if viewer.session_id.as_deref() != Some(session_id) {
                return Ok(None);
            }
            viewer.session_id = None;
            Role::Viewer
        };
        if !room_occupied(room) {
            room.empty_since_ms = now();
        }
        Ok(Some(DisconnectedParticipant {
            room_id: room_id.to_owned(),
            role,
            peer_id: peer_id.to_owned(),
        }))
    }

    /// Removes a Viewer only if it has no current session.
    pub fn remove_disconnected_viewer(&mut self, room_id: &str, peer_id: &str) -> bool {
        let Some(room) = self.rooms.get_mut(&room_id.to_owned()) else {
            return false;
        };
        let Some(client_id) = room
            .viewers
            .iter()
            .find(|(_, viewer)| viewer.peer_id == peer_id)
            .filter(|(_, viewer)| viewer.session_id.is_none())
            .map(|(client_id, _)| client_id.clone())
        else {
            return false;
        };
        room.viewers.delete(&client_id)
    }
}
