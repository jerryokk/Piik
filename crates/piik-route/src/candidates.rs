//! Candidate eligibility, planning, ordering and capacity accounting.

use std::cmp::Ordering;
use std::collections::HashSet;

use piik_protocol::{Role, ViewerRoutePreference};

use crate::controller::{Attempt, Operation};
use crate::controller::{Participant, compare_participant, stable_pair_rank};
use crate::{
    CandidatePlan, CandidateReservation, CandidateTuple, CommittedEdge, Controller,
    EndpointRetirement, EndpointTransition, PublicationPlan, ReservationKind, ResourceRef,
    RetirementKind, TransitionKind, UpstreamKind,
};

impl Controller {
    /// Applies site and Host policy before a Viewer choice. The choice covers
    /// only its incoming hop; a Peer parent may itself use SFU.
    pub fn allows_viewer_route_preference(
        &self,
        preference: impl Into<Option<ViewerRoutePreference>>,
    ) -> bool {
        let preference = preference.into();
        (preference != Some(ViewerRoutePreference::Sfu) || self.sfu_enabled)
            && (preference != Some(ViewerRoutePreference::Peer) || !self.sfu_only)
    }

    pub(crate) fn allows_incoming(&self, child_peer_id: &str, kind: UpstreamKind) -> bool {
        let preference = self
            .participant(child_peer_id)
            .and_then(|child| child.route_preference);
        if kind == UpstreamKind::Sfu {
            return self.sfu_enabled && preference != Some(ViewerRoutePreference::Peer);
        }
        !self.sfu_only && preference != Some(ViewerRoutePreference::Sfu)
    }

    /// Filters the full plan list by the availability opportunity ledger.
    pub(crate) fn build_candidates(
        &self,
        child_peer_id: &str,
        sfu_only: bool,
    ) -> Vec<CandidatePlan> {
        let mut out: Vec<CandidatePlan> = self
            .build_candidate_plans(child_peer_id, sfu_only, true)
            .into_iter()
            .filter(|plan| self.candidate_opportunity_available(child_peer_id, plan))
            .collect();
        // Fresh Peer parents still lead. Once every eligible Peer has been
        // tried, an unconsumed SFU opportunity must precede another NAT retry.
        if out.len() > 1
            && out[0].tuple.kind == UpstreamKind::Peer
            && self.peer_attempts_started(child_peer_id, &out[0].tuple.parent_peer_id) > 0
            && let Some(index) = out
                .iter()
                .position(|plan| plan.tuple.kind == UpstreamKind::Sfu)
            && index > 0
        {
            let fallback = out.remove(index);
            out.insert(0, fallback);
        }
        out
    }

    /// Sorts eligible parents by (untried attempts when `prefer_untried`)
    /// then depth, remaining capacity descending, stable pair rank and join
    /// order; the SFU tuple is spliced into index 1 and the list is deduped
    /// by tuple key.
    pub(crate) fn build_candidate_plans(
        &self,
        child_peer_id: &str,
        sfu_only: bool,
        prefer_untried: bool,
    ) -> Vec<CandidatePlan> {
        let descendants = self.descendants_of(child_peer_id);
        let mut parents: Vec<&Participant> = self
            .participants
            .iter()
            .map(|(_, parent)| parent)
            .filter(|parent| {
                !parent.session_id.is_empty()
                    && !parent.departure_confirmed
                    && parent.peer_id != child_peer_id
                    && !descendants.contains(&parent.peer_id)
                    && self.source_usable(&parent.peer_id)
                    && self.source_video_ready(&parent.peer_id)
                    && self.voice_parent_allowed(child_peer_id, &parent.peer_id)
                    && self.target_fits(&parent.peer_id, child_peer_id)
            })
            .collect();
        parents.sort_by(|left, right| {
            let untried = if prefer_untried {
                self.peer_attempts_started(child_peer_id, &left.peer_id)
                    .cmp(&self.peer_attempts_started(child_peer_id, &right.peer_id))
            } else {
                std::cmp::Ordering::Equal
            };
            untried
                .then_with(|| self.depth(&left.peer_id).cmp(&self.depth(&right.peer_id)))
                .then_with(|| {
                    self.remaining(&right.peer_id, child_peer_id)
                        .cmp(&self.remaining(&left.peer_id, child_peer_id))
                })
                .then_with(|| {
                    stable_pair_rank(child_peer_id, &left.peer_id)
                        .cmp(&stable_pair_rank(child_peer_id, &right.peer_id))
                })
                .then_with(|| compare_participant(left, right))
        });
        let direct_candidates: Vec<CandidateTuple> = parents
            .iter()
            .map(|parent| CandidateTuple::peer(&parent.peer_id))
            .collect();
        let mut sfu_candidate = None;
        if self.allows_incoming(child_peer_id, UpstreamKind::Sfu) {
            match &self.host_publication {
                Some(publication) if publication.usable && publication.physical_active => {
                    sfu_candidate = Some(CandidateTuple::sfu(PublicationPlan::Reuse));
                }
                Some(_) => sfu_candidate = Some(CandidateTuple::sfu(PublicationPlan::Replace)),
                None if self.publication_fits(child_peer_id, false) => {
                    sfu_candidate = Some(CandidateTuple::sfu(PublicationPlan::Create));
                }
                None => {}
            }
        }
        let candidates: Vec<CandidateTuple> =
            if sfu_only || !self.allows_incoming(child_peer_id, UpstreamKind::Peer) {
                sfu_candidate.into_iter().collect()
            } else if let Some(sfu) = sfu_candidate {
                if direct_candidates.is_empty() {
                    vec![sfu]
                } else {
                    let mut list = vec![direct_candidates[0].clone(), sfu];
                    list.extend(direct_candidates[1..].iter().cloned());
                    list
                }
            } else {
                direct_candidates
            };
        let mut seen = HashSet::new();
        let mut plans = Vec::with_capacity(candidates.len());
        for tuple in candidates {
            if !seen.insert(tuple_key(&tuple)) {
                continue;
            }
            if let Some(plan) = self.plan_candidate(child_peer_id, &tuple)
                && self.candidate_valid(child_peer_id, &plan, None)
            {
                plans.push(plan);
            }
        }
        plans
    }

    /// Decides none / overlap / bounded-gap from the producer's copy budget;
    /// `None` when the producer is gone or over budget.
    pub(crate) fn plan_candidate(
        &self,
        child_peer_id: &str,
        tuple: &CandidateTuple,
    ) -> Option<CandidatePlan> {
        if tuple.kind == UpstreamKind::Sfu && tuple.publication == Some(PublicationPlan::Reuse) {
            // No producer on SFU reuse; endpoint_transition_equals compares it.
            return Some(CandidatePlan {
                tuple: tuple.clone(),
                endpoint_transition: EndpointTransition {
                    kind: TransitionKind::None,
                    producer_peer_id: None,
                    retire: None,
                },
            });
        }
        let producer_peer_id = if tuple.kind == UpstreamKind::Peer {
            tuple.parent_peer_id.clone()
        } else {
            self.host_peer_id.clone()
        };
        let producer = self.participant(&producer_peer_id)?;
        if producer.session_id.is_empty() || producer.departure_confirmed {
            return None;
        }
        let copies = self.physical_copies(&producer_peer_id);
        let plan = |kind, retire| {
            Some(CandidatePlan {
                tuple: tuple.clone(),
                endpoint_transition: EndpointTransition {
                    kind,
                    producer_peer_id: Some(producer_peer_id.clone()),
                    retire,
                },
            })
        };
        if copies < producer.effective_downstream_capacity {
            return plan(TransitionKind::None, None);
        }
        if copies < self.transition_copy_limit(producer) {
            return plan(TransitionKind::Overlap, None);
        }
        let retire = self.retirement_for(child_peer_id, &producer_peer_id, tuple)?;
        if copies > producer.effective_downstream_capacity {
            return None;
        }
        plan(TransitionKind::BoundedGap, Some(retire))
    }

    fn retirement_for(
        &self,
        child_peer_id: &str,
        producer_peer_id: &str,
        tuple: &CandidateTuple,
    ) -> Option<EndpointRetirement> {
        let old = self.edge(child_peer_id);
        if let Some(old) = old
            && old.kind == UpstreamKind::Peer
            && old.parent_peer_id == producer_peer_id
            && old.physical_active
        {
            return Some(EndpointRetirement {
                kind: RetirementKind::Edge,
                child_peer_id: child_peer_id.to_owned(),
                child_session_id: old.child_session_id.clone(),
                parent_peer_id: old.parent_peer_id.clone(),
                parent_session_id: old.parent_session_id.clone(),
                transport: Some(old.transport),
                connection_id: old.connection_id.clone(),
                host_session_id: String::new(),
                generation: String::new(),
            });
        }
        let publication = self
            .host_publication
            .as_ref()
            .filter(|publication| publication.physical_active)?;
        if producer_peer_id != self.host_peer_id {
            return None;
        }
        let replaces_publication =
            tuple.kind == UpstreamKind::Sfu && tuple.publication == Some(PublicationPlan::Replace);
        let releases_last_publication = tuple.kind == UpstreamKind::Peer
            && old.is_some_and(|old| old.kind == UpstreamKind::Sfu && old.physical_active)
            && self.sfu_subscriber_count() == 1;
        if !replaces_publication && !releases_last_publication {
            return None;
        }
        Some(EndpointRetirement {
            kind: RetirementKind::Publication,
            child_peer_id: String::new(),
            child_session_id: String::new(),
            parent_peer_id: String::new(),
            parent_session_id: String::new(),
            transport: None,
            connection_id: publication.connection_id.clone(),
            host_session_id: publication.host_session_id.clone(),
            generation: publication.generation.clone(),
        })
    }

    /// Receives `att` when validating a live attempt.
    pub(crate) fn candidate_valid(
        &self,
        child_peer_id: &str,
        plan: &CandidatePlan,
        att: Option<&Attempt>,
    ) -> bool {
        let tuple = &plan.tuple;
        if !self.allows_incoming(child_peer_id, tuple.kind) {
            return false;
        }
        let Some(child) = self.participant(child_peer_id) else {
            return false;
        };
        if child.session_id.is_empty()
            || child.departure_confirmed
            || att.is_some_and(|att| att.child_session_id != child.session_id)
        {
            return false;
        }
        if tuple.kind == UpstreamKind::Peer {
            if self.sfu_only {
                return false;
            }
            let Some(parent) = self.participant(&tuple.parent_peer_id) else {
                return false;
            };
            if parent.session_id.is_empty()
                || parent.departure_confirmed
                || att.is_some_and(|att| parent.session_id != att.parent_session_id)
                || self.descendants_of(child_peer_id).contains(&parent.peer_id)
                || !self.source_usable(&parent.peer_id)
                || !self.source_video_ready(&parent.peer_id)
                || !self.voice_parent_allowed(child_peer_id, &parent.peer_id)
                || !self.target_fits(&parent.peer_id, child_peer_id)
            {
                return false;
            }
        } else {
            if !self.sfu_enabled {
                return false;
            }
            if let Some(att) = att.filter(|att| !att.host_session_id.is_empty())
                && self
                    .participant(&self.host_peer_id)
                    .is_none_or(|host| !host.session_is(&att.host_session_id))
            {
                return false;
            }
            let publication = self.host_publication.as_ref();
            match tuple.publication {
                Some(PublicationPlan::Reuse) => {
                    let Some(publication) = publication else {
                        return false;
                    };
                    if !publication.usable
                        || !publication.physical_active
                        || att
                            .is_some_and(|att| att.publication_generation != publication.generation)
                    {
                        return false;
                    }
                }
                Some(PublicationPlan::Replace) => {
                    if publication.is_none_or(|publication| publication.usable)
                        || !self.publication_fits(child_peer_id, true)
                    {
                        return false;
                    }
                }
                _ => {
                    if publication.is_some() || !self.publication_fits(child_peer_id, false) {
                        return false;
                    }
                }
            }
        }
        self.plan_candidate(child_peer_id, tuple)
            .is_some_and(|current| {
                endpoint_transition_equals(&current.endpoint_transition, &plan.endpoint_transition)
            })
    }

    pub(crate) fn operation_candidate_valid(
        &self,
        op: &Operation,
        plan: &CandidatePlan,
        att: Option<&Attempt>,
    ) -> bool {
        self.candidate_valid(&op.child_peer_id, plan, att)
            && (att.is_some_and(|att| att.connection_attempt.is_some())
                || !crate::operation::is_availability_operation(op.reason)
                || self.candidate_opportunity_available(&op.child_peer_id, plan))
    }

    pub(crate) fn physical_copies(&self, peer_id: &str) -> i64 {
        let mut copies = self
            .upstream_by_viewer
            .iter()
            .filter(|(_, edge)| {
                edge.kind == UpstreamKind::Peer
                    && edge.parent_peer_id == peer_id
                    && edge.physical_active
            })
            .count() as i64;
        if peer_id == self.host_peer_id
            && self
                .host_publication
                .as_ref()
                .is_some_and(|publication| publication.physical_active)
        {
            copies += 1;
        }
        copies
    }

    /// Requires an existing parent.
    pub(crate) fn target_fits(&self, parent_peer_id: &str, child_peer_id: &str) -> bool {
        let parent = self
            .participant(parent_peer_id)
            .expect("target parent exists");
        let edge = self.edge(child_peer_id);
        let already = edge.is_some_and(|edge| {
            edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == parent_peer_id
                && edge.physical_active
        });
        let releases_last_publication = parent.role == Role::Host
            && edge.is_some_and(|edge| edge.kind == UpstreamKind::Sfu && edge.physical_active)
            && self.sfu_subscriber_count() == 1;
        let mut slots = self.used_slots(parent_peer_id);
        if releases_last_publication {
            slots -= 1;
        }
        if !already {
            slots += 1;
        }
        slots <= parent.effective_downstream_capacity
    }

    pub(crate) fn publication_fits(&self, child_peer_id: &str, replacing: bool) -> bool {
        let edge = self.edge(child_peer_id);
        let releases_host = edge.is_some_and(|edge| {
            edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == self.host_peer_id
                && edge.physical_active
        });
        let replaces_physical_publication = replacing
            && self
                .host_publication
                .as_ref()
                .is_some_and(|publication| publication.physical_active);
        let mut next_slots = self.used_slots(&self.host_peer_id);
        if releases_host {
            next_slots -= 1;
        }
        if !replaces_physical_publication {
            next_slots += 1;
        }
        self.participant(&self.host_peer_id).is_some_and(|host| {
            !host.session_id.is_empty()
                && !host.departure_confirmed
                && next_slots <= host.effective_downstream_capacity
        })
    }

    pub(crate) fn host_has_publication_slot(&self) -> bool {
        self.participant(&self.host_peer_id).is_some_and(|host| {
            self.used_slots(&self.host_peer_id) < host.effective_downstream_capacity
        })
    }

    pub(crate) fn used_slots(&self, peer_id: &str) -> i64 {
        self.physical_copies(peer_id)
    }

    /// May be negative; the parent must exist.
    pub(crate) fn remaining(&self, parent_peer_id: &str, child_peer_id: &str) -> i64 {
        let parent = self
            .participant(parent_peer_id)
            .expect("remaining parent exists");
        let already = self.edge(child_peer_id).is_some_and(|edge| {
            edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == parent_peer_id
                && edge.physical_active
        });
        let mut value = parent.effective_downstream_capacity - self.used_slots(parent_peer_id);
        if !already {
            value -= 1;
        }
        value
    }

    /// Yields the safe-integer bound on a cycle.
    pub(crate) fn depth(&self, peer_id: &str) -> i64 {
        if peer_id == self.host_peer_id {
            return 1;
        }
        let mut depth = 1;
        let mut seen = HashSet::new();
        let mut peer_id = peer_id.to_owned();
        loop {
            if !seen.insert(peer_id.clone()) {
                return piik_protocol::MAX_SAFE_INTEGER;
            }
            let Some(edge) = self.edge(&peer_id) else {
                return depth + 1;
            };
            if edge.kind == UpstreamKind::Sfu {
                return depth + 1;
            }
            depth += 1;
            if edge.parent_peer_id == self.host_peer_id {
                return depth;
            }
            peer_id = edge.parent_peer_id.clone();
        }
    }

    /// Direct peer children in committed-graph order, the base order for
    /// every children-derived list.
    pub(crate) fn children_of(&self, parent_peer_id: &str) -> Vec<String> {
        self.upstream_by_viewer
            .iter()
            .filter(|(_, edge)| {
                edge.kind == UpstreamKind::Peer && edge.parent_peer_id == parent_peer_id
            })
            .map(|(child, _)| child.clone())
            .collect()
    }

    /// Walks depth-first, returning visit order without duplicates.
    pub(crate) fn descendants_of(&self, peer_id: &str) -> Vec<String> {
        let mut descendants = Vec::new();
        let mut seen = HashSet::new();
        let mut stack = vec![peer_id.to_owned()];
        while let Some(last) = stack.pop() {
            for child in self.children_of(&last) {
                if !seen.insert(child.clone()) {
                    continue;
                }
                descendants.push(child.clone());
                stack.push(child);
            }
        }
        descendants
    }

    /// Checks that the whole upstream path is usable and active.
    pub(crate) fn source_usable(&self, peer_id: &str) -> bool {
        if peer_id == self.host_peer_id {
            return true;
        }
        let mut seen = HashSet::new();
        let mut peer_id = peer_id.to_owned();
        loop {
            if !seen.insert(peer_id.clone()) {
                return false;
            }
            let Some(edge) = self.edge(&peer_id) else {
                return false;
            };
            if !edge.usable || !edge.physical_active {
                return false;
            }
            if edge.kind == UpstreamKind::Sfu {
                return self.publication_serves(edge);
            }
            if edge.parent_peer_id == self.host_peer_id {
                return true;
            }
            peer_id = edge.parent_peer_id.clone();
        }
    }

    /// The live, usable publication matches the SFU edge's generation.
    pub(crate) fn publication_serves(&self, edge: &CommittedEdge) -> bool {
        self.host_publication.as_ref().is_some_and(|publication| {
            publication.usable
                && publication.physical_active
                && publication.generation == edge.publication_generation
        })
    }

    pub(crate) fn participant_blocked(&self, current: &Participant) -> bool {
        current.availability_exhausted && self.build_candidates(&current.peer_id, false).is_empty()
    }

    /// Connected, non-departed viewers sorted by join order (a total order,
    /// so the map order does not leak). Returns peer IDs.
    pub(crate) fn available_viewers(&self, include_blocked: bool) -> Vec<String> {
        let mut viewers: Vec<&Participant> = self
            .participants
            .iter()
            .map(|(_, current)| current)
            .filter(|current| {
                current.role == Role::Viewer
                    && !current.session_id.is_empty()
                    && !current.departure_confirmed
                    && (include_blocked || !self.participant_blocked(current))
            })
            .collect();
        viewers.sort_by(|left, right| compare_participant(left, right));
        viewers
            .into_iter()
            .map(|viewer| viewer.peer_id.clone())
            .collect()
    }

    /// Returns movable excess work in parent/newest order.
    ///
    /// Only when no overloaded parent has a movable child does it return the
    /// original newest-overflow failure work, before attempt ordering is applied.
    pub(crate) fn overflow_children(&self) -> Vec<String> {
        let mut result = Vec::new();
        let mut fallback = Vec::new();
        for (parent_peer_id, _) in self.participants.iter() {
            let overflowing = self.overflow_peer_children(parent_peer_id);
            if overflowing.is_empty() {
                continue;
            }
            let mut movable: Vec<String> = self
                .children_of(parent_peer_id)
                .into_iter()
                .filter(|id| {
                    let edge_active = self.edge(id).is_some_and(|edge| edge.physical_active);
                    let child_eligible = self.participant(id).is_some_and(|child| {
                        edge_active && !child.session_id.is_empty() && !child.departure_confirmed
                    });
                    // A child with no remaining candidate cannot move.
                    child_eligible && !self.build_candidates(id, false).is_empty()
                })
                .collect();
            if !movable.is_empty() {
                movable.sort_by(|left, right| {
                    let (Some(left_participant), Some(right_participant)) =
                        (self.participant(left), self.participant(right))
                    else {
                        return Ordering::Equal;
                    };
                    compare_participant(right_participant, left_participant)
                });
                result.extend(movable);
                continue;
            }
            let mut overflow: Vec<String> = overflowing
                .into_iter()
                .filter(|id| {
                    self.participant(id).is_some_and(|child| {
                        !child.session_id.is_empty()
                            && !child.departure_confirmed
                            && !self.participant_blocked(child)
                    })
                })
                .collect();
            overflow.reverse();
            fallback.extend(overflow);
        }
        if !result.is_empty() {
            return result;
        }
        fallback
    }

    /// Keeps the oldest physically active direct children within the
    /// parent's capacity and returns the overflow.
    pub(crate) fn overflow_peer_children(&self, parent_peer_id: &str) -> Vec<String> {
        let Some(parent) = self.participant(parent_peer_id) else {
            return Vec::new();
        };
        let publication_copies = i64::from(
            parent.role == Role::Host
                && self
                    .host_publication
                    .as_ref()
                    .is_some_and(|publication| publication.physical_active),
        );
        let capacity = (parent.effective_downstream_capacity - publication_copies).max(0) as usize;
        let mut children: Vec<String> = self
            .children_of(parent_peer_id)
            .into_iter()
            .filter(|id| {
                self.edge(id)
                    .is_some_and(|edge| edge.kind == UpstreamKind::Peer && edge.physical_active)
            })
            .collect();
        children.sort_by(|left, right| {
            compare_participant(
                self.participant(left).unwrap(),
                self.participant(right).unwrap(),
            )
        });
        if capacity >= children.len() {
            return Vec::new();
        }
        children.split_off(capacity)
    }
}

pub(crate) fn tuple_key(tuple: &CandidateTuple) -> String {
    if tuple.kind == UpstreamKind::Peer {
        let mut key = format!("peer:{}", tuple.parent_peer_id);
        if tuple.regenerate {
            key.push_str(":regenerate");
        }
        return key;
    }
    format!("sfu:{}", tuple.publication.map_or("", |plan| plan.as_str()))
}

pub(crate) fn edge_tuple_key(edge: &CommittedEdge) -> String {
    if edge.kind == UpstreamKind::Peer {
        return format!("peer:{}", edge.parent_peer_id);
    }
    "sfu:reuse".to_owned()
}

pub(crate) fn endpoint_transition_rank(transition: &EndpointTransition) -> i64 {
    match transition.kind {
        TransitionKind::None => 0,
        TransitionKind::Overlap => 1,
        TransitionKind::BoundedGap => 2,
    }
}

pub(crate) fn candidate_plan_equals(left: &CandidatePlan, right: &CandidatePlan) -> bool {
    tuple_key(&left.tuple) == tuple_key(&right.tuple)
        && endpoint_transition_equals(&left.endpoint_transition, &right.endpoint_transition)
}

/// Distinguishes an absent producer from every peer, including the Host,
/// even when no retirement is needed.
pub(crate) fn endpoint_transition_equals(
    left: &EndpointTransition,
    right: &EndpointTransition,
) -> bool {
    if left.kind != right.kind {
        return false;
    }
    if left.kind != TransitionKind::BoundedGap {
        return left.producer_peer_id == right.producer_peer_id;
    }
    if left.producer_peer_id != right.producer_peer_id {
        return false;
    }
    let (Some(left), Some(right)) = (&left.retire, &right.retire) else {
        return false;
    };
    if left.kind != right.kind {
        return false;
    }
    match left.kind {
        RetirementKind::Publication => {
            left.host_session_id == right.host_session_id
                && left.generation == right.generation
                && left.connection_id == right.connection_id
        }
        RetirementKind::Edge => {
            left.child_peer_id == right.child_peer_id
                && left.child_session_id == right.child_session_id
                && left.parent_peer_id == right.parent_peer_id
                && left.parent_session_id == right.parent_session_id
                && left.transport == right.transport
                && left.connection_id == right.connection_id
        }
    }
}

/// Panics on a reservation kind that does not match the tuple or a missing
/// overlap for an overlap transition.
pub(crate) fn assert_reservation(
    tuple: &CandidateTuple,
    reservation: &CandidateReservation,
    overlap: bool,
) {
    let expected = if tuple.kind == UpstreamKind::Peer {
        ReservationKind::Direct
    } else if tuple.publication == Some(PublicationPlan::Reuse) {
        ReservationKind::SfuReuse
    } else {
        ReservationKind::SfuCreate
    };
    if reservation.kind != Some(expected) {
        panic!("Candidate reservation kind does not match tuple");
    }
    if overlap && reservation.overlap.is_none() {
        panic!("Candidate requires an endpoint overlap reservation");
    }
}

/// Edge (never for a borrowed SFU reuse edge, which belongs to another
/// holder), publication, overlap.
pub(crate) fn reservation_resources(reservation: &CandidateReservation) -> Vec<ResourceRef> {
    let mut resources = Vec::new();
    let kind = reservation.kind;
    if kind == Some(ReservationKind::SfuCreate)
        || (kind == Some(ReservationKind::SfuReuse) && !reservation.borrowed)
    {
        resources.extend(reservation.edge.clone());
    }
    if kind == Some(ReservationKind::SfuCreate) {
        resources.extend(reservation.publication.clone());
    }
    resources.extend(reservation.overlap.clone());
    resources
}
