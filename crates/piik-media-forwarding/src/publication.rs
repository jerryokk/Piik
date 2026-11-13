//! Shared-estimate allocation for one simulcast publication, including an
//! additional-probe-demand hook. Probe timing is str0m's; this produces the
//! `(minimum, desired)` pair for `Bwe::set_bitrate`.
//!
//! Each RID owns one forwarder that sees exactly its own layer, so another RID
//! can never substitute (and charge twice for) the same lower output.
//!
//! Allocator semantics:
//! - Stable state: every track gets its optimal allocation regardless of
//!   capacity. Confirmed congestion enters the deficient state, where tracks
//!   share the committed capacity lowest layer first and may pause.
//! - After a budgeted allocation leaves no track deficient, the allocator is
//!   stable again.
//! - Probe goal: the smallest next-higher transition of a deficient track.
//!   Assumption for the additional-probe-demand hook (Piik's fork hook): its
//!   value is demand the tracks cannot express (a live encoder restricted
//!   within its current RID), so the goal uses the larger of the two deltas.

use crate::forwarder::{Forwarded, Forwarder};
use crate::layer::{
    BitrateGoal, Bitrates, Congestion, MAX_SPATIAL_LAYERS, VideoLayer, bitrate_goal,
};
use crate::output::{CommittedBudget, controlled_layer_ceiling};
use crate::packet::{CodecKind, InboundPacket};
use crate::source::SourceView;

const RIDS: [&str; MAX_SPATIAL_LAYERS] = ["q", "h", "f"];

/// The wire RID for an encoding; a single encoding has none.
pub fn publication_rid(layer: usize, count: usize) -> &'static str {
    if count <= 1 {
        ""
    } else {
        RIDS.get(layer).copied().unwrap_or("")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublicationError {
    #[error("publication options are invalid")]
    Invalid,
    #[error("publication layer demand is invalid")]
    ActiveCount,
    #[error("publication is closed")]
    Closed,
    #[error("replacement publication source changed its transport contract")]
    Incompatible,
}

/// One RID's view of the shared source.
struct LayerView<'a> {
    source: &'a dyn SourceView,
    layer: i32,
}

impl SourceView for LayerView<'_> {
    fn layered_bitrate(&self) -> (Vec<i32>, Bitrates) {
        let (available, rates) = self.source.layered_bitrate();
        let mut selected = Bitrates::default();
        let index = self.layer as usize;
        if index < MAX_SPATIAL_LAYERS {
            selected[index] = rates[index];
        }
        let available = if available.contains(&self.layer) {
            vec![self.layer]
        } else {
            Vec::new()
        };
        (available, selected)
    }

    fn layer_ceilings(&self) -> Vec<i64> {
        self.source.layer_ceilings()
    }

    fn rate_controlled(&self, layer: i32) -> bool {
        layer == self.layer && self.source.rate_controlled(layer)
    }

    fn max_published_layer(&self) -> i32 {
        if self.layered_bitrate().0.is_empty() {
            -1
        } else {
            self.layer
        }
    }

    fn max_temporal_layer_seen(&self) -> i32 {
        self.source.max_temporal_layer_seen()
    }
}

#[derive(Clone, Debug)]
pub struct PublicationAllocator {
    tracks: Vec<Forwarder>,
    budget: CommittedBudget,
    congestion: Congestion,
    deficient_state: bool,
    active_count: usize,
    attached: bool,
    closed: bool,
}

impl PublicationAllocator {
    pub fn new(
        kind: CodecKind,
        layers: usize,
        initial_bitrate: i64,
    ) -> Result<Self, PublicationError> {
        if !kind.is_video() || layers == 0 || layers > MAX_SPATIAL_LAYERS || initial_bitrate <= 0 {
            return Err(PublicationError::Invalid);
        }
        let tracks = (0..layers)
            .map(|layer| {
                let mut track = Forwarder::new(kind);
                track.set_max_spatial_layer(layer as i32);
                track
            })
            .collect();
        Ok(Self {
            tracks,
            budget: CommittedBudget::new(initial_bitrate),
            congestion: Congestion::None,
            deficient_state: false,
            active_count: layers,
            attached: false,
            closed: false,
        })
    }

    pub fn layer_count(&self) -> usize {
        self.tracks.len()
    }

    pub fn track(&self, layer: usize) -> Option<&Forwarder> {
        self.tracks.get(layer)
    }

    pub fn video_budget(&self) -> i64 {
        self.budget.value()
    }

    pub fn is_probing(&self) -> bool {
        self.budget.is_probing()
    }

    /// The transport connected; tracks begin acquiring the source.
    pub fn set_connected(&mut self, source: &dyn SourceView) -> Result<(), PublicationError> {
        if self.closed {
            return Err(PublicationError::Closed);
        }
        self.attached = true;
        self.reallocate(source);
        Ok(())
    }

    /// Publisher demand is authoritative even while allocation is paused.
    pub fn set_active_count(
        &mut self,
        source: &dyn SourceView,
        count: usize,
    ) -> Result<(), PublicationError> {
        if self.closed {
            return Err(PublicationError::Closed);
        }
        if count > self.tracks.len() {
            return Err(PublicationError::ActiveCount);
        }
        self.active_count = count;
        for (index, track) in self.tracks.iter_mut().enumerate() {
            track.set_pub_muted(index >= count);
        }
        self.reallocate(source);
        Ok(())
    }

    /// Encodings the source must produce. The allocator still needs input for
    /// requested encodings when it probes recovery; this is codec demand, not
    /// affordable or delivered upload.
    pub fn required_active_count(&self) -> usize {
        if self.closed || !self.attached {
            0
        } else {
            self.active_count
        }
    }

    /// Keeps every RID and counter; forwarding re-acquires the next source.
    pub fn replace_source(&mut self, next: &dyn SourceView) -> Result<(), PublicationError> {
        if self.closed {
            return Err(PublicationError::Closed);
        }
        for track in &mut self.tracks {
            track.restart();
        }
        self.reallocate(next);
        Ok(())
    }

    /// Source events (layers, bitrates, discovery) and subscription changes.
    pub fn reconcile(&mut self, source: &dyn SourceView) {
        self.reallocate(source);
    }

    pub fn set_congestion(&mut self, congestion: Congestion) {
        self.congestion = congestion;
    }

    /// Estimator state change. Outside a probe, confirmed congestion can lower
    /// the committed capacity and enters budgeted allocation.
    pub fn on_congestion(&mut self, source: &dyn SourceView, state: Congestion, capacity: i64) {
        self.congestion = state;
        self.budget.on_congestion(state, capacity);
        if state == Congestion::Congested && !self.budget.is_probing() {
            self.deficient_state = true;
        }
        self.reallocate(source);
    }

    pub fn probe_started(&mut self) {
        self.budget.probe_started();
    }

    /// Only a completed non-congesting probe raises the committed capacity.
    pub fn probe_finished(&mut self, source: &dyn SourceView, not_congesting: bool, capacity: i64) {
        if self.budget.probe_finished(not_congesting, capacity) {
            self.reallocate(source);
        }
    }

    /// Owner-supplied capacity (estimator reset or explicit configuration).
    pub fn set_channel_capacity(&mut self, source: &dyn SourceView, capacity: i64) {
        self.budget.set(capacity);
        self.reallocate(source);
    }

    fn reallocate(&mut self, source: &dyn SourceView) {
        if self.closed || !self.attached {
            return;
        }
        let views: Vec<_> = (0..self.tracks.len())
            .map(|layer| LayerView {
                source,
                layer: layer as i32,
            })
            .collect();
        let mut tables = Vec::with_capacity(views.len());
        for (track, view) in self.tracks.iter_mut().zip(&views) {
            let (available, rates) = view.layered_bitrate();
            track.set_max_published_layer(view.max_published_layer());
            track.set_max_temporal_layer_seen(view.max_temporal_layer_seen());
            tables.push((available, rates));
        }
        if !self.deficient_state {
            for (track, (available, rates)) in self.tracks.iter_mut().zip(&tables) {
                track.allocate_optimal(available, rates, false, false);
            }
            return;
        }
        let mut remaining = self.budget.value();
        for (track, (_, rates)) in self.tracks.iter_mut().zip(&tables) {
            track.provisional_allocate_prepare(rates);
        }
        for spatial in 0..MAX_SPATIAL_LAYERS as i32 {
            for temporal in 0..=crate::layer::DEFAULT_MAX_TEMPORAL {
                for track in &mut self.tracks {
                    let (_, used) = track.provisional_allocate(
                        remaining,
                        VideoLayer::new(spatial, temporal),
                        true,
                        false,
                    );
                    remaining -= used;
                }
            }
        }
        for track in &mut self.tracks {
            track.provisional_allocate_commit();
        }
        self.deficient_state = self.tracks.iter().any(Forwarder::is_deficient);
    }

    fn bandwidth_requested(&self, source: &dyn SourceView, layer: usize) -> i64 {
        let view = LayerView {
            source,
            layer: layer as i32,
        };
        self.tracks[layer].bandwidth_requested(&view.layered_bitrate().1)
    }

    /// Committed capacity left for the lowest encoder after higher RIDs.
    pub fn lowest_layer_budget(&self, source: &dyn SourceView) -> i64 {
        let higher: i64 = (1..self.tracks.len())
            .map(|layer| self.bandwidth_requested(source, layer))
            .sum();
        (self.budget.value() - higher).max(0)
    }

    /// A local encoder can need more bandwidth without another spatial layer.
    /// Desired output stays separate from measured source bitrate and committed
    /// capacity.
    pub fn additional_probe_demand(&self, source: &dyn SourceView) -> i64 {
        if self.closed || !self.attached || self.active_count == 0 {
            return 0;
        }
        let ceiling = controlled_layer_ceiling(source, 0);
        if ceiling <= self.lowest_layer_budget(source) {
            return 0;
        }
        (ceiling - self.bandwidth_requested(source, 0)).max(0)
    }

    /// Committed usage and unmet demand for str0m's `Bwe::set_bitrate`.
    pub fn bitrate_goal(&self, source: &dyn SourceView) -> BitrateGoal {
        if self.closed || !self.attached {
            return BitrateGoal::default();
        }
        let usage: i64 = (0..self.tracks.len())
            .map(|layer| self.bandwidth_requested(source, layer))
            .sum();
        let transition = self
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(layer, track)| {
                let view = LayerView {
                    source,
                    layer: layer as i32,
                };
                track.next_higher_transition(&view.layered_bitrate().1, false)
            })
            .map(|transition| transition.bandwidth_delta)
            .min()
            .unwrap_or(0);
        bitrate_goal(usage, transition.max(self.additional_probe_demand(source)))
    }

    /// Categorical limitation: confirmed congestion outside a probe, a
    /// requested encoding's committed deficit, or a live encoder restricted
    /// within its sole RID. Inactive encodings are not bandwidth failures.
    pub fn limited(&self, source: &dyn SourceView) -> bool {
        if self.closed {
            return false;
        }
        let congested = !self.budget.is_probing() && self.congestion == Congestion::Congested;
        let encoder = self.active_count > 0
            && controlled_layer_ceiling(source, 0) > self.lowest_layer_budget(source);
        let deficient = self
            .tracks
            .iter()
            .take(self.active_count)
            .any(Forwarder::is_deficient);
        congested || encoder || deficient
    }

    /// Spatial layers needing a keyframe request.
    pub fn keyframe_requests(&self) -> Vec<i32> {
        if self.closed || !self.attached {
            return Vec::new();
        }
        let mut layers: Vec<_> = self
            .tracks
            .iter()
            .filter_map(Forwarder::keyframe_request)
            .collect();
        layers.dedup();
        layers
    }

    /// Routes one source packet to its RID; returns the RID index.
    pub fn forward(&mut self, packet: &InboundPacket) -> Option<(usize, Forwarded)> {
        if self.closed || !self.attached {
            return None;
        }
        let layer = usize::try_from(packet.layer).ok()?;
        let forwarded = self.tracks.get_mut(layer)?.forward(packet)?;
        Some((layer, forwarded))
    }

    pub fn close(&mut self) {
        self.closed = true;
    }
}

#[cfg(test)]
mod tests;
