//! Shared lower-output encoder groups: their pure planning.
//!
//! Transport owns current attachment; [`OutputGroups`] owns only requested
//! membership. An independent keyframe installs a requested source without
//! replacing the consumer's PeerConnection. Equal connection-level requests
//! share one correction/adaptation owner; the group's RTP-to-codec conversion
//! is applied once, not used as a membership key.
//!
//! Media objects stay with the caller: a group is identified by its slot and
//! refers to its forwarding source through [`MediaId`]; [`GroupMedia`] creates
//! group media and converts budgets.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::Hash;
use std::time::Duration;

/// Identity of one forwarding source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MediaId(pub u64);

/// The original-output recovery bit (slot 1).
pub const ORIGINAL_RECOVERY: u32 = 1 << 1;

/// One capture input's output controls. The caller applies it to the exact
/// stream that produced it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutputPlan {
    pub active: Vec<bool>,
    pub recovery_layers: Vec<usize>,
    pub bitrates: Vec<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputGroup {
    pub slot: usize,
    pub media: MediaId,
    pub budget: u32,
    pub active: bool,
    /// Input PTS from which this group's encoder output is current.
    pub start: Duration,
    /// A relay group still has to copy the received clock correlation.
    pub clock_pending: bool,
}

/// One consumer's request for the next input. `current` is the source the
/// consumer is attached to now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputDemand<K> {
    pub consumer: K,
    pub current: MediaId,
    pub budget: u32,
    pub active: bool,
    /// The consumer needs a lower representation.
    pub lower: bool,
    /// The consumer needs the original representation.
    pub original: bool,
}

/// The edge transport state `collectDemands` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EdgeState {
    pub connected: bool,
    pub local: bool,
    /// Whether a transport-wide target bitrate has been observed.
    pub observed: bool,
    pub video_budget: i64,
    pub target: i32,
    pub current: i32,
    pub prepare: i32,
    pub paused: bool,
    pub required_active_count: usize,
}

/// Creates group media and converts RTP budgets to codec budgets.
pub trait GroupMedia {
    type Error;
    /// Creates the encoded source of a derived group at `slot`. Its owner
    /// starts the established input frame when one exists.
    fn new_group(&mut self, slot: usize) -> Result<MediaId, Self::Error>;
    /// `EncodedSource.CodecBudget(0, rtp_budget)` of `media`.
    fn codec_budget(&self, media: MediaId, rtp_budget: i64) -> i64;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError<E> {
    /// Shared output groups need an original and a derived slot.
    Shape,
    Media(E),
}

impl<E: std::fmt::Display> std::fmt::Display for PlanError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shape => f.write_str("shared output groups need an original and derived slot"),
            Self::Media(error) => error.fmt(f),
        }
    }
}

impl<E: std::error::Error> std::error::Error for PlanError<E> {}

/// Edge demand: the remote lower representation is requested when the
/// framework targets, has acquired, or prepares the low layer.
pub fn edge_demand<K>(
    consumer: K,
    current: MediaId,
    state: &EdgeState,
    ceiling: u32,
) -> OutputDemand<K> {
    let active = state.connected && state.video_budget > 0;
    let lower = active
        && !state.local
        && state.observed
        && (state.target == 0
            || state.current == 0 && !state.paused
            || state.paused && state.prepare == 0);
    OutputDemand {
        consumer,
        current,
        active,
        lower,
        original: state.required_active_count > 1,
        budget: clamp_budget(state.video_budget, ceiling),
    }
}

/// Publication demand. A zero budget is an exhausted residual allocation, not
/// an unknown estimate.
pub fn publication_demand<K>(
    consumer: K,
    current: MediaId,
    required_active_count: usize,
    lowest_layer_budget: i64,
    ceiling: u32,
) -> OutputDemand<K> {
    let active = required_active_count > 0;
    OutputDemand {
        consumer,
        current,
        active,
        lower: active,
        original: required_active_count > 1,
        budget: clamp_budget(lowest_layer_budget, ceiling),
    }
}

fn clamp_budget(budget: i64, ceiling: u32) -> u32 {
    // The result lies in 1000..=max(1000, ceiling), which fits u32.
    budget.min(i64::from(ceiling)).max(1000) as u32
}

/// Requested membership of consumers in the source's output groups. Slot 0 is
/// the original source's lower layer; slot 1 is the original representation
/// and never a group; slots from 2 are derived groups.
#[derive(Debug, Clone)]
pub struct OutputGroups<K> {
    original: MediaId,
    relay: bool,
    groups: BTreeMap<usize, OutputGroup>,
    // `Some(None)` is an explicit "no group" membership, which an original
    // install moves to the original source; an absent key is not moved.
    memberships: HashMap<K, Option<usize>>,
    recovery: u32,
}

impl<K: Eq + Hash + Clone> OutputGroups<K> {
    /// `relay` reports that the source derives its outputs from received RTP.
    pub fn new(original: MediaId, relay: bool) -> Self {
        Self {
            original,
            relay,
            groups: BTreeMap::new(),
            memberships: HashMap::new(),
            recovery: 0,
        }
    }

    pub fn group(&self, slot: usize) -> Option<&OutputGroup> {
        self.groups.get(&slot)
    }

    pub fn group_mut(&mut self, slot: usize) -> Option<&mut OutputGroup> {
        self.groups.get_mut(&slot)
    }

    pub fn groups(&self) -> impl Iterator<Item = &OutputGroup> {
        self.groups.values()
    }

    pub fn groups_mut(&mut self) -> impl Iterator<Item = &mut OutputGroup> {
        self.groups.values_mut()
    }

    /// The consumer's requested group slot, if any.
    pub fn membership(&self, consumer: &K) -> Option<usize> {
        self.memberships.get(consumer).copied().flatten()
    }

    pub fn group_for_media(&self, media: MediaId) -> Option<&OutputGroup> {
        self.groups.values().find(|group| group.media == media)
    }

    /// Drops a retired consumer's membership.
    pub fn forget(&mut self, consumer: &K) {
        self.memberships.remove(consumer);
    }

    /// Records a keyframe request for `slot`'s output.
    pub fn request_recovery(&mut self, slot: usize) {
        self.recovery |= 1 << slot;
    }

    pub fn recovery(&self) -> u32 {
        self.recovery
    }

    pub fn clear_recovery(&mut self, mask: u32) {
        self.recovery &= !mask;
    }

    /// Moves pending recovery requests for active slots into the plan, as the
    /// next input's `BeginFrame` does. Requests for inactive slots are spent.
    pub fn take_recovery_layers(&mut self, plan: &mut OutputPlan) {
        let mut mask = std::mem::take(&mut self.recovery);
        let mut slot = 0;
        while mask != 0 {
            if mask & 1 != 0 && plan.active.get(slot).copied().unwrap_or(false) {
                plan.recovery_layers.push(slot);
            }
            mask >>= 1;
            slot += 1;
        }
    }

    /// Applies one capture generation's ceilings to every existing group.
    pub fn configure(&mut self, ceilings: &[u32]) {
        for (slot, group) in &mut self.groups {
            group.budget = ceilings[*slot];
        }
    }

    /// A new capture generation restarts every group's PTS origin.
    pub fn begin_generation(&mut self) {
        for group in self.groups.values_mut() {
            group.start = Duration::ZERO;
        }
    }

    #[cfg(test)]
    pub(crate) fn insert_test_group(&mut self, slot: usize, media: MediaId) {
        self.insert_group(slot, media, Duration::ZERO);
    }

    fn insert_group(&mut self, slot: usize, media: MediaId, input_pts: Duration) {
        self.groups.insert(
            slot,
            OutputGroup {
                slot,
                media,
                budget: 0,
                active: false,
                start: input_pts,
                clock_pending: self.relay,
            },
        );
    }

    fn ensure_initial(&mut self, budget: u32) {
        if self.groups.is_empty() {
            self.groups.insert(
                0,
                OutputGroup {
                    slot: 0,
                    media: self.original,
                    budget,
                    active: false,
                    start: Duration::ZERO,
                    clock_pending: false,
                },
            );
            self.memberships.clear();
        }
    }

    fn wants(&self, consumer: &K, slot: usize) -> bool {
        self.membership(consumer) == Some(slot)
    }

    fn compatible(
        &self,
        demands: &[OutputDemand<K>],
        slot: usize,
        demand: &OutputDemand<K>,
    ) -> bool {
        let media = self.groups[&slot].media;
        !demands.iter().any(|other| {
            other.active
                && other.lower
                && other.budget != demand.budget
                && (other.current == media || self.wants(&other.consumer, slot))
        })
    }

    /// The previous frame's active bit alone may outlive its last handoff.
    fn in_use(&self, demands: &[OutputDemand<K>], slot: Option<usize>) -> bool {
        let Some(group) = slot.and_then(|slot| self.groups.get(&slot)) else {
            return false;
        };
        group.active
            && demands.iter().any(|demand| {
                demand.active
                    && (demand.current == group.media || self.wants(&demand.consumer, group.slot))
            })
    }

    /// Plans group membership, activity and bitrates for the next input.
    ///
    /// `bitrates` are the configured output ceilings, where zero marks a
    /// failed producer. The plan is returned even with an error, as callers
    /// that only need its controls use it regardless.
    pub fn plan<M: GroupMedia>(
        &mut self,
        demands: &[OutputDemand<K>],
        bitrates: &[u32],
        format_count: usize,
        input_pts: Duration,
        media: &mut M,
    ) -> (OutputPlan, Result<(), PlanError<M::Error>>) {
        let slots = bitrates.len();
        let mut plan = OutputPlan {
            active: vec![false; slots],
            recovery_layers: Vec::new(),
            bitrates: bitrates.to_vec(),
        };
        if format_count != 2 || slots < 2 {
            return (plan, Err(PlanError::Shape));
        }
        self.ensure_initial(bitrates[0]);
        let present: HashSet<&K> = demands.iter().map(|demand| &demand.consumer).collect();
        let mut has_lower_demand = false;
        for demand in demands {
            has_lower_demand |= demand.active && demand.lower;
            plan.active[1] |= demand.active && demand.original;
        }
        self.memberships
            .retain(|consumer, _| present.contains(consumer));

        // Revalidate pending joins before they can prevent an incumbent from
        // adjusting in place. Membership order must not give stale demand
        // priority.
        for demand in demands {
            if let Some(slot) = self.membership(&demand.consumer)
                && demand.active
                && demand.lower
                && demand.current != self.groups[&slot].media
                && !self.compatible(demands, slot, demand)
            {
                self.memberships.remove(&demand.consumer);
            }
        }

        for demand in demands {
            if !demand.active || !demand.lower {
                if self.membership(&demand.consumer).is_some() {
                    self.recovery |= ORIGINAL_RECOVERY;
                }
                self.memberships.insert(demand.consumer.clone(), None);
                continue;
            }
            let mut selected = None;
            for (slot, &ceiling) in bitrates.iter().enumerate() {
                let Some(group) = self.groups.get(&slot) else {
                    continue;
                };
                if ceiling == 0 || group.budget != demand.budget {
                    continue;
                }
                if demand.current != group.media && !self.compatible(demands, slot, demand) {
                    continue;
                }
                let in_use = self.in_use(demands, Some(slot));
                if selected.is_none() || in_use {
                    selected = Some(slot);
                }
                if in_use {
                    break;
                }
            }
            if selected.is_none() || !self.in_use(demands, selected) {
                // A group's rates can change in place only when no other
                // current or requested member still needs a different budget.
                let owned = [
                    self.membership(&demand.consumer),
                    self.group_for_media(demand.current).map(|group| group.slot),
                ];
                let mut ordered: Vec<Option<usize>> = owned
                    .iter()
                    .copied()
                    .filter(|slot| self.in_use(demands, *slot))
                    .collect();
                // Keep an owned live pipeline before reviving an inactive
                // cached match.
                ordered.push(selected);
                ordered.extend(owned);
                ordered.extend(
                    (0..slots)
                        .filter(|slot| self.groups.contains_key(slot))
                        .map(Some),
                );
                if let Some(slot) = ordered
                    .into_iter()
                    .flatten()
                    .find(|slot| bitrates[*slot] != 0 && self.compatible(demands, *slot, demand))
                {
                    selected = Some(slot);
                }
            }
            if selected.is_none()
                && let Some(slot) =
                    (2..slots).find(|slot| !self.groups.contains_key(slot) && bitrates[*slot] > 0)
            {
                match media.new_group(slot) {
                    Ok(id) => {
                        self.insert_group(slot, id, input_pts);
                        selected = Some(slot);
                    }
                    Err(error) => return (plan, Err(PlanError::Media(error))),
                }
            }
            // Existing delivery remains while a prepared split releases its
            // old membership. No timer, retry queue or temporary capacity
            // exemption.
            let Some(slot) = selected else {
                continue;
            };
            let wanted = self.wants(&demand.consumer, slot);
            let group = self.groups.get_mut(&slot).expect("selected group exists");
            group.budget = demand.budget;
            if !group.active {
                group.start = input_pts;
            }
            if !wanted && demand.current != group.media {
                self.recovery |= 1 << slot;
            }
            self.memberships.insert(demand.consumer.clone(), Some(slot));
        }

        let slots_in_use: Vec<usize> = self.groups.keys().copied().collect();
        for slot in slots_in_use {
            let group = &self.groups[&slot];
            let mut active = slot == 0 && plan.active[1] && !has_lower_demand;
            for demand in demands.iter().filter(|demand| demand.active) {
                if self.wants(&demand.consumer, slot)
                    || demand.current == group.media
                        && (demand.lower || !demand.original || slot != 0)
                {
                    active = true;
                }
            }
            let next_active = active && bitrates[slot] > 0;
            let relay = self.relay;
            let group = self.groups.get_mut(&slot).expect("group exists");
            if next_active && !group.active && relay {
                group.clock_pending = true;
            }
            group.active = next_active;
            plan.active[slot] = next_active;
            if next_active {
                let budget = media
                    .codec_budget(group.media, i64::from(group.budget))
                    .min(i64::from(bitrates[slot]))
                    .max(1000);
                plan.bitrates[slot] = budget as u32;
            }
        }
        (plan, Ok(()))
    }

    /// Returns the consumers to move on an independent frame of `group`
    /// (`None` is the original source), with their next source. `current`
    /// reports each consumer's attachment. The caller performs the
    /// replacement.
    pub fn install(
        &self,
        group: Option<usize>,
        original: bool,
        current: impl Fn(&K) -> MediaId,
    ) -> Vec<(K, MediaId)> {
        if group.is_none() && !original {
            return Vec::new();
        }
        let next = group.map_or(self.original, |slot| self.groups[&slot].media);
        self.memberships
            .iter()
            .filter(|(consumer, wanted)| **wanted == group && current(consumer) != next)
            .map(|(consumer, _)| (consumer.clone(), next))
            .collect()
    }
}

#[cfg(test)]
mod tests;
