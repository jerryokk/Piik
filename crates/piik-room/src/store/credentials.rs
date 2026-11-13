//! Viewer credential and code-entry policy updates.

use piik_protocol::CodeEntryPolicy;

use crate::error::{Error, invalid};
use crate::password::DerivedMaterial;
use crate::store::{
    AdmittedBy, CodeEntryUpdate, RevokedViewer, RoomRef, Store, ViewerGrantAction,
    ViewerGrantUpdate, check_identity_and_gate, create_viewer_grant, digest,
    new_authorization_generation, random_digest, room_occupied, split_derived,
};

impl Store {
    pub fn set_viewer_password(
        &mut self,
        room_id: &str,
        host_token: &str,
        derived: DerivedMaterial,
        current: &RoomRef,
    ) -> Result<bool, Error> {
        // Derivation failures return before rechecking authority; gate
        // outcomes do not.
        let (material, gate) = split_derived(derived)?;
        self.ensure_initialized()?;
        let owned = self.host_managed(room_id, host_token)?;
        check_identity_and_gate(owned, current, gate)?;
        if let Some(database) = &self.database {
            database.set_viewer_password(room_id, &owned.host_token_digest, material.as_deref())?;
        }
        let enabled = material.is_some();
        if let Some(room) = self.rooms.get_mut(&room_id.to_owned()) {
            room.viewer_password_material = material;
        }
        Ok(enabled)
    }

    /// Updates code admission under exact Host authority.
    pub fn set_code_entry_policy(
        &mut self,
        room_id: &str,
        policy: CodeEntryPolicy,
        host_token: &str,
    ) -> Result<CodeEntryUpdate, Error> {
        self.ensure_initialized()?;
        let room = self.host_managed(room_id, host_token)?;
        if let Some(database) = &self.database {
            database.set_code_entry_policy(room_id, &room.host_token_digest, policy)?;
        }
        let room = self
            .rooms
            .get_mut(&room_id.to_owned())
            .ok_or_else(|| invalid("Managed room is missing"))?;
        room.code_entry_policy = policy;
        Ok(CodeEntryUpdate {
            code_entry_policy: policy,
            viewer_password_enabled: room.viewer_password_material.is_some(),
        })
    }

    /// Rotates or revokes invitations and removes grant-admitted Viewers.
    pub fn set_viewer_grant(
        &mut self,
        room_id: &str,
        action: ViewerGrantAction,
        host_token: &str,
    ) -> Result<ViewerGrantUpdate, Error> {
        self.ensure_initialized()?;
        let room = self.host_managed(room_id, host_token)?;
        let previous_generation = room.viewer_authorization_generation.clone();
        let viewer_grant = match action {
            ViewerGrantAction::Rotate => Some(create_viewer_grant(&self.random)?),
            ViewerGrantAction::Revoke => None,
        };
        let revoked_viewers: Vec<RevokedViewer> = room
            .viewers
            .iter()
            .filter(|(_, viewer)| viewer.admitted_by == AdmittedBy::Grant)
            .map(|(_, viewer)| RevokedViewer {
                peer_id: viewer.peer_id.clone(),
                session_id: viewer.session_id.clone(),
            })
            .collect();
        let viewer_grant_digest = match &viewer_grant {
            Some(grant) => digest(grant),
            None => random_digest(&self.random)?,
        };
        let generation = new_authorization_generation(&self.random)?;
        if let Some(database) = &self.database {
            database.set_viewer_grant(
                room_id,
                &room.host_token_digest,
                &viewer_grant_digest,
                &generation,
            )?;
        }
        let now = self.now.clone();
        let room = self
            .rooms
            .get_mut(&room_id.to_owned())
            .ok_or_else(|| invalid("Managed room is missing"))?;
        room.viewer_grant_digest = viewer_grant_digest;
        room.viewer_authorization_generation = generation;
        let was_occupied = room_occupied(room);
        let granted: Vec<String> = room
            .viewers
            .iter()
            .filter(|(_, viewer)| viewer.admitted_by == AdmittedBy::Grant)
            .map(|(client_id, _)| client_id.clone())
            .collect();
        for client_id in &granted {
            room.viewers.delete(client_id);
        }
        if was_occupied && !room_occupied(room) {
            room.empty_since_ms = now();
        }
        Ok(ViewerGrantUpdate {
            viewer_grant,
            viewer_authorization_generation: room.viewer_authorization_generation.clone(),
            previous_viewer_authorization_generation: previous_generation,
            revoked_viewers,
        })
    }
}
