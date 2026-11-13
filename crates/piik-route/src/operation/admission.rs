//! Admission of a scheduled candidate: begin, skip and bounded-gap
//! producer retirement at the guarded cursor.

use super::*;

impl Controller {
    /// Starts the candidate at the guarded cursor with the caller's
    /// reservation; every rejection echoes the reservation back in
    /// `released`. Publication attachment takes an empty reservation and
    /// preserves the committed edge. Other attempts panic on a mismatched
    /// reservation.
    pub fn begin_current_candidate(&mut self, input: BeginInput) -> BeginResult {
        let validation = self.validate_or_advance(input.now_ms, None);
        let rejected = |_: &Self,
                        validation: Validation,
                        reservation: &CandidateReservation,
                        operation| BeginResult {
            accepted: false,
            operation,
            failed_peer_ids: failed_peer_ids_from(&[&validation]),
            released: concat(validation.released, reservation_resources(reservation)),
        };
        let ready = self.operation.as_ref().is_some_and(|op| {
            op.current.is_none()
                && op.base_revision == self.revision
                && self.cursor_guard_matches(&input.guard, op)
        });
        if !ready {
            return rejected(self, validation, &input.reservation, None);
        }
        if self.op().reason == DemandReason::PublicationAttach {
            return self.begin_publication_attach(input, validation);
        }
        let op = self.op();
        let Some(plan) = op
            .plan_at(op.cursor)
            .filter(|plan| self.operation_candidate_valid(op, plan, None))
        else {
            return rejected(self, validation, &input.reservation, None);
        };
        if plan.endpoint_transition.kind == TransitionKind::BoundedGap {
            let snapshot = self.operation_snapshot();
            return rejected(self, validation, &input.reservation, snapshot);
        }
        let tuple = plan.tuple.clone();
        let host_session_id = if tuple.kind == UpstreamKind::Sfu {
            self.host_session_id()
        } else {
            String::new()
        };
        if tuple.kind == UpstreamKind::Sfu
            && (input.host_session_id.is_empty() || input.host_session_id != host_session_id)
        {
            let snapshot = self.operation_snapshot();
            return rejected(self, validation, &input.reservation, snapshot);
        }
        assert_reservation(
            &tuple,
            &input.reservation,
            plan.endpoint_transition.kind == TransitionKind::Overlap,
        );
        let mut publication_generation = String::new();
        if tuple.kind == UpstreamKind::Sfu {
            publication_generation = if tuple.publication == Some(PublicationPlan::Reuse) {
                self.host_publication
                    .as_ref()
                    .map(|publication| publication.generation.clone())
                    .unwrap_or_default()
            } else {
                input.publication_generation.clone()
            };
        }
        if tuple.kind == UpstreamKind::Sfu && publication_generation.is_empty() {
            panic!("SFU candidate needs a publication generation");
        }
        if tuple.kind == UpstreamKind::Sfu
            && tuple.publication != Some(PublicationPlan::Reuse)
            && input.publication_connection_id.is_empty()
        {
            panic!("SFU publication candidate needs an ingress connection identity");
        }
        let parent_session_id = if tuple.kind == UpstreamKind::Peer {
            self.participant(&tuple.parent_peer_id)
                .map(|parent| parent.session_id.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        let attempt_host_session_id = if tuple.kind == UpstreamKind::Sfu {
            input.host_session_id.clone()
        } else {
            String::new()
        };
        // The advertised revision is not advanced here.
        let revision = self.allocate_revision();
        let connection_attempt = self.start_candidate_opportunity(&plan);
        let child_session_id = self.op().child_session_id.clone();
        self.op_mut().current = Some(Attempt {
            tuple: tuple.clone(),
            revision,
            connection_id: input.connection_id,
            child_session_id,
            parent_session_id,
            host_session_id: attempt_host_session_id,
            publication_generation,
            publication_connection_id: input.publication_connection_id,
            reservation: input.reservation,
            started_at_ms: input.now_ms,
            transport_connected: false,
            media_ready: false,
            connection_attempt,
            sender_quality_state: None,
            sender_quality_accepted_at_ms: None,
            sender_quality_consecutive_healthy_windows: 0,
            sender_quality_consecutive_degraded_windows: 0,
            sender_quality_sample_timestamp_ms: None,
            sender_quality_identity: String::new(),
            relative_quality_approved_at_ms: None,
        });
        route_debug!(self, "candidate-started",
            "child" => self.debug_peer(&self.op().child_peer_id),
            "candidate" => self.debug_tuple(&tuple),
            "revision" => revision,
            "cursor" => self.op().cursor,
            "connectionAttempt" => connection_attempt
                .map_or_else(String::new, |attempt| format!("{}/{}", attempt.current, attempt.total)));
        let demand_peer_id = self.op().demand_peer_id.clone();
        self.start_candidate_timing(&demand_peer_id, input.now_ms);
        BeginResult {
            accepted: true,
            operation: self.operation_snapshot(),
            failed_peer_ids: failed_peer_ids_from(&[&validation]),
            released: validation.released,
        }
    }

    /// Records a rejection bucket for the guarded cursor while no attempt is
    /// live; true when the guard matched.
    pub fn note_current_candidate_rejection(
        &mut self,
        guard: &CandidateCursorGuard,
        bucket: RejectionBucket,
    ) -> bool {
        let Some(op) = self.operation.as_ref() else {
            return false;
        };
        if op.current.is_some() || !self.cursor_guard_matches(guard, op) {
            return false;
        }
        let demand_peer_id = op.demand_peer_id.clone();
        self.note_rejection(&demand_peer_id, bucket);
        true
    }

    /// Consumes the guarded candidate and advances the cursor, recording the
    /// caller's rejection reason.
    pub fn skip_current_candidate(
        &mut self,
        guard: &CandidateCursorGuard,
        now_ms: i64,
        bucket: RejectionBucket,
    ) -> BeginResult {
        let validation = self.validate_or_advance(now_ms, None);
        let ready = self
            .operation
            .as_ref()
            .is_some_and(|op| op.current.is_none() && self.cursor_guard_matches(guard, op));
        if !ready {
            return BeginResult {
                accepted: false,
                operation: None,
                failed_peer_ids: failed_peer_ids_from(&[&validation]),
                released: validation.released,
            };
        }
        if self.op().reason == DemandReason::PublicationAttach {
            let failed = self.finish_publication_attach(false);
            return BeginResult {
                accepted: true,
                operation: None,
                failed_peer_ids: failed,
                released: validation.released,
            };
        }
        let op = self.op();
        let (reason, child_peer_id, demand_peer_id, cursor) = (
            op.reason,
            op.child_peer_id.clone(),
            op.demand_peer_id.clone(),
            op.cursor,
        );
        if bucket == RejectionBucket::CandidateFailed {
            if is_availability_operation(reason) || reason == DemandReason::DirectConvergence {
                let plan = self.op().candidates[cursor].clone();
                self.consume_candidate_opportunity(&child_peer_id, &plan);
            }
            self.promote_next_direct_within_head_start(now_ms);
        }
        let plan = self.op().plan_at(self.op().cursor);
        self.consume_quality_candidate_opportunity(plan.as_ref());
        self.consume_direct_continuation_candidate();
        self.note_rejection(&demand_peer_id, bucket);
        route_debug!(self, "candidate-rejected",
            "child" => self.debug_peer(&child_peer_id),
            "candidate" => self.debug_tuple(&self.op().candidates[self.op().cursor].tuple),
            "bucket" => bucket,
            "cursor" => self.op().cursor);
        self.op_mut().cursor += 1;
        self.clear_candidate_timing(&demand_peer_id);
        let advanced = self.validate_or_advance(now_ms, None);
        BeginResult {
            operation: self.operation_snapshot(),
            accepted: true,
            failed_peer_ids: failed_peer_ids_from(&[&validation, &advanced]),
            released: concat(validation.released, advanced.released),
        }
    }

    /// Performs the bounded-gap retirement of the guarded candidate's
    /// producer, advances the revision and replans the tail.
    pub fn retire_current_candidate_producer(
        &mut self,
        guard: &CandidateCursorGuard,
        now_ms: i64,
    ) -> BeginResult {
        let validation = self.validate_or_advance(now_ms, None);
        let ready = self
            .operation
            .as_ref()
            .is_some_and(|op| op.current.is_none() && self.cursor_guard_matches(guard, op));
        if !ready {
            return BeginResult {
                accepted: false,
                operation: None,
                failed_peer_ids: failed_peer_ids_from(&[&validation]),
                released: validation.released,
            };
        }
        let op = self.op();
        let Some(plan) = op
            .plan_at(op.cursor)
            .filter(|plan| plan.endpoint_transition.kind == TransitionKind::BoundedGap)
        else {
            return BeginResult {
                accepted: false,
                operation: self.operation_snapshot(),
                failed_peer_ids: failed_peer_ids_from(&[&validation]),
                released: validation.released,
            };
        };
        let failed_peer_ids = failed_peer_ids_from(&[&validation]);
        let mut released = validation.released;
        let retirement = plan
            .endpoint_transition
            .retire
            .clone()
            .expect("bounded gap names a retirement");
        let mut restore_tuple = None;
        if retirement.kind == RetirementKind::Edge {
            let matches = self.edge(&retirement.child_peer_id).is_some_and(|edge| {
                edge.kind == UpstreamKind::Peer
                    && edge.physical_active
                    && edge.child_session_id == retirement.child_session_id
                    && edge.parent_peer_id == retirement.parent_peer_id
                    && edge.parent_session_id == retirement.parent_session_id
                    && Some(edge.transport) == retirement.transport
                    && edge.connection_id == retirement.connection_id
            });
            if !matches {
                return BeginResult {
                    accepted: false,
                    operation: self.operation_snapshot(),
                    failed_peer_ids,
                    released,
                };
            }
            let edge = self.edge_mut(&retirement.child_peer_id).unwrap();
            if edge.usable {
                restore_tuple = Some(CandidateTuple {
                    kind: UpstreamKind::Peer,
                    parent_peer_id: edge.parent_peer_id.clone(),
                    transport: Some(edge.transport),
                    regenerate: false,
                    publication: None,
                });
            }
            edge.usable = false;
            edge.physical_active = false;
        } else {
            let matches = self.host_publication.as_ref().is_some_and(|publication| {
                publication.physical_active
                    && publication.host_session_id == retirement.host_session_id
                    && publication.generation == retirement.generation
                    && publication.connection_id == retirement.connection_id
            });
            if !matches {
                return BeginResult {
                    accepted: false,
                    operation: self.operation_snapshot(),
                    failed_peer_ids,
                    released,
                };
            }
            released.extend(self.remove_publication_generation(&retirement.generation));
        }
        self.advance_active_revision();
        self.touch_facts();
        let mut next_tuple = plan.tuple.clone();
        if retirement.kind == RetirementKind::Publication
            && plan.tuple.kind == UpstreamKind::Sfu
            && plan.tuple.publication == Some(PublicationPlan::Replace)
        {
            next_tuple.publication = Some(PublicationPlan::Create);
        }
        self.replan_remaining(Some(next_tuple), restore_tuple);
        if self.op().cursor >= self.op().candidates.len() {
            let advanced = self.validate_or_advance(now_ms, None);
            let mut failed = vec![];
            failed.extend(failed_peer_ids);
            failed.extend(advanced.exhausted_peer_ids.iter().cloned());
            return BeginResult {
                accepted: true,
                operation: self.operation_snapshot(),
                failed_peer_ids: dedupe_strings(failed),
                released: concat(released, advanced.released),
            };
        }
        BeginResult {
            accepted: true,
            operation: self.operation_snapshot(),
            failed_peer_ids,
            released,
        }
    }

    pub(super) fn cursor_guard_matches(
        &self,
        guard: &CandidateCursorGuard,
        op: &Operation,
    ) -> bool {
        op.plan_at(op.cursor).is_some_and(|plan| {
            guard.child_peer_id == op.child_peer_id
                && guard.child_session_id == op.child_session_id
                && guard.base_revision == op.base_revision
                && guard.fact_version == op.built_at_fact_version
                && guard.cursor == op.cursor
                && candidate_plan_equals(&guard.plan, &plan)
        })
    }
}
