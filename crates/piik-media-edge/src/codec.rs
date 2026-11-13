//! The Engine codec table: native video codecs and the Opus capability.

use std::time::Duration;

use piik_media_forwarding::{Codec, CodecKind, LayerFormat, SourceOptions};
use piik_media_transport::sdp::{H264_PT, OPUS_FMTP, OPUS_PT, VP8_PT, VideoCodec, codec_config};
use piik_media_transport::str0m::RtcConfig;
use piik_media_transport::str0m::bwe::Bitrate;

pub use piik_media_transport::sdp::VideoCodec as NativeCodec;

/// Constrained Baseline in the browser-standard spelling.
pub const H264_PROFILE_LEVEL_ID: &str = "42e034";
pub const VIDEO_CLOCK_RATE: u32 = 90_000;
/// Initial bitrate when the Engine options give none.
pub const DEFAULT_INITIAL_BITRATE: i64 = 3_000_000;
/// Every connection reports stats this often; the selected pair and quality
/// samples read them.
pub(crate) const STATS_INTERVAL: Duration = Duration::from_secs(1);

pub(crate) fn video_pt(codec: VideoCodec) -> u8 {
    match codec {
        VideoCodec::H264 => H264_PT,
        VideoCodec::Vp8 => VP8_PT,
    }
}

pub(crate) fn kind(codec: VideoCodec) -> CodecKind {
    match codec {
        VideoCodec::H264 => CodecKind::H264,
        VideoCodec::Vp8 => CodecKind::Vp8,
    }
}

/// The forwarding codec entry for one native video codec.
pub(crate) fn forwarding_codec(codec: VideoCodec) -> Codec {
    let fmtp = match codec {
        VideoCodec::H264 => format!(
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={H264_PROFILE_LEVEL_ID}"
        ),
        VideoCodec::Vp8 => String::new(),
    };
    Codec {
        kind: kind(codec),
        payload_type: video_pt(codec),
        fmtp,
        channels: 0,
    }
}

/// The native screen source: track `screen` in stream `piik-native`.
pub(crate) fn source_options(codec: VideoCodec, formats: Vec<LayerFormat>) -> SourceOptions {
    SourceOptions {
        id: "screen".into(),
        stream_id: "piik-native".into(),
        codec: forwarding_codec(codec),
        formats,
        max_packets: piik_encoded::MAX_PACKET_WINDOW,
    }
}

pub(crate) fn opus_options(id: &str) -> SourceOptions {
    SourceOptions {
        id: id.into(),
        stream_id: "piik-native".into(),
        codec: Codec {
            kind: CodecKind::Opus,
            payload_type: OPUS_PT,
            fmtp: OPUS_FMTP.into(),
            channels: 2,
        },
        formats: Vec::new(),
        max_packets: 0,
    }
}

/// One connection's str0m configuration: Piik's codecs, GoogCC from the
/// configured start bitrate and periodic stats.
pub(crate) fn rtc_config(video: &[VideoCodec], audio: bool, initial_bitrate: i64) -> RtcConfig {
    codec_config(video, audio)
        .enable_bwe(Some(Bitrate::bps(initial_bitrate.max(1) as u64)))
        .set_stats_interval(Some(STATS_INTERVAL))
}

pub(crate) fn parse_codec(name: &str) -> Option<VideoCodec> {
    VideoCodec::from_name(name)
}
