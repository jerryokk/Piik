//! Layer, bitrate and allocation values shared by forwarders and allocators.

/// Spatial layers per source.
pub const MAX_SPATIAL_LAYERS: usize = 3;
/// Temporal layers per spatial layer.
pub const MAX_TEMPORAL_LAYERS: usize = 4;
pub const DEFAULT_MAX_SPATIAL: i32 = MAX_SPATIAL_LAYERS as i32 - 1;
pub const DEFAULT_MAX_TEMPORAL: i32 = MAX_TEMPORAL_LAYERS as i32 - 1;
/// Probe goal overage and floor.
pub const PROBE_OVERAGE_PCT: i64 = 120;
pub const PROBE_MIN_BPS: i64 = 200_000;

/// Cumulative bits per second: `[spatial][temporal]` includes lower temporal
/// layers of the same spatial layer. Zero means unmeasured or unavailable.
pub type Bitrates = [[i64; MAX_TEMPORAL_LAYERS]; MAX_SPATIAL_LAYERS];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VideoLayer {
    pub spatial: i32,
    pub temporal: i32,
}

impl VideoLayer {
    pub const INVALID: Self = Self::new(-1, -1);

    pub const fn new(spatial: i32, temporal: i32) -> Self {
        Self { spatial, temporal }
    }

    pub const fn is_valid(self) -> bool {
        self.spatial >= 0 && self.temporal >= 0
    }

    pub const fn greater_than(self, other: Self) -> bool {
        self.spatial > other.spatial
            || (self.spatial == other.spatial && self.temporal > other.temporal)
    }
}

/// Rate of `layer`, or zero when it is invalid or outside the table.
pub(crate) fn rate(rates: &Bitrates, layer: VideoLayer) -> i64 {
    if !layer.is_valid()
        || layer.spatial as usize >= MAX_SPATIAL_LAYERS
        || layer.temporal as usize >= MAX_TEMPORAL_LAYERS
    {
        return 0;
    }
    rates[layer.spatial as usize][layer.temporal as usize]
}

/// Congestion state supplied by the connection's estimator owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Congestion {
    #[default]
    None,
    EarlyWarning,
    Congested,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PauseReason {
    #[default]
    None,
    FeedDry,
    PubMuted,
    Bandwidth,
}

/// One committed allocation decision for a forwarder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoAllocation {
    pub pause_reason: PauseReason,
    pub is_deficient: bool,
    pub bandwidth_requested: i64,
    pub bandwidth_delta: i64,
    /// Rate of the optimal (desired) layer under the subscription limit.
    pub bandwidth_needed: i64,
    pub target_layer: VideoLayer,
    pub request_layer_spatial: i32,
    pub max_layer: VideoLayer,
}

impl Default for VideoAllocation {
    fn default() -> Self {
        Self {
            pause_reason: PauseReason::None,
            is_deficient: false,
            bandwidth_requested: 0,
            bandwidth_delta: 0,
            bandwidth_needed: 0,
            target_layer: VideoLayer::INVALID,
            request_layer_spatial: -1,
            max_layer: VideoLayer::INVALID,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VideoTransition {
    pub from: VideoLayer,
    pub to: VideoLayer,
    pub bandwidth_delta: i64,
}

/// Rates for str0m's `Bwe::set_bitrate(minimum, desired)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct BitrateGoal {
    /// Committed media usage. Sustained padding beyond media is not requested.
    pub minimum: i64,
    /// Usage plus the probe goal for unmet demand, or usage when none.
    pub desired: i64,
}

/// The probe goal `usage + max(delta * overage%, floor)` when demand exists.
/// While exploring, the minimum sustains committed usage so padding fills an
/// undershooting encoder; without demand no padding is requested (static
/// screen content would otherwise pad continuously).
pub fn bitrate_goal(usage: i64, delta: i64) -> BitrateGoal {
    let usage = usage.max(0);
    if delta <= 0 {
        return BitrateGoal {
            minimum: 0,
            desired: usage,
        };
    }
    BitrateGoal {
        minimum: usage,
        desired: usage + (delta * PROBE_OVERAGE_PCT / 100).max(PROBE_MIN_BPS),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_ordering_and_validity() {
        assert!(!VideoLayer::INVALID.is_valid());
        assert!(VideoLayer::new(1, 0).greater_than(VideoLayer::new(0, 3)));
        assert!(VideoLayer::new(1, 2).greater_than(VideoLayer::new(1, 1)));
        assert!(!VideoLayer::new(1, 1).greater_than(VideoLayer::new(1, 1)));
        assert!(VideoLayer::new(0, 0).greater_than(VideoLayer::INVALID));
        let mut rates = Bitrates::default();
        rates[2][3] = 7;
        assert_eq!(rate(&rates, VideoLayer::new(2, 3)), 7);
        assert_eq!(rate(&rates, VideoLayer::new(3, 0)), 0);
        assert_eq!(rate(&rates, VideoLayer::INVALID), 0);
    }

    #[test]
    fn probe_goal_uses_overage_and_floor() {
        assert_eq!(
            bitrate_goal(500_000, 0),
            BitrateGoal {
                minimum: 0,
                desired: 500_000
            }
        );
        assert_eq!(
            bitrate_goal(500_000, 50_000),
            BitrateGoal {
                minimum: 500_000,
                desired: 700_000
            }
        );
        assert_eq!(bitrate_goal(500_000, 1_000_000).desired, 1_700_000);
        assert_eq!(bitrate_goal(-1, 0), BitrateGoal::default());
    }
}
