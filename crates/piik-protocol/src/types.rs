//! Shared wire types mirrored from `src/shared/protocol.ts`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::codec::{
    FromWire, Int, Num, Obj, Result, ToWire, in_range, in_range_num, invalid, object, wire_enum,
};
use crate::constants::{
    MAX_AUDIO_BITRATE, MAX_ICE_SERVER_URLS, MAX_MEDIA_ROUTE_REVISION,
    MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS, MAX_SAFE_INTEGER, MAX_STUN_URL_LENGTH,
    MIN_AUDIO_BITRATE,
};
use crate::scalars::{utf16_length, valid_opaque_id, valid_stun_url};

mod diagnostics;
mod evidence;
mod route;
mod signal;

pub use diagnostics::*;
pub use evidence::*;
pub use route::*;
pub use signal::*;

wire_enum! {
    /// A participant role.
    pub enum Role {
        Host = "host",
        Viewer = "viewer",
    }
}

wire_enum! {
    /// `codeEntryPolicySchema`.
    pub enum CodeEntryPolicy {
        Open = "open",
        Private = "private",
    }
}

wire_enum! {
    /// `qualitySettingsSchema.degradationPreference`.
    pub enum DegradationPreference {
        MaintainResolution = "maintain-resolution",
        Balanced = "balanced",
        MaintainFramerate = "maintain-framerate",
    }
}

wire_enum! {
    /// `mediaRoutePhaseSchema`.
    pub enum MediaRoutePhase {
        Prepare = "prepare",
        Active = "active",
    }
}

wire_enum! {
    /// `errorCodeSchema`.
    pub enum ErrorCode {
        AuthRequired = "AUTH_REQUIRED",
        InvalidMessage = "INVALID_MESSAGE",
        InvalidToken = "INVALID_TOKEN",
        RoomAccessDenied = "ROOM_ACCESS_DENIED",
        RoomNotFound = "ROOM_NOT_FOUND",
        RoomFull = "ROOM_FULL",
        HostAlreadyConnected = "HOST_ALREADY_CONNECTED",
        PeerNotFound = "PEER_NOT_FOUND",
        Forbidden = "FORBIDDEN",
        ServerError = "SERVER_ERROR",
    }
}

pub(crate) fn valid_revision(revision: Int) -> bool {
    in_range(revision, 0, MAX_MEDIA_ROUTE_REVISION)
}

pub(crate) static VIDEO_MIME_TYPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^video/[A-Za-z0-9.+-]{1,32}$").expect("constant pattern compiles")
});

// ---------------------------------------------------------------------------
// mediaRouteUpstreamSchema
// ---------------------------------------------------------------------------

/// `mediaRouteUpstreamSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum MediaRouteUpstream {
    /// `{ kind: "none" }`.
    #[default]
    None,
    /// `{ kind: "peer", peerId }`.
    Peer { peer_id: String },
    /// `{ kind: "sfu" }`.
    Sfu,
}

impl MediaRouteUpstream {
    /// Builds `{ kind: "peer", peerId }`.
    pub fn peer(peer_id: impl Into<String>) -> Self {
        Self::Peer {
            peer_id: peer_id.into(),
        }
    }

    /// The `kind` literal.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Peer { .. } => "peer",
            Self::Sfu => "sfu",
        }
    }
}

impl FromWire for MediaRouteUpstream {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["kind", "peerId"])?;
        let kind = f.string("kind")?;
        let peer_id = f.string("peerId")?;
        f.require(&["kind"])?;
        match kind.as_str() {
            "none" | "sfu" => {
                if f.has("peerId") {
                    return Err(invalid(format!("upstream {kind:?} must not carry peerId")));
                }
                Ok(if kind == "none" {
                    Self::None
                } else {
                    Self::Sfu
                })
            }
            "peer" => {
                f.require(&["peerId"])?;
                if !valid_opaque_id(&peer_id) {
                    return Err(invalid("upstream peerId is not an opaque id"));
                }
                Ok(Self::Peer { peer_id })
            }
            _ => Err(invalid(format!("unknown upstream kind {kind:?}"))),
        }
    }
}

impl ToWire for MediaRouteUpstream {
    fn to_wire(&self) -> Value {
        let out = Obj::new().put("kind", self.kind());
        match self {
            Self::Peer { peer_id } => out.put("peerId", peer_id),
            _ => out,
        }
        .build()
    }
}

// ---------------------------------------------------------------------------
// qualitySettingsSchema / routePolicySchema / runtimeCapabilitiesSchema
// ---------------------------------------------------------------------------

/// A requested ceiling, not encoder availability or an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QualityResolution {
    pub width: Int,
    pub height: Int,
}

impl FromWire for QualityResolution {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["width", "height"])?;
        let resolution = Self {
            width: f.int("width")?,
            height: f.int("height")?,
        };
        f.require(&["width", "height"])?;
        if !in_range(resolution.width, 1, 0xffff_ffff)
            || !in_range(resolution.height, 1, 0xffff_ffff)
        {
            return Err(invalid("resolution is out of range"));
        }
        Ok(resolution)
    }
}

impl ToWire for QualityResolution {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("width", &self.width)
            .put("height", &self.height)
            .build()
    }
}

/// `qualitySettingsSchema`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct QualitySettings {
    pub resolution: QualityResolution,
    pub max_framerate: Num,
    pub max_bitrate: Int,
    pub degradation_preference: DegradationPreference,
    /// Omitted when absent or zero.
    pub max_audio_bitrate: Option<Int>,
}

impl FromWire for QualitySettings {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "resolution",
                "maxFramerate",
                "maxBitrate",
                "degradationPreference",
                "maxAudioBitrate",
            ],
        )?;
        f.require(&[
            "resolution",
            "maxFramerate",
            "maxBitrate",
            "degradationPreference",
        ])?;
        f.optional(&["maxAudioBitrate"])?;
        let settings = Self {
            resolution: f.value("resolution")?,
            max_framerate: f.or_default("maxFramerate")?,
            max_bitrate: f.int("maxBitrate")?,
            degradation_preference: f.value("degradationPreference")?,
            max_audio_bitrate: f.opt("maxAudioBitrate")?,
        };
        if settings.max_framerate <= 0.0
            || !in_range_num(settings.max_framerate, 0.0, MAX_SAFE_INTEGER as Num)
        {
            return Err(invalid("maxFramerate is out of range"));
        }
        if !in_range(settings.max_bitrate, 1, 0xffff_ffff) {
            return Err(invalid("maxBitrate is out of range"));
        }
        if settings
            .max_audio_bitrate
            .is_some_and(|bitrate| !in_range(bitrate, MIN_AUDIO_BITRATE, MAX_AUDIO_BITRATE))
        {
            return Err(invalid("maxAudioBitrate is out of range"));
        }
        Ok(settings)
    }
}

impl ToWire for QualitySettings {
    fn to_wire(&self) -> Value {
        let audio = self.max_audio_bitrate.filter(|bitrate| *bitrate != 0);
        Obj::new()
            .put("resolution", &self.resolution)
            .put("maxFramerate", &self.max_framerate)
            .put("maxBitrate", &self.max_bitrate)
            .put("degradationPreference", &self.degradation_preference)
            .put_some("maxAudioBitrate", &audio)
            .build()
    }
}

/// `routePolicySchema`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoutePolicy {
    pub peer_only: bool,
    pub topology_optimization: bool,
    pub nat_prediction: bool,
}

impl FromWire for RoutePolicy {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &["peerOnly", "topologyOptimization", "natPrediction"],
        )?;
        let policy = Self {
            peer_only: f.boolean("peerOnly")?,
            topology_optimization: f.boolean("topologyOptimization")?,
            nat_prediction: f.boolean("natPrediction")?,
        };
        f.require(&["peerOnly", "topologyOptimization", "natPrediction"])?;
        Ok(policy)
    }
}

impl ToWire for RoutePolicy {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("peerOnly", &self.peer_only)
            .put("topologyOptimization", &self.topology_optimization)
            .put("natPrediction", &self.nat_prediction)
            .build()
    }
}

/// `runtimeCapabilitiesSchema`: deployment facts only.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeCapabilities {
    pub sfu: bool,
    pub sfu_only: bool,
    pub nat_prediction: bool,
}

impl RuntimeCapabilities {
    fn flags(&mut self) -> [(&'static str, &mut bool); 3] {
        [
            ("sfu", &mut self.sfu),
            ("sfuOnly", &mut self.sfu_only),
            ("natPrediction", &mut self.nat_prediction),
        ]
    }
}

impl FromWire for RuntimeCapabilities {
    fn from_wire(value: &Value) -> Result<Self> {
        if value.is_null() {
            return Err(invalid("runtime capabilities must be an object"));
        }
        let f = object(value, &["sfu", "sfuOnly", "natPrediction"])?;
        let mut capabilities = Self::default();
        for (key, target) in capabilities.flags() {
            f.optional(&[key])?;
            if let Some(value) = f.get(key) {
                *target = bool::from_wire(value)?;
            }
        }
        Ok(capabilities)
    }
}

impl ToWire for RuntimeCapabilities {
    /// Wire key order: `sfu` and `natPrediction` are always emitted, `sfuOnly`
    /// only when true.
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("sfu", &self.sfu)
            .put_if(self.sfu_only, "sfuOnly", &true)
            .put("natPrediction", &self.nat_prediction)
            .build()
    }
}

// ---------------------------------------------------------------------------
// iceConfigSchema
// ---------------------------------------------------------------------------

/// `stunUrlSchema`: 1..512 UTF-16 code units plus `isValidStunUrl`.
pub fn valid_stun_url_value(value: &str) -> bool {
    (1..=MAX_STUN_URL_LENGTH).contains(&utf16_length(value)) && valid_stun_url(value)
}

/// `z.union([stunUrlSchema, z.array(stunUrlSchema)])`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IceServerUrls {
    Single(String),
    List(Vec<String>),
}

impl FromWire for IceServerUrls {
    fn from_wire(value: &Value) -> Result<Self> {
        if value.is_array() {
            let list = Vec::<String>::from_wire(value)?;
            if list.is_empty() || list.len() > MAX_ICE_SERVER_URLS {
                return Err(invalid("iceServers[].urls length is out of range"));
            }
            if !list.iter().all(|url| valid_stun_url_value(url)) {
                return Err(invalid("Invalid STUN URL"));
            }
            return Ok(Self::List(list));
        }
        let single = String::from_wire(value)?;
        if !valid_stun_url_value(&single) {
            return Err(invalid("Invalid STUN URL"));
        }
        Ok(Self::Single(single))
    }
}

impl ToWire for IceServerUrls {
    fn to_wire(&self) -> Value {
        match self {
            Self::Single(url) => url.to_wire(),
            Self::List(urls) => urls.to_wire(),
        }
    }
}

/// `iceServerSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceServer {
    pub urls: IceServerUrls,
}

impl FromWire for IceServer {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["urls"])?;
        let urls = f.opt("urls")?;
        f.require(&["urls"])?;
        urls.map(|urls| Self { urls })
            .ok_or_else(|| invalid("urls is required"))
    }
}

impl ToWire for IceServer {
    fn to_wire(&self) -> Value {
        Obj::new().put("urls", &self.urls).build()
    }
}

/// `iceConfigSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IceConfig {
    pub ice_servers: Vec<IceServer>,
    pub nat_prediction_stun_urls: Vec<String>,
}

impl FromWire for IceConfig {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["iceServers", "natPredictionStunUrls"])?;
        let config = Self {
            ice_servers: f.or_default("iceServers")?,
            nat_prediction_stun_urls: f.or_default("natPredictionStunUrls")?,
        };
        f.require(&["iceServers", "natPredictionStunUrls"])?;
        if config.ice_servers.len() > 8 {
            return Err(invalid("iceServers holds too many entries"));
        }
        if config.nat_prediction_stun_urls.len() > MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS {
            return Err(invalid("natPredictionStunUrls holds too many entries"));
        }
        if !config
            .nat_prediction_stun_urls
            .iter()
            .all(|url| valid_stun_url_value(url))
        {
            return Err(invalid("Invalid STUN URL"));
        }
        Ok(config)
    }
}

impl ToWire for IceConfig {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("iceServers", &self.ice_servers)
            .put("natPredictionStunUrls", &self.nat_prediction_stun_urls)
            .build()
    }
}
