//! SMED envelope: the helper's private stdin/stdout framing (capture protocol v7).
//!
//! Header (32 bytes, big-endian): `SMED`, version 2, kind, flags (bit 0 =
//! recovery), layer, timestamp and duration in 100 ns units, width, height and
//! payload size. `native/capture/windows/capture_output.h` writes the output
//! side and `capture_input.h` parses the input side.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::Error;

pub(crate) const ENVELOPE_HEADER_BYTES: usize = 32;
pub(crate) const MAX_MEDIA_BYTES: usize = piik_encoded::MAX_ACCESS_UNIT_BYTES;
const MAX_STATUS_BYTES: usize = 4 * 1024;
const MAX_CONTROL_BYTES: usize = 64;
const ENVELOPE_MAGIC: &[u8; 4] = b"SMED";
const ENVELOPE_VERSION: u8 = 2;
/// Keeps every decoded time representable as signed 64-bit nanoseconds.
const MAX_TIME_100NS: u64 = i64::MAX as u64 / 100;

/// Output slots in one capture process, including derived encoder groups.
pub(crate) const MAX_OUTPUTS: usize = 6;
pub const MAX_VIDEO_DIMENSION: u32 = 8192;
pub const MAX_VIDEO_PIXELS: u64 = 36_864 * 16 * 16;
pub const MAX_VIDEO_MACROBLOCKS_PER_SECOND: u64 = 2_073_600;
/// Bounds capture scheduling independently of codec throughput.
pub const MAX_VIDEO_FRAMERATE: u32 = 240;
/// Coded pictures may include alignment outside the visible crop. Kept
/// separate from visible dimensions and within H.264 Level 5.2 storage.
pub const MAX_CODED_VIDEO_DIMENSION: u64 = 8192;
pub const MAX_CODED_VIDEO_PIXELS: u64 = 36_864 * 16 * 16;

fn macroblocks(width: u32, height: u32) -> u64 {
    u64::from(width.div_ceil(16)) * u64::from(height.div_ceil(16))
}

fn valid_dimension(value: u32) -> bool {
    (2..=MAX_VIDEO_DIMENSION).contains(&value) && value.is_multiple_of(2)
}

/// Bounds the decoded picture in the current capture contract.
pub fn valid_video_size(width: u32, height: u32) -> bool {
    valid_dimension(width)
        && valid_dimension(height)
        && macroblocks(width, height) * 256 <= MAX_VIDEO_PIXELS
}

/// Keeps both current encoders within the shared receive ceiling (H.264
/// Level 5.2), including non-preset shapes and frame rates.
pub fn valid_video_format(width: u32, height: u32, framerate: u32) -> bool {
    valid_video_size(width, height)
        && (1..=MAX_VIDEO_FRAMERATE).contains(&framerate)
        && macroblocks(width, height) * u64::from(framerate) <= MAX_VIDEO_MACROBLOCKS_PER_SECOND
}

pub fn valid_coded_video_size(width: u64, height: u64) -> bool {
    let valid =
        |value: u64| (2..=MAX_CODED_VIDEO_DIMENSION).contains(&value) && value.is_multiple_of(2);
    valid(width) && valid(height) && width * height <= MAX_CODED_VIDEO_PIXELS
}

/// Bounds capture and receive to the same coding tools.
pub use piik_encoded::h264::supported_profile_level_id as supported_h264_profile_level_id;

#[repr(u8)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum FrameKind {
    #[default]
    Pcm = 1,
    H264 = 2,
    Status = 3,
    Vp8 = 4,
    Begin = 5,
    LayerUnavailable = 6,
    Control = 7,
}

impl FrameKind {
    fn from_wire(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Pcm,
            2 => Self::H264,
            3 => Self::Status,
            4 => Self::Vp8,
            5 => Self::Begin,
            6 => Self::LayerUnavailable,
            7 => Self::Control,
            _ => return None,
        })
    }

    pub fn is_video(self) -> bool {
        matches!(self, Self::H264 | Self::Vp8)
    }
}

/// One SMED envelope.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub kind: FrameKind,
    /// Independent recovery, including H.264 SPS/PPS when needed.
    pub key_frame: bool,
    pub layer: u8,
    pub width: u32,
    pub height: u32,
    pub timestamp: Duration,
    pub duration: Duration,
    pub data: Vec<u8>,
}

impl Frame {
    pub(crate) fn control(text: &str) -> Self {
        Self {
            kind: FrameKind::Control,
            data: text.as_bytes().to_vec(),
            ..Self::default()
        }
    }
}

fn big_endian(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .fold(0, |value, byte| value << 8 | u64::from(*byte))
}

fn to_100ns(value: Duration) -> Option<u64> {
    u64::try_from(value.as_nanos() / 100).ok()
}

pub(crate) fn parse_frame_header(
    header: &[u8; ENVELOPE_HEADER_BYTES],
) -> Result<(Frame, usize), Error> {
    let kind = FrameKind::from_wire(header[5]);
    let size = big_endian(&header[28..32]) as usize;
    let maximum = match kind {
        Some(FrameKind::Status | FrameKind::LayerUnavailable) => MAX_STATUS_BYTES,
        Some(FrameKind::Control) => MAX_CONTROL_BYTES,
        _ => MAX_MEDIA_BYTES,
    };
    let Some(kind) = kind.filter(|kind| {
        &header[..4] == ENVELOPE_MAGIC
            && header[4] == ENVELOPE_VERSION
            && size <= maximum
            && (*kind == FrameKind::Begin) == (size == 0)
    }) else {
        return Err(Error::Invalid("native capture frame header is invalid"));
    };
    let timestamp = big_endian(&header[8..16]);
    let duration = big_endian(&header[16..24]);
    let width = big_endian(&header[24..26]) as u32;
    let height = big_endian(&header[26..28]) as u32;
    let (flags, layer) = (header[6], header[7]);
    let video = kind.is_video();
    let timed = video || matches!(kind, FrameKind::Pcm | FrameKind::Begin);
    let outside_slots = usize::from(layer) >= MAX_OUTPUTS;
    if timestamp > MAX_TIME_100NS
        || duration > MAX_TIME_100NS
        || (timed && duration == 0)
        || (!timed && (timestamp != 0 || duration != 0))
        || (video && (flags > 1 || outside_slots || !valid_video_size(width, height)))
        || (!video && (flags != 0 || width != 0 || height != 0))
        || (kind == FrameKind::LayerUnavailable && outside_slots)
        || (!video && kind != FrameKind::LayerUnavailable && layer != 0)
    {
        return Err(Error::Invalid("native capture media frame is invalid"));
    }
    let frame = Frame {
        kind,
        key_frame: video && flags == 1,
        layer,
        width,
        height,
        timestamp: Duration::from_nanos(timestamp * 100),
        duration: Duration::from_nanos(duration * 100),
        data: Vec::new(),
    };
    Ok((frame, size))
}

fn validate_frame_data(frame: &Frame) -> Result<(), Error> {
    if frame.kind == FrameKind::LayerUnavailable && std::str::from_utf8(&frame.data).is_err() {
        return Err(Error::Invalid(
            "native unavailable output diagnostic is invalid",
        ));
    }
    if frame.kind == FrameKind::Control
        && frame.data.iter().any(|value| !(32..=126).contains(value))
    {
        return Err(Error::Invalid("native control payload is not ASCII"));
    }
    Ok(())
}

/// Builds and validates a header with the same rules the reader applies, so an
/// invalid frame never writes a partial envelope.
pub(crate) fn encode_frame_header(frame: &Frame) -> Result<[u8; ENVELOPE_HEADER_BYTES], Error> {
    let outside = Error::Invalid("native capture frame is outside its envelope");
    let (Some(timestamp), Some(duration), Ok(width), Ok(height), Ok(size)) = (
        to_100ns(frame.timestamp),
        to_100ns(frame.duration),
        u16::try_from(frame.width),
        u16::try_from(frame.height),
        u32::try_from(frame.data.len()),
    ) else {
        return Err(outside);
    };
    if frame.data.len() > MAX_MEDIA_BYTES {
        return Err(outside);
    }
    let mut header = [0u8; ENVELOPE_HEADER_BYTES];
    header[..4].copy_from_slice(ENVELOPE_MAGIC);
    header[4] = ENVELOPE_VERSION;
    header[5] = frame.kind as u8;
    header[6] = u8::from(frame.key_frame);
    header[7] = frame.layer;
    header[8..16].copy_from_slice(&timestamp.to_be_bytes());
    header[16..24].copy_from_slice(&duration.to_be_bytes());
    header[24..26].copy_from_slice(&width.to_be_bytes());
    header[26..28].copy_from_slice(&height.to_be_bytes());
    header[28..32].copy_from_slice(&size.to_be_bytes());
    parse_frame_header(&header)?;
    validate_frame_data(frame)?;
    Ok(header)
}

/// Reads one envelope. A clean end before any header byte is [`Error::Eof`].
/// Not cancel-safe: dropping the future mid-frame desynchronizes the stream.
pub async fn read_frame<R: AsyncRead + Unpin + ?Sized>(input: &mut R) -> Result<Frame, Error> {
    let mut header = [0u8; ENVELOPE_HEADER_BYTES];
    let mut filled = 0;
    while filled < header.len() {
        let count = input
            .read(&mut header[filled..])
            .await
            .map_err(|error| Error::io("read native capture frame", error))?;
        if count == 0 {
            return Err(if filled == 0 {
                Error::Eof
            } else {
                Error::io(
                    "read native capture frame",
                    std::io::ErrorKind::UnexpectedEof.into(),
                )
            });
        }
        filled += count;
    }
    // The header bounds the payload before it is allocated.
    let (mut frame, size) = parse_frame_header(&header)?;
    frame.data = vec![0; size];
    input
        .read_exact(&mut frame.data)
        .await
        .map_err(|error| Error::io("read native capture frame", error))?;
    validate_frame_data(&frame)?;
    Ok(frame)
}

pub async fn write_frame<W: AsyncWrite + Unpin + ?Sized>(
    output: &mut W,
    frame: &Frame,
) -> Result<(), Error> {
    let header = encode_frame_header(frame)?;
    let write = |error| Error::io("write native capture frame", error);
    output.write_all(&header).await.map_err(write)?;
    if !frame.data.is_empty() {
        output.write_all(&frame.data).await.map_err(write)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(kind: FrameKind) -> [u8; ENVELOPE_HEADER_BYTES] {
        let mut header = [0u8; ENVELOPE_HEADER_BYTES];
        header[..4].copy_from_slice(b"SMED");
        header[4] = 2;
        header[5] = kind as u8;
        header
    }

    fn put(header: &mut [u8], range: std::ops::Range<usize>, value: u64) {
        let bytes = value.to_be_bytes();
        let width = range.len();
        header[range].copy_from_slice(&bytes[8 - width..]);
    }

    async fn read(bytes: &[u8]) -> Result<Frame, Error> {
        let mut input = bytes;
        read_frame(&mut input).await
    }

    async fn encode(frame: &Frame) -> Result<Vec<u8>, Error> {
        let mut output = Vec::new();
        write_frame(&mut output, frame).await.map(|()| output)
    }

    #[tokio::test]
    async fn read_frame_uses_the_bounded_versioned_envelope() {
        let payload = [0, 0, 0, 1, 0x65];
        let mut header = header(FrameKind::H264);
        header[6] = 1;
        header[7] = 1;
        put(&mut header, 8..16, 123);
        put(&mut header, 16..24, 333_333);
        put(&mut header, 24..26, 1920);
        put(&mut header, 26..28, 1080);
        put(&mut header, 28..32, payload.len() as u64);
        let bytes = [&header[..], &payload].concat();
        let frame = read(&bytes).await.expect("H.264 frame");
        assert_eq!(
            frame,
            Frame {
                kind: FrameKind::H264,
                key_frame: true,
                layer: 1,
                width: 1920,
                height: 1080,
                timestamp: Duration::from_nanos(12_300),
                duration: Duration::from_nanos(33_333_300),
                data: payload.to_vec(),
            }
        );
        header[5] = FrameKind::Vp8 as u8;
        let frame = read(&[&header[..], &payload].concat())
            .await
            .expect("VP8 frame");
        assert!(frame.kind == FrameKind::Vp8 && frame.key_frame && frame.data == payload);
    }

    #[tokio::test]
    async fn read_frame_rejects_invalid_kinds_flags_and_bounds() {
        let mut base = [0u8; ENVELOPE_HEADER_BYTES + 1];
        base[..ENVELOPE_HEADER_BYTES].copy_from_slice(&header(FrameKind::H264));
        put(&mut base, 16..24, 1);
        put(&mut base, 24..26, 1280);
        put(&mut base, 26..28, 720);
        put(&mut base, 28..32, 1);
        let mutations: [fn(&mut [u8]); 7] = [
            |value| value[4] = 1,
            |value| value[5] = 9,
            |value| value[6] = 2,
            |value| value[7] = MAX_OUTPUTS as u8,
            |value| put(value, 24..26, 1279),
            |value| put(value, 8..16, u64::MAX),
            |value| put(value, 28..32, MAX_MEDIA_BYTES as u64 + 1),
        ];
        for mutate in mutations {
            let mut candidate = base;
            mutate(&mut candidate);
            assert!(
                read(&candidate).await.is_err(),
                "invalid native frame was accepted"
            );
        }
    }

    #[tokio::test]
    async fn media_envelope_accepts_four_mib_and_rejects_larger_frames() {
        const LIMIT: usize = 4 * 1024 * 1024;
        let mut frame = Frame {
            kind: FrameKind::H264,
            key_frame: true,
            width: 2560,
            height: 1440,
            duration: Duration::from_secs(1) / 60,
            data: vec![0; LIMIT],
            ..Frame::default()
        };
        let output = encode(&frame).await.expect("4 MiB envelope");
        let decoded = read(&output).await.expect("4 MiB media envelope rejected");
        assert!(
            decoded.data.len() == LIMIT
                && decoded.width == frame.width
                && decoded.height == frame.height
        );
        frame.data.push(0);
        assert!(
            encode(&frame).await.is_err(),
            "oversized media wrote an envelope"
        );
        let mut header = [0u8; ENVELOPE_HEADER_BYTES];
        header.copy_from_slice(&output[..ENVELOPE_HEADER_BYTES]);
        put(&mut header, 28..32, LIMIT as u64 + 1);
        let error = read(&header).await.expect_err("oversized input accepted");
        assert!(
            matches!(error, Error::Invalid(_)),
            "oversized input was not rejected before payload allocation: {error}"
        );
    }

    #[tokio::test]
    async fn input_begin_and_unavailable_layer_have_distinct_envelopes() {
        let mut begin = header(FrameKind::Begin);
        put(&mut begin, 16..24, 333_333);
        let frame = read(&begin).await.expect("begin");
        assert!(
            frame.kind == FrameKind::Begin && frame.data.is_empty() && !frame.duration.is_zero()
        );
        let mut unavailable = begin;
        unavailable[5] = FrameKind::LayerUnavailable as u8;
        unavailable[7] = 1;
        put(&mut unavailable, 16..24, 0);
        let payload = b"output encoder unavailable";
        put(&mut unavailable, 28..32, payload.len() as u64);
        let frame = read(&[&unavailable[..], payload].concat())
            .await
            .expect("unavailable");
        assert!(
            frame.kind == FrameKind::LayerUnavailable && frame.layer == 1 && frame.data == payload
        );
        put(&mut unavailable, 28..32, 1);
        assert!(
            read(&[&unavailable[..], &[0xff]].concat()).await.is_err(),
            "non-UTF8 output diagnostic was accepted"
        );
        let control = |text: &str| Frame::control(text);
        for invalid in [
            control(&"Q".repeat(65)),
            control("Q\n"),
            Frame {
                layer: 1,
                ..control("Q")
            },
            Frame {
                timestamp: Duration::from_secs(1),
                ..control("Q")
            },
            Frame {
                key_frame: true,
                ..control("Q")
            },
        ] {
            assert!(
                encode(&invalid).await.is_err(),
                "invalid control wrote a partial envelope"
            );
        }
    }

    #[tokio::test]
    async fn independent_output_envelope_uses_the_last_bounded_slot() {
        let frame = Frame {
            kind: FrameKind::Vp8,
            layer: MAX_OUTPUTS as u8 - 1,
            width: 320,
            height: 180,
            timestamp: Duration::from_secs(1),
            duration: Duration::from_secs(1) / 30,
            data: vec![1],
            ..Frame::default()
        };
        let output = encode(&frame).await.expect("last slot");
        let decoded = read(&output).await.expect("last output slot");
        assert_eq!(usize::from(decoded.layer), MAX_OUTPUTS - 1);
    }

    #[tokio::test]
    async fn clean_and_truncated_ends_are_distinct() {
        assert!(matches!(read(&[]).await, Err(Error::Eof)));
        assert!(matches!(read(b"SMED").await, Err(Error::Io { .. })));
    }

    #[test]
    fn h264_profile_levels_follow_the_receive_contract() {
        assert!(supported_h264_profile_level_id("42e01f"));
        assert!(supported_h264_profile_level_id("42e034"));
        assert!(!supported_h264_profile_level_id("42e035"));
        assert!(!supported_h264_profile_level_id("640034"));
        assert!(valid_coded_video_size(1920, 1088));
        assert!(!valid_coded_video_size(8194, 2));
    }
}
