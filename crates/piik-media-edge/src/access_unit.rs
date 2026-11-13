//! Relay input reorder and access-unit assembly for H.264 and VP8.
//!
//! A sequence-reordering sample builder bounded by the encoded packet window,
//! with H.264 access units owned by their original RTP timestamp, marker and
//! FU chain. Decoder configuration admission (`frame`) stays with the caller:
//! [`Assembler::push`] returns retired complete H.264 NAL units for it.

use piik_encoded::{MAX_ACCESS_UNIT_BYTES, MAX_PACKET_WINDOW};

mod sample_builder;

use sample_builder::SampleBuilder;

pub const VIDEO_CLOCK_RATE: u32 = 90_000;
pub const RELAY_PACKET_LIMIT: usize = MAX_PACKET_WINDOW;

/// Storage bound of one assembling H.264 AU: its payload bytes plus the FU and
/// STAP headers of a full packet window.
const MAX_H264_AU_PAYLOAD: usize = MAX_ACCESS_UNIT_BYTES + 2 * RELAY_PACKET_LIMIT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    H264,
    Vp8,
}

/// The RTP fields assembly consumes. Padding-only packets have an empty
/// payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RtpPacket {
    pub sequence_number: u16,
    pub timestamp: u32,
    pub marker: bool,
    pub payload: Vec<u8>,
}

/// One complete decoder input: Annex-B for H.264, the VP8 frame for VP8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessUnit {
    pub data: Vec<u8>,
    pub timestamp: u32,
}

#[derive(Debug, Default)]
struct H264Au {
    packets: Vec<RtpPacket>,
    bytes: usize,
    failed: bool,
}

/// Decoder configuration belongs to this received stream, not a child encoder.
///
/// Loss that the current assembly owns is counted; the caller drains it with
/// [`Assembler::take_losses`] after each `push` or `pop` and requests
/// independent recovery.
#[derive(Debug)]
pub struct Assembler {
    codec: Codec,
    builder: Option<SampleBuilder>,
    recovery_timestamp: u32,
    recovery_sequence: u16,
    has_recovery: bool,
    pending: bool,
    pending_timestamp: u32,
    latest_timestamp: u32,
    h264_au: Option<H264Au>,
    losses: usize,
}

impl Assembler {
    pub fn new(codec: Codec) -> Self {
        Self {
            codec,
            builder: None,
            recovery_timestamp: 0,
            recovery_sequence: 0,
            has_recovery: false,
            pending: false,
            pending_timestamp: 0,
            latest_timestamp: 0,
            h264_au: None,
            losses: 0,
        }
    }

    /// Returns and clears the loss notifications since the previous call.
    pub fn take_losses(&mut self) -> usize {
        std::mem::take(&mut self.losses)
    }

    /// Accepts one received packet. When a new independent frame replaces the
    /// current assembly, the retired packets' complete H.264 NAL units are
    /// returned (Annex-B) so the caller keeps their decoder configuration;
    /// expired pictures are never returned as access units.
    pub fn push(&mut self, packet: RtpPacket) -> Vec<Vec<u8>> {
        if !packet.payload.is_empty()
            && self.has_recovery
            && (packet.timestamp.wrapping_sub(self.recovery_timestamp) as i32) < 0
        {
            return Vec::new();
        }
        let recovery = is_key_frame(self.codec, &packet.payload);
        if recovery
            && self.has_recovery
            && packet.timestamp == self.recovery_timestamp
            && packet.sequence_number == self.recovery_sequence
        {
            return Vec::new();
        }
        let same_h264_au =
            self.codec == Codec::H264 && self.pending && packet.timestamp == self.pending_timestamp;
        let mut retained = Vec::new();
        if self.builder.is_none()
            || recovery
                && !same_h264_au
                && (!self.has_recovery || packet.timestamp != self.recovery_timestamp)
        {
            let mut retired = match self.h264_au.take() {
                Some(au) if !au.failed => au.packets,
                _ => Vec::new(),
            };
            let previous = self
                .builder
                .replace(SampleBuilder::new(RELAY_PACKET_LIMIT as u16, self.codec));
            self.pending = false;
            // Old gaps cannot request another keyframe after the replacement
            // has arrived: the previous builder's drops are not counted.
            if let Some(mut previous) = previous
                && self.codec == Codec::H264
            {
                while let Some(packets) = previous.pop_packets(true) {
                    retired.extend(packets);
                }
            }
            retained = cache_h264(&retired);
        }
        if recovery {
            self.recovery_timestamp = packet.timestamp;
            self.has_recovery = true;
            self.recovery_sequence = packet.sequence_number;
        }
        if !packet.payload.is_empty() {
            if !self.pending {
                self.pending = true;
                self.pending_timestamp = packet.timestamp;
                self.latest_timestamp = packet.timestamp;
            } else if packet.timestamp.wrapping_sub(self.latest_timestamp) as i32 > 0 {
                self.latest_timestamp = packet.timestamp;
            }
        }
        let builder = self.builder.as_mut().expect("builder exists");
        builder.push(packet);
        self.builder_losses();
        retained
    }

    /// Returns the next complete access unit, if any.
    pub fn pop(&mut self) -> Option<AccessUnit> {
        self.builder.as_ref()?;
        if self.codec == Codec::H264 {
            return self.pop_h264();
        }
        let builder = self.builder.as_mut().expect("builder exists");
        let mut sample = builder.pop_sample(false);
        self.builder_losses();
        // The packet window bounds storage for large access units, not playout
        // delay. After a second of advancing media without a complete sample,
        // discard the gap and request independent recovery. RTP time leaves
        // quiet sources alone and adds no timer or decode owner.
        if sample.is_none() && self.pending && self.stalled() {
            self.losses += 1;
            let builder = self.builder.as_mut().expect("builder exists");
            sample = builder.pop_sample(true);
            self.builder_losses();
            self.pending = false;
        }
        if sample.is_some() {
            self.pending = false;
        }
        sample.map(|(data, timestamp)| AccessUnit { data, timestamp })
    }

    fn stalled(&self) -> bool {
        self.latest_timestamp.wrapping_sub(self.pending_timestamp) as i32 > VIDEO_CLOCK_RATE as i32
    }

    /// Applies the current builder's drop notifications. For H.264 they fail
    /// the assembling AU; for VP8 they are loss notifications.
    fn builder_losses(&mut self) {
        let drops = self.builder.as_mut().map_or(0, SampleBuilder::take_drops);
        for _ in 0..drops {
            if self.codec == Codec::H264 {
                self.discard_h264(self.pending_timestamp);
            } else {
                self.losses += 1;
            }
        }
    }

    /// Only a whole, marked AU is decoder input. The builder retains sequence
    /// reordering; neither a new timestamp nor a padding packet proves a
    /// complete picture.
    fn pop_h264(&mut self) -> Option<AccessUnit> {
        loop {
            let mut packets = self.builder.as_mut()?.pop_packets(false);
            self.builder_losses();
            if packets.is_none() && self.pending && self.stalled() {
                self.discard_h264(self.pending_timestamp);
                packets = self.builder.as_mut()?.pop_packets(true);
                self.builder_losses();
            }
            // Every builder partition is one ordered packet.
            let packet = packets?.into_iter().next()?;
            // Keep padding's sequence within an existing AU, but never use it
            // as source time or a boundary.
            if packet.payload.is_empty() {
                if self.h264_au.as_ref().is_none_or(|au| au.failed) {
                    continue;
                }
            } else {
                if self.h264_au.is_some() && packet.timestamp != self.pending_timestamp {
                    if self.h264_au.as_ref().is_some_and(|au| !au.failed) {
                        self.discard_h264(self.pending_timestamp);
                    }
                    self.h264_au = None;
                }
                if self.h264_au.as_ref().is_some_and(|au| au.failed) {
                    continue;
                }
                if self.h264_au.is_none() {
                    self.h264_au = Some(H264Au::default());
                    self.pending_timestamp = packet.timestamp;
                }
                self.pending = true;
            }
            let timestamp = self.pending_timestamp;
            let au = self.h264_au.as_mut().expect("assembling AU exists");
            au.bytes += packet.payload.len();
            if au.packets.len() >= RELAY_PACKET_LIMIT || au.bytes > MAX_H264_AU_PAYLOAD {
                self.discard_h264(timestamp);
                continue;
            }
            let complete = !packet.payload.is_empty() && packet.marker;
            au.packets.push(packet);
            if !complete {
                continue;
            }
            let Some(data) = h264_access_unit(&au.packets) else {
                self.discard_h264(timestamp);
                continue;
            };
            self.h264_au = None;
            self.pending = false;
            return Some(AccessUnit { data, timestamp });
        }
    }

    fn discard_h264(&mut self, timestamp: u32) {
        if self.h264_au.as_ref().is_some_and(|au| au.failed) && self.pending_timestamp == timestamp
        {
            return;
        }
        self.h264_au = Some(H264Au {
            failed: true,
            ..H264Au::default()
        });
        self.pending = true;
        self.pending_timestamp = timestamp;
        self.losses += 1;
    }
}

/// Depacketizes a complete ordered AU once; FU continuity spans padding and
/// belongs to the AU, independently of the builder's packet partitions.
fn h264_access_unit(packets: &[RtpPacket]) -> Option<Vec<u8>> {
    let mut depacketizer = H264Depacketizer::default();
    let mut data = Vec::new();
    let mut fragmented = false;
    for packet in packets {
        let payload = &packet.payload[..];
        if payload.is_empty() {
            continue;
        }
        if payload[0] & NALU_TYPE_MASK == FUA_NALU_TYPE {
            if payload.len() < 2 {
                return None;
            }
            if payload[1] & FU_START != 0 {
                if fragmented {
                    return None;
                }
                fragmented = true;
            } else if !fragmented {
                return None;
            }
            if payload[1] & FU_END != 0 {
                fragmented = false;
            }
        } else if fragmented {
            return None;
        }
        let decoded = depacketizer.unmarshal(payload)?;
        if data.len() + decoded.len() > MAX_ACCESS_UNIT_BYTES {
            return None;
        }
        data.extend_from_slice(&decoded);
    }
    (!fragmented && !data.is_empty()).then_some(data)
}

/// Retirement keeps complete NAL units only, for their configuration. Never
/// decode expired pictures, or let an abandoned FU contaminate the next
/// independent NAL.
fn cache_h264(packets: &[RtpPacket]) -> Vec<Vec<u8>> {
    let mut retained = Vec::new();
    let mut depacketizer: Option<H264Depacketizer> = None;
    let mut previous = 0u16;
    let mut timestamp = 0u32;
    let mut bytes = 0usize;
    for packet in packets {
        if depacketizer.is_some() && packet.sequence_number != previous.wrapping_add(1) {
            depacketizer = None;
        }
        previous = packet.sequence_number;
        if packet.payload.is_empty() {
            continue;
        }
        if h264_partition_head(&packet.payload) {
            depacketizer = Some(H264Depacketizer::default());
            timestamp = packet.timestamp;
            bytes = 0;
        }
        let Some(current) = depacketizer
            .as_mut()
            .filter(|_| packet.timestamp == timestamp)
        else {
            depacketizer = None;
            continue;
        };
        bytes += packet.payload.len();
        if bytes > MAX_H264_AU_PAYLOAD {
            depacketizer = None;
            continue;
        }
        match current.unmarshal(&packet.payload) {
            None => depacketizer = None,
            Some(data) if !data.is_empty() => {
                depacketizer = None;
                retained.push(data);
            }
            Some(_) => {}
        }
    }
    retained
}

const NALU_TYPE_MASK: u8 = 0x1f;
const NALU_REF_IDC_MASK: u8 = 0x60;
const STAPA_NALU_TYPE: u8 = 24;
const FUA_NALU_TYPE: u8 = 28;
const FUB_NALU_TYPE: u8 = 29;
const SPS_NALU_TYPE: u8 = 7;
const FU_START: u8 = 0x80;
const FU_END: u8 = 0x40;
const ANNEXB_START_CODE: [u8; 4] = [0, 0, 0, 1];

/// RFC 6184 depacketization to Annex-B, as Pion's `codecs.H264Packet`:
/// single NAL units, STAP-A and FU-A. An unfinished FU yields empty data.
#[derive(Debug, Default)]
struct H264Depacketizer {
    fua: Option<Vec<u8>>,
}

impl H264Depacketizer {
    fn unmarshal(&mut self, payload: &[u8]) -> Option<Vec<u8>> {
        let nalu_type = payload.first()? & NALU_TYPE_MASK;
        match nalu_type {
            1..=23 => Some(annexb(payload)),
            STAPA_NALU_TYPE => {
                let mut result = Vec::new();
                let mut offset = 1;
                while offset + 2 <= payload.len() {
                    let size =
                        usize::from(u16::from_be_bytes([payload[offset], payload[offset + 1]]));
                    offset += 2;
                    let nalu = payload.get(offset..offset + size)?;
                    result.extend_from_slice(&ANNEXB_START_CODE);
                    result.extend_from_slice(nalu);
                    offset += size;
                }
                Some(result)
            }
            FUA_NALU_TYPE => {
                if payload.len() < 2 {
                    return None;
                }
                let buffer = self.fua.get_or_insert_with(Vec::new);
                buffer.extend_from_slice(&payload[2..]);
                if payload[1] & FU_END == 0 {
                    return Some(Vec::new());
                }
                let mut nalu =
                    vec![(payload[0] & NALU_REF_IDC_MASK) | (payload[1] & NALU_TYPE_MASK)];
                nalu.extend_from_slice(&self.fua.take().unwrap_or_default());
                Some(annexb(&nalu))
            }
            _ => None,
        }
    }
}

fn annexb(nalu: &[u8]) -> Vec<u8> {
    let mut data = Vec::with_capacity(ANNEXB_START_CODE.len() + nalu.len());
    data.extend_from_slice(&ANNEXB_START_CODE);
    data.extend_from_slice(nalu);
    data
}

/// Pion's `H264Packet.IsPartitionHead`.
fn h264_partition_head(payload: &[u8]) -> bool {
    if payload.len() < 2 {
        return false;
    }
    let nalu_type = payload[0] & NALU_TYPE_MASK;
    if nalu_type == FUA_NALU_TYPE || nalu_type == FUB_NALU_TYPE {
        return payload[1] & FU_START != 0;
    }
    true
}

/// Keyframe detection for the two relay codecs.
fn is_key_frame(codec: Codec, payload: &[u8]) -> bool {
    match codec {
        Codec::H264 => is_h264_key_frame(payload),
        Codec::Vp8 => is_vp8_key_frame(payload),
    }
}

/// A payload carrying an SPS starts independent H.264 recovery.
fn is_h264_key_frame(payload: &[u8]) -> bool {
    let Some(&first) = payload.first() else {
        return false;
    };
    match first & NALU_TYPE_MASK {
        0 => false,
        nalu @ 1..=23 => nalu == SPS_NALU_TYPE,
        nalu @ 24..=27 => {
            let mut index = if nalu == 24 { 1 } else { 3 };
            let offset = match nalu {
                26 => 3,
                27 => 4,
                _ => 0,
            };
            while index < payload.len() {
                if index + 2 > payload.len() {
                    return false;
                }
                let length = usize::from(u16::from_be_bytes([payload[index], payload[index + 1]]));
                index += 2;
                if index + length > payload.len() || offset >= length {
                    return false;
                }
                if payload[index + offset] & NALU_TYPE_MASK == SPS_NALU_TYPE {
                    return true;
                }
                index += length;
            }
            false
        }
        28 | 29 => {
            payload.len() >= 2
                && payload[1] & FU_START != 0
                && payload[1] & NALU_TYPE_MASK == SPS_NALU_TYPE
        }
        _ => false,
    }
}

/// The VP8 payload descriptor's S bit with a clear P bit in the first payload
/// octet.
fn is_vp8_key_frame(payload: &[u8]) -> bool {
    let Some(&first) = payload.first() else {
        return false;
    };
    let start = first & 0x10 != 0;
    let mut index = 0;
    if first & 0x80 != 0 {
        index += 1;
        let Some(&extension) = payload.get(index) else {
            return false;
        };
        let (picture_id, tl0, tid, key_index) = (
            extension & 0x80 != 0,
            extension & 0x40 != 0,
            extension & 0x20 != 0,
            extension & 0x10 != 0,
        );
        if tl0 && !tid {
            return false;
        }
        if picture_id {
            index += 1;
            let Some(&id) = payload.get(index) else {
                return false;
            };
            if id & 0x80 != 0 {
                index += 1;
            }
        }
        if tl0 {
            index += 1;
        }
        if tid || key_index {
            index += 1;
        }
    }
    index += 1;
    payload.get(index).is_some_and(|byte| byte & 1 == 0) && start
}

/// Pion's `codecs.VP8Packet.Unmarshal`: strips the payload descriptor.
fn vp8_payload(payload: &[u8]) -> Option<&[u8]> {
    let first = *payload.first()?;
    let mut index = 1;
    let (mut picture_id, mut tl0, mut tid, mut key_index) = (false, false, false, false);
    if first & 0x80 != 0 {
        let extension = *payload.get(index)?;
        picture_id = extension & 0x80 != 0;
        tl0 = extension & 0x40 != 0;
        tid = extension & 0x20 != 0;
        key_index = extension & 0x10 != 0;
        index += 1;
    }
    if picture_id {
        index += if *payload.get(index)? & 0x80 != 0 {
            payload.get(index + 1)?;
            2
        } else {
            1
        };
    }
    if tl0 {
        payload.get(index)?;
        index += 1;
    }
    if tid || key_index {
        payload.get(index)?;
        index += 1;
    }
    payload.get(index..)
}

#[cfg(test)]
mod tests;
