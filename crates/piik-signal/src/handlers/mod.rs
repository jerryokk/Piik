//! Authenticated message dispatch, sharing lifecycle, quality evidence and
//! peer-assisted signaling.

pub(crate) mod peer;
pub(crate) mod sharing;

use piik_protocol::{
    ClientMessage, ErrorCode, INTERACTION_INTERVAL_MS, MAX_VIEWER_QUALITY_EVIDENCE_BYTES,
    MAX_VIEWERS_PER_ROOM_LIMIT, MediaRoutePhase, MediaRouteUpstream, Role,
    SenderQualityEvidenceMessage, ServerMessage, ServerViewerQualityEvidenceMessage,
    SfuPublisherQualityEvidenceMessage, VIEWER_QUALITY_EVIDENCE_INTERVAL_MS,
    ViewerQualityEvidenceGuard, ViewerQualityEvidenceMessage, encode_server_message,
};
use piik_route::{
    QualityUpstream, RouteQualityEvidenceInput, RouteQualityEvidenceResult, UpstreamKind,
};

use crate::presence::viewer_key;
use crate::router::RouteParticipant;
use crate::router::pump::RouteReady;
use crate::server::{
    Core, MIN_SIGNALING_CHALLENGE_INTERVAL_MS, SenderQualityRateWindow,
    ViewerQualityEvidenceAttempt, ViewerQualityEvidenceGate,
};
use crate::session::AuthState;

pub(super) fn route_participant(session_id: &str, auth: &AuthState) -> RouteParticipant {
    RouteParticipant::new(&auth.room_id, auth.role, &auth.peer_id, session_id)
}

impl Core {
    /// Whether `key` still carries exactly this authentication.
    pub(crate) fn same_auth(&self, key: u64, auth: &AuthState) -> bool {
        self.auth_of(key)
            .is_some_and(|current| current.id == auth.id)
    }

    pub(super) fn session_id_of(&self, key: u64) -> String {
        self.session(key)
            .map(|s| s.session_id.clone())
            .unwrap_or_default()
    }

    pub(crate) fn handle_authenticated_message(
        &mut self,
        key: u64,
        auth: &AuthState,
        message: ClientMessage,
    ) {
        let session_id = self.session_id_of(key);
        let participant = route_participant(&session_id, auth);
        match message {
            ClientMessage::SetRelayVolunteer(volunteer) => {
                self.set_relay_volunteer(key, auth, volunteer)
            }
            ClientMessage::RoomVoiceCommand(command) => {
                self.handle_room_voice_command(key, auth, command)
            }
            ClientMessage::RoomVoiceSignal(signal) => {
                self.handle_room_voice_signal(key, auth, *signal)
            }
            ClientMessage::RoomVoiceTransport {
                connection_id,
                state,
            } => {
                self.handle_room_voice_transport(key, auth, &connection_id, state);
            }
            ClientMessage::SubscribeRemoteInput(subscribe) => {
                self.subscribe_remote_input(key, auth, subscribe)
            }
            ClientMessage::SendRoomInteraction(interaction) => {
                self.handle_room_interaction(key, auth, interaction)
            }
            ClientMessage::SignalingChallenge { sequence } => {
                if !self.same_auth(key, auth) {
                    return;
                }
                let now = self.now();
                let session = self.session_mut(key).expect("session");
                if session.challenged {
                    let elapsed = now - session.last_signaling_challenge_at_ms;
                    if elapsed < 0 {
                        session.last_signaling_challenge_at_ms = now;
                        return;
                    }
                    if elapsed < MIN_SIGNALING_CHALLENGE_INTERVAL_MS {
                        return;
                    }
                }
                session.challenged = true;
                session.last_signaling_challenge_at_ms = now;
                self.send(key, &ServerMessage::SignalingChallengeResponse { sequence });
            }
            ClientMessage::Signal {
                target_peer_id,
                payload,
            } => {
                self.route_peer_assisted_signal(
                    key,
                    auth,
                    target_peer_id.unwrap_or_default(),
                    payload,
                );
            }
            ClientMessage::RestartRequest {
                target_peer_id,
                connection_id,
                rebuild,
            } => {
                if auth.role != Role::Viewer {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only viewers may request an ICE restart",
                    );
                    return;
                }
                self.route_peer_assisted_restart(
                    key,
                    auth,
                    target_peer_id.unwrap_or_default(),
                    connection_id,
                    rebuild,
                );
            }
            ClientMessage::SetQualitySettings(settings) => {
                if auth.role != Role::Host {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only the host may set quality settings",
                    );
                    return;
                }
                if auth.share_generation.is_empty()
                    || self.share(&auth.room_id).generation != auth.share_generation
                {
                    return;
                }
                self.shares
                    .entry(auth.room_id.clone())
                    .or_default()
                    .quality_settings = Some(settings);
                for viewer in self.store.get_connected_viewers(&auth.room_id) {
                    self.send_to_session(
                        &viewer.session_id,
                        &ServerMessage::QualitySettings(settings),
                    );
                }
            }
            ClientMessage::SetViewerRoutePreference(preference) => {
                if auth.role != Role::Viewer {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only a viewer may select an incoming route",
                    );
                    return;
                }
                self.set_viewer_route_preference(&participant, preference);
            }
            ClientMessage::RelayCapacity { downstream_edges } => {
                if auth.role != Role::Viewer {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only a viewer may advertise relay capacity",
                    );
                    return;
                }
                self.set_viewer_relay_capacity(&participant, downstream_edges);
            }
            ClientMessage::RouteReady {
                revision,
                phase,
                quality_approved,
            } => self.handle_route_ready(
                &participant,
                RouteReady {
                    revision,
                    prepare: phase == MediaRoutePhase::Prepare,
                    quality_approved,
                },
            ),
            ClientMessage::RouteTransportConnected {
                revision,
                connection_id,
            } => {
                self.handle_route_transport_connected(&participant, revision, &connection_id);
            }
            ClientMessage::RouteMediaUnavailable { revision } => {
                self.handle_route_media_unavailable(&participant, revision);
            }
            ClientMessage::RouteFailed {
                revision,
                phase,
                connection_id,
            } => self.handle_route_failed(
                &participant,
                revision,
                phase == MediaRoutePhase::Prepare,
                connection_id.as_deref(),
            ),
            ClientMessage::SfuSignal(signal) => self.handle_sfu_signal(&participant, *signal),
            ClientMessage::RefreshSfu { revision } => {
                if self.router.sfu.is_none() {
                    self.send_error(key, ErrorCode::Forbidden, "SFU fallback is not enabled");
                    return;
                }
                self.refresh_sfu(&participant, revision);
            }
            ClientMessage::RequestRouteDiagnostic => {
                if auth.role != Role::Host {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only the host may request route diagnostics",
                    );
                    return;
                }
                let snapshot = self.route_diagnostic_snapshot(&auth.room_id);
                self.send(key, &ServerMessage::RouteDiagnosticSnapshot(snapshot));
            }
            ClientMessage::ViewerQualityEvidence(evidence) => {
                self.handle_viewer_quality_evidence(key, auth, &evidence)
            }
            ClientMessage::SenderQualityEvidence(evidence) => {
                self.handle_sender_quality_evidence(key, auth, &evidence)
            }
            ClientMessage::SfuPublisherQualityEvidence(evidence) => {
                self.handle_sfu_publisher_quality_evidence(key, auth, &evidence);
            }
            ClientMessage::ResetSenderQuality => {
                if self.same_auth(key, auth) {
                    self.router_reset_sender_quality(&participant);
                }
            }
            ClientMessage::SetDisplayName(name) => {
                if let Some(current) = self.session_mut(key).and_then(|s| s.authenticated.as_mut())
                {
                    current.display_name = Some(name);
                }
                self.send_viewer_presence(&auth.room_id);
            }
            ClientMessage::SetAudioPresence {
                share_generation,
                available,
            } => self.set_audio_presence(key, auth, &share_generation, available),
            ClientMessage::SetCoPlayPresence(presence) => {
                self.set_co_play_presence(key, auth, presence)
            }
            ClientMessage::SetSharingPaused {
                share_generation,
                paused,
            } => self.set_sharing_paused(key, auth, &share_generation, paused),
            ClientMessage::StartSharing(message) => self.start_sharing(key, auth, &message),
            ClientMessage::StopSharing { share_generation } => {
                self.handle_stop_sharing(key, auth, share_generation)
            }
            ClientMessage::AbandonRoom => {
                if auth.role != Role::Host {
                    self.send_error(
                        key,
                        ErrorCode::Forbidden,
                        "Only the host may abandon the room",
                    );
                    return;
                }
                if let Err(error) = self.abandon_room(&auth.room_id) {
                    tracing::error!(error = %error, "Room abandonment failed");
                    self.send_error(key, ErrorCode::ServerError, "Room could not be abandoned");
                }
            }
            ClientMessage::AuthenticateHost(_) | ClientMessage::AuthenticateViewer(_) => {}
        }
    }

    /// Encodes the forwarded envelope once; the same bytes reach the upstream
    /// recipient and, when different and opted in, the Host.
    fn handle_viewer_quality_evidence(
        &mut self,
        key: u64,
        source: &AuthState,
        message: &ViewerQualityEvidenceMessage,
    ) {
        if source.role != Role::Viewer || !self.same_auth(key, source) {
            return;
        }
        let session_id = self.session_id_of(key);
        if self
            .store
            .get_connected_viewer(&source.room_id, &source.peer_id)
            .is_none_or(|viewer| viewer.session_id != session_id)
        {
            return;
        }
        // The committed edge owns the viewer's media identity.
        let edge = self.resolve_active_viewer_media_edge(&source.room_id, &source.peer_id);
        let connection_id = edge
            .as_ref()
            .map(|e| e.connection_id.clone())
            .unwrap_or_default();
        let gate_key = viewer_key(&source.room_id, &source.peer_id);
        let now = self.now();
        if let Some(previous) = self.viewer_quality_evidence_attempts.get(&gate_key)
            && previous.connection_id == connection_id
            && now - previous.at_ms < VIEWER_QUALITY_EVIDENCE_INTERVAL_MS
        {
            return;
        }
        self.viewer_quality_evidence_attempts.insert(
            gate_key.clone(),
            ViewerQualityEvidenceAttempt {
                connection_id: connection_id.clone(),
                at_ms: now,
            },
        );
        let Some(edge) = edge.filter(|_| message.guard.connection_id == connection_id) else {
            return;
        };
        let host = self.store.get_connected_host(&source.room_id);
        let recipient = match &edge.upstream {
            MediaRouteUpstream::Peer { peer_id } => self.connected_peer(&source.room_id, peer_id),
            _ => host.clone(),
        };
        let Some(recipient) = recipient else {
            return;
        };
        if message.guard.route_revision != edge.revision
            || edge.upstream == MediaRouteUpstream::peer(&source.peer_id)
        {
            return;
        }
        let presentation_epoch = message.guard.presentation_epoch;
        let sequence = message.sequence;
        let upstream_kind = edge.upstream.kind();
        let upstream_peer_id = match &edge.upstream {
            MediaRouteUpstream::Peer { peer_id } => peer_id.clone(),
            _ => String::new(),
        };
        if let Some(previous) = self.viewer_quality_evidence_gates.get(&gate_key)
            && previous.viewer_session_id == session_id
            && previous.upstream_kind == upstream_kind
            && previous.upstream_peer_id == upstream_peer_id
            && previous.connection_id == connection_id
            && (presentation_epoch < previous.presentation_epoch
                || (presentation_epoch == previous.presentation_epoch
                    && sequence <= previous.sequence))
        {
            return;
        }
        let forwarded =
            ServerMessage::ViewerQualityEvidence(Box::new(ServerViewerQualityEvidenceMessage {
                viewer_peer_id: source.peer_id.clone(),
                upstream: edge.upstream.clone(),
                guard: ViewerQualityEvidenceGuard {
                    connection_id: connection_id.clone(),
                    route_revision: edge.revision,
                    presentation_epoch: message.guard.presentation_epoch,
                },
                sequence: message.sequence,
                window_ms: message.window_ms,
                metrics: message.metrics.clone(),
            }));
        let encoded: std::sync::Arc<[u8]> = encode_server_message(&forwarded).into();
        if encoded.len() > MAX_VIEWER_QUALITY_EVIDENCE_BYTES {
            return;
        }
        let upstream = if upstream_kind == "peer" {
            QualityUpstream {
                kind: UpstreamKind::Peer,
                peer_id: upstream_peer_id.clone(),
            }
        } else {
            QualityUpstream {
                kind: UpstreamKind::Sfu,
                peer_id: String::new(),
            }
        };
        let result = self.router_observe_quality_evidence(
            &source.room_id,
            RouteQualityEvidenceInput {
                child_peer_id: source.peer_id.clone(),
                child_session_id: session_id.clone(),
                route_revision: edge.revision,
                connection_id: connection_id.clone(),
                upstream,
                presentation_epoch,
                window_ms: message.window_ms,
                metrics: message.metrics.clone(),
                accepted_at_ms: now,
            },
        );
        if result == RouteQualityEvidenceResult::Rejected {
            return;
        }
        self.viewer_quality_evidence_gates.insert(
            gate_key,
            ViewerQualityEvidenceGate {
                viewer_session_id: session_id,
                upstream_kind,
                upstream_peer_id,
                connection_id,
                presentation_epoch,
                sequence,
            },
        );
        self.send_encoded_to_session(&recipient.session_id, encoded.clone());
        let Some(host) = host.filter(|host| host.session_id != recipient.session_id) else {
            return;
        };
        if self
            .authenticated_of(&host.session_id)
            .is_some_and(|state| {
                state.role == Role::Host
                    && state.room_id == source.room_id
                    && state.peer_id == host.peer_id
            })
        {
            self.send_encoded_to_session(&host.session_id, encoded);
        }
    }

    fn handle_sender_quality_evidence(
        &mut self,
        key: u64,
        source: &AuthState,
        message: &SenderQualityEvidenceMessage,
    ) {
        if source.peer_id == message.child_peer_id || !self.same_auth(key, source) {
            return;
        }
        let session_id = self.session_id_of(key);
        let now = self.now();
        if !self.consume_sender_quality_budget(&session_id, now) {
            return;
        }
        self.router_observe_sender_quality_evidence(
            &route_participant(&session_id, source),
            message,
        );
    }

    fn handle_sfu_publisher_quality_evidence(
        &mut self,
        key: u64,
        source: &AuthState,
        message: &SfuPublisherQualityEvidenceMessage,
    ) {
        if source.role != Role::Host || !self.same_auth(key, source) {
            return;
        }
        let session_id = self.session_id_of(key);
        let now = self.now();
        if !self.consume_sender_quality_budget(&session_id, now) {
            return;
        }
        self.router_observe_sfu_publisher_quality_evidence(
            &route_participant(&session_id, source),
            message,
        );
    }

    /// Volunteers may send to the whole room: one serial preparation and the
    /// existing double allowance.
    pub(crate) fn consume_sender_quality_budget(&mut self, session_id: &str, now_ms: i64) -> bool {
        let maximum = (MAX_VIEWERS_PER_ROOM_LIMIT + 1) * 2;
        match self.sender_quality_rate_by_session.get_mut(session_id) {
            Some(current)
                if now_ms - current.started_at_ms < VIEWER_QUALITY_EVIDENCE_INTERVAL_MS =>
            {
                if current.count >= maximum {
                    return false;
                }
                current.count += 1;
                true
            }
            _ => {
                self.sender_quality_rate_by_session.insert(
                    session_id.to_owned(),
                    SenderQualityRateWindow {
                        started_at_ms: now_ms,
                        count: 1,
                    },
                );
                true
            }
        }
    }

    /// Room interactions keep no history, routing state or retry worker.
    pub(crate) fn handle_room_interaction(
        &mut self,
        key: u64,
        actor: &AuthState,
        message: piik_protocol::SendRoomInteractionMessage,
    ) {
        use piik_protocol::{
            InteractionPayload, InteractionRejection, InteractionSender, RoomInteractionMessage,
        };
        // Reserve the signaling queue for media/authority work when a reader stalls.
        const MAX_INTERACTION_QUEUE_BYTES: usize = 16 * 1024;
        let reject = |core: &mut Self, reason| {
            core.send(
                key,
                &ServerMessage::RoomInteractionRejected {
                    request_id: message.request_id.clone(),
                    reason,
                },
            );
        };
        let Some(display_name) = actor.display_name.clone() else {
            reject(self, InteractionRejection::Busy);
            return;
        };
        let now = self.now();
        if self
            .session(key)
            .is_none_or(|session| now < session.next_interaction_at_ms)
        {
            reject(self, InteractionRejection::RateLimited);
            return;
        }
        self.session_mut(key)
            .expect("session")
            .next_interaction_at_ms = now + INTERACTION_INTERVAL_MS;
        if let InteractionPayload::Reaction {
            target_peer_id: Some(target_id),
            ..
        } = &message.payload
            && !self.interaction_target_available(&actor.room_id, target_id)
        {
            reject(self, InteractionRejection::TargetUnavailable);
            return;
        }
        if self.session(key).expect("session").queued_bytes >= MAX_INTERACTION_QUEUE_BYTES {
            reject(self, InteractionRejection::Busy);
            return;
        }
        let event = ServerMessage::RoomInteraction(Box::new(RoomInteractionMessage {
            id: crate::runtime::opaque_id(),
            request_id: message.request_id.clone(),
            occurred_at: now,
            sender: InteractionSender {
                peer_id: actor.peer_id.clone(),
                role: actor.role,
                display_name,
            },
            payload: message.payload.clone(),
        }));
        let encoded: std::sync::Arc<[u8]> = encode_server_message(&event).into();
        let mut recipients = Vec::new();
        recipients.extend(self.store.get_connected_host(&actor.room_id));
        recipients.extend(self.store.get_connected_viewers(&actor.room_id));
        for recipient in recipients {
            let Some(target) = self.session_key(&recipient.session_id) else {
                continue;
            };
            let deliver = self.session(target).is_some_and(|s| {
                !s.revoked && s.open() && s.queued_bytes < MAX_INTERACTION_QUEUE_BYTES
            }) && self.is_current_session(target);
            if deliver {
                self.send_encoded(target, encoded.clone());
            }
        }
    }

    fn interaction_target_available(&self, room_id: &str, target_id: &str) -> bool {
        let Some(target) = self.connected_peer(room_id, target_id) else {
            return false;
        };
        let Some(key) = self.session_key(&target.session_id) else {
            return false;
        };
        self.session(key).is_some_and(|s| !s.revoked && s.open()) && self.is_current_session(key)
    }
}
