//! Inbound RTP model and the codec descriptors forwarding decisions need.
//! The transport owner supplies extended sequence numbers and timestamps;
//! [`Unwrapper`] is available when its RTP library does not.

use std::time::{Instant, SystemTime};

const RTP_HEADER_BYTES: usize = 12;
const KEY_IDX_MASK: u8 = 0x1f;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodecKind {
    Vp8,
    H264,
    Opus,
}

impl CodecKind {
    pub const fn clock_rate(self) -> u32 {
        match self {
            Self::Opus => 48_000,
            Self::Vp8 | Self::H264 => 90_000,
        }
    }

    pub const fn is_video(self) -> bool {
        !matches!(self, Self::Opus)
    }
}

/// Extends wrapping RTP counters (16-bit sequence numbers, 32-bit timestamps).
/// The first value starts one cycle up so earlier reordered values stay
/// representable.
#[derive(Clone, Copy, Debug)]
pub struct Unwrapper {
    bits: u32,
    last: Option<u64>,
}

impl Unwrapper {
    pub const fn sequence() -> Self {
        Self::bits(16)
    }

    pub const fn timestamp() -> Self {
        Self::bits(32)
    }

    /// Counter of `bits` width (1..=32), such as a VP8 PictureID.
    pub const fn bits(bits: u32) -> Self {
        Self { bits, last: None }
    }

    pub fn extend(&mut self, value: u64) -> u64 {
        let cycle = 1u64 << self.bits;
        let value = value & (cycle - 1);
        let Some(last) = self.last else {
            let extended = cycle + value;
            self.last = Some(extended);
            return extended;
        };
        // Nearest signed distance from the highest value seen so far.
        let mut delta = (value.wrapping_sub(last) & (cycle - 1)) as i64;
        if delta >= (cycle / 2) as i64 {
            delta -= cycle as i64;
        }
        let extended = last.saturating_add_signed(delta);
        if extended > last {
            self.last = Some(extended);
        }
        extended
    }
}

/// Parsed RFC 7741 VP8 payload descriptor. Offsets are private and valid only
/// for the parsed payload or a byte-identical copy.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Vp8Descriptor {
    /// S bit: first packet of a VP8 partition.
    pub start: bool,
    pub partition: u8,
    pub picture_id: Option<u16>,
    pub picture_id_15bit: bool,
    pub tl0_pic_idx: Option<u8>,
    pub temporal: Option<u8>,
    /// Y bit: this frame depends only on the base layer.
    pub layer_sync: bool,
    pub key_idx: Option<u8>,
    /// First packet of a keyframe (S=1, PID=0, P=0).
    pub keyframe: bool,
    pub header_len: usize,
    picture_id_offset: Option<usize>,
    tl0_offset: Option<usize>,
    key_offset: Option<usize>,
}

impl Vp8Descriptor {
    pub fn parse(payload: &[u8]) -> Option<Self> {
        let required = *payload.first()?;
        let mut offset = 1;
        let mut descriptor = Self {
            start: required & 0x10 != 0,
            partition: required & 0x07,
            picture_id: None,
            picture_id_15bit: false,
            tl0_pic_idx: None,
            temporal: None,
            layer_sync: false,
            key_idx: None,
            keyframe: false,
            header_len: 0,
            picture_id_offset: None,
            tl0_offset: None,
            key_offset: None,
        };
        if required & 0x80 != 0 {
            let extension = *payload.get(offset)?;
            offset += 1;
            let (has_picture, has_tl0) = (extension & 0x80 != 0, extension & 0x40 != 0);
            let (has_tid, has_key) = (extension & 0x20 != 0, extension & 0x10 != 0);
            if has_tl0 && !has_tid {
                return None;
            }
            if has_picture {
                let high = *payload.get(offset)?;
                descriptor.picture_id_offset = Some(offset);
                offset += 1;
                if high & 0x80 != 0 {
                    let low = *payload.get(offset)?;
                    offset += 1;
                    descriptor.picture_id_15bit = true;
                    descriptor.picture_id = Some((u16::from(high & 0x7f) << 8) | u16::from(low));
                } else {
                    descriptor.picture_id = Some(u16::from(high & 0x7f));
                }
            }
            if has_tl0 {
                descriptor.tl0_offset = Some(offset);
                descriptor.tl0_pic_idx = Some(*payload.get(offset)?);
                offset += 1;
            }
            if has_tid || has_key {
                let value = *payload.get(offset)?;
                if has_tid {
                    descriptor.temporal = Some(value >> 6);
                    descriptor.layer_sync = value & 0x20 != 0;
                }
                if has_key {
                    descriptor.key_offset = Some(offset);
                    descriptor.key_idx = Some(value & KEY_IDX_MASK);
                }
                offset += 1;
            }
        }
        let header = *payload.get(offset)?;
        descriptor.header_len = offset;
        descriptor.keyframe = descriptor.start && descriptor.partition == 0 && header & 0x01 == 0;
        Some(descriptor)
    }
}

/// Descriptor values for one forwarded VP8 packet. The transport converts it
/// to str0m's `Vp8Patch` (same fields, same width rules) at serialization.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Vp8Rewrite {
    pub picture_id: Option<u16>,
    pub tl0_pic_idx: Option<u8>,
    pub key_idx: Option<u8>,
}

impl Vp8Rewrite {
    /// Applies the rewrite in place, preserving the descriptor's PictureID
    /// width and the TID/Y bits beside KEYIDX.
    pub fn apply(&self, descriptor: &Vp8Descriptor, payload: &mut [u8]) {
        if let (Some(id), Some(offset)) = (self.picture_id, descriptor.picture_id_offset) {
            if descriptor.picture_id_15bit {
                payload[offset] = 0x80 | ((id >> 8) as u8 & 0x7f);
                payload[offset + 1] = id as u8;
            } else {
                payload[offset] = id as u8 & 0x7f;
            }
        }
        if let (Some(value), Some(offset)) = (self.tl0_pic_idx, descriptor.tl0_offset) {
            payload[offset] = value;
        }
        if let (Some(value), Some(offset)) = (self.key_idx, descriptor.key_offset) {
            payload[offset] = (payload[offset] & !KEY_IDX_MASK) | (value & KEY_IDX_MASK);
        }
    }
}

/// One received RTP packet as forwarding sees it. Sequence numbers and
/// timestamps are extended; `layer` is the spatial layer (simulcast index).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InboundPacket {
    pub ssrc: u32,
    pub payload_type: u8,
    pub layer: i32,
    /// VP8 temporal layer, or 0 when the codec carries none.
    pub temporal: i32,
    pub ext_seq: u64,
    pub ext_timestamp: u64,
    pub marker: bool,
    pub keyframe: bool,
    /// The packet can begin a decodable frame for switching purposes.
    pub start: bool,
    /// Serialized RTP size, counted by stream bitrate measurement.
    pub size: usize,
    pub padding_only: bool,
    pub arrival: Instant,
    /// Capture time on the source's RTP/NTP clock, when its owner knows it.
    pub reference: Option<SystemTime>,
    pub vp8: Option<Vp8Descriptor>,
}

impl InboundPacket {
    /// Describes `payload` (RTP payload without header or padding). An empty
    /// payload is padding only.
    pub fn new(
        kind: CodecKind,
        layer: i32,
        ext_seq: u64,
        ext_timestamp: u64,
        payload: &[u8],
        arrival: Instant,
    ) -> Self {
        let mut packet = Self {
            ssrc: 0,
            payload_type: 0,
            layer,
            temporal: 0,
            ext_seq,
            ext_timestamp,
            marker: false,
            keyframe: false,
            start: !payload.is_empty(),
            size: RTP_HEADER_BYTES + payload.len(),
            padding_only: payload.is_empty(),
            arrival,
            reference: None,
            vp8: None,
        };
        match kind {
            CodecKind::Vp8 if !payload.is_empty() => {
                packet.vp8 = Vp8Descriptor::parse(payload);
                if let Some(descriptor) = packet.vp8 {
                    packet.start = descriptor.start && descriptor.partition == 0;
                    packet.keyframe = descriptor.keyframe;
                    packet.temporal = descriptor.temporal.map_or(0, i32::from);
                }
            }
            CodecKind::H264 => packet.keyframe = h264_keyframe(payload),
            _ => {}
        }
        packet
    }

    pub fn marker(mut self, marker: bool) -> Self {
        self.marker = marker;
        self
    }

    pub fn ssrc(mut self, ssrc: u32) -> Self {
        self.ssrc = ssrc;
        self
    }

    pub fn payload_type(mut self, payload_type: u8) -> Self {
        self.payload_type = payload_type;
        self
    }

    pub fn size(mut self, size: usize) -> Self {
        self.size = size;
        self
    }

    pub fn reference(mut self, reference: Option<SystemTime>) -> Self {
        self.reference = reference;
        self
    }
}

/// An IDR or SPS, single or within STAP-A, or the first FU-A fragment of one.
fn h264_keyframe(payload: &[u8]) -> bool {
    const IDR: u8 = 5;
    const SPS: u8 = 7;
    let key = |nalu: u8| matches!(nalu & 0x1f, IDR | SPS);
    let Some(&first) = payload.first() else {
        return false;
    };
    match first & 0x1f {
        24 => {
            let mut offset = 1;
            while let Some(size) = payload.get(offset..offset + 2) {
                let size = usize::from(u16::from_be_bytes([size[0], size[1]]));
                match payload.get(offset + 2) {
                    Some(&nalu) if size > 0 && key(nalu) => return true,
                    Some(_) => offset += 2 + size,
                    None => return false,
                }
            }
            false
        }
        28 => payload
            .get(1)
            .is_some_and(|header| header & 0x80 != 0 && key(*header)),
        _ => key(first),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vp8_descriptor_parses_and_rewrites_fields() {
        // X, I/L/T/K, 15-bit PictureID 0x1234, TL0PICIDX 0x55, TID=2 Y KEYIDX=7.
        let mut payload = vec![0x90, 0xf0, 0x92, 0x34, 0x55, 0xa7, 0x00, 0x01];
        let descriptor = Vp8Descriptor::parse(&payload).expect("descriptor");
        assert_eq!(descriptor.picture_id, Some(0x1234));
        assert!(descriptor.picture_id_15bit && descriptor.start && descriptor.keyframe);
        assert_eq!(
            (
                descriptor.tl0_pic_idx,
                descriptor.temporal,
                descriptor.key_idx
            ),
            (Some(0x55), Some(2), Some(7))
        );
        assert!(descriptor.layer_sync);
        let rewrite = Vp8Rewrite {
            picture_id: Some(0x7ffe),
            tl0_pic_idx: Some(1),
            key_idx: Some(0x1f),
        };
        rewrite.apply(&descriptor, &mut payload);
        let rewritten = Vp8Descriptor::parse(&payload).expect("rewritten");
        assert_eq!(rewritten.picture_id, Some(0x7ffe));
        assert_eq!(
            (rewritten.tl0_pic_idx, rewritten.key_idx),
            (Some(1), Some(0x1f))
        );
        assert_eq!((rewritten.temporal, rewritten.layer_sync), (Some(2), true));
        // 7-bit identities keep their width.
        let mut short = vec![0x90, 0x80, 0x42, 0x01];
        let parsed = Vp8Descriptor::parse(&short).expect("short");
        assert!(!parsed.picture_id_15bit && !parsed.keyframe);
        Vp8Rewrite {
            picture_id: Some(0x1ff),
            ..Default::default()
        }
        .apply(&parsed, &mut short);
        assert_eq!(
            Vp8Descriptor::parse(&short).and_then(|d| d.picture_id),
            Some(0x7f)
        );
        assert!(Vp8Descriptor::parse(&[0x80, 0x40, 0x12, 0x90]).is_none());
        assert!(Vp8Descriptor::parse(&[0x90, 0x80]).is_none());
    }

    #[test]
    fn h264_keyframes_are_recognized_in_each_packetization() {
        assert!(h264_keyframe(&[0x65, 0]));
        assert!(h264_keyframe(&[0x78, 0, 2, 0x67, 1, 0, 1, 0x68]));
        assert!(h264_keyframe(&[0x7c, 0x85, 0]));
        assert!(!h264_keyframe(&[0x7c, 0x05, 0]));
        assert!(!h264_keyframe(&[0x41, 0]));
        assert!(!h264_keyframe(&[0x78, 0, 1, 0x68]));
    }

    #[test]
    fn unwrapper_extends_across_wraps_and_reordering() {
        let mut sequence = Unwrapper::sequence();
        let first = sequence.extend(65_534);
        assert_eq!(sequence.extend(65_535), first + 1);
        assert_eq!(sequence.extend(0), first + 2);
        assert_eq!(
            sequence.extend(65_535),
            first + 1,
            "reordered packet before wrap"
        );
        assert_eq!(sequence.extend(1), first + 3);
        let mut timestamp = Unwrapper::timestamp();
        let base = timestamp.extend(u64::from(u32::MAX) - 10);
        assert_eq!(timestamp.extend(20), base + 31);
    }
}
