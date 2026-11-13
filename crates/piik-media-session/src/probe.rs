//! Probe demand onto str0m's `Bwe::set_bitrate(minimum, desired)`.
//!
//! str0m starts an allocation probe only when `desired` rises above both its
//! previous value and the estimate; otherwise unmet demand waits for ALR or a
//! 15 s stagnant estimate, and media at about two thirds of the estimate is
//! neither. Unmet demand instead re-probes with a bounded padding cluster at
//! the goal while it stays unmet. This keeps that cadence with str0m's
//! own mechanisms: after each [`REPROBE_INTERVAL`] of unmet demand, `desired`
//! dips to the estimate for one BWE timer turn and is restored, which str0m
//! treats as a new allocation increase; `minimum` then equals `desired` for
//! [`PROBE_BURST`], so padding (capped by str0m at the estimate) carries it.

use std::time::{Duration, Instant};

use piik_media_forwarding::BitrateGoal;
use str0m::Rtc;
use str0m::bwe::Bitrate;

/// Re-probe cadence bound: unmet demand re-probes no more often than this.
pub const REPROBE_INTERVAL: Duration = Duration::from_secs(5);
/// Length of one padding cluster, about a GoogCC probe's settling time.
pub const PROBE_BURST: Duration = Duration::from_secs(1);
/// Long enough for str0m's BWE timer to take the lowered `desired` as its
/// baseline (its probe control runs on BWE timeouts, not on each write).
const DIP: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub(crate) struct ProbeDriver {
    goal: Option<BitrateGoal>,
    next: Option<Instant>,
    dip_until: Option<Instant>,
    burst_until: Option<Instant>,
}

impl ProbeDriver {
    pub fn goal(&self) -> Option<BitrateGoal> {
        self.goal
    }

    pub fn is_probing(&self) -> bool {
        self.burst_until.is_some()
    }

    /// Applies a changed goal. An active burst keeps its raised minimum; a
    /// pending dip keeps its lowered baseline until restored.
    pub fn apply(&mut self, rtc: &mut Rtc, goal: BitrateGoal) {
        if self.goal == Some(goal) {
            return;
        }
        self.goal = Some(goal);
        if self.dip_until.is_some() {
            return;
        }
        let minimum = if self.burst_until.is_some() {
            goal.desired
        } else {
            goal.minimum
        };
        set(rtc, minimum, goal.desired);
    }

    /// Timer path with the current committed estimate.
    pub fn poll(&mut self, rtc: &mut Rtc, estimate: i64, now: Instant) {
        let Some(goal) = self.goal else {
            return;
        };
        if let Some(until) = self.burst_until {
            if now >= until || goal.desired <= estimate {
                self.burst_until = None;
                self.next = Some(now + REPROBE_INTERVAL);
                set(rtc, goal.minimum, goal.desired);
            }
            return;
        }
        if goal.desired <= estimate {
            self.next = None;
            if self.dip_until.take().is_some() {
                set(rtc, goal.minimum, goal.desired);
            } else {
                // `apply` cannot push `minimum` past `desired`, so an output
                // that lowered its goal while the estimate covered it still
                // carries the old sustained level. Re-assert the goal's own
                // minimum once the demand no longer exceeds the estimate.
                set(rtc, goal.minimum, goal.desired.max(goal.minimum));
            }
            return;
        }
        if let Some(dip) = self.dip_until {
            if now >= dip {
                // Restoring `desired` above the estimate is an increase from
                // str0m's baseline: an allocation probe at up to 2x estimate,
                // with padding toward `desired` while it settles.
                self.dip_until = None;
                self.burst_until = Some(now + PROBE_BURST);
                set(rtc, goal.desired, goal.desired);
            }
            return;
        }
        if now >= *self.next.get_or_insert(now + REPROBE_INTERVAL) {
            self.dip_until = Some(now + DIP);
            set(rtc, goal.minimum.min(estimate), estimate.max(0));
        }
    }

    pub fn deadline(&self) -> Option<Instant> {
        self.dip_until.or(self.burst_until).or(self.next)
    }

    /// Withdraws all demand.
    pub fn clear(&mut self, rtc: &mut Rtc) {
        if self.goal.take().is_some() {
            set(rtc, 0, 0);
        }
        self.next = None;
        self.burst_until = None;
        self.dip_until = None;
    }
}

/// How one GoogCC estimate changes committed capacity. str0m exposes no
/// congestion state, so it is inferred: GoogCC reacts to overuse by
/// dropping below the acknowledged rate, while an application-limited
/// stream's estimate decays only toward 1.5x its own throughput.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EstimateChange {
    /// Above the committed value: a completed, non-congesting measurement.
    Raise(i64),
    /// At or below the media actually sent: confirmed congestion.
    Congested(i64),
    /// An application-limited or probe-settling decay; capacity is held.
    Hold,
}

/// Overuse is assumed when the estimate falls within 10% of usage.
const CONGESTION_MARGIN_PCT: i64 = 110;

pub(crate) fn classify(committed: i64, estimate: i64, usage: i64, probing: bool) -> EstimateChange {
    if estimate > committed {
        EstimateChange::Raise(estimate)
    } else if estimate < committed && !probing && estimate <= usage * CONGESTION_MARGIN_PCT / 100 {
        EstimateChange::Congested(estimate)
    } else {
        EstimateChange::Hold
    }
}

/// [`classify`] for an output that forwards an encoding its owner cannot
/// resize (a Native relay's downstream edge).
///
/// The margin above serves a published encoder: it follows a smaller budget
/// within a frame. A forwarded source has no such knob, and its committed
/// budget is what authorizes forwarding a layer at all: once it falls below
/// the layer's ceiling the output pauses and stops producing the feedback that
/// could recover it. So the estimate alone cannot revoke capacity here.
///
/// GoogCC drives the estimate toward the rate actually sent while a stream is
/// application-limited — a mostly static screen encodes at a few hundred kbps
/// — which would otherwise collapse a commitment the link never constrained.
/// Local refusals cannot either: they follow any queue that outlived the
/// pacer's bound, including the burst str0m emits when a forwarder attaches
/// while its estimator is still converging. Loss reported by the remote is the
/// one signal a bottleneck produces and application-limited sending does not.
#[derive(Debug, Default)]
pub(crate) struct CongestionEvidence {
    /// Latest receiver-report loss fraction, `0.0..=1.0`.
    loss: Option<f32>,
}

/// Receiver-report loss at or above which the estimate is capacity evidence
/// (the loss-congested threshold).
pub(crate) const CONGESTED_LOSS: f32 = 0.1;

impl CongestionEvidence {
    /// Records the remote's reported loss fraction.
    pub fn observe_loss(&mut self, loss: Option<f32>) {
        if loss.is_some() {
            self.loss = loss;
        }
    }

    /// Whether the remote currently reports congestion-level loss.
    pub fn confirmed_loss(&self) -> bool {
        self.loss.is_some_and(|loss| loss >= CONGESTED_LOSS)
    }

    pub fn classify(&mut self, committed: i64, estimate: i64, probing: bool) -> EstimateChange {
        if estimate > committed {
            return EstimateChange::Raise(estimate);
        }
        if probing || estimate >= committed {
            return EstimateChange::Hold;
        }
        if self.confirmed_loss() {
            return EstimateChange::Congested(estimate);
        }
        EstimateChange::Hold
    }
}

/// RTP bytes handed to str0m over the last [`METER_WINDOW`].
#[derive(Debug, Default)]
pub(crate) struct Meter {
    samples: std::collections::VecDeque<(Instant, u64)>,
    bytes: u64,
}

const METER_WINDOW: Duration = Duration::from_secs(1);

impl Meter {
    pub fn record(&mut self, now: Instant, bytes: u64) {
        self.samples.push_back((now, bytes));
        self.bytes += bytes;
        self.expire(now);
    }

    fn expire(&mut self, now: Instant) {
        while let Some((at, bytes)) = self.samples.front().copied() {
            if now.saturating_duration_since(at) < METER_WINDOW {
                break;
            }
            self.samples.pop_front();
            self.bytes -= bytes;
        }
    }

    /// Bits per second over the window ending at `now`.
    pub fn rate(&mut self, now: Instant) -> i64 {
        self.expire(now);
        (self.bytes * 8) as i64
    }
}

fn set(rtc: &mut Rtc, minimum: i64, desired: i64) {
    rtc.bwe().set_bitrate(bitrate(minimum), bitrate(desired));
}

pub(crate) fn bitrate(value: i64) -> Bitrate {
    Bitrate::bps(u64::try_from(value).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forwarded_output_holds_capacity_through_application_limited_decay() {
        // The relay's downstream edge started at 3 Mbps and forwards one 720p
        // layer measured at a few hundred kbps. GoogCC's application-limited
        // regime converges the estimate toward that rate; the link was never
        // the constraint, so the commitment stands.
        let mut evidence = CongestionEvidence::default();
        for estimate in [280_166, 210_000, 180_000, 120_000, 90_000] {
            assert_eq!(
                evidence.classify(3_000_000, estimate, false),
                EstimateChange::Hold,
                "an application-limited decay to {estimate} revoked capacity"
            );
        }
        // A completed measurement above the committed value still raises it.
        assert_eq!(
            evidence.classify(600_000, 700_000, false),
            EstimateChange::Raise(700_000)
        );
        // A settling recovery probe is not congestion, even at loss.
        evidence.observe_loss(Some(0.3));
        assert_eq!(
            evidence.classify(633_230, 254_557, true),
            EstimateChange::Hold
        );
    }

    #[test]
    fn forwarded_output_revokes_capacity_only_on_reported_loss() {
        // Startup bursts and forwarder attachment refusals are not evidence.
        let mut evidence = CongestionEvidence::default();
        for _ in 0..5 {
            assert_eq!(
                evidence.classify(3_000_000, 320_000, false),
                EstimateChange::Hold
            );
        }
        // Repair-level loss is ordinary recovery.
        evidence.observe_loss(Some(CONGESTED_LOSS / 2.0));
        assert_eq!(
            evidence.classify(3_000_000, 320_000, false),
            EstimateChange::Hold
        );
        // Confirmed loss with an estimate below the commitment is a link that
        // cannot carry it.
        evidence.observe_loss(Some(CONGESTED_LOSS));
        assert_eq!(
            evidence.classify(3_000_000, 320_000, false),
            EstimateChange::Congested(320_000)
        );
        // A later clean report stops revoking.
        evidence.observe_loss(Some(0.0));
        assert_eq!(
            evidence.classify(320_000, 300_000, false),
            EstimateChange::Hold
        );
    }

    #[test]
    fn fulfilled_demand_cancels_pending_probe_dip() {
        let now = Instant::now();
        let mut rtc = Rtc::builder().enable_bwe(Some(bitrate(100_000))).build(now);
        let mut probe = ProbeDriver::default();
        probe.apply(
            &mut rtc,
            BitrateGoal {
                minimum: 100_000,
                desired: 1_000_000,
            },
        );
        probe.poll(&mut rtc, 100_000, now);
        let dip = now + REPROBE_INTERVAL;
        probe.poll(&mut rtc, 100_000, dip);
        assert_eq!(probe.deadline(), Some(dip + DIP));
        probe.apply(
            &mut rtc,
            BitrateGoal {
                minimum: 0,
                desired: 50_000,
            },
        );
        probe.poll(&mut rtc, 100_000, dip + DIP);
        assert_eq!(
            probe.deadline(),
            None,
            "fulfilled demand retained an expired timer"
        );
        let resumed = dip + DIP;
        let goal = BitrateGoal {
            minimum: 200_000,
            desired: 2_000_000,
        };
        probe.apply(&mut rtc, goal);
        assert_eq!(probe.goal(), Some(goal));
        assert!(probe.dip_until.is_none());
        probe.poll(&mut rtc, 100_000, resumed);
        let next = resumed + REPROBE_INTERVAL;
        assert_eq!(probe.deadline(), Some(next));
        probe.poll(&mut rtc, 100_000, next);
        assert_eq!(probe.deadline(), Some(next + DIP));
        probe.poll(&mut rtc, 100_000, next + DIP);
        assert!(probe.is_probing());
        assert_eq!(probe.deadline(), Some(next + DIP + PROBE_BURST));
    }
}
