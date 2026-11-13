//! Opus encoding of the capture helper's fixed 48 kHz stereo PCM16 frames.

use opus_pure::{Application, OpusEncoder};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 960;
pub const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * 2;
/// Opus RTP keeps each frame intact. Bound VBR peaks at the encoder, reserving
/// 48 bytes for RTP, header extensions and SRTP within a 1200-byte datagram.
pub const MAX_PACKET_BYTES: usize = 1_152;
pub const DEFAULT_BITRATE: u32 = 128_000;
pub const MIN_BITRATE: u32 = 6_000;
pub const MAX_BITRATE: u32 = 510_000;
/// Keep the 20 ms target below the per-packet ceiling, including VBR headroom.
pub const MAX_SEND_BITRATE: u32 = 320_000;
const DEFAULT_COMPLEXITY: i32 = 5;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("native audio bitrate is outside the Opus range")]
    BitrateOutOfRange,
    #[error("native audio PCM frame is invalid")]
    InvalidFrame,
    #[error("native audio encoder returned an invalid packet")]
    InvalidPacket,
    #[error("native audio encoder failed: {0}")]
    Codec(#[from] opus_pure::Error),
}

/// Adapts the capture process's PCM frames to WebRTC Opus packets. Buffers are
/// owned by the encoder and reused; the returned packet borrows the encoder,
/// so it is consumed before the next call.
pub struct Encoder {
    codec: OpusEncoder,
    pcm: Box<[i16; FRAME_SAMPLES * CHANNELS]>,
    packet: Box<[u8; MAX_PACKET_BYTES]>,
}

impl std::fmt::Debug for Encoder {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Encoder")
            .field("bitrate", &self.bitrate())
            .finish_non_exhaustive()
    }
}

fn valid_bitrate(bitrate: u32) -> bool {
    (MIN_BITRATE..=MAX_BITRATE).contains(&bitrate)
}

impl Encoder {
    pub fn new(bitrate: u32) -> Result<Self, Error> {
        if !valid_bitrate(bitrate) {
            return Err(Error::BitrateOutOfRange);
        }
        let mut codec = OpusEncoder::new(SAMPLE_RATE as i32, CHANNELS, Application::Audio)?;
        codec.bitrate_bps = bitrate as i32;
        codec.complexity = DEFAULT_COMPLEXITY;
        codec.use_dtx = false;
        codec.use_inband_fec = false;
        Ok(Self {
            codec,
            pcm: Box::new([0; FRAME_SAMPLES * CHANNELS]),
            packet: Box::new([0; MAX_PACKET_BYTES]),
        })
    }

    pub fn bitrate(&self) -> u32 {
        u32::try_from(self.codec.bitrate_bps).unwrap_or(0)
    }

    /// Changes the target within the same encoder generation. A rejected value
    /// leaves the active setting unchanged.
    pub fn set_bitrate(&mut self, bitrate: u32) -> Result<(), Error> {
        if !valid_bitrate(bitrate) {
            return Err(Error::BitrateOutOfRange);
        }
        self.codec.bitrate_bps = bitrate as i32;
        Ok(())
    }

    /// Encodes one interleaved little-endian PCM16 frame into an Opus packet
    /// backed by the encoder's reusable output buffer.
    pub fn encode(&mut self, pcm: &[u8]) -> Result<&[u8], Error> {
        if pcm.len() != FRAME_BYTES {
            return Err(Error::InvalidFrame);
        }
        for (sample, bytes) in self.pcm.iter_mut().zip(pcm.chunks_exact(2)) {
            *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        // The output length is the packet byte budget, bounding VBR peaks.
        let count = self
            .codec
            .encode_s16(&self.pcm[..], FRAME_SAMPLES, &mut self.packet[..])?;
        if count == 0 || count > self.packet.len() {
            return Err(Error::InvalidPacket);
        }
        Ok(&self.packet[..count])
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use opus_pure::OpusDecoder;

    use super::*;

    fn decoder() -> OpusDecoder {
        OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).expect("decoder")
    }

    fn put(pcm: &mut [u8], frame: usize, channel: usize, sample: i16) {
        let offset = (frame * CHANNELS + channel) * 2;
        pcm[offset..offset + 2].copy_from_slice(&sample.to_le_bytes());
    }

    #[test]
    fn encode_produces_a_reusable_opus_packet() {
        let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
        let pcm = vec![0; FRAME_BYTES];
        let (first, first_address) = {
            let packet = encoder.encode(&pcm).expect("first packet");
            (packet.to_vec(), packet.as_ptr())
        };
        assert!(!first.is_empty());
        let second = encoder.encode(&pcm).expect("second packet");
        assert!(!second.is_empty());
        assert_eq!(
            first, second,
            "silence packet changed between identical frames"
        );
        assert_eq!(
            first_address,
            second.as_ptr(),
            "encoder did not reuse its output buffer"
        );
    }

    #[test]
    fn encode_rejects_non_frame_sized_pcm() {
        let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
        assert!(
            encoder.encode(&vec![0; FRAME_BYTES - 2]).is_err(),
            "short frame accepted"
        );
        assert!(
            encoder.encode(&vec![0; FRAME_BYTES + 2]).is_err(),
            "long frame accepted"
        );
    }

    #[test]
    fn encode_preserves_audible_pcm() {
        let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
        let mut pcm = vec![0; FRAME_BYTES];
        for frame in 0..FRAME_SAMPLES {
            let sample =
                ((2.0 * PI * 440.0 * frame as f64 / f64::from(SAMPLE_RATE)).sin() * 8_000.0) as i16;
            for channel in 0..CHANNELS {
                put(&mut pcm, frame, channel, sample);
            }
        }
        let packet = encoder.encode(&pcm).expect("encode").to_vec();
        let mut decoded = vec![0i16; FRAME_SAMPLES * CHANNELS];
        let samples = decoder()
            .decode_s16(&packet, FRAME_SAMPLES, &mut decoded)
            .expect("decode");
        let energy: i64 = decoded[..samples * CHANNELS]
            .iter()
            .map(|sample| i64::from(*sample).abs())
            .sum();
        assert!(energy != 0, "Opus round trip produced silence");
    }

    #[test]
    fn encode_preserves_stereo_separation() {
        for bitrate in [64_000, 128_000, 192_000, 256_000, 320_000] {
            let mut encoder = Encoder::new(bitrate).expect("encoder");
            let mut decoder = decoder();
            let mut pcm = vec![0; FRAME_BYTES];
            let mut decoded = vec![0i16; FRAME_SAMPLES * CHANNELS];
            let frequencies = [500.0, 1500.0];
            // tones[channel][tone] = (real, imaginary) correlation.
            let mut tones = [[(0.0f64, 0.0f64); 2]; 2];
            let phase = |frequency: f64, block: usize, frame: usize| {
                2.0 * PI * frequency * (block * FRAME_SAMPLES + frame) as f64
                    / f64::from(SAMPLE_RATE)
            };
            for block in 0..30 {
                for frame in 0..FRAME_SAMPLES {
                    for (channel, frequency) in frequencies.iter().enumerate() {
                        let sample = (8_000.0 * phase(*frequency, block, frame).sin()) as i16;
                        put(&mut pcm, frame, channel, sample);
                    }
                }
                let packet = encoder.encode(&pcm).expect("encode").to_vec();
                let count = decoder
                    .decode_s16(&packet, FRAME_SAMPLES, &mut decoded)
                    .expect("decode");
                assert_eq!(count, FRAME_SAMPLES);
                if block < 5 {
                    continue; // Exclude codec startup; compare frequency energy, not phase delay.
                }
                for frame in 0..count {
                    for (tone, frequency) in frequencies.iter().enumerate() {
                        let angle = phase(*frequency, block, frame);
                        for (channel, row) in tones.iter_mut().enumerate() {
                            let value = f64::from(decoded[frame * CHANNELS + channel]);
                            row[tone].0 += value * angle.cos();
                            row[tone].1 += value * angle.sin();
                        }
                    }
                }
            }
            for (channel, row) in tones.iter().enumerate() {
                let (wanted, other) = (row[channel], row[1 - channel]);
                let level = wanted.0.hypot(wanted.1);
                let leak = other.0.hypot(other.1);
                assert!(
                    level >= 1.0 && level >= 10.0 * leak,
                    "{bitrate} bps channel {channel} lost separation: signal {level}, leakage {leak}"
                );
            }
        }
    }

    #[test]
    fn new_encoder_rejects_out_of_range_bitrate() {
        for bitrate in [0, 5_999, 510_001] {
            assert!(
                Encoder::new(bitrate).is_err(),
                "bitrate {bitrate} was accepted"
            );
        }
    }

    #[test]
    fn encoder_updates_bitrate_within_the_same_generation() {
        let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
        let mut decoder = decoder();
        let pcm = vec![0; FRAME_BYTES];
        let mut decoded = vec![0i16; FRAME_SAMPLES * CHANNELS];
        for bitrate in [64_000, 192_000, 256_000, 320_000, 96_000, DEFAULT_BITRATE] {
            encoder.set_bitrate(bitrate).expect("set bitrate");
            assert_eq!(encoder.bitrate(), bitrate, "SetBitrate({bitrate}) readback");
            let packet = encoder.encode(&pcm).expect("encode").to_vec();
            let count = decoder
                .decode_s16(&packet, FRAME_SAMPLES, &mut decoded)
                .expect("decode");
            assert_eq!(count, FRAME_SAMPLES, "decode after bitrate {bitrate}");
        }
        for bitrate in [0, 5_999, 510_001] {
            assert!(
                encoder.set_bitrate(bitrate).is_err(),
                "invalid live bitrate {bitrate}"
            );
            assert_eq!(
                encoder.bitrate(),
                DEFAULT_BITRATE,
                "rejected bitrate replaced setting"
            );
        }
    }

    #[test]
    fn encode_bounds_transient_packets_across_bitrate_changes() {
        let mut encoder = Encoder::new(DEFAULT_BITRATE).expect("encoder");
        let mut decoder = decoder();
        let mut pcm = vec![0; FRAME_BYTES];
        let mut decoded = vec![0i16; FRAME_SAMPLES * CHANNELS];
        // Deterministic xorshift noise; the exact sequence is irrelevant.
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        let mut noise = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64
        };
        for bitrate in [
            DEFAULT_BITRATE,
            MAX_SEND_BITRATE,
            MIN_BITRATE,
            12_345,
            MAX_SEND_BITRATE,
        ] {
            encoder.set_bitrate(bitrate).expect("set bitrate");
            for block in 0..100 {
                for frame in 0..FRAME_SAMPLES {
                    for channel in 0..CHANNELS {
                        let sample = match (block / 10) % 5 {
                            1 => {
                                26_000.0
                                    * (2.0
                                        * PI
                                        * (500 + channel * 1000) as f64
                                        * (block * FRAME_SAMPLES + frame) as f64
                                        / f64::from(SAMPLE_RATE))
                                    .sin()
                            }
                            2 => 32_767.0 * (noise() * 2.0 - 1.0),
                            3 if frame < 10 => 32_767.0,
                            4 => (32_767 - 65_535 * (frame % 2) as i32) as f64,
                            _ => 0.0,
                        };
                        put(&mut pcm, frame, channel, sample as i16);
                    }
                }
                let packet = encoder.encode(&pcm).expect("encode").to_vec();
                // Opus RTP cannot fragment a frame. Reserve RTP/extensions/SRTP space.
                assert!(
                    packet.len() + 48 <= 1200,
                    "{bitrate} bps block {block} exceeded packet budget: {} bytes",
                    packet.len()
                );
                let count = decoder
                    .decode_s16(&packet, FRAME_SAMPLES, &mut decoded)
                    .expect("decode");
                assert_eq!(count, FRAME_SAMPLES, "decode {bitrate} bps block {block}");
            }
        }
    }
}
