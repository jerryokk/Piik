//! Allocation for one video stream on one child connection. Its owner closes
//! it; budget changes and source events reconcile synchronously and create no
//! other scheduler.

use crate::forwarder::{Forwarded, Forwarder};
use crate::layer::{BitrateGoal, Congestion, DEFAULT_MAX_SPATIAL, VideoLayer, bitrate_goal, rate};
use crate::packet::{CodecKind, InboundPacket};
use crate::source::SourceView;

/// Read by the parent at its input-frame boundary. `current` means accepted
/// for forwarding, not remotely decoded. `prepare` admits codec work from the
/// latest budget, not network delivery. -1 means no selected layer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OutputState {
    pub prepare: i32,
    pub target: i32,
    pub requested: i32,
    pub current: i32,
    pub paused: bool,
    /// Committed estimate for this connection's tracked video. Codec input,
    /// not a claim that this bitrate is available or delivered.
    pub video_budget: i64,
}

impl OutputState {
    pub const CLOSED: Self = Self {
        prepare: -1,
        target: -1,
        requested: -1,
        current: -1,
        paused: true,
        video_budget: 0,
    };
}

#[derive(Clone, Debug)]
pub struct OutputAllocator {
    forwarder: Forwarder,
    budget: i64,
    current: i32,
    has_allocation: bool,
    closed: bool,
    feedback_observed: Option<bool>,
    congestion: Congestion,
}

impl OutputAllocator {
    pub fn new(kind: CodecKind, budget: i64) -> Self {
        Self {
            forwarder: Forwarder::new(kind),
            budget: budget.max(0),
            current: -1,
            has_allocation: false,
            closed: false,
            feedback_observed: None,
            congestion: Congestion::None,
        }
    }

    pub fn forwarder(&self) -> &Forwarder {
        &self.forwarder
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }

    pub fn budget(&self) -> i64 {
        self.budget
    }

    /// `Some(false)` until the connection has received transport feedback;
    /// `None` when its owner does not gate bootstrap on feedback.
    pub fn set_feedback_observed(&mut self, observed: Option<bool>) {
        self.feedback_observed = observed;
    }

    pub fn set_congestion(&mut self, congestion: Congestion) {
        self.congestion = congestion;
    }

    pub fn set_budget(&mut self, source: &dyn SourceView, budget: i64) {
        let budget = budget.max(0);
        let reduced = budget < self.budget;
        self.budget = budget;
        self.reconcile_with(source, reduced);
    }

    /// Re-evaluates after any source, subscription or estimator event.
    pub fn reconcile(&mut self, source: &dyn SourceView) {
        self.reconcile_with(source, false);
    }

    fn reconcile_with(&mut self, source: &dyn SourceView, budget_reduced: bool) {
        if self.closed {
            return;
        }
        let (available, rates) = source.layered_bitrate();
        self.observe_layers(source, &available);
        if self.budget == 0 {
            self.forwarder.pause(&rates);
            self.has_allocation = true;
            return;
        }
        let measured = available
            .iter()
            .any(|layer| rate(&rates, VideoLayer::new(*layer, 0)) > 0);
        let congestion = self.congestion;
        if (!self.has_allocation && !measured) || self.feedback_observed == Some(false) {
            if !available.is_empty() {
                // Source history is not this new connection's capacity. Bootstrap
                // until video can establish real feedback; pausing against the
                // initial estimate would prevent that feedback.
                self.forwarder.allocate_optimal(
                    &available,
                    &rates,
                    false,
                    congestion == Congestion::EarlyWarning,
                );
                self.has_allocation = true;
            }
            return;
        }
        let maximum = self.forwarder.max_layer();
        let target = self.forwarder.last_allocation().target_layer;
        let target_allowed = target.is_valid()
            && target.spatial <= maximum.spatial
            && target.temporal <= maximum.temporal
            && available.contains(&target.spatial);
        if !measured && target_allowed {
            return;
        }
        let cooperative =
            !budget_reduced && self.has_allocation && congestion != Congestion::Congested;
        if cooperative && target_allowed && rate(&rates, target) == 0 {
            // An active layer's missing rate window does not revoke its allocation.
            return;
        }
        self.forwarder.provisional_allocate_prepare(&rates);
        let mut remaining = self.budget;
        let mut retained = VideoLayer::INVALID;
        if cooperative {
            // Changed source measurements alone retain a valid target. A lower
            // budget or confirmed congestion constrains it again; larger outputs
            // still need available headroom.
            let transition = self
                .forwarder
                .provisional_allocate_cooperative_transition(false);
            if transition.from.is_valid() && transition.from == transition.to {
                retained = transition.to;
                remaining -= self.forwarder.bandwidth_requested(&rates);
            } else {
                self.forwarder.provisional_allocate_reset();
            }
        }
        // Only a live local encoder can follow this child's budget below the
        // lowest layer's measurement; raw forwarding keeps the pause policy.
        let keep_lowest = source.rate_controlled(0) && available.contains(&0);
        for spatial in 0..=maximum.spatial {
            for temporal in 0..=maximum.temporal {
                let layer = VideoLayer::new(spatial, temporal);
                if retained.is_valid() && !layer.greater_than(retained) {
                    continue;
                }
                let allow_pause = !(keep_lowest && spatial == 0);
                let (_, used) =
                    self.forwarder
                        .provisional_allocate(remaining, layer, allow_pause, false);
                remaining -= used;
            }
        }
        self.forwarder.provisional_allocate_commit();
        self.has_allocation = true;
    }

    /// Availability can precede the source's discovery notification; apply
    /// the observed layer before allocation can invalidate its target.
    fn observe_layers(&mut self, source: &dyn SourceView, available: &[i32]) {
        let highest = available.iter().copied().max().unwrap_or(-1);
        self.forwarder
            .set_max_published_layer(source.max_published_layer().max(highest));
        self.forwarder
            .set_max_temporal_layer_seen(source.max_temporal_layer_seen());
    }

    pub fn set_max_spatial_layer(&mut self, source: &dyn SourceView, layer: i32) {
        self.forwarder.set_max_spatial_layer(layer);
        self.reconcile(source);
    }

    pub fn set_max_temporal_layer(&mut self, source: &dyn SourceView, layer: i32) {
        self.forwarder.set_max_temporal_layer(layer);
        self.reconcile(source);
    }

    pub fn state(&self, source: &dyn SourceView) -> OutputState {
        if self.closed {
            return OutputState::CLOSED;
        }
        let maximum = self.forwarder.max_layer().spatial;
        let (mut prepare, mut lowest) = (-1, -1);
        for (layer, ceiling) in source.layer_ceilings().into_iter().enumerate() {
            let layer = layer as i32;
            if layer <= maximum && ceiling > 0 {
                if lowest < 0 {
                    lowest = layer;
                }
                if ceiling <= self.budget {
                    prepare = prepare.max(layer);
                }
            }
        }
        // Below the lowest ceiling, keep one codec output eligible for rate
        // control; pausing it would retire the output needed to recover.
        if prepare < 0 && self.budget > 0 {
            prepare = lowest;
        }
        if !self.has_allocation {
            return OutputState {
                prepare,
                target: -1,
                requested: -1,
                current: self.current,
                paused: false,
                video_budget: self.budget,
            };
        }
        let allocation = self.forwarder.last_allocation();
        OutputState {
            prepare,
            target: allocation.target_layer.spatial,
            requested: allocation.request_layer_spatial,
            current: self.current,
            paused: !allocation.target_layer.is_valid(),
            video_budget: self.budget,
        }
    }

    /// Unmet demand and committed usage, the inputs of a recovery probe.
    pub fn probe_demand(&self, source: &dyn SourceView) -> (i64, i64) {
        if self.closed || !self.has_allocation {
            return (0, 0);
        }
        let (_, rates) = source.layered_bitrate();
        let usage = self.forwarder.bandwidth_requested(&rates);
        let target = self.forwarder.last_allocation().target_layer.spatial;
        // A live encoder can adapt below its current layer's ceiling. That unmet
        // demand exists even without a higher layer; raw forwarding cannot use
        // it. Restore it before exploring a larger spatial output.
        let ceiling = controlled_layer_ceiling(source, target);
        if ceiling > self.budget {
            return ((ceiling - usage).max(0), usage);
        }
        if let Some(transition) = self.forwarder.next_higher_transition(&rates, false) {
            return (transition.bandwidth_delta, usage);
        }
        // Dormant ceilings are desired demand only. They never enter measured
        // rates or authorize forwarding an unavailable layer.
        let maximum = self.forwarder.max_layer().spatial;
        let (mut next, mut desired) = (DEFAULT_MAX_SPATIAL + 1, 0);
        for (layer, ceiling) in source.layer_ceilings().into_iter().enumerate() {
            let layer = layer as i32;
            if layer > target && layer <= maximum && layer < next && ceiling > self.budget {
                (next, desired) = (layer, ceiling);
            }
        }
        ((desired - usage).max(0), usage)
    }

    /// Rates for str0m's `Bwe::set_bitrate` from this output's demand.
    pub fn bitrate_goal(&self, source: &dyn SourceView) -> BitrateGoal {
        let (delta, usage) = self.probe_demand(source);
        bitrate_goal(usage, delta)
    }

    /// Includes a live encoder restricted inside its current layer. Packet-rate
    /// variation alone is not an allocation or congestion decision.
    pub fn bandwidth_limited(&self, source: &dyn SourceView) -> bool {
        let target = self.forwarder.last_allocation().target_layer.spatial;
        !self.closed
            && self.has_allocation
            && (self.forwarder.is_deficient()
                || controlled_layer_ceiling(source, target) > self.budget)
    }

    /// Encodings the source must keep producing for this child. `observed` is
    /// transport feedback.
    pub fn required_active_count(&self, source: &dyn SourceView, observed: bool) -> usize {
        let state = self.state(source);
        let mut highest = state.target;
        if !state.paused {
            highest = highest.max(state.current);
        }
        if observed {
            highest = highest.max(state.prepare);
        }
        if highest < 0 && !state.paused {
            highest = self.forwarder.max_layer().spatial;
        }
        (highest + 1).max(0) as usize
    }

    /// Spatial layer that needs a keyframe request, if any.
    pub fn keyframe_request(&self) -> Option<i32> {
        if self.closed {
            None
        } else {
            self.forwarder.keyframe_request()
        }
    }

    /// Forwards one packet. A completed frame updates `current`; the caller
    /// then re-reads demand.
    pub fn forward(&mut self, packet: &InboundPacket) -> Option<Forwarded> {
        if self.closed {
            return None;
        }
        let forwarded = self.forwarder.forward(packet)?;
        if packet.marker {
            self.current = packet.layer;
        }
        Some(forwarded)
    }

    /// Fences a controlled Native input before any of its outputs arrive. Raw
    /// RTP has no such boundary and keeps the ordinary switching path.
    pub fn begin_frame(&mut self) {
        let target = self.forwarder.last_allocation().target_layer;
        let switching = !self.closed
            && self.has_allocation
            && self.current >= 0
            && target.is_valid()
            && self.current != target.spatial;
        if switching {
            self.forwarder.resync();
            self.current = -1;
        }
    }

    /// Re-acquires the same source; the next reconcile reallocates.
    pub fn resync(&mut self) {
        self.forwarder.resync();
        self.current = -1;
        self.has_allocation = false;
    }

    /// A different input on the same source owner. Arms the existing
    /// acquisition target before the first recovery access unit arrives,
    /// since availability is only updated after it is forwarded.
    pub fn receiver_restart(&mut self, source: &dyn SourceView) {
        self.forwarder.restart();
        self.current = -1;
        if !self.closed {
            let (available, rates) = source.layered_bitrate();
            self.observe_layers(source, &available);
            self.forwarder
                .allocate_optimal(&available, &rates, false, false);
            self.has_allocation = true;
        }
    }

    /// Moves to a replacement source with its own RTP/NTP references.
    pub fn replace_source(&mut self, next: &dyn SourceView) {
        if self.closed {
            return;
        }
        self.forwarder.restart();
        self.current = -1;
        self.has_allocation = false;
        self.reconcile(next);
    }

    pub fn close(&mut self) {
        self.closed = true;
    }
}

/// A rate-controlled layer's declared ceiling, else zero.
pub(crate) fn controlled_layer_ceiling(source: &dyn SourceView, spatial: i32) -> i64 {
    if spatial < 0 || !source.rate_controlled(spatial) {
        return 0;
    }
    source
        .layer_ceilings()
        .get(spatial as usize)
        .copied()
        .unwrap_or(0)
}

/// Committed video capacity with probe-settling semantics: outside a probe,
/// confirmed congestion can only lower it; only a completed non-congesting
/// probe can raise it. Encoder and allocation budgets read this value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommittedBudget {
    target: i64,
    probing: bool,
}

impl CommittedBudget {
    pub fn new(initial: i64) -> Self {
        Self {
            target: initial.max(0),
            probing: false,
        }
    }

    pub fn value(&self) -> i64 {
        self.target
    }

    pub fn is_probing(&self) -> bool {
        self.probing
    }

    /// Owner-supplied value, e.g. an explicit test or configuration reset.
    pub fn set(&mut self, target: i64) {
        self.target = target.max(0);
    }

    pub fn probe_started(&mut self) {
        self.probing = true;
    }

    /// A finalized probe; returns whether the committed value changed.
    pub fn probe_finished(&mut self, not_congesting: bool, capacity: i64) -> bool {
        self.probing = false;
        if not_congesting && capacity > self.target {
            self.target = capacity;
            return true;
        }
        false
    }

    /// Estimator reset retires any probe without changing the value.
    pub fn reset(&mut self) {
        self.probing = false;
    }

    /// Returns whether the committed value changed. Warning and clear
    /// transitions are not capacity measurements.
    pub fn on_congestion(&mut self, state: Congestion, capacity: i64) -> bool {
        if !self.probing && state == Congestion::Congested && capacity > 0 && capacity < self.target
        {
            self.target = capacity;
            return true;
        }
        false
    }
}

#[cfg(test)]
pub(crate) mod tests;
