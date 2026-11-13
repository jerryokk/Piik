//! `serverMessageSchema`: every message the Server sends to pages.

use std::collections::HashSet;

use serde_json::Value;

use crate::co_play_presence::{CoPlayParticipant, decode_co_play_participants};

mod decode;

use crate::codec::{
    DecodeError, Fields, Int, Obj, Result, ToWire, discriminator, ensure, in_range, invalid,
    object, parse,
};
use crate::constants::{MAX_PARTICIPANTS_PER_ROOM_LIMIT, MAX_SAFE_INTEGER, SIGNALING_PROTOCOL};
use crate::interactions::{
    InteractionRejection, RoomInteractionMessage, decode_interaction_rejected,
    decode_interactions_ready,
};
use crate::relay_volunteer::RelayVolunteer;
use crate::remote_input::{RemoteInputPeer, decode_remote_input_peers};
use crate::scalars::{utf16_length, valid_opaque_id};
use crate::sfu::SfuSignalMessage;
use crate::types::{
    CodeEntryPolicy, ErrorCode, IceConfig, MediaRouteUpstream, ParticipantPresenceEntry,
    ParticipantRouteAssignment, PreparedRouteCandidate, QualitySettings, Role,
    RouteDiagnosticSnapshot, RoutePolicy, SignalPayload, ViewerQualityEvidenceGuard,
    ViewerQualityEvidenceMetrics, valid_revision,
};
use crate::viewer_route::ViewerRoutePreference;
use crate::voice::{RoomVoiceErrorCode, RoomVoiceSignal, RoomVoiceState};
pub(crate) use decode::{decode_authenticated, decode_route_update};

/// `mediaMode`, which has a single accepted value.
pub const MEDIA_MODE: &str = "peer-assisted";

/// The room and transport facts shared by both `authenticated` members.
/// `protocol` is always [`SIGNALING_PROTOCOL`] and `mediaMode` is
/// [`MEDIA_MODE`].
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticatedMessage {
    pub peer_id: String,
    pub max_viewers: Int,
    pub endpoint_media_copy_capacity: Int,
    pub host_online: bool,
    pub host_paused: Option<bool>,
    pub connection_id: Option<String>,
    pub ice_config: IceConfig,
    /// [`DEFAULT_ROUTE_POLICY`] when the message omitted it.
    pub route_policy: RoutePolicy,
    pub code_entry_policy: CodeEntryPolicy,
    pub viewer_authorization_generation: String,
    pub share_generation: Option<String>,
    pub route_revision: Int,
    pub route_assignment: ParticipantRouteAssignment,
    pub quality_settings: QualitySettings,
    pub role: Role,
    /// Present exactly for the Host member.
    pub viewer_password_enabled: Option<bool>,
}

/// Acknowledges a new media publication on an existing Host session.
#[derive(Debug, Clone, PartialEq)]
pub struct SharingStartedMessage {
    pub share_generation: String,
    pub route_policy: RoutePolicy,
    pub route_revision: Int,
    pub route_assignment: ParticipantRouteAssignment,
    pub quality_settings: QualitySettings,
    pub paused: bool,
}

/// The forwarded viewer evidence envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerViewerQualityEvidenceMessage {
    pub viewer_peer_id: String,
    /// Narrowed to the active variants: peer or sfu.
    pub upstream: MediaRouteUpstream,
    pub guard: ViewerQualityEvidenceGuard,
    pub sequence: Int,
    pub window_ms: Int,
    pub metrics: ViewerQualityEvidenceMetrics,
}

/// The crossed (state, reason) union of `route-status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteStatus {
    /// `state: "waiting", reason: "sfu-admission"`.
    WaitingSfuAdmission,
    /// `state: "failed", reason: "route-exhausted"`.
    FailedRouteExhausted,
}

impl RouteStatus {
    fn pair(self) -> (&'static str, &'static str) {
        match self {
            Self::WaitingSfuAdmission => ("waiting", "sfu-admission"),
            Self::FailedRouteExhausted => ("failed", "route-exhausted"),
        }
    }
}

/// `sharing-start-failed` codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharingStartFailure {
    Forbidden,
    ServerError,
}

/// One member of `serverMessageSchema`.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    /// `authenticated`, for either role.
    Authenticated(Box<AuthenticatedMessage>),
    /// `sharing-started`.
    SharingStarted(Box<SharingStartedMessage>),
    /// `sharing-start-failed`.
    SharingStartFailed {
        share_generation: String,
        code: SharingStartFailure,
    },
    /// `signaling-challenge-response`.
    SignalingChallengeResponse { sequence: Int },
    /// Forwarded `signal`.
    Signal {
        from_peer_id: String,
        payload: SignalPayload,
    },
    /// Forwarded `restart-request`.
    RestartRequest {
        from_peer_id: String,
        connection_id: String,
        rebuild: bool,
    },
    /// `route-update` with phase `prepare`.
    RouteUpdatePrepare {
        revision: Int,
        assignment: ParticipantRouteAssignment,
        candidate: PreparedRouteCandidate,
    },
    /// `route-update` with phase `active`.
    RouteUpdateActive {
        revision: Int,
        assignment: ParticipantRouteAssignment,
    },
    /// `route-status`.
    RouteStatus { revision: Int, status: RouteStatus },
    /// `route-diagnostic-snapshot`.
    RouteDiagnosticSnapshot(RouteDiagnosticSnapshot),
    /// `sfu-config`: authorizes one embedded media connection.
    SfuConfig {
        revision: Int,
        publication_generation: String,
        connection_id: String,
    },
    /// `sfu-signal`.
    SfuSignal(Box<SfuSignalMessage>),
    /// `quality-settings`.
    QualitySettings(QualitySettings),
    /// `route-policy`.
    RoutePolicy {
        share_generation: String,
        route_policy: RoutePolicy,
    },
    /// `pause-sharing-source`.
    PauseSharingSource { share_generation: String },
    /// Forwarded `viewer-quality-evidence`.
    ViewerQualityEvidence(Box<ServerViewerQualityEvidenceMessage>),
    /// `host-status`.
    HostStatus { online: bool, paused: bool },
    /// `viewer-presence`: the ordered participant roster.
    ViewerPresence {
        viewers: Vec<ParticipantPresenceEntry>,
    },
    /// `viewer-grant-revoked`.
    ViewerGrantRevoked {
        viewer_authorization_generation: String,
    },
    /// `sharing-stopped`.
    SharingStopped,
    /// `room-closed` with reason `host-ended`.
    RoomClosed,
    /// `error`.
    Error { code: ErrorCode, message: String },
    /// `relay-volunteer`.
    RelayVolunteer(RelayVolunteer),
    /// `room-voice-state`.
    RoomVoiceState(Box<RoomVoiceState>),
    /// `room-voice-signal`.
    RoomVoiceSignal(Box<RoomVoiceSignal>),
    /// `room-voice-error`.
    RoomVoiceError { code: RoomVoiceErrorCode },
    /// `viewer-route-preference`: the applied Viewer route preference.
    ViewerRoutePreference {
        preference: ViewerRoutePreference,
        accepted: bool,
    },
    /// `audio-presence`; `None` means the Host has not reported input
    /// availability for the current share.
    AudioPresence { available: Option<bool> },
    /// `co-play-presence`.
    CoPlayPresence {
        participants: Vec<CoPlayParticipant>,
    },
    /// `remote-input-peers`.
    RemoteInputPeers { peers: Vec<RemoteInputPeer> },
    /// `room-interactions-ready`.
    RoomInteractionsReady { server_time: Int },
    /// `room-interaction`.
    RoomInteraction(Box<RoomInteractionMessage>),
    /// `room-interaction-rejected`.
    RoomInteractionRejected {
        request_id: String,
        reason: InteractionRejection,
    },
}

/// Serializes one server message with the key order of the TypeScript object
/// literal that produces it.
pub fn encode_server_message(message: &ServerMessage) -> Vec<u8> {
    message.to_json()
}

/// Decodes one server message. The Server never reads its own messages in
/// production; this replays the shared wire fixture and supports tests.
pub fn decode_server_message(data: &[u8]) -> std::result::Result<ServerMessage, DecodeError> {
    ServerMessage::decode(&parse(data)?)
}

pub(crate) fn require_opaque(f: &Fields<'_>, key: &str, message: &str) -> Result<String> {
    let value = f.string(key)?;
    ensure(valid_opaque_id(&value), message)?;
    Ok(value)
}

pub(crate) fn revision(f: &Fields<'_>, key: &str) -> Result<Int> {
    let value = f.int(key)?;
    ensure(valid_revision(value), &format!("{key} is out of range"))?;
    Ok(value)
}

impl ServerMessage {
    /// Validates one parsed server message.
    pub fn decode(value: &Value) -> Result<Self> {
        let message_type = discriminator(value, "type")?;
        Ok(match message_type {
            "relay-volunteer" => Self::RelayVolunteer(RelayVolunteer::decode(value)?),
            "room-voice-state" => Self::RoomVoiceState(Box::new(RoomVoiceState::decode(value)?)),
            "room-voice-signal" => Self::RoomVoiceSignal(Box::new(RoomVoiceSignal::decode(value)?)),
            "room-voice-error" => {
                let f = object(value, &["type", "code"])?;
                f.require(&["type", "code"])?;
                let code = RoomVoiceErrorCode::parse(&f.string("code")?)
                    .ok_or_else(|| invalid("invalid voice error"))?;
                Self::RoomVoiceError { code }
            }
            "viewer-route-preference" => {
                let f = object(value, &["type", "preference", "accepted"])?;
                let preference = f.value("preference")?;
                let accepted = f.boolean("accepted")?;
                f.require(&["type", "preference", "accepted"])?;
                Self::ViewerRoutePreference {
                    preference,
                    accepted,
                }
            }
            "audio-presence" => {
                let f = object(value, &["type", "available"])?;
                let available = f.opt("available")?;
                f.require(&["type"])?;
                f.require_nullable(&["available"])?;
                Self::AudioPresence { available }
            }
            "co-play-presence" => Self::CoPlayPresence {
                participants: decode_co_play_participants(value)?,
            },
            "remote-input-peers" => Self::RemoteInputPeers {
                peers: decode_remote_input_peers(value)?,
            },
            "room-interactions-ready" => Self::RoomInteractionsReady {
                server_time: decode_interactions_ready(value)?,
            },
            "room-interaction" => {
                Self::RoomInteraction(Box::new(RoomInteractionMessage::decode(value)?))
            }
            "room-interaction-rejected" => {
                let (request_id, reason) = decode_interaction_rejected(value)?;
                Self::RoomInteractionRejected { request_id, reason }
            }
            "authenticated" => Self::Authenticated(Box::new(decode_authenticated(value)?)),
            "sharing-started" => {
                let keys = [
                    "type",
                    "shareGeneration",
                    "routePolicy",
                    "routeRevision",
                    "routeAssignment",
                    "qualitySettings",
                    "paused",
                ];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                Self::SharingStarted(Box::new(SharingStartedMessage {
                    route_policy: f.value("routePolicy")?,
                    route_assignment: f.value("routeAssignment")?,
                    quality_settings: f.value("qualitySettings")?,
                    paused: f.boolean("paused")?,
                    share_generation: require_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                    route_revision: revision(&f, "routeRevision")?,
                }))
            }
            "sharing-start-failed" => {
                let f = object(value, &["type", "shareGeneration", "code"])?;
                f.require(&["type", "shareGeneration", "code"])?;
                let share_generation = f.string("shareGeneration")?;
                let code = match f.string("code")?.as_str() {
                    "FORBIDDEN" => Some(SharingStartFailure::Forbidden),
                    "SERVER_ERROR" => Some(SharingStartFailure::ServerError),
                    _ => None,
                };
                match code {
                    Some(code) if valid_opaque_id(&share_generation) => Self::SharingStartFailed {
                        share_generation,
                        code,
                    },
                    _ => return Err(invalid("invalid sharing-start-failed response")),
                }
            }
            "signaling-challenge-response" => {
                let f = object(value, &["type", "sequence"])?;
                f.require(&["type", "sequence"])?;
                let sequence = f.int("sequence")?;
                ensure(
                    in_range(sequence, 0, MAX_SAFE_INTEGER),
                    "sequence is out of range",
                )?;
                Self::SignalingChallengeResponse { sequence }
            }
            "signal" => {
                let f = object(value, &["type", "fromPeerId", "payload"])?;
                f.require(&["type", "fromPeerId", "payload"])?;
                let payload = f.value("payload")?;
                Self::Signal {
                    from_peer_id: require_opaque(
                        &f,
                        "fromPeerId",
                        "fromPeerId is not an opaque id",
                    )?,
                    payload,
                }
            }
            "restart-request" => {
                let keys = ["type", "fromPeerId", "connectionId", "rebuild"];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                let from_peer_id = f.string("fromPeerId")?;
                let connection_id = f.string("connectionId")?;
                ensure(
                    valid_opaque_id(&from_peer_id) && valid_opaque_id(&connection_id),
                    "restart-request ids are not opaque ids",
                )?;
                Self::RestartRequest {
                    from_peer_id,
                    connection_id,
                    rebuild: f.boolean("rebuild")?,
                }
            }
            "route-update" => decode_route_update(value)?,
            "route-status" => {
                let keys = ["type", "revision", "state", "reason"];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                let revision = revision(&f, "revision")?;
                let (state, reason) = (f.string("state")?, f.string("reason")?);
                let status = match (state.as_str(), reason.as_str()) {
                    ("waiting", "sfu-admission") => RouteStatus::WaitingSfuAdmission,
                    ("failed", "route-exhausted") => RouteStatus::FailedRouteExhausted,
                    _ => {
                        return Err(invalid(format!(
                            "route-status pairs state {state:?} with reason {reason:?}"
                        )));
                    }
                };
                Self::RouteStatus { revision, status }
            }
            "route-diagnostic-snapshot" => {
                let f = object(value, &["type", "snapshot"])?;
                f.require(&["type", "snapshot"])?;
                Self::RouteDiagnosticSnapshot(f.value("snapshot")?)
            }
            "sfu-config" => {
                let keys = ["type", "revision", "publicationGeneration", "connectionId"];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                let revision = revision(&f, "revision")?;
                let publication_generation = f.string("publicationGeneration")?;
                let connection_id = f.string("connectionId")?;
                ensure(
                    valid_opaque_id(&publication_generation) && valid_opaque_id(&connection_id),
                    "invalid SFU connection fence",
                )?;
                Self::SfuConfig {
                    revision,
                    publication_generation,
                    connection_id,
                }
            }
            "sfu-signal" => Self::SfuSignal(Box::new(SfuSignalMessage::decode(value)?)),
            "quality-settings" => {
                let f = object(value, &["type", "qualitySettings"])?;
                f.require(&["type", "qualitySettings"])?;
                Self::QualitySettings(f.value("qualitySettings")?)
            }
            "route-policy" => {
                let f = object(value, &["type", "shareGeneration", "routePolicy"])?;
                f.require(&["type", "shareGeneration", "routePolicy"])?;
                let route_policy = f.value("routePolicy")?;
                Self::RoutePolicy {
                    share_generation: require_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                    route_policy,
                }
            }
            "pause-sharing-source" => {
                let f = object(value, &["type", "shareGeneration"])?;
                f.require(&["type", "shareGeneration"])?;
                Self::PauseSharingSource {
                    share_generation: require_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                }
            }
            "viewer-quality-evidence" => {
                let keys = [
                    "type",
                    "viewerPeerId",
                    "upstream",
                    "guard",
                    "sequence",
                    "windowMs",
                    "metrics",
                ];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                let message = ServerViewerQualityEvidenceMessage {
                    upstream: f.value("upstream")?,
                    guard: f.value("guard")?,
                    metrics: f.value("metrics")?,
                    sequence: f.int("sequence")?,
                    window_ms: f.int("windowMs")?,
                    viewer_peer_id: require_opaque(
                        &f,
                        "viewerPeerId",
                        "viewerPeerId is not an opaque id",
                    )?,
                };
                ensure(
                    message.upstream != MediaRouteUpstream::None,
                    "forwarded evidence upstream must be peer or sfu",
                )?;
                crate::client::validate_evidence_window(message.sequence, message.window_ms)?;
                Self::ViewerQualityEvidence(Box::new(message))
            }
            "host-status" => {
                let f = object(value, &["type", "online", "paused"])?;
                f.require(&["type", "online", "paused"])?;
                Self::HostStatus {
                    online: f.boolean("online")?,
                    paused: f.boolean("paused")?,
                }
            }
            "viewer-presence" => {
                let f = object(value, &["type", "viewers"])?;
                let viewers: Vec<ParticipantPresenceEntry> = f.or_default("viewers")?;
                f.require(&["type", "viewers"])?;
                ensure(
                    viewers.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT,
                    "viewer presence holds too many entries",
                )?;
                // superRefine: viewer presence peer IDs must be unique.
                let mut seen = HashSet::with_capacity(viewers.len());
                ensure(
                    viewers.iter().all(|viewer| seen.insert(&viewer.peer_id)),
                    "Viewer presence peer IDs must be unique",
                )?;
                Self::ViewerPresence { viewers }
            }
            "viewer-grant-revoked" => {
                let f = object(value, &["type", "viewerAuthorizationGeneration"])?;
                f.require(&["type", "viewerAuthorizationGeneration"])?;
                Self::ViewerGrantRevoked {
                    viewer_authorization_generation: require_opaque(
                        &f,
                        "viewerAuthorizationGeneration",
                        "viewerAuthorizationGeneration is not an opaque id",
                    )?,
                }
            }
            "sharing-stopped" => {
                object(value, &["type"])?.require(&["type"])?;
                Self::SharingStopped
            }
            "room-closed" => {
                let f = object(value, &["type", "reason"])?;
                f.require(&["type", "reason"])?;
                let reason = f.string("reason")?;
                ensure(
                    reason == "host-ended",
                    &format!("unknown room-closed reason {reason:?}"),
                )?;
                Self::RoomClosed
            }
            "error" => {
                let f = object(value, &["type", "code", "message"])?;
                f.require(&["type", "code", "message"])?;
                let code = f.value("code")?;
                let message = f.string("message")?;
                ensure(
                    (1..=256).contains(&utf16_length(&message)),
                    "error message length is out of range",
                )?;
                Self::Error { code, message }
            }
            other => return Err(invalid(format!("unknown server message type {other:?}"))),
        })
    }
}

impl ToWire for AuthenticatedMessage {
    /// The key order follows the object literal in `signaling.ts`.
    fn to_wire(&self) -> Value {
        Obj::message("authenticated")
            .put("protocol", SIGNALING_PROTOCOL)
            .put("peerId", &self.peer_id)
            .put("maxViewers", &self.max_viewers)
            .put(
                "endpointMediaCopyCapacity",
                &self.endpoint_media_copy_capacity,
            )
            .put("hostOnline", &self.host_online)
            .put_some("hostPaused", &self.host_paused)
            .put("connectionId", &self.connection_id)
            .put("iceConfig", &self.ice_config)
            .put("routePolicy", &self.route_policy)
            .put("codeEntryPolicy", &self.code_entry_policy)
            .put(
                "viewerAuthorizationGeneration",
                &self.viewer_authorization_generation,
            )
            .put("mediaMode", MEDIA_MODE)
            .put("shareGeneration", &self.share_generation)
            .put("routeRevision", &self.route_revision)
            .put("routeAssignment", &self.route_assignment)
            .put("qualitySettings", &self.quality_settings)
            .put("role", &self.role)
            .put_some("viewerPasswordEnabled", &self.viewer_password_enabled)
            .build()
    }
}

impl ToWire for ServerMessage {
    fn to_wire(&self) -> Value {
        match self {
            Self::Authenticated(message) => message.to_wire(),
            Self::SharingStarted(m) => Obj::message("sharing-started")
                .put("shareGeneration", &m.share_generation)
                .put("routePolicy", &m.route_policy)
                .put("routeRevision", &m.route_revision)
                .put("routeAssignment", &m.route_assignment)
                .put("qualitySettings", &m.quality_settings)
                .put("paused", &m.paused)
                .build(),
            Self::SharingStartFailed {
                share_generation,
                code,
            } => Obj::message("sharing-start-failed")
                .put("shareGeneration", share_generation)
                .put(
                    "code",
                    match code {
                        SharingStartFailure::Forbidden => "FORBIDDEN",
                        SharingStartFailure::ServerError => "SERVER_ERROR",
                    },
                )
                .build(),
            Self::SignalingChallengeResponse { sequence } => {
                Obj::message("signaling-challenge-response")
                    .put("sequence", sequence)
                    .build()
            }
            Self::Signal {
                from_peer_id,
                payload,
            } => Obj::message("signal")
                .put("fromPeerId", from_peer_id)
                .put("payload", payload)
                .build(),
            Self::RestartRequest {
                from_peer_id,
                connection_id,
                rebuild,
            } => Obj::message("restart-request")
                .put("fromPeerId", from_peer_id)
                .put("connectionId", connection_id)
                .put("rebuild", rebuild)
                .build(),
            Self::RouteUpdatePrepare {
                revision,
                assignment,
                candidate,
            } => Obj::message("route-update")
                .put("revision", revision)
                .put("phase", "prepare")
                .put("assignment", assignment)
                .put("candidate", candidate)
                .build(),
            Self::RouteUpdateActive {
                revision,
                assignment,
            } => Obj::message("route-update")
                .put("revision", revision)
                .put("phase", "active")
                .put("assignment", assignment)
                .build(),
            Self::RouteStatus { revision, status } => {
                let (state, reason) = status.pair();
                Obj::message("route-status")
                    .put("revision", revision)
                    .put("state", state)
                    .put("reason", reason)
                    .build()
            }
            Self::RouteDiagnosticSnapshot(snapshot) => Obj::message("route-diagnostic-snapshot")
                .put("snapshot", snapshot)
                .build(),
            Self::SfuConfig {
                revision,
                publication_generation,
                connection_id,
            } => Obj::message("sfu-config")
                .put("revision", revision)
                .put("publicationGeneration", publication_generation)
                .put("connectionId", connection_id)
                .build(),
            Self::SfuSignal(message) => message.encode(),
            Self::QualitySettings(settings) => Obj::message("quality-settings")
                .put("qualitySettings", settings)
                .build(),
            Self::RoutePolicy {
                share_generation,
                route_policy,
            } => Obj::message("route-policy")
                .put("shareGeneration", share_generation)
                .put("routePolicy", route_policy)
                .build(),
            Self::PauseSharingSource { share_generation } => Obj::message("pause-sharing-source")
                .put("shareGeneration", share_generation)
                .build(),
            Self::ViewerQualityEvidence(m) => Obj::message("viewer-quality-evidence")
                .put("viewerPeerId", &m.viewer_peer_id)
                .put("upstream", &m.upstream)
                .put("guard", &m.guard)
                .put("sequence", &m.sequence)
                .put("windowMs", &m.window_ms)
                .put("metrics", &m.metrics)
                .build(),
            Self::HostStatus { online, paused } => Obj::message("host-status")
                .put("online", online)
                .put("paused", paused)
                .build(),
            Self::ViewerPresence { viewers } => Obj::message("viewer-presence")
                .put("viewers", viewers)
                .build(),
            Self::ViewerGrantRevoked {
                viewer_authorization_generation,
            } => Obj::message("viewer-grant-revoked")
                .put(
                    "viewerAuthorizationGeneration",
                    viewer_authorization_generation,
                )
                .build(),
            Self::SharingStopped => Obj::message("sharing-stopped").build(),
            Self::RoomClosed => Obj::message("room-closed")
                .put("reason", "host-ended")
                .build(),
            Self::Error { code, message } => Obj::message("error")
                .put("code", code)
                .put("message", message)
                .build(),
            Self::RelayVolunteer(volunteer) => volunteer.encode("relay-volunteer"),
            Self::RoomVoiceState(state) => state.encode(),
            Self::RoomVoiceSignal(signal) => signal.encode(),
            Self::RoomVoiceError { code } => {
                Obj::message("room-voice-error").put("code", code).build()
            }
            Self::ViewerRoutePreference {
                preference,
                accepted,
            } => Obj::message("viewer-route-preference")
                .put("preference", preference)
                .put("accepted", accepted)
                .build(),
            Self::AudioPresence { available } => Obj::message("audio-presence")
                .put("available", available)
                .build(),
            Self::CoPlayPresence { participants } => Obj::message("co-play-presence")
                .put("participants", participants)
                .build(),
            Self::RemoteInputPeers { peers } => Obj::message("remote-input-peers")
                .put("peers", peers)
                .build(),
            Self::RoomInteractionsReady { server_time } => Obj::message("room-interactions-ready")
                .put("serverTime", server_time)
                .build(),
            Self::RoomInteraction(message) => message.encode(),
            Self::RoomInteractionRejected { request_id, reason } => {
                Obj::message("room-interaction-rejected")
                    .put("requestId", request_id)
                    .put("reason", reason)
                    .build()
            }
        }
    }
}
