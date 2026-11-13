//! The per-session candidate opportunity ledgers.

use piik_ordered::Map;
use piik_protocol::Role;

use crate::candidates::{endpoint_transition_rank, tuple_key};
use crate::controller::{
    CandidateOpportunity, MAX_NAT_CONNECTION_ATTEMPTS, Operation, SFU_OPPORTUNITY_PREFIX,
};
use crate::operation::is_availability_operation;
use crate::{
    CandidatePlan, CandidateTuple, CommittedEdge, ConnectionAttemptProgress, Controller,
    DemandReason, UpstreamKind,
};

impl Controller {
    /// The NUL-joined ledger key. A session change erases the key because
    /// the session is part of it.
    pub(crate) fn candidate_opportunity_base(
        &self,
        child_peer_id: &str,
        plan: &CandidatePlan,
    ) -> String {
        let session = |peer_id: &str| {
            self.participant(peer_id)
                .map(|current| current.session_id.clone())
                .unwrap_or_default()
        };
        let child_session_id = session(child_peer_id);
        if plan.tuple.kind == UpstreamKind::Peer {
            let parent_session_id = session(&plan.tuple.parent_peer_id);
            return format!(
                "{}\0{child_session_id}\0{parent_session_id}",
                tuple_key(&plan.tuple)
            );
        }
        format!(
            "{SFU_OPPORTUNITY_PREFIX}{child_session_id}\0{}",
            self.host_session_id()
        )
    }

    pub(crate) fn candidate_opportunity_available(
        &self,
        child_peer_id: &str,
        plan: &CandidatePlan,
    ) -> bool {
        let Some(child) = self.participant(child_peer_id) else {
            return true;
        };
        let Some(consumed) = child
            .consumed_candidate_opportunities
            .get(&self.candidate_opportunity_base(child_peer_id, plan))
        else {
            return true;
        };
        endpoint_transition_rank(&plan.endpoint_transition) < consumed.rank
            || (self.nat_prediction_enabled
                && plan.tuple.kind == UpstreamKind::Peer
                && !consumed.exhausted
                && consumed.started_attempts < MAX_NAT_CONNECTION_ATTEMPTS)
    }

    /// Counts one NAT attempt for a peer candidate of an availability or
    /// direct-convergence operation.
    pub(crate) fn start_candidate_opportunity(
        &mut self,
        plan: &CandidatePlan,
    ) -> Option<ConnectionAttemptProgress> {
        let op = self.op();
        if !self.nat_prediction_enabled
            || plan.tuple.kind != UpstreamKind::Peer
            || (!is_availability_operation(op.reason)
                && op.reason != DemandReason::DirectConvergence)
        {
            return None;
        }
        let child_peer_id = op.child_peer_id.clone();
        let key = self.candidate_opportunity_base(&child_peer_id, plan);
        let rank = endpoint_transition_rank(&plan.endpoint_transition);
        let opportunities = &mut self
            .participant_mut(&child_peer_id)
            .unwrap()
            .consumed_candidate_opportunities;
        let previous = opportunities.get(&key).copied();
        let started_attempts = match previous {
            Some(previous) if previous.rank <= rank => previous.started_attempts + 1,
            _ => 1,
        };
        let new_rank = previous.map_or(rank, |previous| previous.rank.min(rank));
        opportunities.set(
            key,
            CandidateOpportunity {
                rank: new_rank,
                started_attempts,
                exhausted: false,
            },
        );
        self.op_mut().peer_attempt_started = true;
        Some(ConnectionAttemptProgress {
            current: started_attempts,
            total: MAX_NAT_CONNECTION_ATTEMPTS,
        })
    }

    pub(crate) fn peer_attempts_started(&self, child_peer_id: &str, parent_peer_id: &str) -> i64 {
        if !self.nat_prediction_enabled {
            return 0;
        }
        let Some(plan) = self.plan_candidate(child_peer_id, &CandidateTuple::peer(parent_peer_id))
        else {
            return 0;
        };
        let Some(child) = self.participant(child_peer_id) else {
            return 0;
        };
        match child
            .consumed_candidate_opportunities
            .get(&self.candidate_opportunity_base(child_peer_id, &plan))
        {
            Some(opportunity)
                if opportunity.rank <= endpoint_transition_rank(&plan.endpoint_transition) =>
            {
                opportunity.started_attempts
            }
            _ => 0,
        }
    }

    /// Sums the attempts over every peer key.
    pub(crate) fn viewer_attempts_started(&self, peer_id: &str) -> i64 {
        if !self.nat_prediction_enabled {
            return 0;
        }
        let Some(current) = self.participant(peer_id) else {
            return 0;
        };
        current
            .consumed_candidate_opportunities
            .iter()
            .filter(|(key, _)| key.starts_with("peer:"))
            .map(|(_, opportunity)| opportunity.started_attempts)
            .sum()
    }

    pub(crate) fn has_remaining_availability_opportunity(&self, op: &Operation) -> bool {
        is_availability_operation(op.reason)
            && !self.build_candidates(&op.child_peer_id, false).is_empty()
    }

    /// Exhausts the key at its best rank.
    pub(crate) fn consume_candidate_opportunity(
        &mut self,
        child_peer_id: &str,
        plan: &CandidatePlan,
    ) {
        if self.participant(child_peer_id).is_none() {
            return;
        }
        let key = self.candidate_opportunity_base(child_peer_id, plan);
        let rank = endpoint_transition_rank(&plan.endpoint_transition);
        let opportunities = &mut self
            .participant_mut(child_peer_id)
            .unwrap()
            .consumed_candidate_opportunities;
        let mut entry = CandidateOpportunity {
            rank,
            started_attempts: 0,
            exhausted: true,
        };
        if let Some(previous) = opportunities.get(&key) {
            entry.rank = previous.rank.min(rank);
            entry.started_attempts = previous.started_attempts;
        }
        opportunities.set(key, entry);
    }

    pub(crate) fn quality_candidate_opportunity_available(
        &self,
        child_peer_id: &str,
        plan: &CandidatePlan,
    ) -> bool {
        let Some(observation) = self.sender_quality_observations.get(child_peer_id) else {
            return true;
        };
        observation
            .consumed_quality_candidates
            .get(&self.candidate_opportunity_base(child_peer_id, plan))
            .is_none_or(|consumed| endpoint_transition_rank(&plan.endpoint_transition) < *consumed)
    }

    pub(crate) fn consume_quality_candidate_opportunity(&mut self, plan: Option<&CandidatePlan>) {
        let op = self.op();
        let Some(plan) = plan.filter(|_| op.reason == DemandReason::QualityConvergence) else {
            return;
        };
        if self.current_quality_operation_observation(op).is_none() {
            return;
        }
        let child_peer_id = op.child_peer_id.clone();
        let base = self.candidate_opportunity_base(&child_peer_id, plan);
        let rank = endpoint_transition_rank(&plan.endpoint_transition);
        let observation = self
            .sender_quality_observations
            .get_mut(&child_peer_id)
            .unwrap();
        consume_opportunity(&mut observation.consumed_quality_candidates, base, rank);
    }

    pub(crate) fn consume_quality_operation_candidates(&mut self) {
        for candidate in self.op().candidates.clone() {
            self.consume_quality_candidate_opportunity(Some(&candidate));
        }
    }

    /// Exhausts the invalidated edge's exact tuple at rank 0.
    pub(crate) fn consume_active_edge_opportunity(
        &mut self,
        child_peer_id: &str,
        edge: &CommittedEdge,
    ) {
        let key = self.candidate_opportunity_base(
            child_peer_id,
            &CandidatePlan::bare(CandidateTuple::bare(edge.kind, &edge.parent_peer_id)),
        );
        // A retained connection can fail after reauthentication. Its old
        // acquisition must not exhaust the replacement session's untried
        // opportunity.
        if edge.opportunity_key != key {
            return;
        }
        let opportunities = &mut self
            .participant_mut(child_peer_id)
            .unwrap()
            .consumed_candidate_opportunities;
        let started_attempts = opportunities
            .get(&key)
            .map_or(0, |previous| previous.started_attempts);
        opportunities.set(
            key,
            CandidateOpportunity {
                rank: 0,
                started_attempts,
                exhausted: true,
            },
        );
    }

    pub(crate) fn consume_availability_operation(&mut self) {
        let op = self.op();
        if !is_availability_operation(op.reason) {
            return;
        }
        // A deadline can expire on a connected Peer before the suffix is
        // reached. Consume only the visited prefix; an untried fallback
        // remains eligible after rollback, under a new operation with its own
        // unchanged deadline.
        let end = (op.cursor + 1).min(op.candidates.len());
        let peer_attempt_started = op.peer_attempt_started;
        let child_peer_id = op.child_peer_id.clone();
        let visited: Vec<_> = op.candidates[..end].to_vec();
        for plan in visited {
            if peer_attempt_started && plan.tuple.kind == UpstreamKind::Peer {
                continue;
            }
            self.consume_candidate_opportunity(&child_peer_id, &plan);
        }
    }

    pub(crate) fn consume_current_availability_opportunity(&mut self) {
        let op = self.op();
        if !is_availability_operation(op.reason) {
            return;
        }
        let plan = op.plan_at(op.cursor);
        let nat_attempt = op
            .current
            .as_ref()
            .is_some_and(|current| current.connection_attempt.is_some());
        if let Some(plan) = plan {
            if nat_attempt && plan.tuple.kind == UpstreamKind::Peer {
                return;
            }
            let child_peer_id = op.child_peer_id.clone();
            self.consume_candidate_opportunity(&child_peer_id, &plan);
        }
    }

    /// Snapshots keys before deletion, preserving safe deletion during
    /// iteration.
    pub(crate) fn clear_opportunities_for_session_change(&mut self, peer_id: &str, role: Role) {
        if let Some(current) = self.participant_mut(peer_id) {
            current.consumed_candidate_opportunities.clear();
            current.availability_exhausted = false;
            current.bootstrap_failure_reported = false;
        }
        let parent_prefix = format!("peer:{peer_id}\0");
        for child_peer_id in self.participants.keys() {
            let child = self.participants.get_mut(&child_peer_id).unwrap();
            for key in child.consumed_candidate_opportunities.keys() {
                if key.starts_with(&parent_prefix)
                    || (role == Role::Host && key.starts_with(SFU_OPPORTUNITY_PREFIX))
                {
                    child.consumed_candidate_opportunities.delete(&key);
                }
            }
            if role == Role::Host {
                child.bootstrap_failure_reported = false;
            }
        }
        if role == Role::Host {
            self.consumed_sfu_bootstrap_opportunities.clear();
            return;
        }
        let carrier_prefix = format!("{peer_id}\0");
        for key in self.consumed_sfu_bootstrap_opportunities.keys() {
            if key.starts_with(&carrier_prefix) {
                self.consumed_sfu_bootstrap_opportunities.delete(&key);
            }
        }
    }

    pub(crate) fn clear_sfu_candidate_opportunities(&mut self) {
        for peer_id in self.participants.keys() {
            let current = self.participants.get_mut(&peer_id).unwrap();
            for key in current.consumed_candidate_opportunities.keys() {
                if key.starts_with(SFU_OPPORTUNITY_PREFIX) {
                    current.consumed_candidate_opportunities.delete(&key);
                }
            }
            current.bootstrap_failure_reported = false;
        }
        self.consumed_sfu_bootstrap_opportunities.clear();
    }
}

/// Keeps the best (lowest) rank.
pub(crate) fn consume_opportunity(consumed: &mut Map<String, i64>, base: String, rank: i64) {
    if consumed.get(&base).is_none_or(|previous| rank < *previous) {
        consumed.set(base, rank);
    }
}
