use super::*;
use crate::layer::{Bitrates, MAX_SPATIAL_LAYERS};

/// Allocation metadata, not simulated network evidence.
#[derive(Clone, Default)]
pub(crate) struct Fixture {
    pub(crate) rates: Bitrates,
    pub(crate) available: Vec<i32>,
    pub(crate) ceilings: Vec<i64>,
    pub(crate) controlled: bool,
    pub(crate) published: i32,
    pub(crate) temporal_seen: i32,
}

impl Fixture {
    pub(crate) fn new(ceilings: &[i64]) -> Self {
        Self {
            ceilings: ceilings.to_vec(),
            published: -1,
            temporal_seen: -1,
            ..Default::default()
        }
    }

    fn rates(&mut self, layers: &[i64]) {
        self.rates = Bitrates::default();
        for (spatial, value) in layers.iter().enumerate().take(MAX_SPATIAL_LAYERS) {
            self.rates[spatial][0] = *value;
        }
    }
}

impl SourceView for Fixture {
    fn layered_bitrate(&self) -> (Vec<i32>, Bitrates) {
        (self.available.clone(), self.rates)
    }

    fn layer_ceilings(&self) -> Vec<i64> {
        self.ceilings.clone()
    }

    fn rate_controlled(&self, layer: i32) -> bool {
        self.controlled && layer == 0
    }

    fn max_published_layer(&self) -> i32 {
        self.published
    }

    fn max_temporal_layer_seen(&self) -> i32 {
        self.temporal_seen
    }
}

fn new_output(source: &Fixture, budget: i64) -> OutputAllocator {
    let mut output = OutputAllocator::new(CodecKind::Vp8, budget);
    output.set_max_spatial_layer(source, 2);
    output.set_max_temporal_layer(source, 0);
    output
}

fn assert_state(
    output: &OutputAllocator,
    source: &Fixture,
    check: impl Fn(&OutputState) -> bool,
    message: &str,
) {
    let state = output.state(source);
    assert!(check(&state), "{message}: {state:?}");
}

#[test]
fn output_uses_library_allocation_and_exact_lifetime() {
    let mut receiver = Fixture::new(&[100_000, 300_000, 900_000]);
    let mut output = new_output(&receiver, 1_000_000);
    assert_state(
        &output,
        &receiver,
        |s| s.target == -1 && s.current == -1,
        "unmeasured source pretended to have a target",
    );
    // Availability can arrive before its discovery notification.
    receiver.available = vec![0];
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| s.target == 0 && !s.paused,
        "available source waited for discovery",
    );
    receiver.published = 2;
    receiver.temporal_seen = 0;
    output.reconcile(&receiver);
    output.set_budget(&receiver, 0);
    assert_state(
        &output,
        &receiver,
        |s| s.paused && s.prepare == -1 && s.video_budget == 0,
        "bootstrap ignored an exhausted budget",
    );
    receiver.rates(&[100_000, 300_000, 900_000]);
    receiver.available = vec![0, 1, 2];
    let mut sibling = OutputAllocator::new(CodecKind::Vp8, 1_000_000);
    sibling.set_max_temporal_layer(&receiver, 0);
    sibling.set_max_spatial_layer(&receiver, 2);
    for (budget, layer) in [
        (1_000_000, 2),
        (200_000, 0),
        (0, -1),
        (400_000, 1),
        (1_000_000, 2),
    ] {
        output.set_budget(&receiver, budget);
        assert_state(
            &output,
            &receiver,
            |s| {
                s.target == layer
                    && s.paused == (layer == -1)
                    && s.current == -1
                    && s.video_budget == budget
            },
            &format!("budget {budget}"),
        );
    }
    // Below every nominal output, the lowest codec stays prepared while only
    // actual reduced media may resume the network target.
    output.set_budget(&receiver, 60_000);
    assert_state(
        &output,
        &receiver,
        |s| s.paused && s.target == -1 && s.prepare == 0 && s.video_budget == 60_000,
        "below-lowest budget",
    );
    // A new child's seed cannot veto a warm source's first video; receiving
    // feedback alone is not congestion.
    output.set_feedback_observed(Some(false));
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| !s.paused && s.target >= 0,
        "warm source could not establish feedback",
    );
    output.set_feedback_observed(Some(true));
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| !s.paused,
        "first feedback reinterpreted the seed as a reduction",
    );
    output.set_budget(&receiver, 50_000);
    assert_state(
        &output,
        &receiver,
        |s| s.paused,
        "bootstrap bypassed a committed reduction",
    );
    output.set_budget(&receiver, 60_000);
    receiver.controlled = true;
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| !s.paused && s.target == 0,
        "live lowest codec could not continue adapting",
    );
    // A constrained encoder inside its layer restores that layer's ceiling
    // before probing higher, including with no higher output at all.
    receiver.rates[0][0] = 50_000;
    output.reconcile(&receiver);
    for maximum in [2, 0] {
        output.set_max_spatial_layer(&receiver, maximum);
        assert_eq!(
            output.probe_demand(&receiver),
            (50_000, 50_000),
            "current codec recovery skipped (maximum={maximum})"
        );
        assert!(
            output.bandwidth_limited(&receiver),
            "same-layer restriction reported healthy (maximum={maximum})"
        );
    }
    output.set_budget(&receiver, 100_000);
    assert_eq!(
        output.probe_demand(&receiver).0,
        0,
        "satisfied codec ceiling retained probe demand"
    );
    assert!(
        !output.bandwidth_limited(&receiver),
        "satisfied codec ceiling retained a restriction"
    );
    receiver.controlled = false;
    output.set_budget(&receiver, 60_000);
    assert_eq!(
        output.probe_demand(&receiver).0,
        0,
        "raw forwarding invented same-layer demand"
    );
    assert!(
        !output.bandwidth_limited(&receiver),
        "raw forwarding invented an encoder restriction"
    );
    receiver.controlled = true;
    output.set_max_spatial_layer(&receiver, 2);
    receiver.rates[0][0] = 100_000;
    output.set_budget(&receiver, 0);
    assert_state(
        &output,
        &receiver,
        |s| s.paused,
        "codec control overrode an exhausted budget",
    );
    receiver.available = vec![1, 2];
    receiver.rates[0][0] = 0;
    output.set_budget(&receiver, 60_000);
    assert_state(
        &output,
        &receiver,
        |s| s.paused,
        "codec control selected an unavailable lowest layer",
    );
    receiver.available = vec![0, 1, 2];
    receiver.controlled = false;
    receiver.rates[0][0] = 50_000;
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| !s.paused && s.target == 0 && s.video_budget == 60_000,
        "real reduced output did not resume",
    );
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| !s.paused && s.target == 2 && s.video_budget == 1_000_000,
        "weak-child adaptation changed the shared high output",
    );
    // A source measurement is not new evidence that this connection lost capacity.
    receiver.rates[2][0] = 0;
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2,
        "unmeasured active layer treated as unavailable",
    );
    receiver.rates[2][0] = 1_050_000;
    sibling.reconcile(&receiver);
    sibling.set_budget(&receiver, 1_000_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2 && s.video_budget == 1_000_000,
        "unchanged budget downgraded a healthy output",
    );
    sibling.set_budget(&receiver, 1_020_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2,
        "increased capacity downgraded an output",
    );
    sibling.set_budget(&receiver, 800_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 1,
        "capacity reduction failed to constrain",
    );
    receiver.rates[1][0] = 850_000;
    sibling.reconcile(&receiver);
    sibling.set_budget(&receiver, 800_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 1,
        "measured variation downgraded a constrained output",
    );
    sibling.set_congestion(Congestion::Congested);
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 0,
        "confirmed congestion retained an output above budget",
    );
    sibling.set_congestion(Congestion::None);

    // A recovered low layer does not authorize an unmeasured high layer.
    receiver.rates(&[50_000]);
    receiver.available = vec![0];
    sibling.set_budget(&receiver, 60_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 0,
        "recovery did not select the measured layer",
    );
    receiver.available = vec![0, 1, 2];
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 0,
        "a resumed unmeasured layer bypassed the budget",
    );
    receiver.rates[1][0] = 300_000;
    receiver.rates[2][0] = 900_000;
    sibling.set_budget(&receiver, 1_000_000);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2,
        "increased capacity did not restore the high output",
    );
    // Missing windows keep a legal target but not an unavailable or
    // over-subscription one.
    receiver.rates = Bitrates::default();
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2 && s.video_budget == 1_000_000,
        "missing windows changed a legal allocation",
    );
    receiver.available = vec![0];
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target <= 0 && s.video_budget == 1_000_000,
        "missing windows kept an unavailable target",
    );
    receiver.available = vec![0, 1, 2];
    receiver.rates(&[50_000, 300_000, 900_000]);
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2,
        "restored source did not resume",
    );
    receiver.rates = Bitrates::default();
    sibling.set_max_spatial_layer(&receiver, 0);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target <= 0 && s.video_budget == 1_000_000,
        "missing windows kept a target above the subscription",
    );
    receiver.rates(&[50_000, 300_000, 900_000]);
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 0,
        "stable allocation ignored the subscription limit",
    );
    // A stopped upper output may be prepared, never selected.
    receiver.rates(&[100_000]);
    receiver.available = vec![0];
    output.set_budget(&receiver, 1_000_000);
    assert_state(
        &output,
        &receiver,
        |s| s.prepare == 2 && s.target == 0,
        "stopped output preparation changed availability",
    );
    output.set_budget(&receiver, 200_000);
    assert_state(
        &output,
        &receiver,
        |s| s.prepare == 0 && s.target == 0,
        "low budget prepared an unaffordable output",
    );
    output.set_max_spatial_layer(&receiver, 1);
    output.set_budget(&receiver, 1_000_000);
    assert_state(
        &output,
        &receiver,
        |s| s.prepare == 1,
        "preparation exceeded the child maximum",
    );
    output.set_max_spatial_layer(&receiver, 2);
    // Put the accepted frame on another layer so begin_frame must fence it.
    output.current = (output.state(&receiver).target + 1) % 3;
    let before = output.state(&receiver);
    output.begin_frame();
    let after = output.state(&receiver);
    assert!(
        after.current == -1 && after.target == before.target && after.requested == before.requested,
        "frame fence replaced its allocation: {before:?} {after:?}"
    );
    output.set_budget(&receiver, 200_000);
    output.resync();
    assert_state(
        &output,
        &receiver,
        |s| s.target == -1 && s.current == -1,
        "resync retained old output state",
    );
    output.reconcile(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| s.target == 0 && s.video_budget == 200_000,
        "resync discarded the budget",
    );
    receiver.available = vec![0, 1, 2];
    receiver.rates(&[100_000, 300_000, 900_000]);
    sibling.set_max_spatial_layer(&receiver, 2);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 2,
        "high layer did not resume before resync",
    );
    sibling.resync();
    receiver.rates(&[100_000]);
    sibling.reconcile(&receiver);
    assert_state(
        &sibling,
        &receiver,
        |s| s.target == 0,
        "resync reused an unmeasured retired allocation",
    );
    output.set_budget(&receiver, 1_000_000);
    output.receiver_restart(&receiver);
    assert_state(
        &output,
        &receiver,
        |s| s.current == -1,
        "restart retained old output state",
    );
    output.close();
    output.set_budget(&receiver, 1_000_000);
    assert_eq!(
        output.state(&receiver),
        OutputState::CLOSED,
        "closed output revived"
    );
    assert_eq!(output.probe_demand(&receiver), (0, 0));
    assert!(!output.bandwidth_limited(&receiver));
}

#[test]
fn dormant_ceiling_is_probe_demand_and_goal_feeds_bwe() {
    let mut receiver = Fixture::new(&[300_000, 1_200_000]);
    receiver.published = 0;
    receiver.temporal_seen = 0;
    receiver.available = vec![0];
    receiver.rates(&[280_000]);
    let mut output = OutputAllocator::new(CodecKind::Vp8, 500_000);
    output.set_max_temporal_layer(&receiver, 0);
    output.set_max_spatial_layer(&receiver, 1);
    assert_state(
        &output,
        &receiver,
        |s| s.target == 0 && s.prepare == 0,
        "dormant high layer",
    );
    // The upper output is stopped: its ceiling is desired demand only.
    assert_eq!(
        output.probe_demand(&receiver),
        (1_200_000 - 280_000, 280_000)
    );
    let goal = output.bitrate_goal(&receiver);
    assert_eq!(goal, bitrate_goal(280_000, 920_000));
    assert!(goal.desired > goal.minimum);
    // With the high layer measured, the next transition is that layer.
    receiver.available = vec![0, 1];
    receiver.rates(&[280_000, 1_100_000]);
    receiver.published = 1;
    output.reconcile(&receiver);
    assert_eq!(output.state(&receiver).target, 0);
    assert!(output.bandwidth_limited(&receiver), "deficient allocation");
    // While the target is still being acquired there is no higher transition,
    // so the dormant ceiling remains the demand.
    assert_eq!(
        output.probe_demand(&receiver),
        (1_200_000 - 280_000, 280_000)
    );
    let key = InboundPacket::new(
        CodecKind::Vp8,
        0,
        1,
        0,
        &[0x10, 0x00],
        std::time::Instant::now(),
    )
    .marker(true);
    output.forward(&key).expect("keyframe");
    assert_eq!(
        output.probe_demand(&receiver),
        (1_100_000 - 280_000, 280_000)
    );
    output.set_budget(&receiver, 2_000_000);
    assert_eq!(output.state(&receiver).target, 1);
    assert_eq!(
        output.probe_demand(&receiver).0,
        0,
        "satisfied output retained demand"
    );
    assert_eq!(
        output.bitrate_goal(&receiver).minimum,
        0,
        "no padding without demand"
    );
}

#[test]
fn required_active_count_follows_target_current_and_prepare() {
    let mut receiver = Fixture::new(&[100_000, 300_000]);
    let mut output = OutputAllocator::new(CodecKind::Vp8, 1_000_000);
    output.set_max_spatial_layer(&receiver, 1);
    assert_eq!(
        output.required_active_count(&receiver, false),
        2,
        "unselected output keeps its maximum"
    );
    assert_eq!(output.required_active_count(&receiver, true), 2);
    receiver.available = vec![0];
    receiver.published = 0;
    receiver.temporal_seen = 0;
    receiver.rates(&[90_000]);
    output.reconcile(&receiver);
    output.set_feedback_observed(Some(true));
    output.set_budget(&receiver, 200_000);
    assert_eq!(output.required_active_count(&receiver, false), 1);
    assert_eq!(
        output.required_active_count(&receiver, true),
        1,
        "upper ceiling is unaffordable"
    );
    output.set_budget(&receiver, 400_000);
    assert_eq!(
        output.required_active_count(&receiver, true),
        2,
        "observed capacity prepares the dormant output"
    );
    output.set_budget(&receiver, 0);
    assert_eq!(output.required_active_count(&receiver, false), 0);
    output.close();
    assert_eq!(output.required_active_count(&receiver, true), 0);
}

#[test]
fn committed_budget_ignores_unverified_and_probe_capacity() {
    let mut budget = CommittedBudget::new(1_200_000);
    budget.on_congestion(Congestion::EarlyWarning, 100_000_000);
    budget.on_congestion(Congestion::Congested, 100_000_000);
    assert_eq!(
        budget.value(),
        1_200_000,
        "congestion promoted an unverified capacity"
    );
    assert!(budget.on_congestion(Congestion::Congested, 500_000));
    budget.on_congestion(Congestion::None, 800_000);
    assert_eq!(
        budget.value(),
        500_000,
        "clearing congestion replaced the committed reduction"
    );
    budget.probe_started();
    assert!(
        !budget.on_congestion(Congestion::Congested, 250_000),
        "probe congestion cut committed capacity"
    );
    assert_eq!(budget.value(), 500_000);
    // An empty or unacknowledged probe cannot raise capacity.
    assert!(!budget.probe_finished(false, 900_000));
    assert_eq!(budget.value(), 500_000);
    budget.on_congestion(Congestion::Congested, 250_000);
    assert_eq!(
        budget.value(),
        250_000,
        "post-probe media congestion did not reduce capacity"
    );
    budget.probe_started();
    assert!(budget.probe_finished(true, 800_000));
    assert_eq!(budget.value(), 800_000);
    budget.probe_started();
    budget.reset();
    assert!(
        budget.on_congestion(Congestion::Congested, 200_000),
        "retired probe suppressed congestion"
    );
}

#[test]
fn forwarded_frames_update_current_and_close_stops_forwarding() {
    use std::time::Instant;
    let mut receiver = Fixture::new(&[100_000]);
    receiver.available = vec![0];
    receiver.published = 0;
    receiver.temporal_seen = 0;
    let mut output = OutputAllocator::new(CodecKind::H264, 1_000_000);
    output.set_max_spatial_layer(&receiver, 0);
    output.reconcile(&receiver);
    assert_eq!(output.keyframe_request(), Some(0));
    let now = Instant::now();
    let delta = InboundPacket::new(CodecKind::H264, 0, 1, 0, &[0x41, 0], now).marker(true);
    assert!(output.forward(&delta).is_none(), "delta started the output");
    let key = InboundPacket::new(CodecKind::H264, 0, 2, 3000, &[0x65, 0], now);
    let forwarded = output.forward(&key).expect("keyframe");
    assert!(
        forwarded.switched && output.state(&receiver).current == -1,
        "current before frame end"
    );
    let end = InboundPacket::new(CodecKind::H264, 0, 3, 3000, &[0x41, 0], now).marker(true);
    output.forward(&end).expect("frame end");
    assert_eq!(output.state(&receiver).current, 0);
    assert_eq!(output.keyframe_request(), None);
    output.close();
    assert!(output.forward(&end).is_none() && output.keyframe_request().is_none());
}
