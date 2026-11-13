//! Local encoded access units into the same `Source` that received RTP feeds.
//! Its capture/codec owner serializes calls and fences replacement input.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use piik_encoded::{Frame, H264Payloader, Packetizer, Payloader, Vp8Payloader};
use piik_media_forwarding::{
    CodecKind, InboundPacket, SenderReport, Source, SourceChanges, SourceOptions, SourceView,
    Unwrapper, ntp_from_system_time,
};

use crate::{Error, SourcePacket};

const MTU: u16 = 1200;

/// Pairs the media core's monotonic clock with wall time at one instant, so
/// a capture time converts to both without reading either clock again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockAnchor {
    pub instant: Instant,
    pub system: SystemTime,
}

impl ClockAnchor {
    pub fn now() -> Self {
        Self {
            instant: Instant::now(),
            system: SystemTime::now(),
        }
    }

    pub fn local(&self, time: SystemTime) -> Option<Instant> {
        match time.duration_since(self.system) {
            Ok(ahead) => self.instant.checked_add(ahead),
            Err(behind) => self.instant.checked_sub(behind.duration()),
        }
    }

    pub fn system(&self, local: Instant) -> SystemTime {
        match local.checked_duration_since(self.instant) {
            Some(ahead) => self.system + ahead,
            None => self.system - self.instant.duration_since(local),
        }
    }
}

/// Where capture times come from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputClock {
    /// Capture times are local wall time; the anchor maps them to `Instant`.
    Local(ClockAnchor),
    /// Capture times are on a received original layer's RTP/NTP clock.
    /// Every write passes that original source.
    Source { layer: usize },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EncodedLayerStats {
    pub packets: u32,
    /// RTP payload bytes, as an RTCP sender report counts them.
    pub octets: u64,
    /// Codec access-unit bytes in this generation.
    pub codec_bytes: u64,
    /// Serialized RTP bytes (header and payload) in this generation.
    pub rtp_bytes: u64,
}

/// One written access unit: packets for every output, then the source events
/// its owner reconciles.
#[derive(Clone, Debug, Default)]
pub struct EncodedWrite {
    pub packets: Vec<SourcePacket>,
    pub changes: SourceChanges,
}

struct Layer {
    packetizer: Packetizer,
    sequence: Unwrapper,
    timestamp: Unwrapper,
    stats: EncodedLayerStats,
}

pub struct EncodedSource {
    source: Source,
    layers: Vec<Layer>,
    clock: InputClock,
    input_pts: Duration,
    anchor_pts: Duration,
    anchor_time: SystemTime,
    origin: Option<SystemTime>,
    has_input: bool,
}

impl EncodedSource {
    pub fn new(options: SourceOptions, clock: InputClock) -> Result<Self, Error> {
        let mut source = Source::new(options)?;
        let codec = source.codec().clone();
        let mut layers = Vec::with_capacity(source.layer_count());
        for layer in 0..source.layer_count() {
            let payloader: Box<dyn Payloader> = match codec.kind {
                CodecKind::Vp8 => Box::new(Vp8Payloader::new(true)),
                CodecKind::H264 => Box::new(H264Payloader::new()),
                CodecKind::Opus => return Err(Error::Invalid),
            };
            // These identities stay inside this source; each output supplies
            // its independently negotiated wire identity.
            source.bind_layer(layer, layer as u32 + 1)?;
            layers.push(Layer {
                packetizer: Packetizer::new(payloader, codec.payload_type, MTU),
                sequence: Unwrapper::sequence(),
                timestamp: Unwrapper::timestamp(),
                stats: EncodedLayerStats::default(),
            });
        }
        Ok(Self {
            source,
            layers,
            clock,
            input_pts: Duration::ZERO,
            anchor_pts: Duration::ZERO,
            anchor_time: SystemTime::UNIX_EPOCH,
            origin: None,
            has_input: false,
        })
    }

    pub fn source(&self) -> &Source {
        &self.source
    }

    pub fn source_mut(&mut self) -> &mut Source {
        &mut self.source
    }

    pub fn layer_stats(&self, layer: usize) -> Option<EncodedLayerStats> {
        self.layers.get(layer).map(|layer| layer.stats)
    }

    pub fn input_pts(&self) -> Option<Duration> {
        self.has_input.then_some(self.input_pts)
    }

    /// Starts one capture input at `pts` (strictly increasing within a
    /// generation) captured at `at` on the input clock.
    pub fn begin_frame(&mut self, pts: Duration, at: SystemTime) -> Result<(), Error> {
        if self.source.is_closed() {
            return Err(Error::Closed);
        }
        if self.has_input && pts <= self.input_pts {
            return Err(piik_encoded::Error::InvalidTimestamp.into());
        }
        if !self.has_input {
            (self.anchor_pts, self.anchor_time) = (pts, at);
        }
        self.origin.get_or_insert(at);
        (self.has_input, self.input_pts) = (true, pts);
        Ok(())
    }

    /// Packetizes one complete codec output of the current input. `original`
    /// is the clock source for [`InputClock::Source`].
    pub fn write_frame(
        &mut self,
        layer: usize,
        frame: &Frame,
        original: Option<&Source>,
    ) -> Result<EncodedWrite, Error> {
        let has_input = self.has_input;
        let (anchor_pts, input_pts) = (self.anchor_pts, self.input_pts);
        let Some(state) = self.layers.get_mut(layer) else {
            return Err(Error::NoInput);
        };
        if !has_input || frame.pts < anchor_pts || frame.pts > input_pts {
            return Err(Error::NoInput);
        }
        // Each encoder generation must begin every layer with an
        // independently decodable frame.
        if state.stats.rtp_bytes == 0 && !frame.recovery {
            return Ok(EncodedWrite::default());
        }
        let source_time = self.anchor_time + (frame.pts - anchor_pts);
        let origin = self.origin.unwrap_or(source_time);
        let elapsed = source_time.duration_since(origin).unwrap_or_default();
        // Resolve the clock before any packet enters the source.
        let local = match self.clock {
            InputClock::Local(anchor) => anchor.local(source_time),
            InputClock::Source { layer } => original
                .ok_or(Error::NoClock)?
                .local_time(layer, source_time),
        }
        .ok_or(Error::NoClock)?;
        let packets = state
            .packetizer
            .packetize(&frame.data, elapsed, frame.duration)?;
        let kind = self.source.codec().kind;
        let mut write = EncodedWrite::default();
        for packet in &packets {
            let size = packet.marshal_size();
            let info = InboundPacket::new(
                kind,
                layer as i32,
                state.sequence.extend(u64::from(packet.sequence_number)),
                state.timestamp.extend(u64::from(packet.timestamp)),
                &packet.payload,
                local,
            )
            .marker(packet.marker)
            .ssrc(layer as u32 + 1)
            .payload_type(packet.payload_type)
            .size(size)
            .reference(Some(source_time));
            let changes = self.source.write_rtp(layer, &info)?;
            write.changes.layers |= changes.layers;
            write.changes.bitrates |= changes.bitrates;
            write.changes.max_published = changes.max_published.or(write.changes.max_published);
            write.changes.max_temporal_seen = changes
                .max_temporal_seen
                .or(write.changes.max_temporal_seen);
            state.stats.packets = state.stats.packets.wrapping_add(1);
            state.stats.octets += packet.payload.len() as u64;
            state.stats.rtp_bytes += size as u64;
            write.packets.push(SourcePacket {
                info,
                payload: Arc::from(packet.payload.as_slice()),
            });
        }
        state.stats.codec_bytes += frame.data.len() as u64;
        let Some(first) = packets.first() else {
            return Ok(write);
        };
        // Correlate with the actual capture clock, not encoder completion.
        self.source.set_correlation(
            layer,
            SenderReport {
                rtp_timestamp: first.timestamp,
                ntp_time: ntp_from_system_time(source_time),
                packets: state.stats.packets,
                octets: state.stats.octets,
                at: local,
                at_adjusted: local,
            },
        )?;
        Ok(write)
    }

    /// Converts a source-RTP allocation to an encoder AU-payload budget. The
    /// framing ratio is this generation's; the rate is the tracker window's.
    pub fn codec_budget(&self, layer: usize, rtp_budget: i64) -> i64 {
        let Some(stats) = self.layers.get(layer).map(|layer| layer.stats) else {
            return rtp_budget.max(0);
        };
        if stats.rtp_bytes <= stats.codec_bytes {
            return rtp_budget.max(0);
        }
        let (_, rates) = self.source.layered_bitrate();
        let rate = rates.get(layer).map_or(0, |rates| rates[0]);
        let overhead =
            rate as f64 * (stats.rtp_bytes - stats.codec_bytes) as f64 / stats.rtp_bytes as f64;
        (rtp_budget - overhead.ceil() as i64).max(0)
    }

    /// A replacement input resets PTS; every layer keeps its RTP identity and
    /// elapsed capture clock, including dormant time.
    pub fn begin_generation(&mut self) {
        self.has_input = false;
        for layer in &mut self.layers {
            layer.stats.codec_bytes = 0;
            layer.stats.rtp_bytes = 0;
        }
    }

    /// Test seam for framing that removes codec bytes (Annex-B delimiters).
    #[cfg(test)]
    fn set_codec_bytes(&mut self, layer: usize, bytes: u64) {
        self.layers[layer].stats.codec_bytes = bytes;
    }

    pub fn close(&mut self) {
        self.source.close();
    }
}

#[cfg(test)]
mod tests;
