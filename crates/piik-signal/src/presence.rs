//! Viewer presence, Host status, disconnect handling, room termination,
//! retention and the small room projections (audio, co-play, remote input,
//! relay volunteer). Everything runs with the authority lock held, so each
//! effect observes the current room membership.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use piik_protocol::{
    CoPlayParticipant, ErrorCode, MediaRouteUpstream, ParticipantPresenceEntry, RelayVolunteer,
    RemoteInputPeer, Role, ServerMessage, SetCoPlayPresenceMessage, SubscribeRemoteInputMessage,
    valid_input_key,
};
use piik_room::{ClosedRoom, ViewerGrantUpdate};

use crate::server::{Core, GraceTimer, room_limit};
use crate::session::{AuthState, CLOSE_NORMAL, CLOSE_VIEWER_ACCESS_REVOKED, encode};

/// Scopes grace timers and quality evidence to one room participant.
pub(crate) fn viewer_key(room_id: &str, peer_id: &str) -> String {
    format!("{room_id}:{peer_id}")
}

impl Core {
    /// The Host entry first when it has a display name, then Viewers in store
    /// order; every authenticated participant receives the projection.
    pub(crate) fn send_viewer_presence(&mut self, room_id: &str) {
        if self.deferred_viewer_presence_rooms.contains(room_id) {
            return;
        }
        let connected_viewers = self.store.get_connected_viewers(room_id);
        let host = self.store.get_connected_host(room_id);
        let host_key = host.as_ref().and_then(|h| self.session_key(&h.session_id));
        let host_state = host_key.and_then(|key| self.auth_of(key)).filter(|state| {
            host.as_ref().is_some_and(|host| {
                state.role == Role::Host
                    && state.room_id == room_id
                    && state.peer_id == host.peer_id
            })
        });
        let mut viewers = Vec::new();
        if let (Some(host), Some(name)) = (&host, host_state.and_then(|s| s.display_name.clone())) {
            viewers.push(ParticipantPresenceEntry::host(host.peer_id.clone(), name));
        }
        let viewer_state = |core: &Self, peer_id: &str, session_id: &str| -> Option<AuthState> {
            core.authenticated_of(session_id)
                .filter(|state| {
                    state.role == Role::Viewer
                        && state.room_id == room_id
                        && state.peer_id == peer_id
                })
                .cloned()
        };
        for viewer in &connected_viewers {
            let Some(name) = viewer_state(self, &viewer.peer_id, &viewer.session_id)
                .and_then(|s| s.display_name)
            else {
                continue;
            };
            let upstream = self.get_viewer_route_upstream(room_id, &viewer.peer_id);
            let media_ready = self
                .resolve_active_viewer_media_edge(room_id, &viewer.peer_id)
                .is_some_and(|edge| {
                    upstream != MediaRouteUpstream::None && edge.upstream == upstream
                });
            viewers.push(ParticipantPresenceEntry::viewer(
                viewer.peer_id.clone(),
                name,
                upstream,
                media_ready,
            ));
        }
        let encoded = encode(&ServerMessage::ViewerPresence { viewers });
        let mut recipients = Vec::new();
        let mut seen = HashSet::new();
        if let Some(key) = host_key
            && seen.insert(key)
        {
            recipients.push(key);
        }
        for viewer in &connected_viewers {
            if let Some(key) = self.session_key(&viewer.session_id)
                && seen.insert(key)
            {
                recipients.push(key);
            }
        }
        for key in recipients {
            self.send_encoded(key, encoded.clone());
        }
    }

    pub(crate) fn broadcast_host_status(&mut self, room_id: &str, online: bool, paused: bool) {
        for viewer in self.store.get_connected_viewers(room_id) {
            self.send_to_session(
                &viewer.session_id,
                &ServerMessage::HostStatus { online, paused },
            );
        }
    }

    /// Runs once, from the reader's exit.
    pub(crate) fn handle_disconnect(&mut self, key: u64) {
        let Some(session) = self.sessions.get_mut(&key) else {
            return;
        };
        session.clear_authentication_timer();
        let (session_id, revoked, auth) = (
            session.session_id.clone(),
            session.revoked,
            session.authenticated.clone(),
        );
        self.sender_quality_rate_by_session.remove(&session_id);
        self.sessions.delete(&key);
        if self.sessions_by_id.get(&session_id) == Some(&key) {
            self.sessions_by_id.remove(&session_id);
        }
        if revoked {
            return;
        }
        let Some(auth) = auth else {
            self.unauthenticated_connections -= 1;
            return;
        };
        if self.closing {
            return;
        }
        let disconnected =
            match self
                .store
                .disconnect_participant(&auth.room_id, &auth.peer_id, &session_id)
            {
                Ok(disconnected) => disconnected,
                Err(error) => panic!("room store failed during disconnect: {error}"),
            };
        let Some(disconnected) = disconnected else {
            return;
        };
        let room_id = disconnected.room_id.clone();
        let peer_id = disconnected.peer_id.clone();
        self.router_disconnect_participant(&room_id, &peer_id, &session_id);
        self.disconnect_room_voice(&room_id, &peer_id, &session_id);
        self.refresh_remote_input_members(&room_id);
        if disconnected.role == Role::Host {
            self.broadcast_host_status(&room_id, false, false);
            self.send_viewer_presence(&room_id);
            return;
        }
        self.send_viewer_presence(&room_id);

        // The timer id rejects stale callbacks; the store then guards against
        // removing a Viewer that has already reconnected.
        let grace_key = viewer_key(&room_id, &peer_id);
        let id = self.router.next_id();
        let locker = self.locker.clone();
        let callback_key = grace_key.clone();
        let stop = (self.after_func)(
            Duration::from_millis(self.viewer_disconnect_grace_ms),
            Box::new(move || {
                locker
                    .with(|core| core.viewer_grace_expired(&callback_key, id, &room_id, &peer_id));
            }),
        );
        self.viewer_grace_timers.insert(
            grace_key,
            GraceTimer {
                id,
                stop: Some(stop),
            },
        );
    }

    fn viewer_grace_expired(&mut self, key: &str, id: u64, room_id: &str, peer_id: &str) {
        if self
            .viewer_grace_timers
            .get(key)
            .is_none_or(|timer| timer.id != id)
        {
            return;
        }
        self.viewer_grace_timers.remove(key);
        if self.store.remove_disconnected_viewer(room_id, peer_id) {
            if let Some(voice) = self.voices.get_mut(room_id) {
                voice.members.remove(peer_id);
                self.sync_room_voice(room_id);
            }
            self.clear_viewer_evidence(room_id, peer_id);
            self.remove_viewer(room_id, peer_id);
        }
    }

    /// Sends sharing-stopped then host-status to each Viewer; the share
    /// generation deliberately survives.
    pub(crate) fn stop_sharing(&mut self, room_id: &str) {
        if let Some(current) = self.shares.get(room_id).cloned() {
            if let Some(host) = self.store.get_connected_host(room_id)
                && let Some(key) = self.session_key(&host.session_id)
                && let Some(auth) = self.session_mut(key).and_then(|s| s.authenticated.as_mut())
                && auth.share_generation == current.generation
            {
                auth.share_generation.clear();
            }
            self.shares.insert(
                room_id.to_owned(),
                crate::server::RoomShare {
                    generation: current.generation,
                    ..Default::default()
                },
            );
        }
        self.broadcast_audio_presence(room_id);
        self.broadcast_co_play_presence(room_id);
        self.clear_room_viewer_evidence(room_id);
        self.stop_room(room_id);
        for viewer in self.store.get_connected_viewers(room_id) {
            self.send_to_session(&viewer.session_id, &ServerMessage::SharingStopped);
            self.send_to_session(
                &viewer.session_id,
                &ServerMessage::HostStatus {
                    online: false,
                    paused: false,
                },
            );
        }
    }

    /// Leaves store-error mapping to the caller.
    pub(crate) fn abandon_room(&mut self, room_id: &str) -> Result<(), piik_room::Error> {
        if let Some(closed) = self.store.abandon_room(room_id)? {
            self.close_room(&closed);
        }
        Ok(())
    }

    /// Closes the Host session first, then Viewers in store order; each is
    /// told room-closed then closed with 1000.
    pub(crate) fn close_room(&mut self, closed: &ClosedRoom) {
        self.close_room_voice(&closed.room_id);
        self.clear_room_grace_timers(&closed.room_id);
        self.clear_room_viewer_evidence(&closed.room_id);
        self.delete_room(&closed.room_id);
        self.shares.remove(&closed.room_id);
        for session_id in &closed.session_ids {
            let Some(key) = self.session_key(session_id) else {
                continue;
            };
            self.send(key, &ServerMessage::RoomClosed);
            self.close_session(key, CLOSE_NORMAL, "Room abandoned");
        }
    }

    /// Three passes over the revoked Viewers, with presence deferred to the end.
    pub(crate) fn revoke_grant_viewers(&mut self, room_id: &str, update: &ViewerGrantUpdate) {
        if update.revoked_viewers.is_empty() {
            return;
        }
        // The store already removed their admission; retire input identity too.
        self.refresh_remote_input_members(room_id);
        self.deferred_viewer_presence_rooms
            .insert(room_id.to_owned());
        for viewer in &update.revoked_viewers {
            self.clear_viewer_state(room_id, &viewer.peer_id);
            if let Some(session_id) = &viewer.session_id {
                self.send_to_session(
                    session_id,
                    &ServerMessage::ViewerGrantRevoked {
                        viewer_authorization_generation: update
                            .previous_viewer_authorization_generation
                            .clone(),
                    },
                );
            }
        }
        for viewer in &update.revoked_viewers {
            self.remove_viewer(room_id, &viewer.peer_id);
        }
        for viewer in &update.revoked_viewers {
            if let Some(session_id) = &viewer.session_id {
                self.close_revoked_viewer_session(session_id);
            }
            if let Some(voice) = self.voices.get_mut(room_id) {
                voice.members.remove(&viewer.peer_id);
            }
        }
        self.sync_voice_demand(room_id);
        self.sync_room_voice(room_id);
        self.deferred_viewer_presence_rooms.remove(room_id);
        self.send_viewer_presence(room_id);
    }

    /// Deauthorizes before the close so queued callbacks of the old generation
    /// cannot route signaling after the authorization commit.
    fn close_revoked_viewer_session(&mut self, session_id: &str) {
        let Some(key) = self.session_key(session_id) else {
            return;
        };
        let session = self.session_mut(key).expect("session");
        session.clear_authentication_timer();
        session.revoked = true;
        self.sessions_by_id.remove(session_id);
        self.close_session(key, CLOSE_VIEWER_ACCESS_REVOKED, "Viewer access revoked");
    }

    pub(crate) fn clear_viewer_grace(&mut self, room_id: &str, peer_id: &str) {
        if let Some(timer) = self
            .viewer_grace_timers
            .remove(&viewer_key(room_id, peer_id))
            && let Some(stop) = timer.stop
        {
            stop();
        }
    }

    fn clear_viewer_state(&mut self, room_id: &str, peer_id: &str) {
        self.clear_viewer_grace(room_id, peer_id);
        self.clear_viewer_evidence(room_id, peer_id);
    }

    fn clear_room_grace_timers(&mut self, room_id: &str) {
        let prefix = format!("{room_id}:");
        let keys: Vec<String> = self
            .viewer_grace_timers
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        for key in keys {
            if let Some(timer) = self.viewer_grace_timers.remove(&key)
                && let Some(stop) = timer.stop
            {
                stop();
            }
        }
    }

    pub(crate) fn clear_room_viewer_evidence(&mut self, room_id: &str) {
        let prefix = format!("{room_id}:");
        self.viewer_quality_evidence_gates
            .retain(|key, _| !key.starts_with(&prefix));
        self.viewer_quality_evidence_attempts
            .retain(|key, _| !key.starts_with(&prefix));
    }

    pub(crate) fn clear_viewer_evidence(&mut self, room_id: &str, peer_id: &str) {
        let key = viewer_key(room_id, peer_id);
        self.viewer_quality_evidence_gates.remove(&key);
        self.viewer_quality_evidence_attempts.remove(&key);
    }

    // -------------------------------------------------------------------
    // retention
    // -------------------------------------------------------------------

    /// Capacity reclamation uses the normal room retirement owner, so grace
    /// timers, route state and share generations cannot outlive a code.
    pub(crate) fn room_capacity_candidate(
        &self,
        error: &piik_room::Error,
        except_room_id: &str,
    ) -> Option<String> {
        if self.room_empty_timeout_ms <= 0 || !room_limit(error) {
            return None;
        }
        let now = self.now();
        // A recently vacated room is not displaced while another empty room
        // has already expired; early reclamation reuses the reconnect window.
        let grace = i64::try_from(self.viewer_disconnect_grace_ms).unwrap_or(i64::MAX);
        [now - self.room_empty_timeout_ms, now - grace]
            .into_iter()
            .find_map(|before| {
                self.store
                    .empty_room_ids(before)
                    .into_iter()
                    .find(|id| id != except_room_id)
            })
    }

    pub(crate) fn expire_empty_rooms(&mut self) {
        if self.room_empty_timeout_ms <= 0 {
            return;
        }
        match self
            .store
            .expire_empty_rooms(self.now() - self.room_empty_timeout_ms)
        {
            Ok(closed) => {
                for room in closed {
                    self.close_room(&room);
                }
            }
            Err(error) => tracing::error!(error = %error, "Could not expire empty rooms"),
        }
    }

    /// The heartbeat interval: each tick re-arms the next; a tick that lost
    /// the stop race sees `closing`.
    pub(crate) fn arm_heartbeat(&mut self) {
        let interval = Duration::from_millis(self.heartbeat_interval_ms);
        let locker = self.locker.clone();
        self.heartbeat_stop = Some((self.after_func)(
            interval,
            Box::new(move || {
                locker.with(|core| {
                    if core.closing {
                        return;
                    }
                    core.heartbeat();
                    core.expire_empty_rooms();
                    core.arm_heartbeat();
                });
            }),
        ));
    }

    // -------------------------------------------------------------------
    // audio presence
    // -------------------------------------------------------------------

    pub(crate) fn set_audio_presence(
        &mut self,
        key: u64,
        actor: &AuthState,
        generation: &str,
        available: bool,
    ) {
        if !self.owns_current_share(key, actor, generation) {
            return;
        }
        self.shares
            .entry(actor.room_id.clone())
            .or_default()
            .audio_available = Some(available);
        self.broadcast_audio_presence(&actor.room_id);
    }

    fn owns_current_share(&self, key: u64, actor: &AuthState, generation: &str) -> bool {
        let session_id = self
            .session(key)
            .map(|s| s.session_id.as_str())
            .unwrap_or_default();
        actor.role == Role::Host
            && self
                .store
                .get_connected_host(&actor.room_id)
                .is_some_and(|host| host.session_id == session_id)
            && !actor.share_generation.is_empty()
            && actor.share_generation == generation
            && self.share(&actor.room_id).generation == generation
    }

    pub(crate) fn send_audio_presence(&mut self, key: u64, room_id: &str) {
        let available = self.share(room_id).audio_available;
        self.send(key, &ServerMessage::AudioPresence { available });
    }

    pub(crate) fn broadcast_audio_presence(&mut self, room_id: &str) {
        for viewer in self.store.get_connected_viewers(room_id) {
            if let Some(key) = self.session_key(&viewer.session_id) {
                self.send_audio_presence(key, room_id);
            }
        }
    }

    // -------------------------------------------------------------------
    // co-play presence: a share-scoped projection; only the Host App grants input
    // -------------------------------------------------------------------

    pub(crate) fn set_co_play_presence(
        &mut self,
        key: u64,
        actor: &AuthState,
        message: SetCoPlayPresenceMessage,
    ) {
        if !self.owns_current_share(key, actor, &message.share_generation) {
            return;
        }
        let endpoints = self.co_play_endpoints(&actor.room_id);
        let grants = message
            .grants
            .into_iter()
            .filter(|grant| endpoints.contains_key(&grant.endpoint_id))
            .collect();
        self.shares
            .entry(actor.room_id.clone())
            .or_default()
            .co_play_grants = grants;
        self.broadcast_co_play_presence(&actor.room_id);
    }

    fn co_play_endpoints(&self, room_id: &str) -> HashMap<String, String> {
        let mut endpoints = HashMap::new();
        for viewer in self.store.get_connected_viewers(room_id) {
            if let Some(session) = self.session_by_id(&viewer.session_id)
                && session.remote_input
                && session.open()
                && !session.revoked
                && !session.input_endpoint.is_empty()
            {
                endpoints.insert(session.input_endpoint.clone(), viewer.peer_id.clone());
            }
        }
        endpoints
    }

    fn prune_co_play_presence(&mut self, room_id: &str) {
        if self
            .shares
            .get(room_id)
            .is_none_or(|share| share.co_play_grants.is_empty())
        {
            return;
        }
        let endpoints = self.co_play_endpoints(room_id);
        if let Some(share) = self.shares.get_mut(room_id) {
            share
                .co_play_grants
                .retain(|grant| endpoints.contains_key(&grant.endpoint_id));
        }
    }

    fn co_play_participants(&self, room_id: &str) -> Vec<CoPlayParticipant> {
        if self.store.get_connected_host(room_id).is_none() {
            return Vec::new();
        }
        let endpoints = self.co_play_endpoints(room_id);
        self.share(room_id)
            .co_play_grants
            .iter()
            .filter_map(|grant| endpoints.get(&grant.endpoint_id))
            .map(|peer_id| CoPlayParticipant {
                peer_id: peer_id.clone(),
            })
            .collect()
    }

    pub(crate) fn send_co_play_presence(&mut self, key: u64, room_id: &str) {
        let participants = self.co_play_participants(room_id);
        self.send(key, &ServerMessage::CoPlayPresence { participants });
    }

    pub(crate) fn broadcast_co_play_presence(&mut self, room_id: &str) {
        let participants = self.co_play_participants(room_id);
        for viewer in self.store.get_connected_viewers(room_id) {
            if let Some(key) = self.session_key(&viewer.session_id) {
                self.send(
                    key,
                    &ServerMessage::CoPlayPresence {
                        participants: participants.clone(),
                    },
                );
            }
        }
    }

    // -------------------------------------------------------------------
    // remote input
    // -------------------------------------------------------------------

    pub(crate) fn subscribe_remote_input(
        &mut self,
        key: u64,
        actor: &AuthState,
        message: SubscribeRemoteInputMessage,
    ) {
        if self.session(key).is_none_or(|s| s.remote_input) {
            return;
        }
        if actor.role == Role::Viewer {
            let Some(input_key) = message
                .key
                .filter(|k| valid_input_key(&k.endpoint_id, &k.secret))
            else {
                return;
            };
            // One endpoint belongs to one authenticated Viewer, never an
            // identity supplied by a relay.
            for viewer in self.store.get_connected_viewers(&actor.room_id) {
                if let Some(other) = self.session_key(&viewer.session_id)
                    && other != key
                    && self
                        .session(other)
                        .is_some_and(|s| s.input_endpoint == input_key.endpoint_id)
                {
                    return;
                }
            }
            let session = self.session_mut(key).expect("session");
            session.input_endpoint = input_key.endpoint_id;
            session.input_secret = input_key.secret;
        } else if message.key.is_some() {
            return;
        }
        self.session_mut(key).expect("session").remote_input = true;
        self.refresh_remote_input_members(&actor.room_id);
    }

    pub(crate) fn refresh_remote_input_members(&mut self, room_id: &str) {
        self.prune_co_play_presence(room_id);
        self.send_remote_input_peers(room_id);
        self.broadcast_co_play_presence(room_id);
    }

    /// Only an opted-in Host receives the keys.
    fn send_remote_input_peers(&mut self, room_id: &str) {
        let Some(host) = self.store.get_connected_host(room_id) else {
            return;
        };
        let Some(target) = self.session_key(&host.session_id) else {
            return;
        };
        if self
            .session(target)
            .is_none_or(|s| !s.remote_input || !s.open() || s.revoked)
        {
            return;
        }
        let mut peers = Vec::new();
        for viewer in self.store.get_connected_viewers(room_id) {
            if let Some(session) = self.session_by_id(&viewer.session_id)
                && session.remote_input
                && session.open()
                && !session.revoked
                && !session.input_endpoint.is_empty()
            {
                peers.push(RemoteInputPeer {
                    peer_id: viewer.peer_id.clone(),
                    endpoint_id: session.input_endpoint.clone(),
                    secret: session.input_secret.clone(),
                });
            }
        }
        self.send(target, &ServerMessage::RemoteInputPeers { peers });
    }

    // -------------------------------------------------------------------
    // relay volunteer
    // -------------------------------------------------------------------

    pub(crate) fn set_relay_volunteer(
        &mut self,
        key: u64,
        auth: &AuthState,
        message: RelayVolunteer,
    ) {
        if auth.role != Role::Viewer {
            self.send_error(
                key,
                ErrorCode::Forbidden,
                "Only viewers may volunteer relay capacity",
            );
            return;
        }
        let session_id = self
            .session(key)
            .map(|s| s.session_id.clone())
            .unwrap_or_default();
        self.ensure_room(&auth.room_id);
        let mut settings = self.viewer_settings(&auth.room_id, &auth.peer_id);
        settings.capacity = message.capacity;
        settings.volunteer = message.enabled;
        settings.session_id = session_id.clone();
        self.set_viewer_settings(&auth.room_id, &auth.peer_id, settings.clone());
        let capacity = if settings.volunteer {
            settings.capacity
        } else {
            settings.capacity.min(self.endpoint_media_copy_capacity)
        };
        let now = self.now_some();
        if self
            .ctl(&auth.room_id, |c| {
                c.set_relay_volunteer(&auth.peer_id, &session_id, settings.volunteer, now);
                c.set_effective_capacity(&auth.peer_id, &session_id, settings.capacity, now);
            })
            .is_some()
        {
            self.request_pump(&auth.room_id);
        }
        self.send(
            key,
            &ServerMessage::RelayVolunteer(RelayVolunteer {
                enabled: settings.volunteer,
                capacity,
            }),
        );
    }
}
