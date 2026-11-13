//! Authentication and Host share settlement.

use piik_protocol::{
    AuthenticatedMessage, ClientMessage, DEFAULT_HOST_DISPLAY_NAME_PREFIX,
    DEFAULT_QUALITY_SETTINGS, DEFAULT_ROUTE_POLICY, DEFAULT_VIEWER_DISPLAY_NAME, DisplayName,
    ErrorCode, MAX_VIEWER_QUALITY_EVIDENCE_BYTES, QualitySettings, Role, RoutePolicy,
    ServerMessage, ViewerRoutePreference, decode_client_message,
};
use piik_room::{
    ConnectParticipantInput, ConnectViewerWithPasswordInput, ConnectedParticipant,
    ErrorCode as RoomErrorCode,
};

use crate::router::{HybridState, RouteParticipant};
use crate::runtime::spawn_blocking;
use crate::server::Core;
use crate::session::{
    AuthState, CLOSE_AUTHENTICATION_FAILED, CLOSE_INTERNAL_ERROR, CLOSE_SESSION_REPLACED,
};

/// The role-agnostic view of an authenticate message. Empty strings and
/// `None` are absent keys.
#[derive(Debug, Clone, Default)]
pub(crate) struct AuthRequest {
    pub role: Option<Role>,
    pub room_id: String,
    pub client_id: String,
    pub token: String,
    pub viewer_grant: String,
    pub viewer_password: String,
    pub route_preference: Option<ViewerRoutePreference>,
    pub share_generation: String,
    pub sharing_paused: bool,
    pub quality_settings: Option<QualitySettings>,
    pub route_policy: Option<RoutePolicy>,
    pub display_name: Option<DisplayName>,
}

impl AuthRequest {
    fn role(&self) -> Role {
        self.role.unwrap_or(Role::Viewer)
    }

    fn policy(&self) -> RoutePolicy {
        self.route_policy.unwrap_or(DEFAULT_ROUTE_POLICY)
    }
}

fn auth_request_of(message: &ClientMessage) -> Option<AuthRequest> {
    match message {
        ClientMessage::AuthenticateHost(m) => Some(AuthRequest {
            role: Some(Role::Host),
            room_id: m.room_id.clone(),
            client_id: m.client_id.clone(),
            token: m.token.clone(),
            share_generation: m.share_generation.clone().unwrap_or_default(),
            sharing_paused: m.sharing_paused == Some(true),
            quality_settings: m.quality_settings,
            route_policy: Some(m.route_policy),
            display_name: m.display_name.clone(),
            ..AuthRequest::default()
        }),
        ClientMessage::AuthenticateViewer(m) => Some(AuthRequest {
            role: Some(Role::Viewer),
            room_id: m.room_id.clone(),
            client_id: m.client_id.clone(),
            viewer_grant: m.viewer_grant.clone().unwrap_or_default(),
            viewer_password: m.viewer_password.clone().unwrap_or_default(),
            route_preference: m.route_preference,
            display_name: m.display_name.clone(),
            ..AuthRequest::default()
        }),
        _ => None,
    }
}

pub(crate) fn authentication_error_message(code: ErrorCode) -> &'static str {
    match code {
        ErrorCode::RoomAccessDenied => "Room access denied",
        ErrorCode::RoomNotFound => "Room not found",
        ErrorCode::RoomFull => "Room is full",
        ErrorCode::HostAlreadyConnected => "A host is already connected",
        ErrorCode::InvalidToken => "Room credentials are invalid",
        _ => "Authentication failed",
    }
}

fn wire_code(code: RoomErrorCode) -> ErrorCode {
    match code {
        RoomErrorCode::InvalidToken => ErrorCode::InvalidToken,
        RoomErrorCode::RoomAccessDenied => ErrorCode::RoomAccessDenied,
        RoomErrorCode::RoomNotFound => ErrorCode::RoomNotFound,
        RoomErrorCode::RoomFull => ErrorCode::RoomFull,
        RoomErrorCode::HostAlreadyConnected => ErrorCode::HostAlreadyConnected,
        RoomErrorCode::RoomBusy | RoomErrorCode::RoomLimit => ErrorCode::ServerError,
    }
}

fn authentication_error_code(request: &AuthRequest, error: &piik_room::Error) -> ErrorCode {
    let Some(code) = error
        .code()
        .filter(|code| *code != RoomErrorCode::RoomLimit)
    else {
        return ErrorCode::ServerError;
    };
    if code == RoomErrorCode::RoomBusy {
        return if request.role() == Role::Viewer {
            ErrorCode::RoomAccessDenied
        } else {
            ErrorCode::ServerError
        };
    }
    if request.role() != Role::Viewer {
        return wire_code(code);
    }
    if request.viewer_grant.is_empty() {
        return if code == RoomErrorCode::RoomNotFound {
            ErrorCode::RoomNotFound
        } else {
            ErrorCode::RoomAccessDenied
        };
    }
    if code == RoomErrorCode::RoomNotFound {
        return ErrorCode::InvalidToken;
    }
    wire_code(code)
}

/// The requested name or the role's default.
fn authenticated_display_name(request: &AuthRequest) -> Option<DisplayName> {
    if let Some(name) = &request.display_name {
        return Some(name.clone());
    }
    let name = if request.role() == Role::Host {
        DEFAULT_HOST_DISPLAY_NAME_PREFIX
    } else {
        DEFAULT_VIEWER_DISPLAY_NAME
    };
    DisplayName::new(name)
}

impl Core {
    /// Every frame of a session enters here with the lock held.
    pub(crate) fn handle_message(&mut self, key: u64, encoded: &[u8]) {
        let closing = self.closing;
        let Some(session) = self.session(key) else {
            return;
        };
        if closing || !session.open() || session.revoked {
            return;
        }
        let Ok(message) = decode_client_message(encoded) else {
            self.reject_invalid_message(key);
            return;
        };
        if matches!(message, ClientMessage::ViewerQualityEvidence(_))
            && encoded.len() > MAX_VIEWER_QUALITY_EVIDENCE_BYTES
        {
            self.reject_invalid_message(key);
            return;
        }
        let session = self.session(key).expect("session");
        if session.authenticated.is_none() {
            let Some(request) = auth_request_of(&message) else {
                self.send_error(
                    key,
                    ErrorCode::AuthRequired,
                    "Authenticate before sending messages",
                );
                self.close_session(key, CLOSE_AUTHENTICATION_FAILED, "Authentication required");
                return;
            };
            if session.authenticating {
                self.reject_invalid_message(key);
                return;
            }
            self.session_mut(key).expect("session").authenticating = true;
            self.authenticate(key, request);
            return;
        }
        if auth_request_of(&message).is_some() {
            self.send_error(
                key,
                ErrorCode::Forbidden,
                "Session is already authenticated",
            );
            return;
        }
        if !self.is_current_session(key) {
            self.close_session(key, CLOSE_SESSION_REPLACED, "Session replaced");
            return;
        }
        let auth = self.auth_of(key).cloned().expect("authenticated");
        self.handle_authenticated_message(key, &auth, message);
    }

    fn fail_authentication(&mut self, key: u64, code: ErrorCode, message: &str, reason: &str) {
        self.send_error(key, code, message);
        self.close_session(key, CLOSE_AUTHENTICATION_FAILED, reason);
        self.finish_authenticating(key);
    }

    /// Completes non-password checks in the caller's critical section;
    /// password checks derive on a blocking thread while the reader continues.
    fn authenticate(&mut self, key: u64, request: AuthRequest) {
        let role = request.role();
        let site_access = self
            .session(key)
            .is_some_and(|s| s.site_access_authenticated);
        // Never turn peer-only into SFU consent for a claimed publication.
        if role == Role::Host
            && !request.share_generation.is_empty()
            && self.router.sfu_only
            && request.policy().peer_only
        {
            self.fail_authentication(
                key,
                ErrorCode::Forbidden,
                "This site requires server media; turn off Privacy mode or use another site",
                "Route policy conflict",
            );
            return;
        }
        if role == Role::Host && !site_access {
            self.fail_authentication(
                key,
                ErrorCode::AuthRequired,
                "Site access is required",
                "Authentication failed",
            );
            return;
        }
        if role == Role::Viewer && !site_access {
            let mut may_enter = false;
            if !request.viewer_grant.is_empty() {
                match self
                    .store
                    .viewer_grant_may_enter(&request.room_id, &request.viewer_grant)
                {
                    Ok(allowed) => may_enter = allowed,
                    Err(error) => {
                        self.authentication_failed(key, &request, &error);
                        self.finish_authenticating(key);
                        return;
                    }
                }
            }
            if !may_enter {
                self.fail_authentication(
                    key,
                    ErrorCode::InvalidToken,
                    authentication_error_message(ErrorCode::InvalidToken),
                    "Authentication failed",
                );
                return;
            }
        }
        if role == Role::Viewer
            && request.viewer_grant.is_empty()
            && !request.viewer_password.is_empty()
        {
            self.authenticate_with_password(key, request);
            return;
        }
        let session_id = self.session(key).expect("session").session_id.clone();
        let connected = self.store.connect_participant(&ConnectParticipantInput {
            room_id: request.room_id.clone(),
            role,
            token: request.token.clone(),
            viewer_grant: request.viewer_grant.clone(),
            client_id: request.client_id.clone(),
            session_id,
        });
        match connected {
            Ok(participant) => self.complete_authentication(key, &request, participant),
            Err(error) => self.authentication_failed(key, &request, &error),
        }
        self.finish_authenticating(key);
    }

    fn finish_authenticating(&mut self, key: u64) {
        if let Some(session) = self.session_mut(key) {
            session.authenticating = false;
        }
    }

    fn may_connect(&self, key: u64) -> bool {
        !self.closing
            && self
                .session(key)
                .is_some_and(|s| !s.revoked && s.authenticated.is_none() && s.open())
    }

    /// The KDF runs with the lock released; `may_connect` is evaluated at gate
    /// admission and again inside the commit, both against live state.
    fn authenticate_with_password(&mut self, key: u64, request: AuthRequest) {
        let challenge = self.store.viewer_password_challenge(&request.room_id);
        let deriver = self.store.password_deriver();
        let locker = self.locker.clone();
        spawn_blocking(move || {
            let derived = challenge.and_then(|challenge| {
                let may_connect = || locker.with(|core| core.may_connect(key)).unwrap_or(false);
                deriver
                    .derive_viewer_password(
                        &request.viewer_password,
                        challenge.salt(),
                        Some(&may_connect),
                    )
                    .map(|derived| (derived, challenge))
            });
            locker.with(move |core| {
                let session_id = core
                    .session(key)
                    .map(|s| s.session_id.clone())
                    .unwrap_or_default();
                let connected = derived.and_then(|(derived, challenge)| {
                    let input = ConnectViewerWithPasswordInput {
                        room_id: request.room_id.clone(),
                        password: request.viewer_password.clone(),
                        client_id: request.client_id.clone(),
                        session_id,
                    };
                    let live = core.may_connect(key);
                    core.store
                        .connect_viewer_with_password(&input, &derived, &challenge, &|| live)
                });
                match connected {
                    Ok(participant) => core.complete_authentication(key, &request, participant),
                    Err(error) => core.authentication_failed(key, &request, &error),
                }
                core.finish_authenticating(key);
            });
        });
    }

    /// Silent for a socket that is gone or revoked, otherwise the mapped
    /// error and 4003.
    fn authentication_failed(&mut self, key: u64, request: &AuthRequest, error: &piik_room::Error) {
        if self.session(key).is_none_or(|s| s.revoked) {
            return;
        }
        if error.code().is_none() {
            tracing::error!("Signaling authentication failed unexpectedly");
        }
        let code = authentication_error_code(request, error);
        self.send_error(key, code, authentication_error_message(code));
        self.close_session(key, CLOSE_AUTHENTICATION_FAILED, "Authentication failed");
    }

    /// Rechecks session ownership after store admission.
    fn complete_authentication(
        &mut self,
        key: u64,
        request: &AuthRequest,
        participant: ConnectedParticipant,
    ) {
        let session_id = self
            .session(key)
            .map(|s| s.session_id.clone())
            .unwrap_or_default();
        if self.closing || self.session(key).is_none_or(|s| s.revoked || !s.open()) {
            if let Err(error) = self.store.disconnect_participant(
                &participant.room_id,
                &participant.peer_id,
                &session_id,
            ) {
                // Failing to retire the aborted admission is an invariant failure.
                panic!("room store failed while releasing an aborted authentication: {error}");
            }
            return;
        }
        self.session_mut(key)
            .expect("session")
            .clear_authentication_timer();
        self.unauthenticated_connections -= 1;
        let room_id = participant.room_id.clone();
        let role = participant.role;
        let share_generation = if role == Role::Host && request.role() == Role::Host {
            self.settle_host_share(&room_id, request)
        } else {
            String::new()
        };
        let auth_id = self.router.next_id();
        let auth = AuthState {
            id: auth_id,
            room_id: room_id.clone(),
            role,
            peer_id: participant.peer_id.clone(),
            share_generation: share_generation.clone(),
            display_name: authenticated_display_name(request),
        };
        self.session_mut(key).expect("session").authenticated = Some(auth.clone());
        if role == Role::Viewer {
            self.clear_viewer_evidence(&room_id, &participant.peer_id);
        }
        self.clear_viewer_grace(&room_id, &participant.peer_id);
        let media_active = if role == Role::Viewer {
            !self.active_host_share_generation(&room_id).is_empty()
        } else {
            !share_generation.is_empty()
        };
        let mut route_participant =
            RouteParticipant::new(&room_id, role, &participant.peer_id, &session_id);
        route_participant.route_preference = (role == Role::Viewer).then(|| {
            request
                .route_preference
                .unwrap_or(ViewerRoutePreference::Auto)
        });
        if role == Role::Host && media_active {
            route_participant.route_policy = Some(self.route_policy_of(&room_id));
        }
        let mut hybrid = HybridState::empty();
        if role == Role::Viewer || media_active {
            match self.connect_route_participant(&route_participant) {
                Some(state) => hybrid = state,
                None => {
                    tracing::error!("Media router did not assign an authenticated participant");
                    self.send_error(key, ErrorCode::ServerError, "Media assignment failed");
                    self.close_session(key, CLOSE_INTERNAL_ERROR, "Media assignment failed");
                    return;
                }
            }
        }
        // The viewer's media identity is derived from the committed edge.
        let connection_id = (role == Role::Viewer)
            .then(|| self.resolve_active_viewer_media_edge(&room_id, &participant.peer_id))
            .flatten()
            .map(|edge| edge.connection_id);
        // The host-status fan-out below uses this pre-router snapshot.
        let connected_viewers = if role == Role::Host {
            self.store.get_connected_viewers(&room_id)
        } else {
            Vec::new()
        };
        let share = self.share(&room_id);
        let route_policy = self.route_policy_of(&room_id);
        let quality_settings = share.quality_settings.unwrap_or(DEFAULT_QUALITY_SETTINGS);
        let room_share_generation = match role {
            Role::Host if media_active => Some(share_generation.clone()),
            Role::Viewer if media_active => Some(self.active_host_share_generation(&room_id)),
            _ => None,
        };
        let host_online = if role == Role::Host {
            participant.host_online
        } else {
            participant.host_online && media_active
        };
        let message = AuthenticatedMessage {
            peer_id: participant.peer_id.clone(),
            max_viewers: self.store.max_viewers_per_room() as i64,
            endpoint_media_copy_capacity: self.endpoint_media_copy_capacity,
            host_online,
            host_paused: (role == Role::Viewer)
                .then_some(host_online && share.paused_generation == share.generation),
            connection_id,
            ice_config: self.ice.clone(),
            route_policy,
            code_entry_policy: participant.code_entry_policy,
            viewer_authorization_generation: participant.viewer_authorization_generation.clone(),
            share_generation: room_share_generation,
            route_revision: hybrid.route_revision,
            route_assignment: hybrid.route_assignment.clone(),
            quality_settings,
            role,
            viewer_password_enabled: (role == Role::Host)
                .then_some(participant.viewer_password_enabled),
        };
        self.send(key, &ServerMessage::Authenticated(Box::new(message)));
        // Room data starts with the authenticated session.
        self.send(
            key,
            &ServerMessage::RoomInteractionsReady {
                server_time: self.now(),
            },
        );
        self.join_voice_roster(&room_id, &participant.peer_id, &session_id);
        self.sync_voice_demand(&room_id);
        self.sync_room_voice(&room_id);
        if role == Role::Viewer {
            self.send_viewer_route_preference(&room_id, &participant.peer_id, true);
            self.send_audio_presence(key, &room_id);
            self.send_co_play_presence(key, &room_id);
        }
        if let Some(replaced) = participant.replaced_session_id.as_deref() {
            if let Some(replaced_key) = self.session_key(replaced)
                && replaced_key != key
            {
                self.close_session(replaced_key, CLOSE_SESSION_REPLACED, "Session replaced");
            }
            if role == Role::Viewer {
                self.refresh_remote_input_members(&room_id);
            }
        }
        let paused = self.share(&room_id).paused_generation == share_generation;
        if role == Role::Host && media_active {
            if self.room_media(&room_id)
                && self
                    .controller(&room_id)
                    .is_some_and(|c| c.snapshot().video_generation != share_generation)
            {
                self.set_video_publication(&room_id, &share_generation, paused);
            }
            self.router_set_paused(&room_id, paused);
        }
        if role == Role::Viewer || media_active {
            self.router_complete_authentication(&route_participant);
        }
        if role == Role::Host
            && media_active
            && self.share(&room_id).paused_generation == share_generation
        {
            self.send_to_session(
                &session_id,
                &ServerMessage::PauseSharingSource {
                    share_generation: share_generation.clone(),
                },
            );
        }
        self.send_viewer_presence(&room_id);
        if role == Role::Host && media_active {
            let paused = self.share(&room_id).paused_generation == share_generation;
            for viewer in connected_viewers {
                self.send_to_session(
                    &viewer.session_id,
                    &ServerMessage::HostStatus {
                        online: true,
                        paused,
                    },
                );
            }
        }
    }

    /// Settles which share generation this Host connection owns, retires a
    /// superseded one and broadcasts the quality settings and route policy the
    /// new generation starts with. Returns the settled generation.
    pub(crate) fn settle_host_share(&mut self, room_id: &str, request: &AuthRequest) -> String {
        // A Host connection without a share generation owns the room only.
        if request.share_generation.is_empty() {
            if !self.share(room_id).generation.is_empty() {
                self.stop_sharing(room_id);
            }
            return String::new();
        }
        let current = self.share(room_id);
        let current_generation = current.generation.clone();
        let share_generation = request.share_generation.clone();
        // A reconnect may carry a generation that a stop overtook; the
        // retained generation fences that publication.
        if !current_generation.is_empty()
            && share_generation == current_generation
            && current.route_policy.is_none()
        {
            return String::new();
        }
        let same_share_paused =
            current_generation == share_generation && current.paused_generation == share_generation;
        if !current_generation.is_empty() && current_generation != share_generation {
            self.stop_sharing(room_id);
        }
        // `stop_sharing` may have cleared fields, so read the surviving intent.
        let mut share = self.share(room_id);
        if !request.share_generation.is_empty()
            && (current_generation != share_generation || share.quality_settings.is_none())
        {
            let initial = request.quality_settings.unwrap_or(DEFAULT_QUALITY_SETTINGS);
            share.quality_settings = Some(initial);
            for viewer in self.store.get_connected_viewers(room_id) {
                self.send_to_session(&viewer.session_id, &ServerMessage::QualitySettings(initial));
            }
        }
        if current_generation != share_generation || share.route_policy.is_none() {
            let route_policy = self.configured_route_policy(request.policy());
            share.route_policy = Some(route_policy);
            for viewer in self.store.get_connected_viewers(room_id) {
                self.send_to_session(
                    &viewer.session_id,
                    &ServerMessage::RoutePolicy {
                        share_generation: share_generation.clone(),
                        route_policy,
                    },
                );
            }
        }
        share.generation = share_generation.clone();
        share.paused_generation = if request.sharing_paused || same_share_paused {
            share_generation.clone()
        } else {
            String::new()
        };
        if current_generation != share_generation {
            share.co_play_grants.clear();
            self.shares.insert(room_id.to_owned(), share);
            self.broadcast_audio_presence(room_id);
            self.broadcast_co_play_presence(room_id);
        } else {
            self.shares.insert(room_id.to_owned(), share);
        }
        share_generation
    }

    /// Leaves the participant unassigned if a controller assertion prevents
    /// route admission.
    pub(crate) fn connect_route_participant(
        &mut self,
        participant: &RouteParticipant,
    ) -> Option<HybridState> {
        let mut state = None;
        let failed = self.recover_pump(|core| {
            if participant.role == Role::Host
                && let Some(policy) = participant.route_policy
                && core.room_media(&participant.room_id)
                && core.has_controller(&participant.room_id)
            {
                let now = core.now();
                let released = core
                    .ctl(&participant.room_id, |c| c.set_route_policy(policy, now))
                    .unwrap_or_default();
                core.release_resources(released);
                let room_id = participant.room_id.clone();
                let peers: Vec<String> = core
                    .room_rt(&room_id)
                    .map(|rm| rm.viewer_settings.keys())
                    .unwrap_or_default();
                for peer_id in peers {
                    let mut settings = core.viewer_settings(&room_id, &peer_id);
                    if !core
                        .controller(&room_id)
                        .is_some_and(|c| c.allows_viewer_route_preference(settings.preference))
                    {
                        settings.preference = ViewerRoutePreference::Auto;
                        core.set_viewer_settings(&room_id, &peer_id, settings);
                        core.send_viewer_route_preference(&room_id, &peer_id, true);
                    }
                }
            }
            state = Some(core.connect_participant(participant));
        });
        if failed { None } else { state }
    }

    pub(crate) fn route_policy_of(&self, room_id: &str) -> RoutePolicy {
        self.shares
            .get(room_id)
            .and_then(|share| share.route_policy)
            .unwrap_or_else(|| self.configured_route_policy(DEFAULT_ROUTE_POLICY))
    }

    /// The media truth for a room: the share record keeps its generation as a
    /// stale reconnect fence after stop, so membership alone is not publishing.
    pub(crate) fn active_host_share_generation(&self, room_id: &str) -> String {
        let Some(host) = self.store.get_connected_host(room_id) else {
            return String::new();
        };
        let Some(auth) = self.authenticated_of(&host.session_id) else {
            return String::new();
        };
        if auth.role != Role::Host || auth.room_id != room_id {
            return String::new();
        }
        if auth.share_generation.is_empty()
            || self.share(room_id).generation != auth.share_generation
        {
            return String::new();
        }
        auth.share_generation.clone()
    }

    /// Restricts share preferences to the services this runtime owns.
    pub(crate) fn configured_route_policy(&self, mut policy: RoutePolicy) -> RoutePolicy {
        policy.peer_only = self.router.sfu.is_none() || policy.peer_only;
        policy.nat_prediction =
            self.nat_prediction_enabled && !self.router.sfu_only && policy.nat_prediction;
        policy.topology_optimization = !self.router.sfu_only && policy.topology_optimization;
        policy
    }

    /// Checked on every authenticated message before dispatch.
    pub(crate) fn is_current_session(&self, key: u64) -> bool {
        let Some(session) = self.session(key) else {
            return false;
        };
        let Some(auth) = session.authenticated.as_ref() else {
            return false;
        };
        let current = if auth.role == Role::Host {
            self.store.get_connected_host(&auth.room_id)
        } else {
            self.store
                .get_connected_viewer(&auth.room_id, &auth.peer_id)
        };
        current.is_some_and(|current| current.session_id == session.session_id)
    }
}
