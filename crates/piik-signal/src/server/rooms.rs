//! Room authority operations: access updates, replacement and creation.

use std::sync::Arc;

use piik_protocol::{CodeEntryPolicy, RoomAccessUpdateRequest, RoomAccessUpdateResponse};
use piik_room::{CreatedRoom, DerivedMaterial, ViewerGrantAction};

use crate::runtime::lock;
use crate::server::close::run_blocking;
use crate::server::{Server, invite_url, locked_with_io};

/// A derivation outcome is consumed by each commit attempt; the retry after
/// capacity reclamation reuses the same result.
pub(crate) fn clone_derived(derived: &DerivedMaterial) -> DerivedMaterial {
    match derived {
        Ok(material) => Ok(material.clone()),
        Err(piik_room::Error::Room(code)) => Err(piik_room::Error::Room(*code)),
        Err(piik_room::Error::DerivationCancelled) => Err(piik_room::Error::DerivationCancelled),
        Err(error) => Err(piik_room::Error::Invalid(error.to_string())),
    }
}

impl Server {
    /// Room mutations take no cancellation: they run to completion even when
    /// the browser goes away. Password derivation runs on a blocking thread.
    pub async fn update_room_access(
        &self,
        room_id: &str,
        host_token: &str,
        request: RoomAccessUpdateRequest,
    ) -> Result<RoomAccessUpdateResponse, piik_room::Error> {
        match request {
            RoomAccessUpdateRequest::SetCodeEntryPolicy { policy } => {
                let update = lock(&self.core)
                    .store
                    .set_code_entry_policy(room_id, policy, host_token)?;
                Ok(RoomAccessUpdateResponse::CodeEntryPolicyUpdated {
                    code_entry_policy: update.code_entry_policy,
                    viewer_password_enabled: update.viewer_password_enabled,
                })
            }
            RoomAccessUpdateRequest::SetViewerPassword { password } => {
                let current = lock(&self.core)
                    .store
                    .host_managed_room(room_id, host_token)?;
                let derived = self
                    .derive_for_host(room_id, host_token, password, &current)
                    .await;
                let enabled = lock(&self.core)
                    .store
                    .set_viewer_password(room_id, host_token, derived, &current)?;
                Ok(RoomAccessUpdateResponse::ViewerPasswordUpdated { enabled })
            }
            RoomAccessUpdateRequest::RotateViewerGrant
            | RoomAccessUpdateRequest::RevokeViewerGrant => {
                let rotate = request == RoomAccessUpdateRequest::RotateViewerGrant;
                let action = if rotate {
                    ViewerGrantAction::Rotate
                } else {
                    ViewerGrantAction::Revoke
                };
                locked_with_io(&self.core, |core| {
                    let update = core.store.set_viewer_grant(room_id, action, host_token)?;
                    core.revoke_grant_viewers(room_id, &update);
                    let invite_url = rotate.then(|| {
                        invite_url(
                            &core.public_base_url,
                            room_id,
                            update.viewer_grant.as_deref().unwrap_or_default(),
                        )
                    });
                    Ok(RoomAccessUpdateResponse::ViewerGrantUpdated {
                        viewer_authorization_generation: update.viewer_authorization_generation,
                        invite_url,
                    })
                })
            }
        }
    }

    /// Derives unlocked, rechecking exact room/Host authority when the gate
    /// admits the work. An absent or empty password derives nothing.
    async fn derive_for_host(
        &self,
        room_id: &str,
        host_token: &str,
        password: Option<String>,
        current: &piik_room::RoomRef,
    ) -> DerivedMaterial {
        let Some(password) = password.filter(|password| !password.is_empty()) else {
            return Ok(None);
        };
        let deriver = lock(&self.core).store.password_deriver();
        let core = Arc::downgrade(&self.core);
        let (room_id, host_token, current) =
            (room_id.to_owned(), host_token.to_owned(), current.clone());
        run_blocking(move || {
            let may_start = || {
                core.upgrade().is_some_and(|core| {
                    lock(&core)
                        .store
                        .host_still_owns_room(&room_id, &host_token, &current)
                })
            };
            deriver
                .derive_viewer_password_material(&password, Some(&may_start))
                .map(Some)
        })
        .await
    }

    /// Runs the KDF unlocked; the commit and room closure run locked.
    pub async fn replace_room(
        &self,
        room_id: &str,
        host_token: &str,
        code_entry_policy: CodeEntryPolicy,
        room_password: Option<String>,
    ) -> Result<CreatedRoom, piik_room::Error> {
        let current = lock(&self.core)
            .store
            .host_managed_room(room_id, host_token)?;
        let derived = self
            .derive_for_host(room_id, host_token, room_password, &current)
            .await;
        locked_with_io(&self.core, |core| {
            let derivation_ok = derived.is_ok();
            let mut replacement = core.store.replace_room(
                room_id,
                host_token,
                code_entry_policy,
                clone_derived(&derived),
                &current,
            );
            if derivation_ok
                && let Err(error) = &replacement
                && let Some(candidate) = core.room_capacity_candidate(error, room_id)
            {
                replacement = core.abandon_room(&candidate).and_then(|()| {
                    core.store.replace_room(
                        room_id,
                        host_token,
                        code_entry_policy,
                        clone_derived(&derived),
                        &current,
                    )
                });
            }
            let replacement = replacement?;
            core.close_room(&replacement.closed);
            Ok(replacement.created)
        })
    }

    /// Serializes room admission and commit, deriving any password unlocked.
    pub async fn create_room(
        &self,
        code_entry_policy: CodeEntryPolicy,
        room_password: Option<String>,
        preferred_room_id: Option<String>,
    ) -> Result<CreatedRoom, piik_room::Error> {
        {
            let core = lock(&self.core);
            if let Err(error) = core.store.begin_create_room()
                && core.room_capacity_candidate(&error, "").is_none()
            {
                return Err(error);
            }
        }
        let derived: DerivedMaterial = match room_password.filter(|password| !password.is_empty()) {
            None => Ok(None),
            Some(password) => {
                let deriver = lock(&self.core).store.password_deriver();
                run_blocking(move || {
                    deriver
                        .derive_viewer_password_material(&password, None)
                        .map(Some)
                })
                .await
            }
        };
        locked_with_io(&self.core, |core| {
            let derivation_ok = derived.is_ok();
            let preferred = preferred_room_id.as_deref();
            let mut created =
                core.store
                    .create_room(code_entry_policy, clone_derived(&derived), preferred);
            if derivation_ok
                && let Err(error) = &created
                && let Some(candidate) = core.room_capacity_candidate(error, "")
            {
                created = core.abandon_room(&candidate).and_then(|()| {
                    core.store
                        .create_room(code_entry_policy, clone_derived(&derived), preferred)
                });
            }
            created
        })
    }
}
