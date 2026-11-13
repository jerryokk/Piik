//! Decoder-input admission and timing for relay derivation. Assembly lives
//! in [`crate::access_unit`].

use piik_media_forwarding::Vp8Descriptor;
use piik_media_transport::sdp::{VideoCodec, forwardable_h264};
use piik_native_capture::{Frame, FrameKind, valid_coded_video_size, valid_video_size};
use std::time::Duration;

use crate::access_unit::VIDEO_CLOCK_RATE;
use crate::error::{Error, Result};

const NAL_SPS: u8 = 7;
const NAL_PPS: u8 = 8;
const NAL_IDR: u8 = 5;
const NAL_STAP_A: u8 = 24;

/// Decoder configuration of one received stream, not of a child encoder.
#[derive(Debug, Clone)]
pub(crate) struct RelayInput {
    codec: VideoCodec,
    sps: Vec<u8>,
    pps: Vec<u8>,
    width: u32,
    height: u32,
}

impl RelayInput {
    pub fn new(codec: VideoCodec) -> Self {
        Self {
            codec,
            sps: Vec::new(),
            pps: Vec::new(),
            width: 0,
            height: 0,
        }
    }

    /// One complete access unit as a capture-process input. Every SPS is
    /// admitted before any decoder sees it; an H.264 IDR is prefixed with the
    /// cached parameter sets, which browsers may send earlier.
    pub fn frame(&mut self, data: Vec<u8>) -> Result<Frame> {
        let mut frame = Frame {
            kind: FrameKind::H264,
            ..Frame::default()
        };
        match self.codec {
            VideoCodec::Vp8 => {
                frame.kind = FrameKind::Vp8;
                frame.key_frame = data.len() >= 10 && data[0] & 1 == 0;
                if let Some((width, height)) = vp8_size(frame.key_frame, &data) {
                    self.width = width;
                    self.height = height;
                }
                frame.data = data;
            }
            VideoCodec::H264 => {
                for nal in annex_b(&data) {
                    let Some(&header) = nal.first() else {
                        continue;
                    };
                    match header & 0x1f {
                        NAL_SPS => {
                            let (width, height) = relay_h264_size(nal)?;
                            self.sps = nal.to_vec();
                            self.width = width;
                            self.height = height;
                        }
                        NAL_PPS => self.pps = nal.to_vec(),
                        NAL_IDR => frame.key_frame = true,
                        _ => {}
                    }
                }
                frame.key_frame &= !self.sps.is_empty() && !self.pps.is_empty();
                if frame.key_frame {
                    let mut prefixed =
                        Vec::with_capacity(self.sps.len() + self.pps.len() + data.len() + 8);
                    for nal in [&self.sps, &self.pps] {
                        prefixed.extend_from_slice(&[0, 0, 0, 1]);
                        prefixed.extend_from_slice(nal);
                    }
                    prefixed.extend_from_slice(&data);
                    frame.data = prefixed;
                } else {
                    frame.data = data;
                }
            }
        }
        frame.width = self.width;
        frame.height = self.height;
        Ok(frame)
    }
}

/// NAL units of an Annex-B stream, without start codes.
pub(crate) fn annex_b(data: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 <= data.len() {
        if data[index] == 0 && data[index + 1] == 0 && data[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else {
            index += 1;
        }
    }
    let mut nals = Vec::with_capacity(starts.len());
    for (position, start) in starts.iter().enumerate() {
        let mut end = starts.get(position + 1).map_or(data.len(), |next| next - 3);
        // A four-byte start code leaves one zero before the next three.
        while end > *start && data[end - 1] == 0 && starts.get(position + 1).is_some() {
            end -= 1;
        }
        nals.push(&data[*start..end]);
    }
    nals
}

/// `mediacodec.ExtractVP8VideoSize` for a keyframe payload.
fn vp8_size(key_frame: bool, data: &[u8]) -> Option<(u32, u32)> {
    if !key_frame || data.len() < 10 {
        return None;
    }
    let width = u32::from(u16::from_le_bytes([data[6], data[7]]) & 0x3fff);
    let height = u32::from(u16::from_le_bytes([data[8], data[9]]) & 0x3fff);
    (width > 0 && height > 0).then_some((width, height))
}

/// Received input dimensions from one RTP payload of a relay source (an SPS
/// alone or in STAP-A; a VP8 keyframe header).
pub(crate) fn payload_size(codec: VideoCodec, payload: &[u8]) -> Option<(u32, u32)> {
    match codec {
        VideoCodec::H264 => {
            let header = *payload.first()?;
            match header & 0x1f {
                NAL_SPS => relay_h264_size(payload).ok(),
                NAL_STAP_A => {
                    let mut rest = &payload[1..];
                    while rest.len() >= 2 {
                        let size = usize::from(u16::from_be_bytes([rest[0], rest[1]]));
                        let nal = rest.get(2..2 + size)?;
                        if nal.first().is_some_and(|header| header & 0x1f == NAL_SPS) {
                            return relay_h264_size(nal).ok();
                        }
                        rest = &rest[2 + size..];
                    }
                    None
                }
                _ => None,
            }
        }
        VideoCodec::Vp8 => {
            let descriptor = Vp8Descriptor::parse(payload)?;
            vp8_size(descriptor.keyframe, payload.get(descriptor.header_len..)?)
        }
    }
}

/// Exp-Golomb reader over an RBSP (emulation prevention removed).
struct Bits {
    data: Vec<u8>,
    position: usize,
}

impl Bits {
    fn new(escaped: &[u8]) -> Self {
        let mut data = Vec::with_capacity(escaped.len());
        let mut zeros = 0;
        for &byte in escaped {
            if zeros >= 2 && byte == 3 {
                zeros = 0;
                continue;
            }
            zeros = if byte == 0 { zeros + 1 } else { 0 };
            data.push(byte);
        }
        Self { data, position: 0 }
    }

    fn bit(&mut self) -> Option<u32> {
        let byte = *self.data.get(self.position / 8)?;
        let bit = (byte >> (7 - self.position % 8)) & 1;
        self.position += 1;
        Some(u32::from(bit))
    }

    fn bits(&mut self, count: u32) -> Option<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = value << 1 | self.bit()?;
        }
        Some(value)
    }

    fn flag(&mut self) -> Option<bool> {
        self.bit().map(|bit| bit == 1)
    }

    fn ue(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.bit()? == 0 {
            zeros += 1;
            if zeros > 31 {
                return None;
            }
        }
        let rest = self.bits(zeros)?;
        Some(((1u64 << zeros) - 1 + u64::from(rest)).min(u64::from(u32::MAX)) as u32)
    }

    fn se(&mut self) -> Option<i64> {
        let value = i64::from(self.ue()?);
        Some(if value % 2 == 1 {
            (value + 1) / 2
        } else {
            -(value / 2)
        })
    }
}

/// The SPS fields admission needs.
#[derive(Debug, Default)]
struct Sps {
    max_num_ref_frames: u32,
    width_mbs: u64,
    height_map_units: u64,
    frame_mbs_only: bool,
    crop: Option<[u32; 4]>,
    /// (max_num_reorder_frames, max_dec_frame_buffering)
    restriction: Option<(u32, u32)>,
}

fn hrd(bits: &mut Bits) -> Option<()> {
    let count = bits.ue()?;
    if count > 31 {
        return None;
    }
    bits.bits(8)?;
    for _ in 0..=count {
        bits.ue()?;
        bits.ue()?;
        bits.flag()?;
    }
    bits.bits(20)?;
    Some(())
}

/// Parses a Baseline-family SPS NAL (with its header byte). Truncation
/// anywhere, including the VUI, is an error.
fn parse_sps(nal: &[u8]) -> Option<Sps> {
    let mut bits = Bits::new(nal.get(1..)?);
    let profile = bits.bits(8)?;
    bits.bits(16)?;
    if bits.ue()? > 31 {
        return None;
    }
    if matches!(
        profile,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    ) {
        // Never admitted: only Constrained Baseline passes the profile check.
        return None;
    }
    bits.ue()?; // log2_max_frame_num_minus4
    match bits.ue()? {
        0 => {
            bits.ue()?;
        }
        1 => {
            bits.flag()?;
            bits.se()?;
            bits.se()?;
            let cycle = bits.ue()?;
            if cycle > 255 {
                return None;
            }
            for _ in 0..cycle {
                bits.se()?;
            }
        }
        _ => {}
    }
    let mut sps = Sps {
        max_num_ref_frames: bits.ue()?,
        ..Sps::default()
    };
    bits.flag()?;
    sps.width_mbs = u64::from(bits.ue()?) + 1;
    sps.height_map_units = u64::from(bits.ue()?) + 1;
    sps.frame_mbs_only = bits.flag()?;
    if !sps.frame_mbs_only {
        bits.flag()?;
    }
    bits.flag()?;
    if bits.flag()? {
        sps.crop = Some([bits.ue()?, bits.ue()?, bits.ue()?, bits.ue()?]);
    }
    if bits.flag()? {
        // VUI.
        if bits.flag()? && bits.bits(8)? == 255 {
            bits.bits(32)?;
        }
        if bits.flag()? {
            bits.flag()?;
        }
        if bits.flag()? {
            bits.bits(4)?;
            if bits.flag()? {
                bits.bits(24)?;
            }
        }
        if bits.flag()? {
            bits.ue()?;
            bits.ue()?;
        }
        if bits.flag()? {
            bits.bits(32)?;
            bits.bits(32)?;
            bits.flag()?;
        }
        let nal_hrd = bits.flag()?;
        if nal_hrd {
            hrd(&mut bits)?;
        }
        let vcl_hrd = bits.flag()?;
        if vcl_hrd {
            hrd(&mut bits)?;
        }
        if nal_hrd || vcl_hrd {
            bits.flag()?;
        }
        bits.flag()?;
        if bits.flag()? {
            bits.flag()?;
            for _ in 0..4 {
                bits.ue()?;
            }
            sps.restriction = Some((bits.ue()?, bits.ue()?));
        }
    }
    Some(sps)
}

/// The H.264 input size. A size summary is not a decoder allocation bound:
/// one access unit may carry several SPS IDs and a slice need not reference
/// the last one, so every SPS is admitted, including its uncropped storage
/// and decoded-picture buffer.
pub(crate) fn relay_h264_size(nal: &[u8]) -> Result<(u32, u32)> {
    if nal.len() < 4
        || !forwardable_h264(&format!(
            "profile-level-id={};packetization-mode=1",
            hex::encode(&nal[1..4])
        ))
    {
        return Err(Error::Invalid("native H264 input profile is unsupported"));
    }
    let sps = parse_sps(nal).ok_or(Error::Invalid("invalid native H264 input SPS"))?;
    let mut width = sps.width_mbs * 16;
    let mut height = sps.height_map_units * 16;
    let mut crop_unit_y = 2;
    if !sps.frame_mbs_only {
        height *= 2;
        crop_unit_y *= 2;
    }
    // Constrained Baseline is 8-bit 4:2:0. Bound uncropped storage before
    // padding can be removed from the visible picture.
    if !valid_coded_video_size(width, height) {
        return Err(Error::Invalid(
            "native H264 input storage exceeds its bound",
        ));
    }
    let mut buffered = u64::from(sps.max_num_ref_frames.max(1));
    if let Some((reorder, buffering)) = sps.restriction {
        buffered = buffered.max(u64::from(reorder)).max(u64::from(buffering));
    }
    // Level 5.2 also bounds the decoded-picture buffer.
    if buffered > 16 || width * height / 256 * buffered > 184_320 {
        return Err(Error::Invalid(
            "native H264 input reference storage exceeds its bound",
        ));
    }
    if let Some([left, right, top, bottom]) = sps.crop {
        let x = (u64::from(left) + u64::from(right)) * 2;
        let y = (u64::from(top) + u64::from(bottom)) * crop_unit_y;
        if x >= width || y >= height {
            return Err(Error::Invalid(
                "native H264 input crop is outside its picture",
            ));
        }
        width -= x;
        height -= y;
    }
    let (width, height) = (width as u32, height as u32);
    if !valid_video_size(width, height) {
        return Err(Error::Invalid(
            "native H264 input picture exceeds its bound",
        ));
    }
    Ok((width, height))
}

/// One source RTP clock for decoder presentation and frame intervals, with
/// 32-bit timestamps extended across wraparound.
#[derive(Debug, Clone)]
pub(crate) struct RelayClock {
    start: i64,
    highest: i64,
}

impl RelayClock {
    pub fn new(anchor: u32) -> Self {
        Self {
            start: i64::from(anchor),
            highest: i64::from(anchor),
        }
    }

    /// `(pts, duration)` of `timestamp`, or `None` before the anchor. The
    /// first frame and reordered frames carry no interval.
    pub fn timing(&mut self, timestamp: u32) -> Option<(Duration, Duration)> {
        let delta = timestamp.wrapping_sub(self.highest as u32) as i32;
        let extended = self.highest + i64::from(delta);
        if extended < self.start {
            return None;
        }
        let previous = self.highest;
        self.highest = self.highest.max(extended);
        let ticks = (extended - self.start) as u64;
        let rate = u64::from(VIDEO_CLOCK_RATE);
        let pts = Duration::from_secs(ticks / rate)
            + Duration::from_nanos(ticks % rate * 1_000_000_000 / rate);
        let duration = if previous != self.start && extended > previous {
            Duration::from_nanos((extended - previous) as u64 * 1_000_000_000 / rate)
        } else {
            Duration::ZERO
        };
        Some((pts, duration))
    }
}

#[cfg(test)]
mod tests;
