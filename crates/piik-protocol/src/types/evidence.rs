//! `viewerQualityEvidenceMetricsSchema` / `senderQualityDiagnosticsSchema`.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use crate::codec::{
    FromWire, Int, Nullable, Num, Obj, Result, ToWire, in_range, in_range_num, invalid, object,
    wire_enum,
};
use crate::constants::{MAX_SAFE_INTEGER, NatTraversalPath};
use crate::helpers::is_canonical_video_codec_evidence;
use crate::scalars::{utf16_length, valid_opaque_id};

use super::{VIDEO_MIME_TYPE_PATTERN, valid_revision};

const SAFE: Num = MAX_SAFE_INTEGER as Num;

static CODEC_PROFILE_PATTERN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9-]+=[a-z0-9]+$").expect("constant pattern compiles"));
static CODEC_PARAMETERS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[a-z0-9-]+=[a-z0-9]+(?:; [a-z0-9-]+=[a-z0-9]+)*$")
        .expect("constant pattern compiles")
});
static AUDIO_MIME_TYPE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^audio/[A-Za-z0-9.+-]{1,32}$").expect("constant pattern compiles")
});

wire_enum! {
    /// `senderQualityStateSchema`.
    pub enum SenderQualityState {
        Unknown = "unknown",
        Healthy = "healthy",
        Degraded = "degraded",
    }
}

wire_enum! {
    /// `senderQualityDegradedReasonSchema`.
    pub enum SenderQualityReason {
        None = "none",
        Bandwidth = "bandwidth",
        Cpu = "cpu",
    }
}

/// `viewerQualityEvidenceGuardSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerQualityEvidenceGuard {
    pub connection_id: String,
    pub route_revision: Int,
    pub presentation_epoch: Int,
}

impl FromWire for ViewerQualityEvidenceGuard {
    fn from_wire(value: &Value) -> Result<Self> {
        let keys = ["connectionId", "routeRevision", "presentationEpoch"];
        let f = object(value, &keys)?;
        let guard = Self {
            connection_id: f.string("connectionId")?,
            route_revision: f.int("routeRevision")?,
            presentation_epoch: f.int("presentationEpoch")?,
        };
        f.require(&keys)?;
        if !valid_opaque_id(&guard.connection_id) {
            return Err(invalid("guard connectionId is not an opaque id"));
        }
        if !valid_revision(guard.route_revision) {
            return Err(invalid("guard routeRevision is out of range"));
        }
        if !in_range(guard.presentation_epoch, 0, MAX_SAFE_INTEGER) {
            return Err(invalid("guard presentationEpoch is out of range"));
        }
        Ok(guard)
    }
}

impl ToWire for ViewerQualityEvidenceGuard {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("connectionId", &self.connection_id)
            .put("routeRevision", &self.route_revision)
            .put("presentationEpoch", &self.presentation_epoch)
            .build()
    }
}

/// Declares the metrics struct once so the decoder, encoder and key list
/// cannot drift apart. Every key is required and nullable.
macro_rules! metrics {
    ($($field:ident: $ty:ty = $key:literal),+ $(,)?) => {
        /// `viewerQualityEvidenceMetricsSchema`. Every member except
        /// `nat_traversal_path` is nullable and always emitted.
        #[derive(Debug, Clone, PartialEq)]
        pub struct ViewerQualityEvidenceMetrics {
            pub nat_traversal_path: NatTraversalPath,
            $(pub $field: Option<$ty>,)+
        }

        const METRIC_KEYS: &[&str] = &["natTraversalPath", $($key),+];

        impl ViewerQualityEvidenceMetrics {
            /// Metrics with every nullable member null.
            pub fn empty(nat_traversal_path: NatTraversalPath) -> Self {
                Self { nat_traversal_path, $($field: None,)+ }
            }

            fn read(f: &crate::codec::Fields<'_>) -> Result<Self> {
                Ok(Self {
                    nat_traversal_path: f.value("natTraversalPath")?,
                    $($field: f.opt($key)?,)+
                })
            }
        }

        impl ToWire for ViewerQualityEvidenceMetrics {
            fn to_wire(&self) -> Value {
                Obj::new()
                    .put("natTraversalPath", &self.nat_traversal_path)
                    $(.put($key, &self.$field))+
                    .build()
            }
        }
    };
}

metrics! {
    width: Int = "width",
    height: Int = "height",
    frames_per_second: Num = "framesPerSecond",
    bitrate_kbps: Num = "bitrateKbps",
    packets_received_delta: Int = "packetsReceivedDelta",
    packets_lost_delta: Int = "packetsLostDelta",
    rtt_ms: Num = "rttMs",
    jitter_ms: Num = "jitterMs",
    frames_decoded_delta: Int = "framesDecodedDelta",
    frames_dropped_delta: Int = "framesDroppedDelta",
    decode_ms_per_frame: Num = "decodeMsPerFrame",
    freeze_count_delta: Int = "freezeCountDelta",
    freeze_duration_ms_delta: Num = "freezeDurationMsDelta",
    pause_count_delta: Int = "pauseCountDelta",
    pause_duration_ms_delta: Num = "pauseDurationMsDelta",
    codec: String = "codec",
    codec_profile: String = "codecProfile",
    codec_parameters: String = "codecParameters",
    audio_bitrate_kbps: Num = "audioBitrateKbps",
    audio_packet_loss_percent: Num = "audioPacketLossPercent",
    audio_jitter_ms: Num = "audioJitterMs",
    audio_video_playout_delta_ms: Num = "audioVideoPlayoutDeltaMs",
    video_jitter_buffer_delay_ms: Num = "videoJitterBufferDelayMs",
    audio_jitter_buffer_delay_ms: Num = "audioJitterBufferDelayMs",
    audio_concealed_samples_percent: Num = "audioConcealedSamplesPercent",
    audio_concealment_events_delta: Int = "audioConcealmentEventsDelta",
    audio_codec: String = "audioCodec",
}

fn bounded_text(value: &Option<String>, maximum: usize, pattern: &Regex) -> bool {
    value
        .as_deref()
        .is_none_or(|text| utf16_length(text) <= maximum && pattern.is_match(text))
}

impl FromWire for ViewerQualityEvidenceMetrics {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, METRIC_KEYS)?;
        f.require_nullable(METRIC_KEYS)?;
        f.require(&["natTraversalPath"])?;
        let m = Self::read(&f)?;
        let integers = [
            (m.width, 1),
            (m.height, 1),
            (m.packets_received_delta, 0),
            (m.packets_lost_delta, 0),
            (m.frames_decoded_delta, 0),
            (m.frames_dropped_delta, 0),
            (m.freeze_count_delta, 0),
            (m.pause_count_delta, 0),
            (m.audio_concealment_events_delta, 0),
        ];
        if integers
            .iter()
            .any(|(value, minimum)| value.is_some_and(|v| !in_range(v, *minimum, MAX_SAFE_INTEGER)))
        {
            return Err(invalid("viewer quality evidence integer is out of range"));
        }
        let numbers = [
            (m.frames_per_second, 0.0, SAFE),
            (m.bitrate_kbps, 0.0, SAFE),
            (m.rtt_ms, 0.0, SAFE),
            (m.jitter_ms, 0.0, SAFE),
            (m.decode_ms_per_frame, 0.0, SAFE),
            (m.freeze_duration_ms_delta, 0.0, SAFE),
            (m.pause_duration_ms_delta, 0.0, SAFE),
            (m.audio_bitrate_kbps, 0.0, SAFE),
            (m.audio_packet_loss_percent, 0.0, 100.0),
            (m.audio_jitter_ms, 0.0, SAFE),
            (m.audio_video_playout_delta_ms, -SAFE, SAFE),
            (m.video_jitter_buffer_delay_ms, 0.0, SAFE),
            (m.audio_jitter_buffer_delay_ms, 0.0, SAFE),
            (m.audio_concealed_samples_percent, 0.0, 100.0),
        ];
        if numbers.iter().any(|(value, minimum, maximum)| {
            value.is_some_and(|v| !in_range_num(v, *minimum, *maximum))
        }) {
            return Err(invalid("viewer quality evidence number is out of range"));
        }
        if !bounded_text(&m.codec, 64, &VIDEO_MIME_TYPE_PATTERN) {
            return Err(invalid("codec is not a video mime type"));
        }
        if !bounded_text(&m.codec_profile, 64, &CODEC_PROFILE_PATTERN) {
            return Err(invalid("codecProfile is malformed"));
        }
        if !bounded_text(&m.codec_parameters, 128, &CODEC_PARAMETERS_PATTERN) {
            return Err(invalid("codecParameters is malformed"));
        }
        if !bounded_text(&m.audio_codec, 64, &AUDIO_MIME_TYPE_PATTERN) {
            return Err(invalid("audioCodec is not an audio mime type"));
        }
        // refine: dimensions travel together.
        if m.width.is_none() != m.height.is_none() {
            return Err(invalid(
                "Viewer quality dimensions must be present together",
            ));
        }
        // refine: codec evidence must be canonical.
        if !is_canonical_video_codec_evidence(
            m.codec.as_deref(),
            m.codec_profile.as_deref(),
            m.codec_parameters.as_deref(),
        ) {
            return Err(invalid("Viewer codec evidence is not canonical"));
        }
        Ok(m)
    }
}

/// `senderQualityDiagnosticsSchema`.
#[derive(Debug, Clone, PartialEq)]
pub struct SenderQualityDiagnostics {
    pub nat_traversal_path: NatTraversalPath,
    pub reason: Option<SenderQualityReason>,
    pub frames_per_second: Option<Num>,
    pub bitrate_kbps: Option<Num>,
    pub capture_frames_per_second: Nullable<Num>,
    pub media_source_frames_per_second: Nullable<Num>,
    pub width: Nullable<Int>,
    pub height: Nullable<Int>,
    pub video_encoding_count: Nullable<Int>,
    pub active_video_encoding_count: Nullable<Int>,
    pub available_outgoing_kbps: Nullable<Num>,
    pub rtt_ms: Nullable<Num>,
    pub packet_loss_percent: Nullable<Num>,
}

impl FromWire for SenderQualityDiagnostics {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "natTraversalPath",
                "reason",
                "framesPerSecond",
                "bitrateKbps",
                "captureFramesPerSecond",
                "mediaSourceFramesPerSecond",
                "width",
                "height",
                "videoEncodingCount",
                "activeVideoEncodingCount",
                "availableOutgoingKbps",
                "rttMs",
                "packetLossPercent",
            ],
        )?;
        f.require(&["natTraversalPath"])?;
        f.require_nullable(&["reason", "framesPerSecond", "bitrateKbps"])?;
        let d = Self {
            nat_traversal_path: f.value("natTraversalPath")?,
            reason: f.opt("reason")?,
            frames_per_second: f.opt("framesPerSecond")?,
            bitrate_kbps: f.opt("bitrateKbps")?,
            capture_frames_per_second: f.nullable("captureFramesPerSecond")?,
            media_source_frames_per_second: f.nullable("mediaSourceFramesPerSecond")?,
            width: f.nullable("width")?,
            height: f.nullable("height")?,
            video_encoding_count: f.nullable("videoEncodingCount")?,
            active_video_encoding_count: f.nullable("activeVideoEncodingCount")?,
            available_outgoing_kbps: f.nullable("availableOutgoingKbps")?,
            rtt_ms: f.nullable("rttMs")?,
            packet_loss_percent: f.nullable("packetLossPercent")?,
        };
        let numbers = [
            (d.frames_per_second, SAFE),
            (d.bitrate_kbps, SAFE),
            (d.capture_frames_per_second.value().copied(), SAFE),
            (d.media_source_frames_per_second.value().copied(), SAFE),
            (d.available_outgoing_kbps.value().copied(), SAFE),
            (d.rtt_ms.value().copied(), SAFE),
            (d.packet_loss_percent.value().copied(), 100.0),
        ];
        if numbers
            .iter()
            .any(|(value, maximum)| value.is_some_and(|v| !in_range_num(v, 0.0, *maximum)))
        {
            return Err(invalid("sender quality diagnostic number is out of range"));
        }
        let integers = [
            &d.width,
            &d.height,
            &d.video_encoding_count,
            &d.active_video_encoding_count,
        ];
        if integers.iter().any(|value| {
            value
                .value()
                .is_some_and(|v| !in_range(*v, 0, MAX_SAFE_INTEGER))
        }) {
            return Err(invalid("sender quality diagnostic integer is out of range"));
        }
        Ok(d)
    }
}

impl ToWire for SenderQualityDiagnostics {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("natTraversalPath", &self.nat_traversal_path)
            .put("reason", &self.reason)
            .put("framesPerSecond", &self.frames_per_second)
            .put("bitrateKbps", &self.bitrate_kbps)
            .put_nullable("captureFramesPerSecond", &self.capture_frames_per_second)
            .put_nullable(
                "mediaSourceFramesPerSecond",
                &self.media_source_frames_per_second,
            )
            .put_nullable("width", &self.width)
            .put_nullable("height", &self.height)
            .put_nullable("videoEncodingCount", &self.video_encoding_count)
            .put_nullable(
                "activeVideoEncodingCount",
                &self.active_video_encoding_count,
            )
            .put_nullable("availableOutgoingKbps", &self.available_outgoing_kbps)
            .put_nullable("rttMs", &self.rtt_ms)
            .put_nullable("packetLossPercent", &self.packet_loss_percent)
            .build()
    }
}
