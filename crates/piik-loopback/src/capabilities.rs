//! The App runtime facts published by `/health`.

use piik_protocol::QualityResolution;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Native media facts. Optional facts are omitted when zero or false. Facts
/// that every App of this build supports are always present, so their absence
/// is never read as a different build.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NativeMediaCapabilities {
    #[serde(
        default,
        skip_serializing_if = "zero_resolution",
        serialize_with = "serialize_resolution",
        deserialize_with = "deserialize_resolution"
    )]
    pub max_video_resolution: Option<QualityResolution>,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_video_pixels: i64,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_video_macroblocks_per_second: i64,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_video_framerate: i64,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_video_bitrate: i64,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_audio_bitrate: i64,
    /// Receiver reuse is part of this build, not a per-App option.
    pub receiver_reuse: bool,
    #[serde(default, skip_serializing_if = "zero")]
    pub max_relay_children: i64,
    #[serde(default, skip_serializing_if = "not")]
    pub remote_input: bool,
    #[serde(default, skip_serializing_if = "not")]
    pub remote_stop_shortcut: bool,
    /// Co-play is part of this build; false only when remote input is absent.
    pub remote_co_play: bool,
    #[serde(default)]
    pub video: bool,
    /// Whether the probed capture helper can control the Windows border.
    pub capture_border_control: bool,
    #[serde(default, skip_serializing_if = "not")]
    pub microphone: bool,
    /// Source cancellation, capture failure details and share volume are
    /// control features of this build, independent of the probed helper.
    pub source_cancellation: bool,
    pub capture_failure_details: bool,
    pub share_volume: bool,
    #[serde(default)]
    pub process_audio: bool,
    #[serde(default, skip_serializing_if = "not")]
    pub process_audio_exclusion: bool,
    #[serde(default)]
    pub system_audio: bool,
    #[serde(rename = "hardwareH264", default)]
    pub hardware_h264: bool,
    #[serde(rename = "softwareVP8", default)]
    pub software_vp8: bool,
}

/// The `/health` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    pub version: String,
    pub revision: String,
    pub package_target: String,
    pub protocol: i64,
    pub service: String,
    pub port: u16,
    pub instance_token: String,
    pub native_media: NativeMediaCapabilities,
}

fn zero(value: &i64) -> bool {
    *value == 0
}

fn not(value: &bool) -> bool {
    !*value
}

fn zero_resolution(value: &Option<QualityResolution>) -> bool {
    value.is_none_or(|resolution| resolution.width == 0 && resolution.height == 0)
}

#[derive(Serialize, Deserialize)]
struct Resolution {
    width: i64,
    height: i64,
}

fn serialize_resolution<S: Serializer>(
    value: &Option<QualityResolution>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    let resolution = value.unwrap_or(QualityResolution {
        width: 0,
        height: 0,
    });
    Resolution {
        width: resolution.width,
        height: resolution.height,
    }
    .serialize(serializer)
}

fn deserialize_resolution<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<QualityResolution>, D::Error> {
    let resolution = Resolution::deserialize(deserializer)?;
    Ok(Some(QualityResolution {
        width: resolution.width,
        height: resolution.height,
    }))
}
