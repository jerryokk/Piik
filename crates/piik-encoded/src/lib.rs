//! Shared encoded-video primitives, independent of room authority, capture
//! implementation and PeerConnection lifecycle.

pub mod h264;
mod vp8;

use std::time::Duration;

pub use h264::{H264Payloader, h264_constrained_baseline_level};
pub use vp8::Vp8Payloader;

pub const MAX_ACCESS_UNIT_BYTES: usize = 4 * 1024 * 1024;

/// At the 1200-byte media MTU, this window covers a maximum-sized fragmented AU
/// with header headroom. It does not promise unlimited tiny-NAL packetization.
pub const MAX_PACKET_WINDOW: usize = MAX_ACCESS_UNIT_BYTES / 1024;

const VIDEO_CLOCK_RATE: f64 = 90_000.0;
const RTP_HEADER_BYTES: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("encoded video access unit is invalid")]
    InvalidAccessUnit,
    #[error("encoded video timestamp is not monotonic")]
    InvalidTimestamp,
}

/// One complete codec access unit. Recovery includes the codec configuration
/// needed to decode independently of previous output.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Frame {
    pub data: Vec<u8>,
    pub pts: Duration,
    pub duration: Duration,
    pub recovery: bool,
}

/// Splits one access unit into codec payloads that each fit `mtu` bytes.
pub trait Payloader: Send {
    fn payload(&mut self, mtu: usize, data: &[u8]) -> Vec<Vec<u8>>;
}

/// One outbound RTP packet without CSRCs or header extensions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    pub marker: bool,
    pub payload_type: u8,
    pub sequence_number: u16,
    pub timestamp: u32,
    pub ssrc: u32,
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Serialized RTP size: the fixed 12-byte header plus payload.
    pub fn marshal_size(&self) -> usize {
        RTP_HEADER_BYTES + self.payload.len()
    }
}

/// Preserves one outbound RTP sequence and source timeline across complete
/// access units. Its owner serializes calls; it buffers no media.
pub struct Packetizer {
    payloader: Box<dyn Payloader>,
    payload_type: u8,
    // The payloader receives the MTU less the fixed RTP header.
    mtu: usize,
    sequence_number: u16,
    rtp_timestamp: u32,
    last_timestamp: Option<Duration>,
    timestamp_remainder: f64,
}

impl Packetizer {
    /// Starts at a random sequence number and RTP timestamp.
    pub fn new(payloader: Box<dyn Payloader>, payload_type: u8, mtu: u16) -> Self {
        Self {
            payloader,
            payload_type,
            mtu: usize::from(mtu),
            sequence_number: rand::random(),
            rtp_timestamp: rand::random(),
            last_timestamp: None,
            timestamp_remainder: 0.0,
        }
    }

    /// Packetizes one access unit at `timestamp` on the source timeline. The
    /// RTP clock advances by the elapsed source time, including dormant gaps.
    pub fn packetize(
        &mut self,
        data: &[u8],
        timestamp: Duration,
        duration: Duration,
    ) -> Result<Vec<RtpPacket>, Error> {
        if data.is_empty() || data.len() > MAX_ACCESS_UNIT_BYTES || duration.is_zero() {
            return Err(Error::InvalidAccessUnit);
        }
        if let Some(last) = self.last_timestamp {
            if timestamp <= last {
                return Err(Error::InvalidTimestamp);
            }
            let ticks_float =
                (timestamp - last).as_secs_f64() * VIDEO_CLOCK_RATE + self.timestamp_remainder;
            // Float-to-int casts saturate; RTP time is modulo 2^32 by definition.
            let ticks = ticks_float as u64;
            self.timestamp_remainder = ticks_float - ticks as f64;
            self.rtp_timestamp = self.rtp_timestamp.wrapping_add(ticks as u32);
        }
        self.last_timestamp = Some(timestamp);
        let payloads = self
            .payloader
            .payload(self.mtu.saturating_sub(RTP_HEADER_BYTES), data);
        let count = payloads.len();
        let packets = payloads
            .into_iter()
            .enumerate()
            .map(|(index, payload)| {
                self.sequence_number = self.sequence_number.wrapping_add(1);
                RtpPacket {
                    marker: index + 1 == count,
                    payload_type: self.payload_type,
                    sequence_number: self.sequence_number,
                    timestamp: self.rtp_timestamp,
                    ssrc: 0,
                    payload,
                }
            })
            .collect();
        Ok(packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parses the RFC 7741 descriptor, returning (PictureID, payload offset).
    fn vp8_descriptor(payload: &[u8]) -> (u16, usize) {
        assert!(payload[0] & 0x80 != 0 && payload[1] & 0x80 != 0);
        if payload[2] & 0x80 != 0 {
            (
                (u16::from(payload[2] & 0x7f) << 8) | u16::from(payload[3]),
                4,
            )
        } else {
            (u16::from(payload[2]), 3)
        }
    }

    fn frame_period() -> Duration {
        Duration::from_secs(1) / 30
    }

    #[test]
    fn packetizer_keeps_vp8_identity_across_access_units() {
        let mut packetizer = Packetizer::new(Box::new(Vp8Payloader::new(true)), 96, 1200);
        let (mut sequence, mut timestamp, mut picture_id) = (0u16, 0u32, 0u16);
        for frame in 0..4u32 {
            let data = vec![frame as u8; 1000 + frame as usize * 1100];
            let packets = packetizer
                .packetize(&data, frame_period() * frame, frame_period())
                .expect("valid access unit");
            let mut decoded = Vec::new();
            for (index, packet) in packets.iter().enumerate() {
                let (id, offset) = vp8_descriptor(&packet.payload);
                decoded.extend_from_slice(&packet.payload[offset..]);
                if frame > 0 || index > 0 {
                    assert_eq!(
                        packet.sequence_number,
                        sequence.wrapping_add(1),
                        "sequence changed between encoded representations"
                    );
                }
                if index == 0 && frame > 0 {
                    let delta = packet.timestamp.wrapping_sub(timestamp);
                    assert!(
                        (2999..=3001).contains(&delta),
                        "source clock delta = {delta}"
                    );
                    assert_eq!(id, picture_id + 1, "VP8 picture identity restarted");
                }
                assert_eq!(
                    packet.marker,
                    index == packets.len() - 1,
                    "access unit marker did not match frame boundary"
                );
                assert!(packet.marshal_size() <= 1200);
                (sequence, timestamp, picture_id) = (packet.sequence_number, packet.timestamp, id);
            }
            assert_eq!(
                data, decoded,
                "packetization changed the shared encoded payload"
            );
        }
        assert_eq!(
            packetizer.packetize(&[1], Duration::ZERO, frame_period()),
            Err(Error::InvalidTimestamp),
            "backward source timestamp"
        );
        let packets = packetizer
            .packetize(&[1], Duration::from_secs(5), frame_period())
            .expect("dormant source");
        assert_eq!(packets.len(), 1);
        assert_eq!(
            packets[0].sequence_number,
            sequence.wrapping_add(1),
            "dormant source did not retain packet identity"
        );
        let advance = packets[0].timestamp.wrapping_sub(timestamp);
        assert!(
            (440_999..=441_001).contains(&advance),
            "dormant source clock advanced {advance} ticks, want 4.9 seconds"
        );
    }

    #[test]
    fn packetizer_rejects_invalid_access_units() {
        let mut packetizer = Packetizer::new(Box::new(Vp8Payloader::new(true)), 96, 1200);
        for (data, duration) in [
            (Vec::new(), frame_period()),
            (vec![0; MAX_ACCESS_UNIT_BYTES + 1], frame_period()),
            (vec![0], Duration::ZERO),
        ] {
            assert_eq!(
                packetizer.packetize(&data, Duration::ZERO, duration),
                Err(Error::InvalidAccessUnit)
            );
        }
        // A rejected unit does not start the source timeline.
        assert!(
            packetizer
                .packetize(&[0], Duration::ZERO, frame_period())
                .is_ok()
        );
    }

    #[test]
    fn maximum_access_unit_fits_the_packet_window() {
        let mut packetizer = Packetizer::new(Box::new(Vp8Payloader::new(true)), 96, 1200);
        let packets = packetizer
            .packetize(
                &vec![0; MAX_ACCESS_UNIT_BYTES],
                Duration::ZERO,
                frame_period(),
            )
            .expect("maximum access unit");
        assert!(packets.len() <= MAX_PACKET_WINDOW);
    }
}
