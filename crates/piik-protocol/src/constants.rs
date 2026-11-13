//! Limits and identifiers in the Piik wire contract.

use crate::codec::{Int, wire_enum};
use crate::types::{DegradationPreference, QualityResolution, QualitySettings, RoutePolicy};

/// JavaScript's `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE_INTEGER: Int = 9_007_199_254_740_991;

/// Opus format bounds, independent of UI presets and sender capabilities.
pub const MIN_AUDIO_BITRATE: Int = 6_000;
/// Upper Opus format bound.
pub const MAX_AUDIO_BITRATE: Int = 510_000;

pub const MAX_VIEWERS_PER_ROOM_LIMIT: usize = 20;
pub const MAX_PARTICIPANTS_PER_ROOM_LIMIT: usize = MAX_VIEWERS_PER_ROOM_LIMIT + 1;
pub const MAX_SIGNAL_BYTES: usize = 64 * 1024;
pub const SIGNALING_PROTOCOL: &str = "piik-v24";
pub const ROOM_CODE_LENGTH: usize = 4;
pub const MAX_MEDIA_ROUTE_REVISION: Int = MAX_SAFE_INTEGER;
pub const MAX_ICE_SERVER_URLS: usize = 8;

pub const MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS: usize = 2;

pub const MAX_VIEWER_QUALITY_EVIDENCE_BYTES: usize = 2 * 1024;
pub const VIEWER_QUALITY_EVIDENCE_INTERVAL_MS: Int = 2_000;
pub const VIEWER_QUALITY_EVIDENCE_EXPIRY_MS: Int = 5_000;
pub const PERSISTENT_NATIVE_EDGE_DEGRADED_WINDOWS: Int = 3;

pub const MAX_DISPLAY_NAME_CODE_POINTS: usize = 24;
pub const DEFAULT_VIEWER_DISPLAY_NAME: &str = "观众";
pub const DEFAULT_HOST_DISPLAY_NAME_PREFIX: &str = "房主";

pub const MIN_VIEWER_PASSWORD_LENGTH: usize = 1;
pub const MAX_VIEWER_PASSWORD_LENGTH: usize = 64;

/// `stunUrlSchema`'s `.max(512)`, in UTF-16 code units.
pub const MAX_STUN_URL_LENGTH: usize = 512;

/// Endpoint media copy accounting bounds, from
/// `src/shared/media-copy-accounting.ts`.
pub const DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY: Int = 2;
pub const MAX_ENDPOINT_MEDIA_COPY_CAPACITY: Int = 3;

/// `SIGNAL_CLOSE_CODES`: WebSocket close codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum SignalCloseCode {
    ServiceRestart = 1012,
    SessionReplaced = 4001,
    ClientReconnect = 4002,
    AuthenticationFailed = 4003,
    ViewerAccessRevoked = 4004,
}

impl SignalCloseCode {
    /// The numeric close code.
    pub const fn code(self) -> u16 {
        self as u16
    }
}

wire_enum! {
    /// `NAT_TRAVERSAL_PATHS` in `src/shared/nat-candidate.ts`.
    pub enum NatTraversalPath {
        Unknown = "unknown",
        Ordinary = "ordinary",
        Predicted = "predicted",
    }
}

/// `DEFAULT_QUALITY_SETTINGS`.
pub const DEFAULT_QUALITY_SETTINGS: QualitySettings = QualitySettings {
    resolution: QualityResolution {
        width: 1920,
        height: 1080,
    },
    max_framerate: 30.0,
    max_bitrate: 5_000_000,
    degradation_preference: DegradationPreference::Balanced,
    max_audio_bitrate: Some(128_000),
};

/// `DEFAULT_ROUTE_POLICY`.
pub const DEFAULT_ROUTE_POLICY: RoutePolicy = RoutePolicy {
    peer_only: false,
    topology_optimization: true,
    nat_prediction: false,
};
