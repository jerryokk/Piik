//! Settling a live attempt: commit or fail it, transport evidence,
//! deadline expiry and the committed-edge installation.

use super::*;

impl Controller {
    /// The commit gate for the guarded live attempt: commits when the
    /// required proof is present and `commit_reservation` accepts, fails the
    /// candidate otherwise.
    pub fn candidate_ready(
        &mut self,
        guard: &CandidateGuard,
        now_ms: i64,
        commit_reservation: CommitReservation<'_>,
        proof: CandidateProof,
    ) -> SettleResult {
        let validation = self.validate_or_advance(now_ms, None);
        let matches = self.operation.as_ref().is_some_and(|op| {
            op.current
                .as_ref()
                .is_some_and(|att| self.guard_matches(guard, op, att))
        });
        if !matches {
            return self.settle(
                false,
                failed_peer_ids_from(&[&validation]),
                validation.released,
            );
        }
        let op = self.op();
        let att = op.current.as_ref().unwrap();
        if self.room_media
            && !op.video_generation.is_empty()
            && proof.video_generation != op.video_generation
        {
            return self.settle(false, Vec::new(), validation.released);
        }
        if self.room_media && op.video_generation.is_empty() && !att.transport_connected {
            return self.settle(false, Vec::new(), validation.released);
        }
        if op.reason == DemandReason::PublicationAttach {
            self.finish_publication_attach(true);
            let mut result = self.settle(true, Vec::new(), validation.released);
            result.committed = true;
            return result;
        }
        let quality_peer =
            op.reason == DemandReason::QualityConvergence && att.tuple.kind == UpstreamKind::Peer;
        if quality_peer
            && proof.relative_quality_approved
            && att.relative_quality_approved_at_ms.is_none()
        {
            self.op_mut()
                .current
                .as_mut()
                .unwrap()
                .relative_quality_approved_at_ms = Some(now_ms);
        }
        let op = self.op();
        let att = op.current.as_ref().unwrap();
        let relative_p2p_approved = quality_peer
            && att
                .relative_quality_approved_at_ms
                .is_some_and(|at| now_ms < at + QUALITY_EVIDENCE_EXPIRY_MS);
        let relative_p2p_expired =
            quality_peer && att.relative_quality_approved_at_ms.is_some() && !relative_p2p_approved;
        let relative_p2p_pending = quality_peer && !relative_p2p_approved;
        if relative_p2p_approved && candidate_sender_persistently_degraded(att, now_ms) {
            return self.candidate_failed(guard, now_ms);
        }
        let native_candidate_proof = operation_requires_native_candidate_proof(Some(op));
        if relative_p2p_approved || relative_p2p_pending || native_candidate_proof {
            // validate_or_advance already owns the ongoing operation's eligibility.
            self.op_mut().current.as_mut().unwrap().media_ready = true;
        }
        if relative_p2p_expired {
            // A one-shot client proof cannot be renewed by a late sender
            // sample. Release this candidate so the existing bounded cursor
            // can try the next route instead of waiting for the deadline.
            return self.candidate_failed(guard, now_ms);
        }
        if relative_p2p_pending {
            return self.settle(
                true,
                failed_peer_ids_from(&[&validation]),
                validation.released,
            );
        }
        let att = self.op().current.as_ref().unwrap();
        let sender_fresh = att
            .sender_quality_accepted_at_ms
            .is_some_and(|at| now_ms < at + QUALITY_EVIDENCE_EXPIRY_MS);
        if relative_p2p_approved
            && (att.sender_quality_state != Some(SenderQualityState::Healthy) || !sender_fresh)
        {
            return self.settle(
                true,
                failed_peer_ids_from(&[&validation]),
                validation.released,
            );
        }
        if native_candidate_proof {
            let sfu_reuse = att.tuple.kind == UpstreamKind::Sfu
                && att.tuple.publication == Some(PublicationPlan::Reuse);
            let candidate_quality_state = if sfu_reuse {
                self.sfu_publisher_quality_state(&att.publication_generation, now_ms)
            } else {
                att.sender_quality_state
                    .unwrap_or(SenderQualityState::Unknown)
            };
            let candidate_quality_ready = candidate_quality_state == SenderQualityState::Healthy
                && (att.tuple.kind != UpstreamKind::Sfu
                    || att.tuple.publication == Some(PublicationPlan::Reuse)
                    || att.sender_quality_consecutive_healthy_windows
                        >= PERSISTENT_DEGRADED_WINDOWS);
            if !candidate_quality_ready && candidate_quality_state != SenderQualityState::Degraded {
                return self.settle(
                    true,
                    failed_peer_ids_from(&[&validation]),
                    validation.released,
                );
            }
            if !sfu_reuse && !sender_fresh {
                return self.candidate_failed(guard, now_ms);
            }
            if candidate_quality_state == SenderQualityState::Degraded {
                return self.candidate_failed(guard, now_ms);
            }
        }
        let att = self.op().current.as_ref().unwrap();
        let accepted = commit_reservation.is_none_or(|commit| commit(&att.reservation));
        if !accepted {
            self.consume_current_availability_opportunity();
            route_debug!(self, "candidate-commit-rejected",
                "child" => self.debug_peer(&self.op().child_peer_id),
                "candidate" => self.debug_tuple(&self.op().current.as_ref().unwrap().tuple),
                "revision" => self.op().current.as_ref().unwrap().revision);
            self.promote_next_direct_within_head_start(now_ms);
            let demand_peer_id = self.op().demand_peer_id.clone();
            self.note_rejection(&demand_peer_id, RejectionBucket::CandidateFailed);
            let failed = self.validate_or_advance(now_ms, Some(guard));
            return self.settle(
                false,
                failed_peer_ids_from(&[&validation, &failed]),
                concat(validation.released, failed.released),
            );
        }
        let op = self.op().clone();
        let att = op.current.clone().unwrap();
        if op.reason != DemandReason::SfuBootstrap {
            let final_route = if att.tuple.kind == UpstreamKind::Peer {
                FinalRoute::Direct
            } else {
                FinalRoute::Sfu
            };
            self.finish_timing(
                &op.demand_peer_id,
                now_ms,
                final_route,
                RejectionBucket::None,
                true,
            );
        }
        route_debug!(self, "candidate-ready",
            "child" => self.debug_peer(&op.child_peer_id),
            "candidate" => self.debug_tuple(&att.tuple),
            "revision" => att.revision);
        // Displaced SFU children in map order.
        let mut displaced_sfu_children = Vec::new();
        if att.tuple.kind == UpstreamKind::Sfu
            && att.tuple.publication == Some(PublicationPlan::Replace)
        {
            for (child_peer_id, edge) in self.upstream_by_viewer.iter() {
                if *child_peer_id != op.child_peer_id
                    && edge.kind == UpstreamKind::Sfu
                    && edge.physical_active
                {
                    displaced_sfu_children.push(child_peer_id.clone());
                }
            }
        }
        let committed = self.commit_attempt(op, att);
        let released = concat(validation.released.clone(), committed);
        for child_peer_id in displaced_sfu_children {
            self.ensure_demand(&child_peer_id, now_ms, DemandReason::EdgeUnavailable);
        }
        SettleResult {
            accepted: true,
            committed: true,
            failed_peer_ids: failed_peer_ids_from(&[&validation]),
            active_revision: self.revision,
            released,
        }
    }

    /// Records actual transport establishment. Peer attempts survive the
    /// direct head start; room-mode silent attempts also require this
    /// evidence before `candidate_ready` may commit.
    pub fn candidate_transport_connected(
        &mut self,
        guard: &CandidateGuard,
        now_ms: i64,
    ) -> SettleResult {
        let validation = self.validate_or_advance(now_ms, None);
        let matches = self.operation.as_ref().is_some_and(|op| {
            op.current.as_ref().is_some_and(|att| {
                (att.tuple.kind == UpstreamKind::Peer || self.room_media)
                    && self.guard_matches(guard, op, att)
            })
        });
        if !matches {
            return self.settle(
                false,
                failed_peer_ids_from(&[&validation]),
                validation.released,
            );
        }
        self.op_mut().current.as_mut().unwrap().transport_connected = true;
        route_debug!(self, "candidate-transport-connected",
            "child" => self.debug_peer(&self.op().child_peer_id),
            "candidate" => self.debug_tuple(&self.op().current.as_ref().unwrap().tuple),
            "revision" => self.op().current.as_ref().unwrap().revision);
        self.settle(
            true,
            failed_peer_ids_from(&[&validation]),
            validation.released,
        )
    }

    /// Fails the guarded live attempt, releasing its reservation and
    /// advancing the cursor; `accepted` reports whether the guard was consumed.
    pub fn candidate_failed(&mut self, guard: &CandidateGuard, now_ms: i64) -> SettleResult {
        if self
            .operation
            .as_ref()
            .is_some_and(|op| op.reason == DemandReason::PublicationAttach)
        {
            let validation = self.validate_or_advance(now_ms, Some(guard));
            return self.settle(
                validation.consumed_guard,
                failed_peer_ids_from(&[&validation]),
                validation.released,
            );
        }
        let matches = self.operation.as_ref().is_some_and(|op| {
            op.current
                .as_ref()
                .is_some_and(|att| self.guard_matches(guard, op, att))
        });
        if matches {
            self.consume_current_availability_opportunity();
            route_debug!(self, "candidate-failed",
                "child" => self.debug_peer(&self.op().child_peer_id),
                "candidate" => self.debug_tuple(&self.op().current.as_ref().unwrap().tuple),
                "revision" => self.op().current.as_ref().unwrap().revision);
            self.promote_next_direct_within_head_start(now_ms);
            let demand_peer_id = self.op().demand_peer_id.clone();
            self.note_rejection(&demand_peer_id, RejectionBucket::CandidateFailed);
        }
        let validation = self.validate_or_advance(now_ms, Some(guard));
        self.settle(
            validation.consumed_guard,
            failed_peer_ids_from(&[&validation]),
            validation.released,
        )
    }

    /// Applies head-start and deadline expiry at `now_ms`; `accepted` reports
    /// whether something expired.
    pub fn operation_expired(&mut self, now_ms: i64) -> SettleResult {
        if let Some(op) = self.operation.as_ref().filter(|_| self.debug_enabled()) {
            let no_progress = op.current.as_ref().is_some_and(|current| {
                current.tuple.kind == UpstreamKind::Peer
                    && !current.transport_connected
                    && self.peer_candidate_has_successor(op)
                    && now_ms >= current.started_at_ms + self.direct_head_start_ms()
                    && now_ms < op.deadline_at_ms
            });
            let event = if no_progress {
                "candidate-no-progress"
            } else {
                "operation-deadline"
            };
            route_debug!(self, event,
                "child" => self.debug_peer(&op.child_peer_id),
                "candidate" => op.current.as_ref().map_or_else(String::new, |current| self.debug_tuple(&current.tuple)),
                "cursor" => op.cursor,
                "candidateCount" => op.candidates.len());
        }
        let validation = self.validate_or_advance(now_ms, None);
        self.settle(
            validation.expired,
            failed_peer_ids_from(&[&validation]),
            validation.released,
        )
    }

    /// Installs the attempt as the child's committed edge and returns the
    /// resources committed before but not after, plus the overlap.
    fn commit_attempt(&mut self, op: Operation, att: Attempt) -> Vec<ResourceRef> {
        let mut before = ResourceSet::default();
        before.add(self.committed_resources());
        let old = self.edge(&op.child_peer_id).cloned();
        let old_peer = old.as_ref().filter(|old| old.kind == UpstreamKind::Peer);
        let same_parent_regeneration = op.reason == DemandReason::QualityConvergence
            && att.tuple.kind == UpstreamKind::Peer
            && att.tuple.regenerate
            && old_peer.is_some_and(|old| old.parent_peer_id == att.tuple.parent_peer_id);
        let creates_host_root = att.tuple.kind == UpstreamKind::Peer
            && att.tuple.parent_peer_id == self.host_peer_id
            && old_peer.is_none_or(|old| old.parent_peer_id != self.host_peer_id);
        self.clear_quality_for_participant(&op.child_peer_id);

        if att.tuple.kind == UpstreamKind::Sfu
            && att.tuple.publication == Some(PublicationPlan::Replace)
        {
            // The return value is discarded on purpose: the before/after
            // diff below picks those resources up.
            let generation = self.host_publication.as_ref().unwrap().generation.clone();
            self.remove_publication_generation(&generation);
        }

        if att.tuple.kind == UpstreamKind::Peer {
            let parent_session_id = self
                .participant(&att.tuple.parent_peer_id)
                .map(|parent| parent.session_id.clone())
                .unwrap_or_default();
            if parent_session_id.is_empty() {
                panic!("Candidate parent session is unavailable");
            }
            // Regenerate distinguishes quality trials, not the acquired
            // route's availability opportunity if this connection later fails.
            let opportunity_key = self.candidate_opportunity_base(
                &op.child_peer_id,
                &CandidatePlan::bare(CandidateTuple::bare(
                    UpstreamKind::Peer,
                    &att.tuple.parent_peer_id,
                )),
            );
            // Map set keeps an existing child's position.
            self.upstream_by_viewer.set(
                op.child_peer_id.clone(),
                CommittedEdge {
                    kind: UpstreamKind::Peer,
                    child_session_id: op.child_session_id.clone(),
                    parent_peer_id: att.tuple.parent_peer_id.clone(),
                    parent_session_id,
                    publication_generation: String::new(),
                    transport: Transport::Direct,
                    connection_id: att.connection_id.clone(),
                    usable: true,
                    physical_active: true,
                    video_generation: String::new(),
                    video_connection_id: String::new(),
                    video_ready: false,
                    resource: None,
                    opportunity_key,
                },
            );
            self.direct_continuations.delete(&op.child_peer_id);
        } else {
            let generation = att.publication_generation.clone();
            if att.tuple.publication != Some(PublicationPlan::Reuse) {
                let host_session_id = self.host_session_id();
                if host_session_id.is_empty() || att.publication_connection_id.is_empty() {
                    panic!("SFU publication identity is unavailable");
                }
                if att.host_session_id != host_session_id {
                    panic!("SFU publication Host session is stale");
                }
                self.host_publication = Some(HostPublication {
                    generation: generation.clone(),
                    host_session_id,
                    connection_id: att.publication_connection_id.clone(),
                    usable: true,
                    physical_active: true,
                    resource: att.reservation.publication.clone(),
                });
            }
            let opportunity_key = self.candidate_opportunity_base(
                &op.child_peer_id,
                &CandidatePlan::bare(att.tuple.clone()),
            );
            self.upstream_by_viewer.set(
                op.child_peer_id.clone(),
                CommittedEdge {
                    kind: UpstreamKind::Sfu,
                    child_session_id: op.child_session_id.clone(),
                    parent_peer_id: String::new(),
                    parent_session_id: String::new(),
                    publication_generation: generation.clone(),
                    transport: Transport::Sfu,
                    connection_id: att.connection_id.clone(),
                    usable: true,
                    physical_active: true,
                    video_generation: String::new(),
                    video_connection_id: String::new(),
                    video_ready: false,
                    resource: att.reservation.edge.clone(),
                    opportunity_key,
                },
            );
            let mut remaining_parent_peer_ids = Vec::new();
            if op.reason == DemandReason::SfuBootstrap
                && let Some(old) = old_peer.filter(|old| old.usable && old.physical_active)
            {
                remaining_parent_peer_ids.push(old.parent_peer_id.clone());
            } else if op.reason != DemandReason::DirectConvergence {
                let start = if self.nat_prediction_enabled {
                    0
                } else {
                    op.cursor + 1
                };
                for candidate in &op.candidates[start.min(op.candidates.len())..] {
                    if self.nat_prediction_enabled
                        && !self.candidate_opportunity_available(&op.child_peer_id, candidate)
                    {
                        continue;
                    }
                    if candidate.tuple.kind == UpstreamKind::Peer {
                        remaining_parent_peer_ids.push(candidate.tuple.parent_peer_id.clone());
                    }
                }
                remaining_parent_peer_ids.extend(op.deferred_parent_peer_ids.iter().cloned());
            }
            if !remaining_parent_peer_ids.is_empty()
                && self.allows_incoming(&op.child_peer_id, UpstreamKind::Peer)
            {
                self.direct_continuations.set(
                    op.child_peer_id.clone(),
                    DirectContinuation {
                        child_session_id: op.child_session_id.clone(),
                        sfu_connection_id: att.connection_id.clone(),
                        publication_generation: generation,
                        parent_peer_ids: dedupe_strings(remaining_parent_peer_ids),
                    },
                );
            } else {
                self.direct_continuations.delete(&op.child_peer_id);
            }
        }
        if self.room_media && !op.video_generation.is_empty() {
            let edge = self.edge_mut(&op.child_peer_id).unwrap();
            edge.video_generation = op.video_generation.clone();
            edge.video_connection_id = att.connection_id.clone();
            edge.video_ready = true;
        }
        let mut producer_peer_ids = Vec::new();
        if let Some(old) = old_peer {
            producer_peer_ids.push(old.parent_peer_id.clone());
        }
        if att.tuple.kind == UpstreamKind::Peer {
            producer_peer_ids.push(att.tuple.parent_peer_id.clone());
        } else {
            producer_peer_ids.push(self.host_peer_id.clone());
        }
        for producer_peer_id in dedupe_strings(producer_peer_ids) {
            for child_peer_id in self.children_of(&producer_peer_id) {
                self.sender_quality_observations.remove(&child_peer_id);
                self.sender_quality_baselines.insert(child_peer_id, true);
            }
        }
        self.require_sender_quality_baseline(&op.child_peer_id, true);
        let baseline = if same_parent_regeneration {
            false
        } else {
            op.reason == DemandReason::QualityConvergence
                || op.reason == DemandReason::RootConvergence
        };
        self.sender_quality_baselines
            .insert(op.child_peer_id.clone(), baseline);
        if same_parent_regeneration
            && att.tuple.kind == UpstreamKind::Peer
            && !att.parent_session_id.is_empty()
            && !att.sender_quality_identity.is_empty()
        {
            self.sender_quality_regeneration_blocks.insert(
                op.child_peer_id.clone(),
                SenderQualityIdentity {
                    child_session_id: op.child_session_id.clone(),
                    parent_peer_id: att.tuple.parent_peer_id.clone(),
                    parent_session_id: att.parent_session_id.clone(),
                    connection_id: att.connection_id.clone(),
                    sender_identity: att.sender_quality_identity.clone(),
                },
            );
        } else {
            self.sender_quality_regeneration_blocks
                .remove(&op.child_peer_id);
        }
        self.revision = att.revision;
        self.operation = None;
        self.prune_retiring_sfu_anchors();
        self.touch_facts();
        if creates_host_root {
            self.stage_root_convergence(&op.child_peer_id);
        }
        if let Some(current) = self.participant_mut(&op.child_peer_id) {
            current.availability_exhausted = false;
            current.bootstrap_failure_reported = false;
        }
        if old
            .as_ref()
            .is_some_and(|old| old.kind == UpstreamKind::Sfu)
            && !self.has_sfu_subscribers()
            && self.host_publication.is_some()
        {
            self.host_publication = None;
        }
        if op.reason == DemandReason::SfuBootstrap && att.tuple.kind == UpstreamKind::Sfu {
            if let Some(demand) = self.participant_mut(&op.demand_peer_id)
                && demand.session_is(&op.demand_session_id)
            {
                demand.sfu_first_at_next_route = true;
            }
            self.sfu_bootstrap_intent = None;
        } else if self.usable_route(&op.demand_peer_id) {
            self.clear_sfu_bootstrap_for_demand(&op.demand_peer_id);
        }
        self.assert_graph();
        route_debug!(self, "route-committed",
            "child" => self.debug_peer(&op.child_peer_id),
            "route" => self.debug_tuple(&att.tuple),
            "revision" => att.revision);
        let mut after = ResourceSet::default();
        after.add(self.committed_resources());
        let mut released: Vec<ResourceRef> = before
            .iter()
            .filter(|resource| !after.has(resource))
            .cloned()
            .collect();
        released.extend(att.reservation.overlap.clone());
        released
    }

    /// Advances the active revision past the live attempt; quality trials
    /// first rebaseline the producer's other children.
    pub(crate) fn advance_active_revision(&mut self) {
        let op = self.op();
        if op.reason == DemandReason::QualityConvergence
            && let Some(current) = &op.current
        {
            let producer_peer_id = if current.tuple.kind == UpstreamKind::Peer {
                current.tuple.parent_peer_id.clone()
            } else {
                self.host_peer_id.clone()
            };
            let regenerating_child =
                current.tuple.kind == UpstreamKind::Peer && current.tuple.regenerate;
            let child = op.child_peer_id.clone();
            for child_peer_id in self.children_of(&producer_peer_id) {
                if regenerating_child && child_peer_id == child {
                    // A failed same-edge trial must not turn the retained
                    // sender into a global baseline block. Its exact
                    // candidate opportunity owns retry damping while the
                    // retained sender identity remains current.
                    continue;
                }
                self.sender_quality_observations.remove(&child_peer_id);
                self.sender_quality_baselines.insert(child_peer_id, true);
            }
        }
        self.revision = self.allocate_revision();
        let revision = self.revision;
        self.op_mut().base_revision = revision;
    }
}
