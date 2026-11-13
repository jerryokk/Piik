//! Edge and publication quality samples and selected-path provenance.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// One edge quality sample. `state` is `unknown`, `healthy` or `degraded`;
/// `reason` is present exactly when the state is known.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QualitySample {
    pub sample_timestamp_ms: i64,
    pub sample_window_ms: i64,
    pub rtp_stats_id: String,
    pub track_identifier: String,
    pub state: String,
    pub reason: Option<String>,
    pub interval_frames_sent: u64,
    pub frames_per_second: f64,
    pub bitrate_kbps: f64,
    pub available_outgoing_kbps: f64,
    pub width: u32,
    pub height: u32,
}

/// One publication quality sample with its wire field names.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationQualitySample {
    pub sample_timestamp_ms: i64,
    pub sample_window_ms: i64,
    #[serde(rename = "rtpStatsId")]
    pub rtp_stats_id: String,
    pub track_identifier: String,
    pub codec: String,
    pub video_encoding_count: usize,
    pub active_video_encoding_count: usize,
    pub rid: String,
    pub interval_frames_sent: u64,
    pub frames_per_second: f64,
    pub width: u32,
    pub height: u32,
    pub bitrate_kbps: f64,
    pub audio_bitrate_kbps: f64,
    pub available_outgoing_kbps: Option<f64>,
    pub state: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationLayer {
    pub rid: String,
    pub width: u32,
    pub height: u32,
    pub bitrate: u32,
}

/// The descriptor of one SFU publication.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationMedia {
    pub codec: String,
    pub layers: Vec<PublicationLayer>,
    pub audio: bool,
    pub audio_bitrate: u32,
}

/// Pion `ICECandidateType` wire strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateType {
    Host,
    Srflx,
    Prflx,
    Relay,
}

impl CandidateType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Host => "host",
            Self::Srflx => "srflx",
            Self::Prflx => "prflx",
            Self::Relay => "relay",
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "host" => Self::Host,
            "srflx" => Self::Srflx,
            "prflx" => Self::Prflx,
            "relay" => Self::Relay,
            _ => return None,
        })
    }
}

/// The selected ICE candidate pair. The path names only anonymous provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedPair {
    pub local: CandidateType,
    pub remote: CandidateType,
    /// `predicted`, `ordinary` or `unknown`.
    pub nat_traversal_path: String,
}

/// A punched (`sp`) or mapped (`sm`) foundation with a numeric suffix is
/// predicted.
pub fn selected_nat_traversal_path(foundations: &[&str]) -> String {
    let predicted = foundations.iter().any(|foundation| {
        let bytes = foundation.as_bytes();
        bytes.len() >= 3
            && bytes[0] == b's'
            && (bytes[1] == b'p' || bytes[1] == b'm')
            && foundation[2..].parse::<i64>().is_ok()
    });
    if predicted {
        "predicted"
    } else if foundations.iter().any(|foundation| !foundation.is_empty()) {
        "ordinary"
    } else {
        "unknown"
    }
    .to_owned()
}

/// Counters between two edge samples.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Baseline {
    pub at: Option<SystemTime>,
    pub frames: u64,
    pub video_bytes: u64,
    pub audio_bytes: u64,
}

pub(crate) fn unix_millis(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_millis() as i64)
}

/// The accepted sample window: one to five seconds.
pub(crate) fn window(previous: Option<SystemTime>, now: SystemTime) -> Option<Duration> {
    let window = now.duration_since(previous?).ok()?;
    (Duration::from_secs(1)..=Duration::from_secs(5))
        .contains(&window)
        .then_some(window)
}
