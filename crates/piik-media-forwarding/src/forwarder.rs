//! One output's layer selection and identity projection for simulcast and VP8
//! temporal layers, plus the allocation primitives an output calls.
//!
//! Behavioral rules, checked by this crate's tests:
//! - Spatial moves need a keyframe. Without a current layer any keyframe at or
//!   below the target latches (opportunistic); otherwise a keyframe between
//!   current and target moves toward the target. A keyframe also restores the
//!   target temporal layer.
//! - VP8 temporal upgrades need a layer-sync (Y) frame for exactly the next
//!   temporal layer; downgrades apply at the current frame's marker.
//! - The max published spatial layer follows the latest notification; the max
//!   temporal layer seen only grows.
//! - A paused allocation (no target) invalidates the current layer, so resuming
//!   needs a keyframe and starts a new munger segment.

use std::time::{Instant, SystemTime};

use crate::layer::{
    Bitrates, DEFAULT_MAX_SPATIAL, DEFAULT_MAX_TEMPORAL, MAX_SPATIAL_LAYERS, MAX_TEMPORAL_LAYERS,
    PauseReason, VideoAllocation, VideoLayer, VideoTransition, rate,
};
use crate::munger::{RtpMunger, Translated, Vp8Munger};
use crate::packet::{CodecKind, InboundPacket, Vp8Rewrite};

/// One packet accepted for this output, on its own RTP timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Forwarded {
    pub ext_seq: u64,
    pub ext_timestamp: u64,
    pub marker: bool,
    pub vp8: Option<Vp8Rewrite>,
    /// Spatial layer and the temporal layer this packet was admitted under.
    pub layer: VideoLayer,
    /// First packet of a new segment (start, switch, resync or restart).
    pub switched: bool,
}

#[derive(Clone, Copy, Debug)]
struct Provisional {
    pub_muted: bool,
    max_seen: VideoLayer,
    max_layer: VideoLayer,
    current: VideoLayer,
    bitrates: Bitrates,
    allocated: VideoLayer,
}

impl Default for Provisional {
    fn default() -> Self {
        Self {
            pub_muted: false,
            max_seen: VideoLayer::INVALID,
            max_layer: VideoLayer::INVALID,
            current: VideoLayer::INVALID,
            bitrates: Bitrates::default(),
            allocated: VideoLayer::INVALID,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Forwarder {
    kind: CodecKind,
    pub_muted: bool,
    max_layer: VideoLayer,
    max_seen: VideoLayer,
    target: VideoLayer,
    current: VideoLayer,
    request_spatial: i32,
    last_allocation: VideoAllocation,
    provisional: Provisional,
    rtp: RtpMunger,
    vp8: Vp8Munger,
    last_reference: Option<SystemTime>,
    last_arrival: Option<Instant>,
}

impl Forwarder {
    pub fn new(kind: CodecKind) -> Self {
        Self {
            kind,
            pub_muted: false,
            max_layer: VideoLayer::new(DEFAULT_MAX_SPATIAL, DEFAULT_MAX_TEMPORAL),
            max_seen: VideoLayer::INVALID,
            target: VideoLayer::INVALID,
            current: VideoLayer::INVALID,
            request_spatial: -1,
            last_allocation: VideoAllocation::default(),
            provisional: Provisional::default(),
            rtp: RtpMunger::default(),
            vp8: Vp8Munger::default(),
            last_reference: None,
            last_arrival: None,
        }
    }

    pub fn kind(&self) -> CodecKind {
        self.kind
    }

    pub fn max_layer(&self) -> VideoLayer {
        self.max_layer
    }

    pub fn set_max_spatial_layer(&mut self, spatial: i32) {
        self.max_layer.spatial = spatial.min(DEFAULT_MAX_SPATIAL);
    }

    pub fn set_max_temporal_layer(&mut self, temporal: i32) {
        self.max_layer.temporal = temporal.min(DEFAULT_MAX_TEMPORAL);
    }

    pub fn set_max_published_layer(&mut self, spatial: i32) {
        self.max_seen.spatial = spatial.clamp(-1, DEFAULT_MAX_SPATIAL);
    }

    /// Assumption: a target capped only by the previously seen temporal layer
    /// follows newly seen layers within the subscription, so L1T3 input keeps
    /// its enhancement frames.
    pub fn set_max_temporal_layer_seen(&mut self, temporal: i32) {
        let cap = self.max_temporal();
        self.max_seen.temporal = self
            .max_seen
            .temporal
            .max(temporal.min(DEFAULT_MAX_TEMPORAL));
        if self.target.is_valid() && self.target.temporal == cap {
            self.target.temporal = self.max_temporal();
            self.last_allocation.target_layer = self.target;
        }
    }

    /// Publisher demand mute: this encoding is intentionally inactive.
    pub fn set_pub_muted(&mut self, muted: bool) {
        self.pub_muted = muted;
    }

    pub fn current_layer(&self) -> VideoLayer {
        self.current
    }

    pub fn target_layer(&self) -> VideoLayer {
        self.target
    }

    pub fn last_allocation(&self) -> VideoAllocation {
        self.last_allocation
    }

    pub fn is_deficient(&self) -> bool {
        self.last_allocation.is_deficient
    }

    /// Spatial layer this output needs a keyframe from, if any.
    pub fn keyframe_request(&self) -> Option<i32> {
        let waiting = !self.current.is_valid() || self.current.spatial != self.target.spatial;
        (self.kind.is_video() && !self.pub_muted && self.target.is_valid() && waiting)
            .then_some(self.target.spatial)
    }

    /// The source's RTP state is unchanged but forwarding must re-acquire it.
    /// The allocation is retained.
    pub fn resync(&mut self) {
        self.current = VideoLayer::INVALID;
    }

    /// A different source with its own RTP/NTP references. Output identities
    /// continue from their last values.
    pub fn restart(&mut self) {
        self.current = VideoLayer::INVALID;
        self.last_reference = None;
    }

    fn max_temporal(&self) -> i32 {
        if self.max_seen.temporal >= 0 && self.max_seen.temporal < self.max_layer.temporal {
            self.max_seen.temporal
        } else {
            self.max_layer.temporal
        }
    }

    /// The bandwidth this output requests: the target layer's measured rate,
    /// else the committed request.
    pub fn bandwidth_requested(&self, rates: &Bitrates) -> i64 {
        bandwidth_needed(rates, self.target, self.last_allocation.bandwidth_requested)
    }

    fn update_allocation(&mut self, allocation: VideoAllocation) -> VideoAllocation {
        self.last_allocation = allocation;
        self.target = allocation.target_layer;
        self.request_spatial = allocation.request_layer_spatial;
        if !self.target.is_valid() {
            self.current = VideoLayer::INVALID;
        }
        allocation
    }

    /// Takes the best available layer within the subscription, ignoring
    /// capacity. Assumption: `hold` keeps a current layer rather than moving
    /// (Piik passes it while the estimator warns).
    pub fn allocate_optimal(
        &mut self,
        available: &[i32],
        rates: &Bitrates,
        allow_overshoot: bool,
        hold: bool,
    ) -> VideoAllocation {
        let max_layer = self.max_layer;
        let max_seen = self.max_seen;
        let optimal = optimal_bandwidth(self.pub_muted, max_seen.spatial, rates, max_layer);
        let mut allocation = VideoAllocation {
            pause_reason: if optimal == 0 {
                PauseReason::FeedDry
            } else {
                PauseReason::None
            },
            bandwidth_needed: optimal,
            request_layer_spatial: self.request_spatial,
            max_layer,
            ..Default::default()
        };
        if !max_layer.is_valid() || max_seen.spatial < 0 {
        } else if self.pub_muted {
            allocation.pause_reason = PauseReason::PubMuted;
        } else if !self.kind.is_video() {
            allocation.target_layer = VideoLayer::new(0, 0);
        } else {
            let limit = max_layer.spatial.min(max_seen.spatial);
            let (mut request, mut highest) = (-1, -1);
            for &layer in available {
                if layer > request && layer <= limit {
                    request = layer;
                }
                highest = highest.max(layer);
            }
            if request < 0 && highest >= 0 && allow_overshoot {
                request = highest;
            }
            let temporal = self.max_temporal();
            if self.current.is_valid() {
                let locked =
                    request == self.request_spatial && self.current.spatial == self.request_spatial;
                let spatial = if locked || request < 0 || hold {
                    self.current.spatial
                } else {
                    request
                };
                allocation.target_layer = VideoLayer::new(spatial, temporal);
                allocation.request_layer_spatial = spatial;
            } else {
                // Opportunistically latch on to anything up to the limit.
                let spatial = if allow_overshoot {
                    max_seen.spatial
                } else {
                    limit
                };
                allocation.target_layer = VideoLayer::new(spatial.min(max_seen.spatial), temporal);
                allocation.request_layer_spatial = if request < 0 { limit } else { request };
            }
        }
        if !allocation.target_layer.is_valid() {
            allocation.target_layer = VideoLayer::INVALID;
            allocation.request_layer_spatial = -1;
        } else {
            allocation.bandwidth_requested = optimal;
        }
        allocation.bandwidth_delta = allocation.bandwidth_requested
            - bandwidth_needed(
                rates,
                self.last_allocation.target_layer,
                self.last_allocation.bandwidth_requested,
            );
        self.update_allocation(allocation)
    }

    /// Pauses the output: no target under the current budget.
    pub fn pause(&mut self, rates: &Bitrates) -> VideoAllocation {
        let max_layer = self.max_layer;
        let optimal = optimal_bandwidth(self.pub_muted, self.max_seen.spatial, rates, max_layer);
        let mut allocation = VideoAllocation {
            bandwidth_delta: -bandwidth_needed(
                rates,
                self.target,
                self.last_allocation.bandwidth_requested,
            ),
            bandwidth_needed: optimal,
            max_layer,
            ..Default::default()
        };
        if !max_layer.is_valid() || self.max_seen.spatial < 0 {
        } else if self.pub_muted {
            allocation.pause_reason = PauseReason::PubMuted;
        } else {
            allocation.is_deficient = true;
            allocation.pause_reason = PauseReason::Bandwidth;
        }
        self.update_allocation(allocation)
    }

    pub fn provisional_allocate_prepare(&mut self, rates: &Bitrates) {
        self.provisional = Provisional {
            pub_muted: self.pub_muted,
            max_seen: self.max_seen,
            max_layer: self.max_layer,
            current: self.current,
            bitrates: *rates,
            allocated: VideoLayer::INVALID,
        };
    }

    pub fn provisional_allocate_reset(&mut self) {
        self.provisional.allocated = VideoLayer::INVALID;
    }

    /// Takes `layer` when it fits the remaining capacity plus what this output
    /// already holds. Without `allow_pause` the first measured layer is taken
    /// regardless of capacity. Returns whether the layer was taken and the
    /// additional bitrate it uses.
    pub fn provisional_allocate(
        &mut self,
        capacity: i64,
        layer: VideoLayer,
        allow_pause: bool,
        allow_overshoot: bool,
    ) -> (bool, i64) {
        let provisional = &mut self.provisional;
        if provisional.pub_muted
            || provisional.max_seen.spatial < 0
            || !provisional.max_layer.is_valid()
            || (!allow_overshoot && layer.greater_than(provisional.max_layer))
        {
            return (false, 0);
        }
        let required = rate(&provisional.bitrates, layer);
        if required == 0 {
            return (false, 0);
        }
        let held = rate(&provisional.bitrates, provisional.allocated);
        let fits = !layer.greater_than(provisional.max_layer) && required <= capacity + held;
        let forced = !allow_pause
            && (!provisional.allocated.is_valid() || !layer.greater_than(provisional.allocated));
        if fits || forced {
            provisional.allocated = layer;
            return (true, required - held);
        }
        (false, 0)
    }

    /// Keeps a still streaming target, steps down to the highest measured
    /// layer when above it, else starts from the lowest measured layer (or the
    /// current layer when nothing is measured).
    pub fn provisional_allocate_cooperative_transition(
        &mut self,
        _allow_overshoot: bool,
    ) -> VideoTransition {
        let existing = self.target;
        let last_needed = bandwidth_needed(
            &self.provisional.bitrates,
            existing,
            self.last_allocation.bandwidth_requested,
        );
        let provisional = &mut self.provisional;
        if provisional.pub_muted {
            provisional.allocated = VideoLayer::INVALID;
            return VideoTransition {
                from: existing,
                to: VideoLayer::INVALID,
                bandwidth_delta: -last_needed,
            };
        }
        let rates = provisional.bitrates;
        if existing.is_valid() {
            let maximal = highest_measured(&rates, provisional.max_layer);
            if maximal.is_valid() {
                if !existing.greater_than(maximal) && rate(&rates, existing) != 0 {
                    provisional.allocated = existing;
                    return VideoTransition {
                        from: existing,
                        to: existing,
                        bandwidth_delta: 0,
                    };
                }
                if existing.greater_than(maximal) {
                    provisional.allocated = maximal;
                    return VideoTransition {
                        from: existing,
                        to: maximal,
                        bandwidth_delta: rate(&rates, maximal) - last_needed,
                    };
                }
            }
        }
        let mut lowest = lowest_measured(&rates, provisional.max_layer);
        let required = rate(&rates, lowest);
        if !lowest.is_valid() {
            lowest = provisional.current;
        }
        provisional.allocated = lowest;
        VideoTransition {
            from: existing,
            to: lowest,
            bandwidth_delta: required - last_needed,
        }
    }

    /// Commits the provisional allocation. An allocation below the optimal
    /// measured layer is deficient.
    pub fn provisional_allocate_commit(&mut self) -> VideoAllocation {
        let provisional = self.provisional;
        let rates = provisional.bitrates;
        let optimal = optimal_bandwidth(
            provisional.pub_muted,
            provisional.max_seen.spatial,
            &rates,
            provisional.max_layer,
        );
        let last_needed = bandwidth_needed(
            &rates,
            self.target,
            self.last_allocation.bandwidth_requested,
        );
        let mut allocated = provisional.allocated;
        let mut allocation = VideoAllocation {
            bandwidth_delta: -last_needed,
            bandwidth_needed: optimal,
            max_layer: provisional.max_layer,
            ..Default::default()
        };
        if !provisional.max_layer.is_valid() || provisional.max_seen.spatial < 0 {
        } else if optimal == 0 {
            if allocated.is_valid() {
                allocation.bandwidth_requested = rate(&rates, allocated);
                allocation.bandwidth_delta = allocation.bandwidth_requested - last_needed;
            } else {
                allocation.pause_reason = PauseReason::FeedDry;
                // Leave the target at current for opportunistic forwarding.
                if self.current.is_valid() && self.current.spatial <= provisional.max_layer.spatial
                {
                    allocated = self.current;
                }
            }
        } else if provisional.pub_muted {
            allocation.pause_reason = PauseReason::PubMuted;
        } else {
            allocation.bandwidth_requested = rate(&rates, allocated);
            allocation.bandwidth_delta = allocation.bandwidth_requested - last_needed;
            if !allocated.greater_than(provisional.max_layer)
                && allocation.bandwidth_requested < optimal
            {
                allocation.is_deficient = true;
                if !allocated.is_valid() {
                    allocation.pause_reason = PauseReason::Bandwidth;
                }
            }
        }
        allocation.target_layer = allocated;
        allocation.request_layer_spatial = allocated.spatial;
        if !allocated.is_valid() {
            allocation.target_layer = VideoLayer::INVALID;
            allocation.request_layer_spatial = -1;
        }
        self.update_allocation(allocation)
    }

    /// While deficient and settled, the first measured layer above the target
    /// needing more than it uses.
    pub fn next_higher_transition(
        &self,
        rates: &Bitrates,
        _allow_overshoot: bool,
    ) -> Option<VideoTransition> {
        if !self.kind.is_video()
            || !self.last_allocation.is_deficient
            || self.target != self.current && self.target.is_valid()
            || self.pub_muted
            || self.max_seen.spatial < 0
            || !self.max_layer.is_valid()
        {
            return None;
        }
        let held = rate(rates, self.target);
        let (first_spatial, first_temporal) = if self.target.is_valid() {
            (self.target.spatial, self.target.temporal + 1)
        } else {
            (0, 0)
        };
        let max_spatial = self.max_layer.spatial.min(MAX_SPATIAL_LAYERS as i32 - 1);
        let max_temporal = self.max_layer.temporal.min(MAX_TEMPORAL_LAYERS as i32 - 1);
        for spatial in first_spatial..=max_spatial {
            let start = if spatial == first_spatial {
                first_temporal
            } else {
                0
            };
            for temporal in start..=max_temporal {
                let layer = VideoLayer::new(spatial, temporal);
                let required = rate(rates, layer);
                if required > held {
                    return Some(VideoTransition {
                        from: self.target,
                        to: layer,
                        bandwidth_delta: required - held,
                    });
                }
            }
        }
        None
    }

    /// Selects, projects and rewrites one inbound packet for this output.
    pub fn forward(&mut self, packet: &InboundPacket) -> Option<Forwarded> {
        if self.pub_muted {
            return None;
        }
        if !self.kind.is_video() {
            let switched = !self.rtp.is_started();
            return self.emit(packet, VideoLayer::new(0, 0), switched);
        }
        if !self.target.is_valid() {
            return None;
        }
        let mut switched = false;
        if packet.layer != self.current.spatial {
            let toward_target = if packet.layer > self.current.spatial {
                packet.layer <= self.target.spatial
            } else {
                packet.layer >= self.target.spatial
            };
            if !packet.keyframe || !toward_target {
                return None;
            }
            self.current = VideoLayer::new(packet.layer, self.target.temporal);
            switched = true;
        } else if packet.keyframe {
            self.current.temporal = self.target.temporal;
        }
        let descriptor = packet
            .vp8
            .filter(|descriptor| descriptor.temporal.is_some());
        let admitted = match descriptor {
            Some(descriptor) => {
                self.select_temporal(packet, descriptor.start && descriptor.layer_sync)
            }
            None => {
                self.current.temporal = self.target.temporal;
                self.current.temporal
            }
        };
        if switched {
            let out_timestamp = self.switch_timestamp(packet);
            self.rtp
                .switch(packet.ext_seq, packet.ext_timestamp, out_timestamp);
            if let Some(descriptor) = packet.vp8 {
                self.vp8.switch(&descriptor);
            }
        }
        if descriptor.is_some() && packet.temporal > admitted {
            self.rtp.drop_packet(packet.ext_seq);
            if let Some(descriptor) = packet.vp8 {
                self.vp8.rewrite(&descriptor, true);
            }
            return None;
        }
        self.emit(packet, VideoLayer::new(packet.layer, admitted), switched)
    }

    /// VP8 temporal selector; returns the layer this packet is judged under.
    fn select_temporal(&mut self, packet: &InboundPacket, sync: bool) -> i32 {
        let (current, target) = (self.current.temporal, self.target.temporal);
        if current < target && packet.temporal == current + 1 && sync {
            self.current.temporal = packet.temporal;
        } else if current > target && packet.marker {
            // Finish this frame at the current layer, then reduce.
            self.current.temporal = target;
            return current;
        }
        self.current.temporal
    }

    /// Output timestamp for the first packet of a new segment: the elapsed
    /// source time (capture reference when both are known, else arrival)
    /// after the last forwarded packet, at least one tick.
    fn switch_timestamp(&self, packet: &InboundPacket) -> u64 {
        if !self.rtp.is_started() {
            return packet.ext_timestamp;
        }
        let clock = f64::from(self.kind.clock_rate());
        let elapsed = match (self.last_reference, packet.reference) {
            (Some(last), Some(next)) => next.duration_since(last).ok(),
            _ => None,
        }
        .or_else(|| {
            self.last_arrival
                .map(|last| packet.arrival.saturating_duration_since(last))
        });
        let ticks = elapsed.map_or(1, |elapsed| (elapsed.as_secs_f64() * clock).round() as u64);
        self.rtp.last_timestamp() + ticks.max(1)
    }

    fn emit(
        &mut self,
        packet: &InboundPacket,
        layer: VideoLayer,
        switched: bool,
    ) -> Option<Forwarded> {
        if !self.rtp.is_started() {
            self.rtp.start(packet.ext_seq, packet.ext_timestamp);
            if let Some(descriptor) = packet.vp8 {
                self.vp8.start(&descriptor);
            }
        }
        let vp8 = match packet.vp8 {
            Some(descriptor) => match self.vp8.rewrite(&descriptor, false) {
                Some(rewrite) => Some(rewrite),
                None => {
                    self.rtp.drop_packet(packet.ext_seq);
                    return None;
                }
            },
            None => None,
        };
        let Translated {
            ext_seq,
            ext_timestamp,
        } = self
            .rtp
            .translate(packet.ext_seq, packet.ext_timestamp, packet.marker)?;
        if ext_seq == self.rtp.last_sequence() {
            self.last_reference = packet.reference.or(self.last_reference);
            self.last_arrival = Some(packet.arrival);
        }
        Some(Forwarded {
            ext_seq,
            ext_timestamp,
            marker: packet.marker,
            vp8,
            layer,
            switched,
        })
    }

    /// Padding positions after the last forwarded packet (see
    /// [`RtpMunger::padding`]).
    pub fn padding(&mut self, count: u64, force: bool) -> Vec<Translated> {
        self.rtp.padding(count, force)
    }
}

/// The layer's measured rate, else the already-allocated value.
pub(crate) fn bandwidth_needed(rates: &Bitrates, layer: VideoLayer, allocated: i64) -> i64 {
    match rate(rates, layer) {
        0 => allocated,
        measured => measured,
    }
}

/// Highest measured layer within `max_layer`, scanning spatial then temporal
/// downward.
fn highest_measured(rates: &Bitrates, max_layer: VideoLayer) -> VideoLayer {
    let spatial_max = max_layer.spatial.min(MAX_SPATIAL_LAYERS as i32 - 1);
    let temporal_max = max_layer.temporal.min(MAX_TEMPORAL_LAYERS as i32 - 1);
    for spatial in (0..=spatial_max).rev() {
        for temporal in (0..=temporal_max).rev() {
            let layer = VideoLayer::new(spatial, temporal);
            if rate(rates, layer) != 0 {
                return layer;
            }
        }
    }
    VideoLayer::INVALID
}

fn lowest_measured(rates: &Bitrates, max_layer: VideoLayer) -> VideoLayer {
    for spatial in 0..=max_layer.spatial.min(MAX_SPATIAL_LAYERS as i32 - 1) {
        for temporal in 0..=max_layer.temporal.min(MAX_TEMPORAL_LAYERS as i32 - 1) {
            let layer = VideoLayer::new(spatial, temporal);
            if rate(rates, layer) != 0 {
                return layer;
            }
        }
    }
    VideoLayer::INVALID
}

/// Optimal demand: the highest measured layer's rate, or zero when muted or
/// nothing is published.
fn optimal_bandwidth(
    pub_muted: bool,
    max_published: i32,
    rates: &Bitrates,
    max_layer: VideoLayer,
) -> i64 {
    if pub_muted || max_published < 0 {
        return 0;
    }
    rate(rates, highest_measured(rates, max_layer))
}

#[cfg(test)]
mod tests;
