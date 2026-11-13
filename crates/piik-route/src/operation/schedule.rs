//! Candidate scheduling: head-start deadlines, wake times and the
//! next-child and direct-continuation selection order.

use super::*;

impl Controller {
    pub(super) fn advance_expired_direct_head_start(
        &mut self,
        now_ms: i64,
        released: &mut Vec<ResourceRef>,
    ) -> bool {
        let op = self.op();
        let current_tuple = op.current_tuple();
        let sfu_index = self.foreground_sfu_index(op);
        if current_tuple.is_none_or(|tuple| tuple.kind != UpstreamKind::Peer)
            || op
                .current
                .as_ref()
                .is_some_and(|current| current.transport_connected)
            || sfu_index <= op.cursor as i64
        {
            return false;
        }
        let head_start_deadline_at_ms = self.direct_head_start_deadline_at(op);
        if now_ms < head_start_deadline_at_ms {
            return false;
        }
        let sfu_index = sfu_index as usize;
        let mut active_candidate = String::new();
        if let Some(current) = &op.current {
            active_candidate = self.debug_tuple(&current.tuple);
            let demand_peer_id = op.demand_peer_id.clone();
            let reservation = current.reservation.clone();
            self.note_rejection(&demand_peer_id, RejectionBucket::FirstFrameTimeout);
            self.consume_current_availability_opportunity();
            released.extend(reservation_resources(&reservation));
            self.advance_active_revision();
            self.op_mut().current = None;
            self.clear_candidate_timing(&demand_peer_id);
        }
        // Deferred parents keep candidate order.
        let op = self.op_mut();
        let end = sfu_index.min(op.candidates.len());
        let deferred: Vec<String> = op.candidates[op.cursor..end]
            .iter()
            .filter(|candidate| candidate.tuple.kind == UpstreamKind::Peer)
            .map(|candidate| candidate.tuple.parent_peer_id.clone())
            .collect();
        op.deferred_parent_peer_ids.extend(deferred);
        op.cursor = sfu_index;
        route_debug!(self, "direct-head-start-expired",
            "child" => self.debug_peer(&self.op().child_peer_id),
            "activeCandidate" => active_candidate,
            "cursor" => self.op().cursor,
            "deadlineAtMs" => head_start_deadline_at_ms,
            "observedAtMs" => now_ms);
        true
    }

    pub(super) fn operation_wake_at(&self, op: &Operation) -> i64 {
        if op.reason == DemandReason::PublicationAttach {
            return op.deadline_at_ms;
        }
        let connected = op
            .current
            .as_ref()
            .is_some_and(|current| current.transport_connected);
        if op
            .current_tuple()
            .is_some_and(|tuple| tuple.kind == UpstreamKind::Peer)
            && !connected
            && self.foreground_sfu_index(op) > op.cursor as i64
        {
            return self.direct_head_start_deadline_at(op);
        }
        if let Some(current) = &op.current
            && current.tuple.kind == UpstreamKind::Peer
            && !current.transport_connected
            && self.peer_candidate_has_successor(op)
        {
            return op
                .deadline_at_ms
                .min(current.started_at_ms + self.direct_head_start_ms());
        }
        op.deadline_at_ms
    }

    pub(super) fn peer_candidate_has_successor(&self, op: &Operation) -> bool {
        op.cursor + 1 < op.candidates.len() || self.foreground_sfu_index(op) > op.cursor as i64
    }

    pub(super) fn direct_head_start_deadline_at(&self, op: &Operation) -> i64 {
        op.deadline_at_ms - self.operation_timeout_ms + self.direct_head_start_ms()
    }

    /// The index of the first SFU candidate at or after the cursor, the
    /// candidate count when a bootstrap carrier exists, -1 otherwise (always
    /// -1 for background convergence).
    pub(super) fn foreground_sfu_index(&self, op: &Operation) -> i64 {
        if is_background_convergence(op.reason) {
            return -1;
        }
        if let Some(index) = (op.cursor..op.candidates.len())
            .find(|index| op.candidates[*index].tuple.kind == UpstreamKind::Sfu)
        {
            return index as i64;
        }
        if self.bootstrap_candidate_available(&op.demand_peer_id) {
            return op.candidates.len() as i64;
        }
        -1
    }

    pub(super) fn promote_next_direct_within_head_start(&mut self, now_ms: i64) {
        let op = self.op();
        let sfu_index = self.foreground_sfu_index(op);
        if sfu_index != op.cursor as i64 + 1 || now_ms >= self.direct_head_start_deadline_at(op) {
            return;
        }
        let sfu_index = sfu_index as usize;
        let Some(next_direct_index) = (sfu_index + 1..op.candidates.len())
            .find(|index| op.candidates[*index].tuple.kind == UpstreamKind::Peer)
        else {
            return;
        };
        let op = self.op_mut();
        let next_direct = op.candidates.remove(next_direct_index);
        op.candidates.insert(sfu_index, next_direct);
    }

    pub(crate) fn guard_matches(
        &self,
        guard: &CandidateGuard,
        op: &Operation,
        att: &Attempt,
    ) -> bool {
        guard.child_peer_id == op.child_peer_id
            && guard.child_session_id == op.child_session_id
            && guard.revision == att.revision
            && guard.connection_id == att.connection_id
    }

    /// Five priority buckets, first-occurrence deduplication, then a stable
    /// sort by started attempts.
    pub(crate) fn select_next_child(&self) -> String {
        let mut departed_parents = Vec::new();
        let mut unusable = Vec::new();
        let mut stale_sfu = Vec::new();
        let mut without_edge = Vec::new();
        for viewer in self.available_viewers(false) {
            let Some(edge) = self.edge(&viewer) else {
                without_edge.push(viewer);
                continue;
            };
            if edge.kind == UpstreamKind::Peer
                && self
                    .participant(&edge.parent_peer_id)
                    .is_some_and(|parent| parent.departure_confirmed)
            {
                departed_parents.push(viewer.clone());
            }
            if !edge.usable
                || !self.allows_incoming(&viewer, edge.kind)
                || self.parent_route_retired(edge)
                || self.voice_route_missing(&viewer)
            {
                unusable.push(viewer.clone());
            }
            if edge.kind == UpstreamKind::Sfu
                && self.host_publication.as_ref().is_none_or(|publication| {
                    !publication.usable || publication.generation != edge.publication_generation
                })
            {
                stale_sfu.push(viewer.clone());
            }
        }
        let mut candidates = dedupe_strings(
            departed_parents
                .into_iter()
                .chain(self.overflow_children())
                .chain(unusable)
                .chain(stale_sfu)
                .chain(without_edge),
        );
        candidates.sort_by_key(|candidate| self.viewer_attempts_started(candidate));
        candidates.into_iter().next().unwrap_or_default()
    }

    /// Restores newly available session opportunities behind a healthy SFU
    /// edge. Existing parent intents survive temporary capacity constraints;
    /// the opportunity ledger still owns eligibility and the attempt budget.
    pub(super) fn restore_direct_continuation(&mut self, peer_id: &str) {
        let Some(child) = self.participant(peer_id) else {
            return;
        };
        let Some(edge) = self.edge(peer_id) else {
            return;
        };
        if child.session_id.is_empty()
            || !self.allows_incoming(peer_id, UpstreamKind::Peer)
            || edge.kind != UpstreamKind::Sfu
            || !edge.usable
            || !edge.physical_active
            || !self.usable_route(peer_id)
        {
            return;
        }
        let mut parents = Vec::new();
        if let Some(previous) = self.direct_continuations.get(&peer_id.to_owned()) {
            parents.extend(previous.parent_peer_ids.iter().cloned());
        }
        for plan in self.build_candidates(peer_id, false) {
            if plan.tuple.kind == UpstreamKind::Peer
                && plan.endpoint_transition.kind != TransitionKind::BoundedGap
            {
                parents.push(plan.tuple.parent_peer_id);
            }
        }
        if !parents.is_empty() {
            let continuation = DirectContinuation {
                child_session_id: child.session_id.clone(),
                sfu_connection_id: edge.connection_id.clone(),
                publication_generation: edge.publication_generation.clone(),
                parent_peer_ids: dedupe_strings(parents),
            };
            self.direct_continuations
                .set(peer_id.to_owned(), continuation);
        }
    }

    /// A stable sort over the round-robin map order, then the first valid
    /// parent per child.
    pub(super) fn select_direct_continuation(&mut self) -> Option<DirectContinuationPick> {
        let mut entries: Vec<(String, DirectContinuation)> = self
            .direct_continuations
            .iter()
            .map(|(child, continuation)| (child.clone(), continuation.clone()))
            .collect();
        entries.sort_by_key(|(child, _)| self.viewer_attempts_started(child));
        for (child_peer_id, continuation) in entries {
            let child = self.participant(&child_peer_id);
            let edge = self.edge(&child_peer_id);
            let valid = child.is_some_and(|child| {
                !child.session_id.is_empty()
                    && !child.departure_confirmed
                    && child.session_is(&continuation.child_session_id)
            }) && edge.is_some_and(|edge| {
                edge.kind == UpstreamKind::Sfu
                    && edge.usable
                    && edge.physical_active
                    && edge.connection_id == continuation.sfu_connection_id
                    && edge.publication_generation == continuation.publication_generation
            }) && self.host_publication.as_ref().is_some_and(|publication| {
                publication.usable
                    && publication.physical_active
                    && publication.generation == continuation.publication_generation
            });
            if !valid {
                self.direct_continuations.delete(&child_peer_id);
                continue;
            }
            let mut parents = continuation.parent_peer_ids.clone();
            parents.sort_by_key(|parent| self.peer_attempts_started(&child_peer_id, parent));
            for parent_peer_id in parents {
                let Some(plan) =
                    self.plan_candidate(&child_peer_id, &CandidateTuple::peer(&parent_peer_id))
                else {
                    continue;
                };
                if plan.endpoint_transition.kind != TransitionKind::BoundedGap
                    && self.candidate_valid(&child_peer_id, &plan, None)
                    && (!self.nat_prediction_enabled
                        || self.candidate_opportunity_available(&child_peer_id, &plan))
                {
                    return Some(DirectContinuationPick {
                        child_peer_id,
                        plan,
                    });
                }
            }
            if continuation.parent_peer_ids.is_empty() {
                self.direct_continuations.delete(&child_peer_id);
                route_debug!(self, "direct-convergence-complete",
                    "child" => self.debug_peer(&child_peer_id),
                    "route" => "sfu");
            }
        }
        None
    }

    /// Removes the tried parent from the retry order (re-appending it while
    /// NAT attempts remain) and moves the continuation to the tail of the
    /// round-robin.
    pub(super) fn consume_direct_continuation_candidate(&mut self) {
        let op = self.op();
        let Some(tuple) = op
            .current_tuple()
            .filter(|tuple| tuple.kind == UpstreamKind::Peer)
        else {
            return;
        };
        if op.reason != DemandReason::DirectConvergence {
            return;
        }
        let child_peer_id = op.child_peer_id.clone();
        // Unstarted candidates invalidated by changing facts retain their opportunity.
        let plan = op.plan_at(op.cursor);
        let nat_attempt = op
            .current
            .as_ref()
            .map(|current| current.connection_attempt.is_some());
        if let (Some(false), Some(plan)) = (nat_attempt, &plan) {
            self.consume_candidate_opportunity(&child_peer_id, plan);
        }
        let Some(mut continuation) = self.direct_continuations.get(&child_peer_id).cloned() else {
            return;
        };
        let Some(index) = continuation
            .parent_peer_ids
            .iter()
            .position(|parent| *parent == tuple.parent_peer_id)
        else {
            return;
        };
        continuation.parent_peer_ids.remove(index);
        if nat_attempt == Some(true)
            && plan
                .as_ref()
                .is_some_and(|plan| self.candidate_opportunity_available(&child_peer_id, plan))
        {
            continuation
                .parent_peer_ids
                .push(tuple.parent_peer_id.clone());
        }
        route_debug!(self, "direct-convergence-advanced",
            "child" => self.debug_peer(&child_peer_id),
            "parent" => self.debug_peer(&tuple.parent_peer_id),
            "remaining" => continuation.parent_peer_ids.len());
        if continuation.parent_peer_ids.is_empty() {
            self.direct_continuations.delete(&child_peer_id);
        } else {
            // Give the next continuation a turn before retrying this child.
            self.direct_continuations
                .move_to_back(child_peer_id, continuation);
        }
    }
}
