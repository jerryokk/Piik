// Output group planning. Clock, sender-report and encoded-write cases need
// forwarding media and live with the Source tests.

use std::collections::HashMap;
use std::convert::Infallible;

use super::*;

const ORIGINAL: MediaId = MediaId(1);
const CEILINGS: [u32; 4] = [500_000, 2_000_000, 500_000, 500_000];

/// No RTP has been written, so codec budgets equal RTP budgets.
struct FakeMedia;

impl GroupMedia for FakeMedia {
    type Error = Infallible;
    fn new_group(&mut self, slot: usize) -> Result<MediaId, Infallible> {
        Ok(MediaId(100 + slot as u64))
    }
    fn codec_budget(&self, _: MediaId, rtp_budget: i64) -> i64 {
        rtp_budget.max(0)
    }
}

#[derive(Clone, Copy)]
struct Spec {
    consumer: u32,
    budget: u32,
    active: bool,
    lower: bool,
    original: bool,
}

fn lower(consumer: u32, budget: u32) -> Spec {
    Spec {
        consumer,
        budget,
        active: true,
        lower: true,
        original: false,
    }
}

struct Harness {
    groups: OutputGroups<u32>,
    current: HashMap<u32, MediaId>,
    pts: Duration,
}

impl Harness {
    fn new() -> Self {
        Self {
            groups: OutputGroups::new(ORIGINAL, false),
            current: HashMap::new(),
            pts: Duration::ZERO,
        }
    }

    fn current(&self, consumer: u32) -> MediaId {
        self.current.get(&consumer).copied().unwrap_or(ORIGINAL)
    }

    fn plan_at(&mut self, pts: Duration, specs: &[Spec]) -> OutputPlan {
        self.pts = pts;
        let demands: Vec<_> = specs
            .iter()
            .map(|spec| OutputDemand {
                consumer: spec.consumer,
                current: self.current(spec.consumer),
                budget: spec.budget,
                active: spec.active,
                lower: spec.lower,
                original: spec.original,
            })
            .collect();
        let (plan, result) = self
            .groups
            .plan(&demands, &CEILINGS, 2, pts, &mut FakeMedia);
        result.unwrap();
        plan
    }

    fn plan(&mut self, specs: &[Spec]) -> OutputPlan {
        self.plan_at(self.pts + Duration::from_secs(1), specs)
    }

    fn install(&mut self, group: Option<usize>, original: bool) {
        let current = self.current.clone();
        let moves = self.groups.install(group, original, |consumer| {
            current.get(consumer).copied().unwrap_or(ORIGINAL)
        });
        self.current.extend(moves);
    }

    fn media(&self, slot: usize) -> MediaId {
        self.groups.group(slot).unwrap().media
    }

    fn member(&self, consumer: u32) -> Option<usize> {
        self.groups.membership(&consumer)
    }
}

#[test]
fn share_split_rejoin_and_retire() {
    let mut h = Harness::new();
    let (a, b) = (1, 2);
    let mut demands = vec![lower(a, 100_000), lower(b, 100_000)];
    let shared = h.plan(&demands);
    assert!(
        h.member(a) == h.member(b)
            && h.groups.groups().count() == 1
            && shared.bitrates[0] == 100_000
            && !shared.active[1],
        "compatible consumers did not share one output"
    );

    demands[1].budget = 300_000;
    let split = h.plan(&demands);
    let (ga, gb) = (h.member(a).unwrap(), h.member(b).unwrap());
    assert!(
        ga != gb
            && split.bitrates[ga] == 100_000
            && split.bitrates[gb] == 300_000
            && h.current(b) == ORIGINAL,
        "different budgets coupled or moved before recovery"
    );
    h.install(Some(gb), false);
    assert_eq!(
        h.current(b),
        h.media(gb),
        "recovery did not install prepared group"
    );

    demands[0].budget = 60_000;
    let changed = h.plan(&demands);
    assert!(
        h.member(a) == Some(ga) && h.member(b) == Some(gb) && changed.bitrates[gb] == 300_000,
        "one consumer's rate change altered its sibling"
    );

    demands[0].budget = 300_000;
    h.plan(&demands);
    assert!(
        h.member(a) == Some(gb) && h.current(a) == ORIGINAL,
        "compatible rejoin bypassed recovery"
    );
    h.install(Some(gb), false);
    let rejoined = h.plan(&demands);
    assert!(
        !rejoined.active[ga] && rejoined.active[gb],
        "unused encoder did not retire after rejoin"
    );

    demands[0].lower = false;
    demands[0].original = true;
    let pending = h.plan(&demands);
    assert!(
        pending.active[1] && pending.active[gb] && h.current(a) == h.media(gb),
        "original handoff retired active source"
    );
    h.install(None, true);
    assert!(
        h.current(a) == ORIGINAL && h.current(b) == h.media(gb),
        "original handoff moved sibling"
    );

    demands.truncate(1);
    let retired = h.plan(&demands);
    assert!(
        !retired.active[gb],
        "last lower member did not release its encoder"
    );
    demands[0].active = false;
    let inactive = h.plan(&demands);
    assert!(
        !inactive.active[ga] && !inactive.active[gb],
        "disconnected consumer kept an output active"
    );

    demands[0] = lower(a, 300_000);
    let reactivated = h.plan(&demands);
    assert!(
        reactivated.active[gb] && !reactivated.active[1] && h.member(a) == Some(gb),
        "inactive compatible group could not reactivate"
    );
    h.install(Some(gb), false);
    let started = h.groups.group(gb).unwrap().start;
    h.groups.clear_recovery(u32::MAX);
    let ga_budget = h.groups.group(ga).unwrap().budget;
    demands[0].budget = ga_budget;
    let repriced = h.plan(&demands);
    assert!(
        h.member(a) == Some(gb)
            && h.current(a) == h.media(gb)
            && repriced.active[gb]
            && !repriced.active[ga]
            && repriced.bitrates[gb] == ga_budget
            && h.groups.group(gb).unwrap().start == started
            && h.groups.recovery() == 0,
        "inactive budget match replaced live pipeline"
    );

    let c = 3;
    demands.push(lower(c, ga_budget));
    h.plan(&demands);
    assert_eq!(
        h.member(c),
        Some(gb),
        "inactive budget match took precedence over a compatible active output"
    );
    h.install(Some(gb), false);
    let settled = h.plan(&demands);
    assert!(
        !settled.active[ga] && h.current(c) == h.media(gb),
        "last handoff retained an unneeded output"
    );
}

/// Consumers a (100 kb/s) and b (300 kb/s) at input 10 s; b owns a derived group.
fn lifecycle_fixture() -> (Harness, Vec<Spec>, usize) {
    let mut h = Harness::new();
    let demands = vec![lower(1, 100_000), lower(2, 300_000)];
    h.plan_at(Duration::from_secs(10), &demands);
    let gb = h.member(2).expect("fixture did not prepare b's group");
    assert!(
        gb >= 2,
        "fixture did not prepare an independent lower group"
    );
    (h, demands, gb)
}

#[test]
fn preserve_current_demand_during_admission() {
    for pending in [false, true] {
        for reverse in [false, true] {
            let (mut h, mut demands, gb) = lifecycle_fixture();
            let (a, b) = (1, 2);
            let ga = h.member(a).unwrap();
            h.install(Some(ga), false);
            h.install(Some(gb), false);
            let group_count = h.groups.groups().count();
            demands[1].budget = 100_000;
            if pending {
                h.plan(&demands);
                assert!(
                    h.member(b) == Some(ga) && h.current(b) == h.media(gb),
                    "fixture did not prepare a compatible rejoin"
                );
            }
            h.groups.clear_recovery(u32::MAX);
            demands[0].budget = 60_000;
            if reverse {
                demands.swap(0, 1);
            }
            let plan = h.plan(&demands);
            assert!(
                h.member(a) == Some(ga)
                    && h.member(b) == Some(gb)
                    && h.groups.groups().count() == group_count
                    && plan.bitrates[ga] == 60_000
                    && plan.bitrates[gb] == 100_000
                    && h.groups.recovery() == 0,
                "pending={pending} reverse={reverse}: admission displaced an incumbent: {plan:?}"
            );
        }
    }
}

#[test]
fn retain_original_recovery_after_pending_join() {
    let (mut h, mut demands, gb) = lifecycle_fixture();
    let ga = h.member(1).unwrap();
    h.install(Some(ga), false);
    h.install(Some(gb), false);
    demands[1].budget = demands[0].budget;
    h.plan(&demands);
    h.groups.clear_recovery(u32::MAX);
    demands[0].budget = 60_000;
    demands[1].lower = false;
    demands[1].original = true;
    h.plan(&demands);
    assert!(
        h.member(2).is_none()
            && h.groups.recovery() & ORIGINAL_RECOVERY != 0
            && h.current(2) == h.media(gb),
        "return to original lost recovery or replaced the current source early"
    );
}

#[test]
fn reactivation_restarts_the_group_at_current_input() {
    let (mut h, mut demands, gb) = lifecycle_fixture();
    h.install(Some(gb), false);
    demands[1].lower = false;
    demands[1].original = true;
    h.plan(&demands);
    h.install(None, true);
    h.plan(&demands);
    assert!(
        !h.groups.group(gb).unwrap().active,
        "retired group remained active"
    );
    demands[1].lower = true;
    demands[1].original = false;
    let plan = h.plan_at(Duration::from_secs(12), &demands);
    let group = h.groups.group(gb).unwrap();
    // Earlier access units (before 12 s) cannot install the reactivated group.
    assert!(plan.active[gb] && group.start == Duration::from_secs(12));
    assert_eq!(h.current(2), ORIGINAL);
    assert!(
        h.groups.recovery() & (1 << gb) != 0,
        "rejoin did not request group recovery"
    );
    let mut plan = plan;
    h.groups.take_recovery_layers(&mut plan);
    assert!(plan.recovery_layers.contains(&gb) && h.groups.recovery() == 0);
}

#[test]
fn relay_group_reactivation_copies_the_received_clock() {
    let mut groups = OutputGroups::new(ORIGINAL, true);
    let demands = [OutputDemand {
        consumer: 1u32,
        current: ORIGINAL,
        budget: 100_000,
        active: true,
        lower: true,
        original: false,
    }];
    groups
        .plan(
            &demands,
            &[100_000, 1_000_000, 100_000, 100_000],
            2,
            Duration::ZERO,
            &mut FakeMedia,
        )
        .1
        .unwrap();
    // Activation marks the clock copy; the caller clears it after copying.
    let group = groups.group_mut(0).unwrap();
    assert!(group.active && group.clock_pending);
    group.clock_pending = false;
    groups
        .plan(
            &demands,
            &[100_000, 1_000_000, 100_000, 100_000],
            2,
            Duration::ZERO,
            &mut FakeMedia,
        )
        .1
        .unwrap();
    assert!(
        !groups.group(0).unwrap().clock_pending,
        "a continuing group copied again"
    );
    groups.group_mut(0).unwrap().active = false;
    groups
        .plan(
            &demands,
            &[100_000, 1_000_000, 100_000, 100_000],
            2,
            Duration::ZERO,
            &mut FakeMedia,
        )
        .1
        .unwrap();
    assert!(groups.group(0).unwrap().clock_pending);
}

#[test]
fn failed_slot_is_never_selected_or_activated() {
    let mut h = Harness::new();
    let demands = [lower(1, 100_000), lower(2, 300_000)];
    h.plan(&demands);
    let gb = h.member(2).unwrap();
    let mut groups = h.groups.clone();
    let mut ceilings = CEILINGS;
    ceilings[gb] = 0;
    let demand = |consumer, budget| OutputDemand {
        consumer,
        current: ORIGINAL,
        budget,
        active: true,
        lower: true,
        original: false,
    };
    let (plan, result) = groups.plan(
        &[demand(1, 100_000), demand(2, 300_000)],
        &ceilings,
        2,
        Duration::from_secs(5),
        &mut FakeMedia,
    );
    result.unwrap();
    // With no free slot, the existing membership remains for the caller's
    // failure retirement, but the failed encoder is never activated.
    assert!(!plan.active[gb] && !groups.group(gb).unwrap().active);
    assert_eq!(groups.membership(&2), Some(gb));
}

#[test]
fn rejects_sources_without_original_and_derived_slots() {
    let mut groups = OutputGroups::<u32>::new(ORIGINAL, false);
    let (plan, result) = groups.plan(&[], &[500_000], 1, Duration::ZERO, &mut FakeMedia);
    assert_eq!(result, Err(PlanError::Shape));
    assert_eq!(plan.bitrates, [500_000]);
}

#[test]
fn edge_and_publication_demands() {
    let state = EdgeState {
        connected: true,
        local: false,
        observed: true,
        video_budget: 200_000,
        target: 1,
        current: 1,
        prepare: -1,
        paused: false,
        required_active_count: 2,
    };
    let original = edge_demand(1, ORIGINAL, &state, 100_000);
    assert!(original.active && !original.lower && original.original && original.budget == 100_000);
    let lower_target = edge_demand(1, ORIGINAL, &EdgeState { target: 0, ..state }, 500_000);
    assert!(lower_target.lower && lower_target.budget == 200_000);
    let prepare = EdgeState {
        paused: true,
        prepare: 0,
        ..state
    };
    assert!(edge_demand(1, ORIGINAL, &prepare, 500_000).lower);
    assert!(
        !edge_demand(
            1,
            ORIGINAL,
            &EdgeState {
                local: true,
                target: 0,
                ..state
            },
            1
        )
        .lower
    );
    assert!(
        !edge_demand(
            1,
            ORIGINAL,
            &EdgeState {
                observed: false,
                target: 0,
                ..state
            },
            1
        )
        .lower
    );
    let disconnected = edge_demand(
        1,
        ORIGINAL,
        &EdgeState {
            connected: false,
            ..state
        },
        1,
    );
    assert!(!disconnected.active && disconnected.budget == 1000);

    for (count, active, original) in [(0, false, false), (1, true, false), (2, true, true)] {
        let demand = publication_demand(1, ORIGINAL, count, 0, 500_000);
        assert_eq!(
            (demand.active, demand.lower, demand.original),
            (active, active, original)
        );
        assert_eq!(
            demand.budget, 1000,
            "an exhausted allocation keeps the floor"
        );
    }
}
