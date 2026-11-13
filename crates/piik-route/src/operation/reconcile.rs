//! The reconcile loop: validation, expiry, replanning and clearing of the
//! single in-flight operation.

use super::*;

impl Controller {
    /// Validates or advances the current operation, prunes departed leaves
    /// and starts at most one new operation in strict priority (SFU bootstrap
    /// carrier, next child, direct continuation, quality child, root
    /// convergence).
    pub fn reconcile(&mut self, now_ms: i64) -> ReconcileResult {
        let mut released = self.retire_departed_edges(now_ms);
        let validation = self.validate_or_advance(now_ms, None);
        let mut failed_peer_ids = failed_peer_ids_from(&[&validation]);
        released.extend(validation.released);
        if let Some((reason, child_peer_id)) = self
            .operation
            .as_ref()
            .map(|op| (op.reason, op.child_peer_id.clone()))
            && is_background_convergence(reason)
            && (self.has_sfu_bootstrap_work() || !self.select_next_child().is_empty())
        {
            route_debug!(self, format!("{}-preempted", reason.as_str()),
                    "child" => self.debug_peer(&child_peer_id));
            released.extend(self.abort_operation(Some(now_ms), RejectionBucket::Aborted));
        }
        let mut removed_peer_ids = Vec::new();
        if self.operation.is_none() {
            removed_peer_ids = self.prune_departed_leaves(&mut released);
        }
        let result =
            |controller: &Self, removed_peer_ids, failed_peer_ids, released| ReconcileResult {
                operation: controller.operation_snapshot(),
                removed_peer_ids,
                failed_peer_ids,
                released,
            };
        if self.paused || self.operation.is_some() {
            return result(self, removed_peer_ids, failed_peer_ids, released);
        }
        // Attach existing upstreams before asking them to supply a new video child.
        if self.start_publication_attach(now_ms) {
            return result(self, removed_peer_ids, failed_peer_ids, released);
        }

        // Refresh current parent/session opportunities; the ledger preserves
        // spent attempts.
        for peer_id in self.upstream_by_viewer.keys() {
            self.restore_direct_continuation(&peer_id);
        }

        // The loop bound is a count, not an order.
        for _ in 0..self.participants.len() {
            let bootstrap = self.take_sfu_bootstrap_carrier(now_ms, &mut failed_peer_ids);
            let route_child_peer_id = if bootstrap.is_none() {
                self.select_next_child()
            } else {
                String::new()
            };
            let continuation = if route_child_peer_id.is_empty() && bootstrap.is_none() {
                self.select_direct_continuation()
            } else {
                None
            };
            let quality_child_peer_id = if route_child_peer_id.is_empty()
                && bootstrap.is_none()
                && continuation.is_none()
                && self.video_work_enabled()
            {
                self.select_quality_child(now_ms)
            } else {
                String::new()
            };
            let root_convergence = if route_child_peer_id.is_empty()
                && bootstrap.is_none()
                && continuation.is_none()
                && quality_child_peer_id.is_empty()
                && self.video_work_enabled()
            {
                self.take_root_convergence_intent()
            } else {
                None
            };
            let child_peer_id = if !route_child_peer_id.is_empty() {
                route_child_peer_id
            } else if let Some(bootstrap) = &bootstrap {
                bootstrap.carrier_peer_id.clone()
            } else if let Some(continuation) = &continuation {
                continuation.child_peer_id.clone()
            } else if !quality_child_peer_id.is_empty() {
                quality_child_peer_id.clone()
            } else if let Some(root) = &root_convergence {
                root.child_peer_id.clone()
            } else {
                String::new()
            };
            if child_peer_id.is_empty() {
                return ReconcileResult {
                    operation: None,
                    removed_peer_ids,
                    failed_peer_ids,
                    released,
                };
            }
            let reason = if continuation.is_some() {
                DemandReason::DirectConvergence
            } else if bootstrap.is_some() {
                DemandReason::SfuBootstrap
            } else if !quality_child_peer_id.is_empty() {
                DemandReason::QualityConvergence
            } else if root_convergence.is_some() {
                DemandReason::RootConvergence
            } else {
                self.route_demand_reason(&child_peer_id)
            };
            let demand_peer_id = bootstrap.as_ref().map_or_else(
                || child_peer_id.clone(),
                |bootstrap| bootstrap.demand_peer_id.clone(),
            );
            self.ensure_demand(&demand_peer_id, now_ms, reason);
            let mut candidates = if let Some(continuation) = &continuation {
                vec![continuation.plan.clone()]
            } else if !quality_child_peer_id.is_empty() {
                self.build_quality_candidates(&child_peer_id, now_ms)
            } else if let Some(root) = &root_convergence {
                vec![root.plan.clone()]
            } else {
                self.build_candidates(&child_peer_id, bootstrap.is_some())
            };
            let sfu_first = self
                .participant(&demand_peer_id)
                .unwrap()
                .sfu_first_at_next_route;
            if bootstrap.is_none() && continuation.is_none() && sfu_first {
                // The SFU candidate moves to the front.
                if let Some(index) = candidates
                    .iter()
                    .position(|candidate| candidate.tuple.kind == UpstreamKind::Sfu)
                    && index > 0
                {
                    let sfu = candidates.remove(index);
                    candidates.insert(0, sfu);
                }
            }
            if reason == DemandReason::SfuBootstrap {
                // Bootstrap sets drop bounded-gap candidates.
                candidates.retain(|candidate| {
                    candidate.endpoint_transition.kind != TransitionKind::BoundedGap
                });
            }
            if candidates.is_empty() {
                route_debug!(self, "operation-unavailable",
                    "child" => self.debug_peer(&child_peer_id),
                    "reason" => reason);
                if is_background_convergence(reason) {
                    if reason == DemandReason::DirectConvergence {
                        self.direct_continuations.delete(&child_peer_id);
                    }
                    let final_route = self.current_final_route(&child_peer_id);
                    self.finish_timing(
                        &child_peer_id,
                        now_ms,
                        final_route,
                        RejectionBucket::CandidateFailed,
                        false,
                    );
                    continue;
                }
                if reason == DemandReason::SfuBootstrap {
                    let failed_demand = self.complete_sfu_bootstrap_carrier(
                        &child_peer_id,
                        now_ms,
                        RejectionBucket::CandidateFailed,
                        None,
                    );
                    if !failed_demand.is_empty() {
                        failed_peer_ids.push(failed_demand);
                    }
                    continue;
                }
                if self.retire_invalid_operation_edge(&child_peer_id, &mut released) {
                    self.revision = self.allocate_revision();
                    self.touch_facts();
                }
                self.participant_mut(&child_peer_id)
                    .unwrap()
                    .availability_exhausted = true;
                let bucket = if reason == DemandReason::CapacityReduction {
                    RejectionBucket::EndpointCapacity
                } else {
                    RejectionBucket::CandidateFailed
                };
                self.finish_timing(&demand_peer_id, now_ms, FinalRoute::Failed, bucket, false);
                if self.bootstrap_candidate_available(&demand_peer_id) {
                    continue;
                }
                self.remember_unavailable_sfu_bootstrap(&demand_peer_id);
                failed_peer_ids.push(demand_peer_id);
                continue;
            }
            self.start_operation_timing(&demand_peer_id, now_ms);
            let quality_source = if reason == DemandReason::QualityConvergence {
                self.current_quality_operation_source(&child_peer_id)
            } else {
                None
            };
            let child_session_id = self.participant(&child_peer_id).unwrap().session_id.clone();
            let demand_session_id = self
                .participant(&demand_peer_id)
                .unwrap()
                .session_id
                .clone();
            let routes: Vec<String> = if self.debug_enabled() {
                candidates
                    .iter()
                    .map(|candidate| {
                        format!(
                            "{}/{}",
                            self.debug_tuple(&candidate.tuple),
                            candidate.endpoint_transition.kind
                        )
                    })
                    .collect()
            } else {
                Vec::new()
            };
            self.operation = Some(Operation {
                video_generation: self.required_video_generation(),
                attach_connection_id: String::new(),
                child_peer_id: child_peer_id.clone(),
                child_session_id,
                demand_peer_id,
                demand_session_id,
                reason,
                base_revision: self.revision,
                candidates,
                cursor: 0,
                deadline_at_ms: now_ms + self.operation_timeout_ms,
                built_at_fact_version: self.fact_version,
                deferred_parent_peer_ids: Vec::new(),
                peer_attempt_started: false,
                quality_source,
                current: None,
            });
            route_debug!(self, "operation-started",
                "child" => self.debug_peer(&child_peer_id),
                "reason" => reason,
                "candidates" => routes.join(","));
            return result(self, removed_peer_ids, failed_peer_ids, released);
        }
        ReconcileResult {
            operation: None,
            removed_peer_ids,
            failed_peer_ids,
            released,
        }
    }

    /// The single place that expires, replans and clears the operation.
    /// `failed_guard` names an attempt the caller failed.
    pub(crate) fn validate_or_advance(
        &mut self,
        now_ms: i64,
        failed_guard: Option<&CandidateGuard>,
    ) -> Validation {
        let active_revision_at_start = self.revision;
        let mut result = Validation::default();
        let Some(op) = self.operation.as_ref() else {
            return result;
        };
        if op.reason == DemandReason::PublicationAttach {
            return self.validate_publication_attach(now_ms, failed_guard);
        }
        if !self.bootstrap_operation_owned(op) {
            if let Some(current) = &op.current {
                result
                    .released
                    .extend(reservation_resources(&current.reservation));
                self.advance_active_revision();
            }
            let demand_peer_id = self.op().demand_peer_id.clone();
            let final_route = self.current_final_route(&demand_peer_id);
            self.finish_timing(
                &demand_peer_id,
                now_ms,
                final_route,
                RejectionBucket::Stale,
                false,
            );
            self.operation = None;
            return result;
        }
        if op.reason == DemandReason::QualityConvergence
            && !self.quality_operation_still_eligible(op, now_ms)
        {
            result
                .released
                .extend(self.abort_operation(Some(now_ms), RejectionBucket::Aborted));
            return result;
        }
        if op.reason == DemandReason::RootConvergence
            && !self.root_convergence_operation_still_eligible(op)
        {
            result
                .released
                .extend(self.abort_operation(Some(now_ms), RejectionBucket::Aborted));
            return result;
        }
        if now_ms >= op.deadline_at_ms {
            let reason = op.reason;
            let sfu_bootstrap = reason == DemandReason::SfuBootstrap;
            let background_convergence = is_background_convergence(reason);
            if reason == DemandReason::DirectConvergence {
                self.consume_direct_continuation_candidate();
            }
            if reason == DemandReason::QualityConvergence {
                self.consume_quality_operation_candidates();
            }
            let revision_advanced = self.op().current.is_some();
            if let Some(current) = &self.op().current {
                result
                    .released
                    .extend(reservation_resources(&current.reservation));
                self.advance_active_revision();
            }
            let facts_changed = self.op().built_at_fact_version != self.fact_version;
            let deadline_bucket = if facts_changed {
                RejectionBucket::Stale
            } else {
                RejectionBucket::OperationDeadline
            };
            let mut failed_bootstrap_demand = String::new();
            if sfu_bootstrap {
                let child_peer_id = self.op().child_peer_id.clone();
                let plan = self.op().plan_at_or_last();
                failed_bootstrap_demand = self.complete_sfu_bootstrap_carrier(
                    &child_peer_id,
                    now_ms,
                    deadline_bucket,
                    plan,
                );
            }
            let demand_peer_id = self.op().demand_peer_id.clone();
            let bootstrap_available = !background_convergence
                && !sfu_bootstrap
                && self.bootstrap_candidate_available(&demand_peer_id);
            if bootstrap_available {
                self.consume_current_availability_opportunity();
            } else {
                self.consume_availability_operation();
            }
            let exhausted = if sfu_bootstrap {
                !failed_bootstrap_demand.is_empty()
            } else {
                !background_convergence
                    && !bootstrap_available
                    && !self.has_remaining_availability_opportunity(self.op())
            };
            if bootstrap_available {
                self.stage_sfu_bootstrap();
            }
            if !sfu_bootstrap {
                let final_route =
                    self.expired_final_route(&demand_peer_id, background_convergence, exhausted);
                self.finish_timing(&demand_peer_id, now_ms, final_route, deadline_bucket, false);
            }
            self.block_and_clear(
                !background_convergence && !sfu_bootstrap,
                &mut result.released,
                revision_advanced,
            );
            if exhausted && !sfu_bootstrap {
                self.remember_unavailable_sfu_bootstrap(&demand_peer_id);
            }
            result.expired = true;
            if !failed_bootstrap_demand.is_empty() {
                result.exhausted_peer_ids = vec![failed_bootstrap_demand];
            } else if exhausted {
                result.exhausted_peer_ids = vec![demand_peer_id];
            }
            return result;
        }
        if self.advance_expired_direct_head_start(now_ms, &mut result.released) {
            result.expired = true;
        }
        let op = self.op();
        let child_ok = self
            .participant(&op.child_peer_id)
            .is_some_and(|child| child.session_is(&op.child_session_id));
        let demand_ok = self
            .participant(&op.demand_peer_id)
            .is_some_and(|demand| demand.session_is(&op.demand_session_id));
        if !child_ok || !demand_ok {
            if let Some(current) = &op.current {
                result
                    .released
                    .extend(reservation_resources(&current.reservation));
                self.advance_active_revision();
            }
            let demand_peer_id = self.op().demand_peer_id.clone();
            let final_route = self.current_final_route(&demand_peer_id);
            self.finish_timing(
                &demand_peer_id,
                now_ms,
                final_route,
                RejectionBucket::Aborted,
                false,
            );
            self.operation = None;
            return result;
        }

        while self.operation.is_some() {
            let op = self.op();
            if op.current.is_none() && op.built_at_fact_version != self.fact_version {
                self.replan_remaining(None, None);
            }
            let op = self.op();
            if let Some(att) = &op.current {
                let guard_failed =
                    failed_guard.is_some_and(|guard| self.guard_matches(guard, op, att));
                let plan = op.plan_at(op.cursor);
                let peer_no_progress_expired = att.tuple.kind == UpstreamKind::Peer
                    && !att.transport_connected
                    && self.peer_candidate_has_successor(op)
                    && now_ms >= att.started_at_ms + self.direct_head_start_ms();
                if !guard_failed
                    && !peer_no_progress_expired
                    && plan
                        .as_ref()
                        .is_some_and(|plan| self.operation_candidate_valid(op, plan, Some(att)))
                {
                    return result;
                }
                let demand_peer_id = op.demand_peer_id.clone();
                let reason = op.reason;
                if !guard_failed {
                    let bucket = if peer_no_progress_expired {
                        RejectionBucket::FirstFrameTimeout
                    } else {
                        RejectionBucket::Stale
                    };
                    self.note_rejection(&demand_peer_id, bucket);
                }
                if peer_no_progress_expired {
                    result.expired = true;
                }
                if guard_failed || peer_no_progress_expired {
                    self.consume_quality_candidate_opportunity(plan.as_ref());
                }
                if reason == DemandReason::DirectConvergence {
                    self.consume_direct_continuation_candidate();
                }
                let reservation = self.op().current.as_ref().unwrap().reservation.clone();
                result.released.extend(reservation_resources(&reservation));
                self.advance_active_revision();
                let op = self.op_mut();
                op.current = None;
                op.cursor += 1;
                self.clear_candidate_timing(&demand_peer_id);
                result.consumed_guard = guard_failed;
                self.replan_remaining(None, None);
            }
            loop {
                let op = self.op();
                if op.cursor >= op.candidates.len()
                    || self.operation_candidate_valid(op, &op.candidates[op.cursor], None)
                {
                    break;
                }
                let demand_peer_id = op.demand_peer_id.clone();
                let reason = op.reason;
                self.note_rejection(&demand_peer_id, RejectionBucket::Stale);
                if reason == DemandReason::DirectConvergence {
                    self.consume_direct_continuation_candidate();
                }
                self.op_mut().cursor += 1;
                self.clear_candidate_timing(&demand_peer_id);
            }
            let op = self.op();
            if op.cursor < op.candidates.len() {
                return result;
            }
            let reason = op.reason;
            let sfu_bootstrap = reason == DemandReason::SfuBootstrap;
            let background_convergence = is_background_convergence(reason);
            let facts_changed = op.built_at_fact_version != self.fact_version;
            let demand_peer_id = op.demand_peer_id.clone();
            let rejection_bucket = if facts_changed {
                RejectionBucket::Stale
            } else {
                match self.route_timings.get(&demand_peer_id) {
                    Some(record) if record.rejection_bucket != RejectionBucket::None => {
                        record.rejection_bucket
                    }
                    _ => RejectionBucket::CandidateFailed,
                }
            };
            let mut failed_bootstrap_demand = String::new();
            if sfu_bootstrap {
                let child_peer_id = op.child_peer_id.clone();
                let plan = op.plan_at_or_last();
                failed_bootstrap_demand = self.complete_sfu_bootstrap_carrier(
                    &child_peer_id,
                    now_ms,
                    rejection_bucket,
                    plan,
                );
            }
            let bootstrap_available = !background_convergence
                && !sfu_bootstrap
                && self.bootstrap_candidate_available(&demand_peer_id);
            if !bootstrap_available {
                self.consume_availability_operation();
            }
            let exhausted = if sfu_bootstrap {
                !failed_bootstrap_demand.is_empty()
            } else {
                !background_convergence
                    && !bootstrap_available
                    && !self.has_remaining_availability_opportunity(self.op())
            };
            if bootstrap_available {
                self.stage_sfu_bootstrap();
            }
            if !sfu_bootstrap {
                let final_route =
                    self.expired_final_route(&demand_peer_id, background_convergence, exhausted);
                self.finish_timing(
                    &demand_peer_id,
                    now_ms,
                    final_route,
                    rejection_bucket,
                    false,
                );
            }
            let revision_advanced = self.revision != active_revision_at_start;
            self.block_and_clear(
                !background_convergence && !sfu_bootstrap,
                &mut result.released,
                revision_advanced,
            );
            if !failed_bootstrap_demand.is_empty() {
                result.exhausted_peer_ids = vec![failed_bootstrap_demand];
            } else if exhausted {
                result.exhausted_peer_ids = vec![demand_peer_id.clone()];
                self.remember_unavailable_sfu_bootstrap(&demand_peer_id);
            }
        }
        result
    }

    pub(super) fn expired_final_route(
        &self,
        demand_peer_id: &str,
        background_convergence: bool,
        exhausted: bool,
    ) -> FinalRoute {
        if background_convergence {
            return self.current_final_route(demand_peer_id);
        }
        if exhausted {
            FinalRoute::Failed
        } else {
            FinalRoute::Waiting
        }
    }

    pub(super) fn block_and_clear(
        &mut self,
        block: bool,
        released: &mut Vec<ResourceRef>,
        revision_advanced: bool,
    ) {
        let child_peer_id = self.op().child_peer_id.clone();
        let child_exists = self.participant(&child_peer_id).is_some();
        self.operation = None;
        let retired_invalid =
            block && child_exists && self.retire_invalid_operation_edge(&child_peer_id, released);
        if retired_invalid && !revision_advanced {
            self.revision = self.allocate_revision();
        }
        if retired_invalid {
            self.touch_facts();
        }
        if block && child_exists {
            self.participant_mut(&child_peer_id)
                .unwrap()
                .availability_exhausted = true;
        }
    }

    /// Replans the suffix from the cursor, forcing `first_tuple` into the
    /// first slot and appending `restore_tuple` when absent.
    pub(super) fn replan_remaining(
        &mut self,
        first_tuple: Option<CandidateTuple>,
        restore_tuple: Option<CandidateTuple>,
    ) {
        let op = self.op();
        let cursor = op.cursor.min(op.candidates.len());
        let mut replanned = op.candidates[..cursor].to_vec();
        let mut tuples: Vec<CandidateTuple> = op.candidates[cursor..]
            .iter()
            .map(|candidate| candidate.tuple.clone())
            .collect();
        if let Some(first) = first_tuple {
            if tuples.is_empty() {
                tuples.push(first);
            } else {
                tuples[0] = first;
            }
        }
        if let Some(restore) = restore_tuple {
            let restore_key = tuple_key(&restore);
            if !tuples.iter().any(|tuple| tuple_key(tuple) == restore_key) {
                tuples.push(restore);
            }
        }
        let child_peer_id = op.child_peer_id.clone();
        for tuple in tuples {
            if let Some(plan) = self.plan_candidate(&child_peer_id, &tuple)
                && self.operation_candidate_valid(self.op(), &plan, None)
            {
                replanned.push(plan);
            }
        }
        let fact_version = self.fact_version;
        let op = self.op_mut();
        op.candidates = replanned;
        op.built_at_fact_version = fact_version;
    }
}
