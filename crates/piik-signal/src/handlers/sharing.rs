//! Host sharing lifecycle: pause, stop and start of the media share.

use piik_protocol::{
    DEFAULT_QUALITY_SETTINGS, ErrorCode, Role, ServerMessage, SharingStartFailure,
    SharingStartedMessage, StartSharingMessage,
};

use crate::auth::AuthRequest;
use crate::server::Core;
use crate::session::{AuthState, CLOSE_SESSION_REPLACED};

use super::route_participant;

impl Core {
    pub(super) fn set_sharing_paused(
        &mut self,
        key: u64,
        auth: &AuthState,
        generation: &str,
        paused: bool,
    ) {
        if auth.role != Role::Host {
            self.send_error(key, ErrorCode::Forbidden, "只有当前房主可以暂停分享");
            return;
        }
        let session_id = self.session_id_of(key);
        let host_matches = self
            .store
            .get_connected_host(&auth.room_id)
            .is_some_and(|host| host.session_id == session_id);
        if !host_matches
            || auth.share_generation.is_empty()
            || self.share(&auth.room_id).generation != auth.share_generation
            || generation != auth.share_generation
        {
            self.close_session(key, CLOSE_SESSION_REPLACED, "Sharing generation replaced");
            return;
        }
        self.shares
            .entry(auth.room_id.clone())
            .or_default()
            .paused_generation = if paused {
            auth.share_generation.clone()
        } else {
            String::new()
        };
        self.router_set_paused(&auth.room_id, paused);
        self.broadcast_host_status(&auth.room_id, true, paused);
    }

    pub(super) fn handle_stop_sharing(
        &mut self,
        key: u64,
        auth: &AuthState,
        generation: Option<String>,
    ) {
        if auth.role != Role::Host {
            self.send_error(key, ErrorCode::Forbidden, "只有当前房主可以停止分享");
            return;
        }
        if auth.share_generation.is_empty() {
            return;
        }
        if self.share(&auth.room_id).generation != auth.share_generation
            || generation.is_some_and(|generation| generation != auth.share_generation)
        {
            self.close_session(key, CLOSE_SESSION_REPLACED, "Sharing generation replaced");
            return;
        }
        // Stopping media keeps the persistent Host session and presence.
        self.stop_sharing(&auth.room_id);
        self.send_viewer_presence(&auth.room_id);
    }

    /// Claims a new publication generation without replacing the Host
    /// participant, so room interactions keep the same session.
    pub(super) fn start_sharing(
        &mut self,
        key: u64,
        auth: &AuthState,
        message: &StartSharingMessage,
    ) {
        let failed = |code| ServerMessage::SharingStartFailed {
            share_generation: message.share_generation.clone(),
            code,
        };
        if auth.role != Role::Host {
            self.send_error(key, ErrorCode::Forbidden, "Only the host may start sharing");
            return;
        }
        if !self.same_auth(key, auth)
            || !auth.share_generation.is_empty()
            || message.share_generation == self.share(&auth.room_id).generation
            || (self.router.sfu_only && message.route_policy.peer_only)
        {
            self.send(key, &failed(SharingStartFailure::Forbidden));
            return;
        }
        // stop-sharing leaves the previous generation as a stale fence; a fresh
        // start supersedes it without a second sharing-stopped event.
        {
            let share = self.shares.entry(auth.room_id.clone()).or_default();
            share.generation.clear();
            share.paused_generation.clear();
            share.audio_available = None;
            share.co_play_grants.clear();
        }
        let request = AuthRequest {
            role: Some(Role::Host),
            room_id: auth.room_id.clone(),
            share_generation: message.share_generation.clone(),
            sharing_paused: message.sharing_paused,
            quality_settings: message.quality_settings,
            route_policy: Some(message.route_policy),
            ..AuthRequest::default()
        };
        let session_id = self.session_id_of(key);
        let generation = self.settle_host_share(&auth.room_id, &request);
        self.set_share_generation(key, &generation);
        let mut participant = route_participant(&session_id, auth);
        participant.route_policy = Some(self.route_policy_of(&auth.room_id));
        let Some(state) = self.connect_route_participant(&participant) else {
            self.set_share_generation(key, "");
            self.stop_sharing(&auth.room_id);
            self.send(key, &failed(SharingStartFailure::ServerError));
            return;
        };
        let share = self.share(&auth.room_id);
        let paused = share.paused_generation == generation;
        self.send(
            key,
            &ServerMessage::SharingStarted(Box::new(SharingStartedMessage {
                share_generation: generation.clone(),
                route_policy: self.route_policy_of(&auth.room_id),
                route_revision: state.route_revision,
                route_assignment: state.route_assignment,
                quality_settings: share.quality_settings.unwrap_or(DEFAULT_QUALITY_SETTINGS),
                paused,
            })),
        );
        self.set_video_publication(&auth.room_id, &generation, paused);
        // Acknowledge publication authority before any media effects use it.
        self.router_set_paused(&auth.room_id, paused);
        self.router_complete_authentication(&participant);
        self.send_viewer_presence(&auth.room_id);
        self.broadcast_host_status(&auth.room_id, true, paused);
    }

    pub(super) fn set_share_generation(&mut self, key: u64, generation: &str) {
        if let Some(auth) = self.session_mut(key).and_then(|s| s.authenticated.as_mut()) {
            auth.share_generation = generation.to_owned();
        }
    }
}
