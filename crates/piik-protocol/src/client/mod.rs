//! `clientMessageSchema`: every message a Host or Viewer page sends.

use serde_json::Value;

use crate::co_play_presence::SetCoPlayPresenceMessage;

mod auth;
mod quality;

use crate::codec::{
    DecodeError, Fields, Int, Num, Obj, Result, ToWire, discriminator, ensure, in_range, invalid,
    object, parse,
};
use crate::constants::{MAX_ENDPOINT_MEDIA_COPY_CAPACITY, MAX_SAFE_INTEGER, SIGNALING_PROTOCOL};
use crate::interactions::SendRoomInteractionMessage;
use crate::relay_volunteer::RelayVolunteer;
use crate::remote_input::SubscribeRemoteInputMessage;
pub(crate) use auth::decode_authenticate;
pub(crate) use quality::{
    decode_sender_quality_evidence, decode_sfu_publisher_quality_evidence, validate_evidence_window,
};

use crate::scalars::{DisplayName, valid_opaque_id};
use crate::sfu::SfuSignalMessage;
use crate::types::{
    MediaRoutePhase, QualitySettings, RoutePolicy, SenderQualityDiagnostics, SenderQualityState,
    SignalPayload, ViewerQualityEvidenceGuard, ViewerQualityEvidenceMetrics, valid_revision,
};
use crate::viewer_route::ViewerRoutePreference;
use crate::voice::{RoomVoiceCommand, RoomVoiceSignal, VoiceTransportState};

/// The Host member of `authenticateMessageSchema`. `protocol` is always
/// [`SIGNALING_PROTOCOL`] and `role` is `"host"`.
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticateHostMessage {
    pub room_id: String,
    pub token: String,
    pub client_id: String,
    pub share_generation: Option<String>,
    pub sharing_paused: Option<bool>,
    pub quality_settings: Option<QualitySettings>,
    /// [`DEFAULT_ROUTE_POLICY`] when the page omitted it.
    pub route_policy: RoutePolicy,
    pub display_name: Option<DisplayName>,
}

/// The Viewer member of `authenticateMessageSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthenticateViewerMessage {
    pub room_id: String,
    pub client_id: String,
    pub viewer_grant: Option<String>,
    pub viewer_password: Option<String>,
    pub display_name: Option<DisplayName>,
    pub route_preference: Option<ViewerRoutePreference>,
}

/// `viewerQualityEvidenceMessageSchema`.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewerQualityEvidenceMessage {
    pub guard: ViewerQualityEvidenceGuard,
    pub sequence: Int,
    pub window_ms: Int,
    pub metrics: ViewerQualityEvidenceMetrics,
}

/// `senderQualityEvidenceMessageSchema`.
#[derive(Debug, Clone, PartialEq)]
pub struct SenderQualityEvidenceMessage {
    pub child_peer_id: String,
    pub connection_id: String,
    pub rtp_stats_id: Option<String>,
    pub track_identifier: Option<String>,
    pub sample_timestamp_ms: Option<Num>,
    pub route_revision: Int,
    pub state: SenderQualityState,
    pub diagnostics: SenderQualityDiagnostics,
}

/// `sfuPublisherQualityEvidenceMessageSchema`.
#[derive(Debug, Clone, PartialEq)]
pub struct SfuPublisherQualityEvidenceMessage {
    pub publication_generation: String,
    pub route_revision: Int,
    pub state: SenderQualityState,
    pub sample_timestamp_ms: Option<Num>,
    pub diagnostics: SenderQualityDiagnostics,
}

/// Begins a new media publication on an existing Host room session.
#[derive(Debug, Clone, PartialEq)]
pub struct StartSharingMessage {
    pub share_generation: String,
    pub sharing_paused: bool,
    pub quality_settings: Option<QualitySettings>,
    pub route_policy: RoutePolicy,
}

/// One member of `clientMessageSchema`. Every member carries a distinct
/// `type` literal, so that key resolves the union.
#[derive(Debug, Clone, PartialEq)]
pub enum ClientMessage {
    AuthenticateHost(Box<AuthenticateHostMessage>),
    AuthenticateViewer(Box<AuthenticateViewerMessage>),
    /// `signaling-challenge`.
    SignalingChallenge {
        sequence: Int,
    },
    /// `signal`.
    Signal {
        target_peer_id: Option<String>,
        payload: SignalPayload,
    },
    /// `restart-request`.
    RestartRequest {
        target_peer_id: Option<String>,
        connection_id: String,
        rebuild: bool,
    },
    /// `set-quality-settings`.
    SetQualitySettings(QualitySettings),
    /// `relay-capacity`.
    RelayCapacity {
        downstream_edges: Int,
    },
    /// `route-ready`; `quality_approved` is emitted only when true.
    RouteReady {
        revision: Int,
        phase: MediaRoutePhase,
        quality_approved: bool,
    },
    /// `route-transport-connected`.
    RouteTransportConnected {
        revision: Int,
        connection_id: String,
    },
    /// `route-media-unavailable`.
    RouteMediaUnavailable {
        revision: Int,
    },
    /// `route-failed`.
    RouteFailed {
        revision: Int,
        phase: MediaRoutePhase,
        connection_id: Option<String>,
    },
    /// `refresh-sfu`.
    RefreshSfu {
        revision: Int,
    },
    /// `sfu-signal`.
    SfuSignal(Box<SfuSignalMessage>),
    /// `request-route-diagnostic`.
    RequestRouteDiagnostic,
    /// `viewer-quality-evidence`.
    ViewerQualityEvidence(Box<ViewerQualityEvidenceMessage>),
    /// `sender-quality-evidence`.
    SenderQualityEvidence(Box<SenderQualityEvidenceMessage>),
    /// `sfu-publisher-quality-evidence`.
    SfuPublisherQualityEvidence(Box<SfuPublisherQualityEvidenceMessage>),
    /// `reset-sender-quality`.
    ResetSenderQuality,
    /// `set-display-name`.
    SetDisplayName(DisplayName),
    /// `set-sharing-paused`.
    SetSharingPaused {
        share_generation: String,
        paused: bool,
    },
    /// `stop-sharing`.
    StopSharing {
        share_generation: Option<String>,
    },
    /// `start-sharing`.
    StartSharing(Box<StartSharingMessage>),
    /// `abandon-room`.
    AbandonRoom,
    /// `set-relay-volunteer`.
    SetRelayVolunteer(RelayVolunteer),
    /// `room-voice-command`.
    RoomVoiceCommand(RoomVoiceCommand),
    /// `room-voice-signal`.
    RoomVoiceSignal(Box<RoomVoiceSignal>),
    /// `room-voice-transport`.
    RoomVoiceTransport {
        connection_id: String,
        state: VoiceTransportState,
    },
    /// `set-viewer-route-preference`.
    SetViewerRoutePreference(ViewerRoutePreference),
    /// `set-audio-presence`.
    SetAudioPresence {
        share_generation: String,
        available: bool,
    },
    /// `set-co-play-presence`.
    SetCoPlayPresence(SetCoPlayPresenceMessage),
    /// `subscribe-remote-input`.
    SubscribeRemoteInput(SubscribeRemoteInputMessage),
    /// `send-room-interaction`.
    SendRoomInteraction(SendRoomInteractionMessage),
}

/// Parses JSON and validates the client message schema.
pub fn decode_client_message(data: &[u8]) -> std::result::Result<ClientMessage, DecodeError> {
    ClientMessage::decode(&parse(data)?)
}

/// Serializes one client message in the wire key order.
pub fn encode_client_message(message: &ClientMessage) -> Vec<u8> {
    message.to_json()
}

fn decode_empty(value: &Value) -> Result<()> {
    object(value, &["type"])?.require(&["type"])
}

/// An optional opaque id: absent is `None`, present must be valid.
fn optional_opaque(f: &Fields<'_>, key: &str, message: &str) -> Result<Option<String>> {
    f.optional(&[key])?;
    if !f.has(key) {
        return Ok(None);
    }
    let value = f.string(key)?;
    ensure(valid_opaque_id(&value), message)?;
    Ok(Some(value))
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

/// A `.literal(true).optional()` member.
fn true_when_present(f: &Fields<'_>, key: &str) -> Result<bool> {
    let value = f.boolean(key)?;
    ensure(
        !f.has(key) || value,
        &format!("{key} must be true when present"),
    )?;
    Ok(value)
}

fn current_protocol(f: &Fields<'_>) -> Result<()> {
    ensure(
        f.string("protocol")? == SIGNALING_PROTOCOL,
        "protocol is not the current signaling generation",
    )
}

impl ClientMessage {
    /// Validates one parsed client message.
    pub fn decode(value: &Value) -> Result<Self> {
        let message_type = discriminator(value, "type")?;
        Ok(match message_type {
            "set-relay-volunteer" => Self::SetRelayVolunteer(RelayVolunteer::decode(value)?),
            "room-voice-command" => {
                let f = object(value, &["type", "command"])?;
                f.require(&["type", "command"])?;
                Self::RoomVoiceCommand(f.value("command")?)
            }
            "room-voice-signal" => Self::RoomVoiceSignal(Box::new(RoomVoiceSignal::decode(value)?)),
            "room-voice-transport" => {
                let f = object(value, &["type", "connectionId", "state"])?;
                f.require(&["type", "connectionId", "state"])?;
                let connection_id = f.string("connectionId")?;
                let state = VoiceTransportState::parse(&f.string("state")?);
                match state {
                    Some(state) if valid_opaque_id(&connection_id) => Self::RoomVoiceTransport {
                        connection_id,
                        state,
                    },
                    _ => return Err(invalid("invalid voice transport")),
                }
            }
            "set-viewer-route-preference" => {
                let f = object(value, &["type", "preference"])?;
                let preference = f.value("preference")?;
                f.require(&["type", "preference"])?;
                Self::SetViewerRoutePreference(preference)
            }
            "set-co-play-presence" => {
                Self::SetCoPlayPresence(SetCoPlayPresenceMessage::decode(value)?)
            }
            "set-audio-presence" => {
                let f = object(value, &["type", "shareGeneration", "available"])?;
                let share_generation = f.string("shareGeneration")?;
                let available = f.boolean("available")?;
                f.require(&["type", "shareGeneration", "available"])?;
                ensure(
                    valid_opaque_id(&share_generation),
                    "invalid share generation",
                )?;
                Self::SetAudioPresence {
                    share_generation,
                    available,
                }
            }
            "subscribe-remote-input" => {
                Self::SubscribeRemoteInput(SubscribeRemoteInputMessage::decode(value)?)
            }
            "send-room-interaction" => {
                Self::SendRoomInteraction(SendRoomInteractionMessage::decode(value)?)
            }
            "authenticate" => decode_authenticate(value)?,
            "signaling-challenge" => {
                let f = object(value, &["type", "sequence"])?;
                f.require(&["type", "sequence"])?;
                let sequence = f.int("sequence")?;
                ensure(
                    in_range(sequence, 0, MAX_SAFE_INTEGER),
                    "sequence is out of range",
                )?;
                Self::SignalingChallenge { sequence }
            }
            "signal" => {
                let f = object(value, &["type", "targetPeerId", "payload"])?;
                f.require(&["type", "payload"])?;
                let payload = f.value("payload")?;
                let target_peer_id =
                    optional_opaque(&f, "targetPeerId", "targetPeerId is not an opaque id")?;
                Self::Signal {
                    target_peer_id,
                    payload,
                }
            }
            "restart-request" => {
                let f = object(value, &["type", "targetPeerId", "connectionId", "rebuild"])?;
                f.require(&["type", "connectionId", "rebuild"])?;
                let rebuild = f.boolean("rebuild")?;
                let target_peer_id =
                    optional_opaque(&f, "targetPeerId", "targetPeerId is not an opaque id")?;
                let connection_id =
                    require_opaque(&f, "connectionId", "connectionId is not an opaque id")?;
                Self::RestartRequest {
                    target_peer_id,
                    connection_id,
                    rebuild,
                }
            }
            "set-quality-settings" => {
                let f = object(value, &["type", "qualitySettings"])?;
                f.require(&["type", "qualitySettings"])?;
                Self::SetQualitySettings(f.value("qualitySettings")?)
            }
            "relay-capacity" => {
                let f = object(value, &["type", "downstreamEdges"])?;
                f.require(&["type", "downstreamEdges"])?;
                let downstream_edges = f.int("downstreamEdges")?;
                ensure(
                    in_range(downstream_edges, 0, MAX_ENDPOINT_MEDIA_COPY_CAPACITY),
                    "downstreamEdges is out of range",
                )?;
                Self::RelayCapacity { downstream_edges }
            }
            "route-ready" => {
                let f = object(value, &["type", "revision", "phase", "qualityApproved"])?;
                f.require(&["type", "revision", "phase"])?;
                f.optional(&["qualityApproved"])?;
                Self::RouteReady {
                    revision: revision(&f, "revision")?,
                    phase: f.value("phase")?,
                    quality_approved: true_when_present(&f, "qualityApproved")?,
                }
            }
            "route-transport-connected" => {
                let f = object(value, &["type", "revision", "connectionId"])?;
                f.require(&["type", "revision", "connectionId"])?;
                Self::RouteTransportConnected {
                    revision: revision(&f, "revision")?,
                    connection_id: require_opaque(
                        &f,
                        "connectionId",
                        "connectionId is not an opaque id",
                    )?,
                }
            }
            "route-media-unavailable" => {
                let f = object(value, &["type", "revision"])?;
                f.require(&["type", "revision"])?;
                Self::RouteMediaUnavailable {
                    revision: revision(&f, "revision")?,
                }
            }
            "route-failed" => {
                let f = object(value, &["type", "revision", "phase", "connectionId"])?;
                f.require(&["type", "revision", "phase"])?;
                f.require_nullable(&["connectionId"])?;
                let connection_id: Option<String> = f.opt("connectionId")?;
                ensure(
                    connection_id.as_deref().is_none_or(valid_opaque_id),
                    "connectionId is not an opaque id",
                )?;
                Self::RouteFailed {
                    revision: revision(&f, "revision")?,
                    phase: f.value("phase")?,
                    connection_id,
                }
            }
            "refresh-sfu" => {
                let f = object(value, &["type", "revision"])?;
                f.require(&["type", "revision"])?;
                Self::RefreshSfu {
                    revision: revision(&f, "revision")?,
                }
            }
            "sfu-signal" => Self::SfuSignal(Box::new(SfuSignalMessage::decode(value)?)),
            "request-route-diagnostic" => {
                decode_empty(value)?;
                Self::RequestRouteDiagnostic
            }
            "viewer-quality-evidence" => {
                let keys = ["type", "guard", "sequence", "windowMs", "metrics"];
                let f = object(value, &keys)?;
                f.require(&keys)?;
                let message = ViewerQualityEvidenceMessage {
                    guard: f.value("guard")?,
                    sequence: f.int("sequence")?,
                    window_ms: f.int("windowMs")?,
                    metrics: f.value("metrics")?,
                };
                validate_evidence_window(message.sequence, message.window_ms)?;
                Self::ViewerQualityEvidence(Box::new(message))
            }
            "sender-quality-evidence" => decode_sender_quality_evidence(value)?,
            "sfu-publisher-quality-evidence" => decode_sfu_publisher_quality_evidence(value)?,
            "reset-sender-quality" => {
                decode_empty(value)?;
                Self::ResetSenderQuality
            }
            "set-display-name" => {
                let f = object(value, &["type", "displayName"])?;
                let name = f.value("displayName")?;
                f.require(&["type", "displayName"])?;
                Self::SetDisplayName(name)
            }
            "set-sharing-paused" => {
                let f = object(value, &["type", "shareGeneration", "paused"])?;
                f.require(&["type", "shareGeneration", "paused"])?;
                Self::SetSharingPaused {
                    share_generation: require_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                    paused: f.boolean("paused")?,
                }
            }
            "stop-sharing" => {
                let f = object(value, &["type", "shareGeneration"])?;
                f.require(&["type"])?;
                Self::StopSharing {
                    share_generation: optional_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                }
            }
            "start-sharing" => {
                let f = object(
                    value,
                    &[
                        "type",
                        "shareGeneration",
                        "sharingPaused",
                        "qualitySettings",
                        "routePolicy",
                    ],
                )?;
                let quality_settings = f.opt("qualitySettings")?;
                let route_policy = f.opt("routePolicy")?;
                let sharing_paused = f.boolean("sharingPaused")?;
                f.require(&["type", "shareGeneration", "routePolicy"])?;
                f.optional(&["sharingPaused", "qualitySettings"])?;
                Self::StartSharing(Box::new(StartSharingMessage {
                    share_generation: require_opaque(
                        &f,
                        "shareGeneration",
                        "shareGeneration is not an opaque id",
                    )?,
                    sharing_paused,
                    quality_settings,
                    route_policy: route_policy.ok_or_else(|| invalid("routePolicy is required"))?,
                }))
            }
            "abandon-room" => {
                decode_empty(value)?;
                Self::AbandonRoom
            }
            other => return Err(invalid(format!("unknown client message type {other:?}"))),
        })
    }
}

impl ToWire for ClientMessage {
    fn to_wire(&self) -> Value {
        match self {
            Self::AuthenticateHost(m) => Obj::message("authenticate")
                .put("protocol", SIGNALING_PROTOCOL)
                .put("roomId", &m.room_id)
                .put("role", "host")
                .put("token", &m.token)
                .put("clientId", &m.client_id)
                .put_some("shareGeneration", &m.share_generation)
                .put_some("sharingPaused", &m.sharing_paused)
                .put_some("qualitySettings", &m.quality_settings)
                .put("routePolicy", &m.route_policy)
                .put_some("displayName", &m.display_name)
                .build(),
            Self::AuthenticateViewer(m) => Obj::message("authenticate")
                .put("protocol", SIGNALING_PROTOCOL)
                .put("roomId", &m.room_id)
                .put("role", "viewer")
                .put("clientId", &m.client_id)
                .put_some("viewerGrant", &m.viewer_grant)
                .put_some("viewerPassword", &m.viewer_password)
                .put_some("displayName", &m.display_name)
                .put_some("routePreference", &m.route_preference)
                .build(),
            Self::SignalingChallenge { sequence } => Obj::message("signaling-challenge")
                .put("sequence", sequence)
                .build(),
            Self::Signal {
                target_peer_id,
                payload,
            } => Obj::message("signal")
                .put_some("targetPeerId", target_peer_id)
                .put("payload", payload)
                .build(),
            Self::RestartRequest {
                target_peer_id,
                connection_id,
                rebuild,
            } => Obj::message("restart-request")
                .put_some("targetPeerId", target_peer_id)
                .put("connectionId", connection_id)
                .put("rebuild", rebuild)
                .build(),
            Self::SetQualitySettings(settings) => Obj::message("set-quality-settings")
                .put("qualitySettings", settings)
                .build(),
            Self::RelayCapacity { downstream_edges } => Obj::message("relay-capacity")
                .put("downstreamEdges", downstream_edges)
                .build(),
            Self::RouteReady {
                revision,
                phase,
                quality_approved,
            } => Obj::message("route-ready")
                .put("revision", revision)
                .put("phase", phase)
                .put_if(*quality_approved, "qualityApproved", &true)
                .build(),
            Self::RouteTransportConnected {
                revision,
                connection_id,
            } => Obj::message("route-transport-connected")
                .put("revision", revision)
                .put("connectionId", connection_id)
                .build(),
            Self::RouteMediaUnavailable { revision } => Obj::message("route-media-unavailable")
                .put("revision", revision)
                .build(),
            Self::RouteFailed {
                revision,
                phase,
                connection_id,
            } => Obj::message("route-failed")
                .put("revision", revision)
                .put("phase", phase)
                .put("connectionId", connection_id)
                .build(),
            Self::RefreshSfu { revision } => Obj::message("refresh-sfu")
                .put("revision", revision)
                .build(),
            Self::SfuSignal(message) => message.encode(),
            Self::RequestRouteDiagnostic => Obj::message("request-route-diagnostic").build(),
            Self::ViewerQualityEvidence(m) => Obj::message("viewer-quality-evidence")
                .put("guard", &m.guard)
                .put("sequence", &m.sequence)
                .put("windowMs", &m.window_ms)
                .put("metrics", &m.metrics)
                .build(),
            Self::SenderQualityEvidence(m) => Obj::message("sender-quality-evidence")
                .put("childPeerId", &m.child_peer_id)
                .put("connectionId", &m.connection_id)
                .put("rtpStatsId", &m.rtp_stats_id)
                .put("trackIdentifier", &m.track_identifier)
                .put("sampleTimestampMs", &m.sample_timestamp_ms)
                .put("routeRevision", &m.route_revision)
                .put("state", &m.state)
                .put("diagnostics", &m.diagnostics)
                .build(),
            Self::SfuPublisherQualityEvidence(m) => Obj::message("sfu-publisher-quality-evidence")
                .put("publicationGeneration", &m.publication_generation)
                .put("routeRevision", &m.route_revision)
                .put("state", &m.state)
                .put("sampleTimestampMs", &m.sample_timestamp_ms)
                .put("diagnostics", &m.diagnostics)
                .build(),
            Self::ResetSenderQuality => Obj::message("reset-sender-quality").build(),
            Self::SetDisplayName(name) => Obj::message("set-display-name")
                .put("displayName", name)
                .build(),
            Self::SetSharingPaused {
                share_generation,
                paused,
            } => Obj::message("set-sharing-paused")
                .put("shareGeneration", share_generation)
                .put("paused", paused)
                .build(),
            Self::StopSharing { share_generation } => Obj::message("stop-sharing")
                .put_some("shareGeneration", share_generation)
                .build(),
            Self::StartSharing(m) => Obj::message("start-sharing")
                .put("shareGeneration", &m.share_generation)
                .put_if(m.sharing_paused, "sharingPaused", &true)
                .put_some("qualitySettings", &m.quality_settings)
                .put("routePolicy", &m.route_policy)
                .build(),
            Self::AbandonRoom => Obj::message("abandon-room").build(),
            Self::SetRelayVolunteer(volunteer) => volunteer.encode("set-relay-volunteer"),
            Self::RoomVoiceCommand(command) => Obj::message("room-voice-command")
                .put("command", command)
                .build(),
            Self::RoomVoiceSignal(signal) => signal.encode(),
            Self::RoomVoiceTransport {
                connection_id,
                state,
            } => Obj::message("room-voice-transport")
                .put("connectionId", connection_id)
                .put("state", state)
                .build(),
            Self::SetViewerRoutePreference(preference) => {
                Obj::message("set-viewer-route-preference")
                    .put("preference", preference)
                    .build()
            }
            Self::SetAudioPresence {
                share_generation,
                available,
            } => Obj::message("set-audio-presence")
                .put("shareGeneration", share_generation)
                .put("available", available)
                .build(),
            Self::SetCoPlayPresence(message) => message.encode(),
            Self::SubscribeRemoteInput(message) => message.encode(),
            Self::SendRoomInteraction(message) => message.encode(),
        }
    }
}
