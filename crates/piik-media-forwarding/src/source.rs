//! Finite RTP layers of one forwarded source: identity binding, per-layer
//! stream tracking, layered bitrate measurement and RTP/NTP correlation.
//!
//! The transport owns RTP parsing, dedupe, NACK and RTX (str0m); this type
//! sees accepted packets only.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::layer::{Bitrates, MAX_SPATIAL_LAYERS, MAX_TEMPORAL_LAYERS};
use crate::packet::{CodecKind, InboundPacket};

/// Screen-share bitrate report interval. A layer's rate is the completed
/// window's average; the first value follows one full window.
pub const BITRATE_WINDOW: Duration = Duration::from_secs(4);
/// Assumption: a layer with no media for this long is stopped. The frame
/// tracker tolerates very low frame rates; Piik's native idle refresh is
/// 1 fps, so this keeps quiet but live layers available.
pub const STREAM_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum initial packet buffer for video and audio respectively.
const MIN_VIDEO_PACKETS: usize = 300;
const AUDIO_PACKETS: usize = 70;
const NTP_UNIX_OFFSET_SECS: u64 = 2_208_988_800;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LayerFormat {
    pub width: u32,
    pub height: u32,
    /// Declared bitrate ceiling in bits per second; zero is unknown.
    pub bitrate: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Codec {
    pub kind: CodecKind,
    pub payload_type: u8,
    pub fmtp: String,
    pub channels: u16,
}

#[derive(Clone, Debug)]
pub struct SourceOptions {
    pub id: String,
    pub stream_id: String,
    pub codec: Codec,
    pub formats: Vec<LayerFormat>,
    /// Packet window bound for queues fed by this source; 0 selects the default.
    pub max_packets: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("forwarded source is invalid")]
    Invalid,
    #[error("forwarded packet bound is invalid")]
    PacketBound,
    #[error("forwarded video layer count is invalid")]
    LayerCount,
    #[error("forwarded video layer format is invalid")]
    LayerFormat,
    #[error("forwarded audio cannot declare video layers")]
    AudioLayers,
    #[error("forwarded video layer count changed")]
    LayerCountChanged,
    #[error("forwarded video layer dimensions are invalid")]
    Dimensions,
    #[error("forwarded source is closed")]
    Closed,
    #[error("forwarded layer is invalid")]
    InvalidLayer,
    #[error("forwarded layer is already bound")]
    DuplicateLayer,
    #[error("forwarded video layers cannot share an SSRC")]
    SharedSsrc,
    #[error("forwarded RTP packet does not match its layer")]
    PacketMismatch,
    #[error("forwarded sender report does not match its layer")]
    ReportMismatch,
}

/// One RTP/NTP correlation. `at` is the local arrival (or known input) time;
/// `at_adjusted` removes the estimated propagation delay from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SenderReport {
    pub rtp_timestamp: u32,
    pub ntp_time: u64,
    pub packets: u32,
    pub octets: u64,
    pub at: Instant,
    pub at_adjusted: Instant,
}

/// Events for subscribers after a source mutation. The owner forwards these
/// to each output (`OutputAllocator::reconcile`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SourceChanges {
    pub layers: bool,
    pub bitrates: bool,
    pub max_published: Option<i32>,
    pub max_temporal_seen: Option<i32>,
}

impl SourceChanges {
    pub fn any(&self) -> bool {
        self.layers
            || self.bitrates
            || self.max_published.is_some()
            || self.max_temporal_seen.is_some()
    }

    fn merge(&mut self, other: Self) {
        self.layers |= other.layers;
        self.bitrates |= other.bitrates;
        self.max_published = other.max_published.or(self.max_published);
        self.max_temporal_seen = other.max_temporal_seen.or(self.max_temporal_seen);
    }
}

/// What allocation needs from a source. Implemented by [`Source`]; tests and
/// publication layer filters supply their own views.
pub trait SourceView {
    /// Available spatial layers in ascending order with their measured rates.
    fn layered_bitrate(&self) -> (Vec<i32>, Bitrates);
    /// Declared bitrate ceiling per spatial layer; zero is unknown.
    fn layer_ceilings(&self) -> Vec<i64>;
    /// A live local encoder follows this layer's budget below its ceiling.
    fn rate_controlled(&self, layer: i32) -> bool;
    /// Highest available spatial layer, or -1.
    fn max_published_layer(&self) -> i32;
    /// Highest temporal layer observed, or -1.
    fn max_temporal_layer_seen(&self) -> i32;
}

#[derive(Clone, Debug)]
struct Tracker {
    active: bool,
    last_media: Option<Instant>,
    window_start: Option<Instant>,
    window_bytes: [u64; MAX_TEMPORAL_LAYERS],
    max_temporal: i32,
    rates: [i64; MAX_TEMPORAL_LAYERS],
}

impl Tracker {
    const fn new() -> Self {
        Self {
            active: false,
            last_media: None,
            window_start: None,
            window_bytes: [0; MAX_TEMPORAL_LAYERS],
            max_temporal: -1,
            rates: [0; MAX_TEMPORAL_LAYERS],
        }
    }

    /// Completes the current window; returns whether the reported rates changed.
    fn roll(&mut self, now: Instant) -> bool {
        let Some(start) = self.window_start else {
            return false;
        };
        let elapsed = now.saturating_duration_since(start);
        if elapsed < BITRATE_WINDOW {
            return false;
        }
        let mut rates = [0i64; MAX_TEMPORAL_LAYERS];
        let mut cumulative = 0u64;
        for (temporal, bytes) in self.window_bytes.iter().enumerate() {
            cumulative += bytes;
            // Cumulative rates stop at the highest temporal layer observed.
            if temporal as i32 <= self.max_temporal.max(0) {
                rates[temporal] = (cumulative as f64 * 8.0 / elapsed.as_secs_f64()) as i64;
            }
        }
        self.window_bytes = [0; MAX_TEMPORAL_LAYERS];
        self.window_start = Some(now);
        let changed = rates != self.rates;
        self.rates = rates;
        changed
    }
}

#[derive(Clone, Debug)]
struct Layer {
    ssrc: Option<u32>,
    tracker: Tracker,
    report: Option<SenderReport>,
    known_input_clock: bool,
}

/// One source owner's forwarding state. Its owner serializes calls with the
/// capture/source generation and supplies time.
#[derive(Clone, Debug)]
pub struct Source {
    id: String,
    stream_id: String,
    codec: Codec,
    formats: Vec<LayerFormat>,
    layers: Vec<Layer>,
    max_packets: usize,
    max_published: i32,
    max_temporal_seen: i32,
    lowest_rate_controlled: bool,
    closed: bool,
}

impl Source {
    pub fn new(options: SourceOptions) -> Result<Self, SourceError> {
        let audio = options.codec.kind == CodecKind::Opus;
        if options.id.is_empty() || options.stream_id.is_empty() {
            return Err(SourceError::Invalid);
        }
        let (minimum, default_packets) = if audio {
            (AUDIO_PACKETS, AUDIO_PACKETS)
        } else {
            (MIN_VIDEO_PACKETS, piik_encoded::MAX_PACKET_WINDOW)
        };
        let max_packets = if options.max_packets == 0 {
            default_packets
        } else {
            options.max_packets
        };
        if !(minimum..=1 << 16).contains(&max_packets) {
            return Err(SourceError::PacketBound);
        }
        let count = if audio {
            if !options.formats.is_empty() {
                return Err(SourceError::AudioLayers);
            }
            1
        } else {
            validate_formats(&options.formats)?;
            options.formats.len()
        };
        Ok(Self {
            id: options.id,
            stream_id: options.stream_id,
            codec: options.codec,
            formats: options.formats,
            layers: vec![
                Layer {
                    ssrc: None,
                    tracker: Tracker::new(),
                    report: None,
                    known_input_clock: false
                };
                count
            ],
            max_packets,
            max_published: -1,
            max_temporal_seen: -1,
            lowest_rate_controlled: false,
            closed: false,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub fn codec(&self) -> &Codec {
        &self.codec
    }

    pub fn formats(&self) -> &[LayerFormat] {
        &self.formats
    }

    pub fn layer_count(&self) -> usize {
        self.layers.len()
    }

    pub fn max_packets(&self) -> usize {
        self.max_packets
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// A replacement must keep the outgoing transport contract unchanged.
    pub fn compatible_with(&self, next: &Self) -> bool {
        !next.closed
            && self.codec == next.codec
            && self.id == next.id
            && self.stream_id == next.stream_id
            && self.max_packets == next.max_packets
            && self.layers.len() == next.layers.len()
    }

    /// The live codec owner sets this after startup and clears it at retirement.
    pub fn set_lowest_layer_rate_controlled(&mut self, controlled: bool) {
        self.lowest_rate_controlled = controlled;
    }

    /// Changes declared layer metadata without touching bindings, trackers or
    /// clocks. Zero values remain unknown. The owner then reconciles outputs.
    pub fn update_formats(&mut self, formats: Vec<LayerFormat>) -> Result<(), SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        if !self.codec.kind.is_video() || formats.len() != self.layers.len() {
            return Err(SourceError::LayerCountChanged);
        }
        validate_formats(&formats)?;
        self.formats = formats;
        Ok(())
    }

    /// Publishes actual encoder dimensions; the bitrate ceiling is unchanged.
    pub fn update_dimensions(
        &mut self,
        layer: usize,
        width: u32,
        height: u32,
    ) -> Result<(), SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        if !self.codec.kind.is_video() || layer >= self.layers.len() || width == 0 || height == 0 {
            return Err(SourceError::Dimensions);
        }
        let format = &mut self.formats[layer];
        format.width = width;
        format.height = height;
        Ok(())
    }

    /// Installs one layer identity. A different SSRC needs a new source.
    pub fn bind_layer(&mut self, layer: usize, ssrc: u32) -> Result<(), SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        if layer >= self.layers.len() || ssrc == 0 {
            return Err(SourceError::InvalidLayer);
        }
        if self.layers[layer].ssrc.is_some() {
            return Err(SourceError::DuplicateLayer);
        }
        if self
            .layers
            .iter()
            .any(|existing| existing.ssrc == Some(ssrc))
        {
            return Err(SourceError::SharedSsrc);
        }
        self.layers[layer].ssrc = Some(ssrc);
        Ok(())
    }

    pub fn layer_ssrc(&self, layer: usize) -> Option<u32> {
        self.layers.get(layer).and_then(|state| state.ssrc)
    }

    fn bound(&self, layer: usize) -> Result<&Layer, SourceError> {
        if self.closed {
            return Err(SourceError::Closed);
        }
        self.layers
            .get(layer)
            .filter(|state| state.ssrc.is_some())
            .ok_or(SourceError::InvalidLayer)
    }

    fn bound_mut(&mut self, layer: usize) -> Result<&mut Layer, SourceError> {
        self.bound(layer)?;
        Ok(&mut self.layers[layer])
    }

    /// Records one accepted packet. Padding-only packets do not establish a
    /// layer or count toward its media bitrate.
    pub fn write_rtp(
        &mut self,
        layer: usize,
        packet: &InboundPacket,
    ) -> Result<SourceChanges, SourceError> {
        let payload_type = self.codec.payload_type;
        let state = self.bound_mut(layer)?;
        if Some(packet.ssrc) != state.ssrc
            || packet.payload_type != payload_type
            || packet.layer != layer as i32
        {
            return Err(SourceError::PacketMismatch);
        }
        let mut changes = self.poll(packet.arrival);
        if packet.padding_only {
            return Ok(changes);
        }
        let tracker = &mut self.layers[layer].tracker;
        let temporal = packet.temporal.clamp(0, MAX_TEMPORAL_LAYERS as i32 - 1);
        tracker.last_media = Some(packet.arrival);
        tracker.window_start.get_or_insert(packet.arrival);
        tracker.window_bytes[temporal as usize] += packet.size as u64;
        tracker.max_temporal = tracker.max_temporal.max(temporal);
        if !tracker.active {
            tracker.active = true;
            changes.layers = true;
        }
        if self.codec.kind.is_video() && temporal > self.max_temporal_seen {
            self.max_temporal_seen = temporal;
            changes.max_temporal_seen = Some(temporal);
        }
        changes.merge(self.refresh_published());
        Ok(changes)
    }

    /// Advances stream trackers: completes bitrate windows and stops layers
    /// without media. The owner calls this from its timeout path.
    pub fn poll(&mut self, now: Instant) -> SourceChanges {
        let mut changes = SourceChanges::default();
        if self.closed {
            return changes;
        }
        for state in &mut self.layers {
            let tracker = &mut state.tracker;
            if !tracker.active {
                continue;
            }
            if tracker
                .last_media
                .is_some_and(|last| now.saturating_duration_since(last) >= STREAM_STOP_TIMEOUT)
            {
                *tracker = Tracker::new();
                changes.layers = true;
                changes.bitrates = true;
                continue;
            }
            changes.bitrates |= tracker.roll(now);
        }
        changes.merge(self.refresh_published());
        changes
    }

    /// Earliest time `poll` can change state.
    pub fn poll_deadline(&self) -> Option<Instant> {
        self.layers
            .iter()
            .filter(|state| state.tracker.active)
            .flat_map(|state| {
                let tracker = &state.tracker;
                [
                    tracker.window_start.map(|start| start + BITRATE_WINDOW),
                    tracker.last_media.map(|last| last + STREAM_STOP_TIMEOUT),
                ]
            })
            .flatten()
            .min()
    }

    fn refresh_published(&mut self) -> SourceChanges {
        let highest = self
            .layers
            .iter()
            .rposition(|state| state.tracker.active)
            .map_or(-1, |layer| layer as i32);
        if highest == self.max_published || !self.codec.kind.is_video() {
            return SourceChanges::default();
        }
        self.max_published = highest;
        SourceChanges {
            max_published: Some(highest),
            ..Default::default()
        }
    }

    /// Stores a received report. `at_adjusted` carries the transport's delay
    /// estimate unless this layer already has a known input clock.
    pub fn sender_report(
        &mut self,
        layer: usize,
        ssrc: u32,
        report: SenderReport,
    ) -> Result<(), SourceError> {
        let state = self.bound_mut(layer)?;
        if state.ssrc != Some(ssrc) {
            return Err(SourceError::ReportMismatch);
        }
        state.report = Some(report);
        if state.known_input_clock {
            state.report = Some(SenderReport {
                at_adjusted: report.at,
                ..report
            });
        }
        Ok(())
    }

    /// Accepted correlation for a layer, as subscribers should replay it.
    pub fn current_sender_report(&self, layer: usize) -> Option<SenderReport> {
        self.bound(layer).ok()?.report
    }

    /// Capture time for an RTP timestamp on this layer's source clock. The
    /// bounded input history makes the nearest 32-bit delta unambiguous.
    pub fn reference_time(&self, layer: usize, timestamp: u32) -> Option<SystemTime> {
        let report = self
            .current_sender_report(layer)
            .filter(|report| report.ntp_time != 0)?;
        let ticks = timestamp.wrapping_sub(report.rtp_timestamp) as i32;
        let delta = i128::from(ticks) * 1_000_000_000 / i128::from(self.codec.kind.clock_rate());
        offset_system_time(system_time_from_ntp(report.ntp_time), delta)
    }

    /// Projects a source-clock time through the current report onto the
    /// media core's local clock. Derived encoders share this offset.
    pub fn local_time(&self, layer: usize, source_time: SystemTime) -> Option<Instant> {
        let report = self
            .current_sender_report(layer)
            .filter(|report| report.ntp_time != 0)?;
        let origin = system_time_from_ntp(report.ntp_time);
        match source_time.duration_since(origin) {
            Ok(ahead) => report.at_adjusted.checked_add(ahead),
            Err(behind) => report.at_adjusted.checked_sub(behind.duration()),
        }
    }

    /// Shares an already received clock with an internal RTP copy. The original
    /// owns network-delay estimation; this layer still orders its own RTP.
    pub fn copy_correlation(&mut self, layer: usize, original: &Self) -> Result<(), SourceError> {
        original.bound(layer)?;
        let Some(report) = original.current_sender_report(layer) else {
            return Ok(());
        };
        self.set_correlation(
            layer,
            SenderReport {
                at: report.at_adjusted,
                ..report
            },
        )
    }

    /// Installs the source owner's mapping with its known local input time in
    /// `at`. Encoder completion is not a network-delay observation.
    pub fn set_correlation(
        &mut self,
        layer: usize,
        correlation: SenderReport,
    ) -> Result<(), SourceError> {
        let state = self.bound_mut(layer)?;
        state.known_input_clock = true;
        state.report = Some(SenderReport {
            at_adjusted: correlation.at,
            ..correlation
        });
        Ok(())
    }

    /// Retires the source; later writes and bindings fail.
    pub fn close(&mut self) {
        self.closed = true;
        self.lowest_rate_controlled = false;
        for state in &mut self.layers {
            state.tracker = Tracker::new();
            state.report = None;
        }
        self.max_published = -1;
    }
}

impl SourceView for Source {
    fn layered_bitrate(&self) -> (Vec<i32>, Bitrates) {
        let mut rates = Bitrates::default();
        let mut available = Vec::new();
        if self.closed || !self.codec.kind.is_video() {
            return (available, rates);
        }
        for (layer, state) in self.layers.iter().enumerate().take(MAX_SPATIAL_LAYERS) {
            if state.tracker.active {
                available.push(layer as i32);
                rates[layer] = state.tracker.rates;
            }
        }
        (available, rates)
    }

    fn layer_ceilings(&self) -> Vec<i64> {
        self.formats
            .iter()
            .map(|format| i64::from(format.bitrate))
            .collect()
    }

    fn rate_controlled(&self, layer: i32) -> bool {
        layer == 0 && !self.closed && self.lowest_rate_controlled
    }

    fn max_published_layer(&self) -> i32 {
        self.max_published
    }

    fn max_temporal_layer_seen(&self) -> i32 {
        self.max_temporal_seen
    }
}

fn validate_formats(formats: &[LayerFormat]) -> Result<(), SourceError> {
    if formats.is_empty() || formats.len() > MAX_SPATIAL_LAYERS {
        return Err(SourceError::LayerCount);
    }
    if formats
        .iter()
        .any(|format| (format.width == 0) != (format.height == 0))
    {
        return Err(SourceError::LayerFormat);
    }
    Ok(())
}

fn offset_system_time(time: SystemTime, nanos: i128) -> Option<SystemTime> {
    let magnitude = Duration::from_nanos(u64::try_from(nanos.unsigned_abs()).ok()?);
    if nanos >= 0 {
        time.checked_add(magnitude)
    } else {
        time.checked_sub(magnitude)
    }
}

/// 64-bit NTP (32.32 fixed point since 1900) for a wall-clock time.
pub fn ntp_from_system_time(time: SystemTime) -> u64 {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let seconds = since.as_secs() + NTP_UNIX_OFFSET_SECS;
    // Rounded so a round trip is exact to the nanosecond.
    let fraction = ((u128::from(since.subsec_nanos()) << 32) + 500_000_000) / 1_000_000_000;
    (seconds << 32) + fraction as u64
}

pub fn system_time_from_ntp(ntp: u64) -> SystemTime {
    let seconds = (ntp >> 32).saturating_sub(NTP_UNIX_OFFSET_SECS);
    let nanos = ((u128::from(ntp & 0xffff_ffff) * 1_000_000_000 + (1 << 31)) >> 32) as u64;
    UNIX_EPOCH + Duration::from_secs(seconds) + Duration::from_nanos(nanos)
}

#[cfg(test)]
mod tests;
