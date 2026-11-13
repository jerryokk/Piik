//! Loopback v10 native-control wire shapes.
//!
//! Requests decode strictly: unknown fields are rejected at every depth,
//! absent fields take their zero value and JSON `null` for a non-optional
//! field also reads as its zero value.
//! Responses and events keep their JSON names and omission rules.

use piik_native_capture::{Adapter, CaptureTarget, Encoder, Failure, Microphone};
use piik_protocol::{FromWire, PunchPrediction, QualitySettings, RemoteInputPeer};
use piik_remote_input::{State as RemoteInputState, StopShortcut};
use serde::de::{DeserializeOwned, Error as _};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::media::{PublicationMedia, PublicationQualitySample, QualitySample};

pub const MAX_SDP_UNITS: usize = 48 * 1024;
pub const MAX_CANDIDATE_UNITS: usize = 4096;
pub const MAX_STUN_URL_BYTES: usize = 512;
pub const MAX_ICE_SERVERS: usize = 8;
pub const MAX_URLS_PER_SERVER: usize = 8;
/// `mediaedge.MaxRelayChildren`.
pub const MAX_EDGE_CAPACITY: i64 = 20;
pub const MAX_REMOTE_INPUT_PEERS: usize = 20;

/// `^[A-Za-z0-9_-]{8,256}$`, shared by request, share, connection,
/// publication and remote-control request identities.
pub fn valid_identity(value: &str) -> bool {
    piik_loopback::valid_request_id(value)
}

pub fn valid_identities(values: &[&str]) -> bool {
    values.iter().all(|value| valid_identity(value))
}

/// Decodes `payload` as `T` with strict object fields; `None` is malformed.
pub fn decode_strict<T: DeserializeOwned>(payload: &[u8]) -> Option<T> {
    serde_json::from_slice(payload).ok()
}

/// Reads JSON `null` for a non-optional field as its zero value.
fn null_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Option::<T>::deserialize(deserializer).map(Option::unwrap_or_default)
}

/// `protocol.QualitySettings` keeps its own strict decoder. An absent profile
/// is the zero profile, which no capture accepts; `null` or an invalid object
/// is malformed.
fn quality_settings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<QualitySettings>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    QualitySettings::from_wire(&value)
        .map(Some)
        .map_err(D::Error::custom)
}

fn remote_input_peers<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<RemoteInputPeer>, D::Error> {
    Option::<Vec<Value>>::deserialize(deserializer)?
        .unwrap_or_default()
        .iter()
        .map(|value| RemoteInputPeer::from_wire(value).map_err(D::Error::custom))
        .collect()
}

/// Declares one strict request: the version/id/type envelope, then fields
/// that read `null` as zero, then `raw` fields that keep their own decoder.
macro_rules! request {
    (
        $(#[$meta:meta])*
        $name:ident {
            $($field:ident : $ty:ty),* $(,)?
        }
        $(raw { $($(#[$raw_meta:meta])* $raw:ident : $raw_ty:ty),* $(,)? })?
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Default, PartialEq, Deserialize)]
        #[serde(deny_unknown_fields, rename_all = "camelCase")]
        pub struct $name {
            #[serde(default, deserialize_with = "null_default")]
            pub version: i64,
            #[serde(default, deserialize_with = "null_default")]
            pub id: String,
            #[serde(default, rename = "type", deserialize_with = "null_default")]
            pub kind: String,
            $(
                #[serde(default, deserialize_with = "null_default")]
                pub $field: $ty,
            )*
            $($(
                $(#[$raw_meta])*
                pub $raw: $raw_ty,
            )*)?
        }
    };
}

request!(
    /// capture-options, list-sources, list-microphones and the publication
    /// teardown/media commands carry no other field.
    EnvelopeRequest {}
);

request!(SourcePreviewRequest {
    source: CaptureTarget
});

request!(
    StartShareRequest {
        report_source_cancellation: bool,
        report_capture_failure: bool,
        share_id: String,
        source: CaptureTarget,
        audio: bool,
        microphone_mixing: bool,
        remote_input: bool,
        exclude_audio: Option<CaptureTarget>,
        show_capture_border: bool,
        adapter_index: u32,
        encoder_index: u32,
        edge_capacity: i64,
        codec: String,
    }
    raw {
        #[serde(default, deserialize_with = "quality_settings")]
        profile: Option<QualitySettings>,
    }
);

request!(MicrophoneRequest {
    share_id: String,
    enabled: Option<bool>,
    volume: Option<f64>,
    device_id: Option<String>,
});

request!(ShareVolumeRequest {
    share_id: String,
    volume: Option<f64>,
});

request!(RemoteControlRequest {
    share_id: String,
    request_id: String,
    allow: Option<bool>,
});

request!(
    RemoteInputPeersRequest {
        share_id: String,
    }
    raw {
        #[serde(default, deserialize_with = "remote_input_peers")]
        peers: Vec<RemoteInputPeer>,
    }
);

request!(RemoteControlPolicyRequest {
    share_id: String,
    mode: String,
    password: String,
    co_play: bool,
});

request!(RemoteControlSimultaneousRequest {
    share_id: String,
    enabled: Option<bool>,
});

request!(RemoteStopShortcutRequest {
    share_id: String,
    key: i64,
});

request!(
    UpdateShareRequest {
        share_id: String,
    }
    raw {
        #[serde(default, deserialize_with = "quality_settings")]
        profile: Option<QualitySettings>,
    }
);

request!(ReplaceShareSourceRequest {
    report_source_cancellation: bool,
    share_id: String,
    source: CaptureTarget,
    audio: bool,
    exclude_audio: Option<CaptureTarget>,
    show_capture_border: bool,
    adapter_index: u32,
    encoder_index: u32,
});

request!(
    /// stop-share and stop-receive.
    ShareRequest {
        share_id: String,
    }
);

request!(PauseShareRequest {
    share_id: String,
    paused: bool,
});

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IceServer {
    #[serde(default, deserialize_with = "null_default")]
    pub urls: Vec<String>,
}

request!(PrepareEdgeRequest {
    share_id: String,
    connection_id: String,
    source_connection_id: String,
    ice_servers: Vec<IceServer>,
    punched: bool,
});

request!(PrepareLocalEdgeRequest {
    share_id: String,
    connection_id: String,
    source_connection_id: String,
    restart: bool,
});

request!(ReceiveOfferRequest {
    share_id: String,
    connection_id: String,
    edge_capacity: i64,
    ice_servers: Vec<IceServer>,
    sdp: String,
    reuse_receiver: bool,
    punched: bool,
});

/// The WebRTC `RTCIceCandidateInit` shape; `null` members stay present.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct IceCandidateInit {
    #[serde(default, deserialize_with = "null_default")]
    pub candidate: String,
    #[serde(default)]
    pub sdp_mid: Option<String>,
    #[serde(default, rename = "sdpMLineIndex")]
    pub sdp_m_line_index: Option<u16>,
    #[serde(default)]
    pub username_fragment: Option<String>,
}

request!(
    /// receive-candidate and edge-candidate. A `null` candidate ends gathering.
    CandidateRequest {
        share_id: String,
        connection_id: String,
        candidate: Option<IceCandidateInit>,
    }
);

request!(
    /// close-receiver and close-edge.
    ConnectionRequest {
        share_id: String,
        connection_id: String,
    }
);

request!(EdgeAnswerRequest {
    share_id: String,
    connection_id: String,
    sdp: String,
    offer_id: String,
});

/// The lax identity read before a publication command is validated.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationIdentity {
    #[serde(default, deserialize_with = "null_default")]
    pub share_id: String,
    #[serde(default, deserialize_with = "null_default")]
    pub publication_generation: String,
    #[serde(default, deserialize_with = "null_default")]
    pub connection_id: String,
}

request!(
    /// close-publication and publication-media.
    PublicationRequest {
        share_id: String,
        publication_generation: String,
        connection_id: String,
    }
);

request!(PreparePublicationRequest {
    share_id: String,
    publication_generation: String,
    connection_id: String,
    ice_servers: Vec<IceServer>,
});

request!(PublicationAnswerRequest {
    share_id: String,
    publication_generation: String,
    connection_id: String,
    sdp: String,
});

request!(PublicationCandidateRequest {
    share_id: String,
    publication_generation: String,
    connection_id: String,
    candidate: Option<IceCandidateInit>,
});

request!(PublicationLayersRequest {
    share_id: String,
    publication_generation: String,
    connection_id: String,
    active_count: Option<i64>,
});

/// An absent prediction is the zero observation, which no target accepts;
/// `null` or an invalid object is malformed.
fn punch_prediction<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<PunchPrediction>, D::Error> {
    let value = Value::deserialize(deserializer)?;
    PunchPrediction::from_wire(&value)
        .map(Some)
        .map_err(D::Error::custom)
}

// An explicit `edgeCapacity: null` reads as absent.
request!(PreparePunchRequest {
    share_id: String,
    connection_id: String,
    source_connection_id: String,
    edge_capacity: Option<i64>,
    ice_servers: Vec<IceServer>,
    secret: String,
});

request!(
    RunPunchRequest {
        share_id: String,
        connection_id: String,
    }
    raw {
        #[serde(default, deserialize_with = "punch_prediction")]
        prediction: Option<PunchPrediction>,
    }
);

/// Returns the STUN URL lists, or the protocol violation for an oversized or
/// non-STUN list.
pub fn ice_servers(values: &[IceServer]) -> Result<Vec<Vec<String>>, &'static str> {
    if values.len() > MAX_ICE_SERVERS {
        return Err("native ICE server list is too large");
    }
    values
        .iter()
        .map(|server| {
            if server.urls.is_empty() || server.urls.len() > MAX_URLS_PER_SERVER {
                return Err("native ICE server is invalid");
            }
            if !server.urls.iter().all(|url| valid_stun_url(url)) {
                return Err("native STUN URL is invalid");
            }
            Ok(server.urls.clone())
        })
        .collect()
}

/// `stun:` plus a host and optional port only: no user, path, query or fragment.
pub fn valid_stun_url(value: &str) -> bool {
    if value.len() < 6
        || value.len() > MAX_STUN_URL_BYTES
        || !value.is_char_boundary(5)
        || !value[..5].eq_ignore_ascii_case("stun:")
    {
        return false;
    }
    let authority = &value[5..];
    if authority.is_empty()
        || authority.contains(['/', '?', '#', '@', '\\'])
        || authority.chars().any(char::is_whitespace)
        || authority.ends_with(':')
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(&format!("http://{authority}")) else {
        return false;
    };
    parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && parsed.port().is_none_or(|port| port > 0)
}

/// Relayed media text keeps the Browser/server's UTF-16 limits. Its rejection
/// fails one request, not the authenticated Browser's entire native session.
pub fn valid_sdp(sdp: &str) -> bool {
    !sdp.is_empty() && piik_protocol::utf16_length(sdp) <= MAX_SDP_UNITS
}

pub fn valid_candidate(candidate: Option<&IceCandidateInit>) -> bool {
    let Some(candidate) = candidate else {
        return true;
    };
    let units = piik_protocol::utf16_length;
    units(&candidate.candidate) <= MAX_CANDIDATE_UNITS
        && candidate
            .sdp_mid
            .as_deref()
            .is_none_or(|mid| units(mid) <= 128)
        && candidate.sdp_m_line_index.is_none_or(|index| index <= 255)
        && candidate
            .username_fragment
            .as_deref()
            .is_none_or(|fragment| units(fragment) <= 256)
}

pub fn capture_target_key(target: &CaptureTarget) -> String {
    if target.kind == "window" {
        format!(
            "window:{}:{}:{}",
            target.source_id, target.pid, target.creation_time
        )
    } else {
        format!("display:{}", target.source_id)
    }
}

// Responses.

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ResponseEnvelope {
    pub version: i64,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
}

impl ResponseEnvelope {
    pub fn new(id: &str, kind: &str) -> Self {
        Self {
            version: piik_loopback::PROTOCOL_VERSION,
            id: id.to_owned(),
            kind: kind.to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RequestFailedResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub code: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderView {
    pub index: u32,
    pub name: String,
    pub identity: String,
}

/// The serialized form of a probed adapter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdapterView {
    pub index: u32,
    pub name: String,
    pub identity: String,
    #[serde(rename = "hardwareH264")]
    pub hardware_h264: Vec<EncoderView>,
}

impl From<&Adapter> for AdapterView {
    fn from(adapter: &Adapter) -> Self {
        let encoder = |encoder: &Encoder| EncoderView {
            index: encoder.index,
            name: encoder.name.clone(),
            identity: encoder.identity.clone(),
        };
        Self {
            index: adapter.index,
            name: adapter.name.clone(),
            identity: adapter.identity.clone(),
            hardware_h264: adapter.hardware_h264.iter().map(encoder).collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CaptureOptionsResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub adapters: Vec<AdapterView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MicrophoneListResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub devices: Vec<Microphone>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceListResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub sources: Vec<CaptureTarget>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePreviewResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub source_key: String,
    pub mime: &'static str,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareStartedResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub share_id: String,
    pub audio: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_audio: Option<bool>,
    pub codec: String,
}

/// share-updated and share-source-replaced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub share_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteStopShortcutResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    #[serde(flatten)]
    pub shortcut: StopShortcut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeOfferResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub share_id: String,
    pub connection_id: String,
    pub sdp: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveAnswerResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub share_id: String,
    pub connection_id: String,
    pub sdp: String,
    pub audio: bool,
    pub codec: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reused: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationResponse {
    #[serde(flatten)]
    pub envelope: ResponseEnvelope,
    pub share_id: String,
    pub publication_generation: String,
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub sdp: String,
    pub media: PublicationMedia,
}

// Events.

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventEnvelope {
    pub version: i64,
    #[serde(rename = "type")]
    pub kind: String,
    pub share_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub connection_id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub publication_generation: String,
}

impl EventEnvelope {
    pub fn new(kind: &str, share_id: &str) -> Self {
        Self {
            version: piik_loopback::PROTOCOL_VERSION,
            kind: kind.to_owned(),
            share_id: share_id.to_owned(),
            connection_id: String::new(),
            publication_generation: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemoteControlStateEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    #[serde(flatten)]
    pub state: RemoteInputState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStateEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    pub source_audio: bool,
    pub microphone: bool,
    pub failed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PublicationQualityEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    #[serde(flatten)]
    pub sample: PublicationQualitySample,
}

/// capture-state, edge-state and publication-state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StateEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    pub state: String,
}

/// edge-candidate and publication-candidate; `null` ends gathering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CandidateEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    pub candidate: Option<IceCandidateInit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgePathEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    pub local_type: String,
    pub remote_type: String,
    pub nat_traversal_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeQualityEvent {
    #[serde(flatten)]
    pub envelope: EventEnvelope,
    pub sample_timestamp_ms: i64,
    pub sample_window_ms: i64,
    pub rtp_stats_id: String,
    pub track_identifier: String,
    pub state: String,
    pub reason: Option<String>,
    /// The v10 name for output frames.
    #[serde(rename = "intervalFramesEncoded")]
    pub interval_frames_sent: u64,
    pub frames_per_second: f64,
    pub bitrate_kbps: f64,
    pub available_outgoing_kbps: f64,
    pub width: u32,
    pub height: u32,
}

impl EdgeQualityEvent {
    pub fn new(envelope: EventEnvelope, quality: &QualitySample) -> Self {
        Self {
            envelope,
            sample_timestamp_ms: quality.sample_timestamp_ms,
            sample_window_ms: quality.sample_window_ms,
            rtp_stats_id: quality.rtp_stats_id.clone(),
            track_identifier: quality.track_identifier.clone(),
            state: quality.state.clone(),
            reason: quality.reason.clone(),
            interval_frames_sent: quality.interval_frames_sent,
            frames_per_second: quality.frames_per_second,
            bitrate_kbps: quality.bitrate_kbps,
            available_outgoing_kbps: quality.available_outgoing_kbps,
            width: quality.width,
            height: quality.height,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ShareEndedEvent {
    pub version: i64,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub share_id: String,
    pub failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<Failure>,
}
