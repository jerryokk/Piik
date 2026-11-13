//! Receiver and native sender quality evidence: validation, aggregation and
//! folding a sample into the live candidate.

use super::*;

impl Controller {
    /// Aggregates one receiver-side window onto the child's committed edge:
    /// rejected on any mismatch, accepted when consumed without aggregation,
    /// observed when aggregated.
    pub fn observe_quality_evidence(
        &mut self,
        input: RouteQualityEvidenceInput,
    ) -> RouteQualityEvidenceResult {
        if self.paused
            || !self.video_work_enabled()
            || input.route_revision != self.revision
            || !is_safe_integer(input.presentation_epoch)
            || input.presentation_epoch < 0
            || !is_safe_integer(input.window_ms)
            || input.window_ms <= 0
            || !is_safe_integer(input.accepted_at_ms)
            || input.accepted_at_ms < 0
        {
            return RouteQualityEvidenceResult::Rejected;
        }
        let child_ok = self
            .participant(&input.child_peer_id)
            .is_some_and(|child| child.session_is(&input.child_session_id));
        let edge_ok = self.edge(&input.child_peer_id).is_some_and(|edge| {
            edge.usable
                && edge.physical_active
                && edge.child_session_id == input.child_session_id
                && self.quality_connection_id(edge) == input.connection_id
                && quality_upstream_matches(edge, &input.upstream)
                && self.source_usable_for_quality(edge)
        });
        if !child_ok || !edge_ok {
            return RouteQualityEvidenceResult::Rejected;
        }
        let edge_kind = self.edge(&input.child_peer_id).unwrap().kind;
        let upstream_peer_id = if input.upstream.kind == UpstreamKind::Peer {
            input.upstream.peer_id.clone()
        } else {
            String::new()
        };
        let same_edge = self
            .quality_observations
            .get(&input.child_peer_id)
            .filter(|observation| {
                observation.child_session_id == input.child_session_id
                    && observation.upstream_kind == input.upstream.kind
                    && observation.upstream_peer_id == upstream_peer_id
                    && observation.connection_id == input.connection_id
            });
        if same_edge
            .is_some_and(|observation| input.presentation_epoch < observation.presentation_epoch)
        {
            return RouteQualityEvidenceResult::Rejected;
        }
        if same_edge
            .is_none_or(|observation| observation.presentation_epoch != input.presentation_epoch)
        {
            self.quality_observations.insert(
                input.child_peer_id.clone(),
                RouteQualityObservation {
                    child_session_id: input.child_session_id.clone(),
                    upstream_kind: input.upstream.kind,
                    upstream_peer_id,
                    connection_id: input.connection_id.clone(),
                    presentation_epoch: input.presentation_epoch,
                    eligible_windows: 0,
                    eligible_duration_ms: 0,
                    freeze_windows: 0,
                    freeze_count: 0,
                    freeze_duration_ms: 0,
                    pause_count: 0,
                    pause_duration_ms: 0,
                    last_accepted_at_ms: input.accepted_at_ms,
                    last_decoded_progress_at_ms: None,
                },
            );
        }

        let metrics = &input.metrics;
        if metrics
            .frames_decoded_delta
            .is_none_or(|frames| frames <= 0)
        {
            return RouteQualityEvidenceResult::Accepted;
        }
        let observation = self
            .quality_observations
            .get_mut(&input.child_peer_id)
            .unwrap();
        let had_fresh_decoded_progress = observation
            .last_decoded_progress_at_ms
            .is_some_and(|at| input.accepted_at_ms < at + QUALITY_EVIDENCE_EXPIRY_MS);
        observation.last_decoded_progress_at_ms = Some(input.accepted_at_ms);
        if !had_fresh_decoded_progress && edge_kind == UpstreamKind::Sfu {
            self.touch_facts();
        }
        let (Some(freeze_count), Some(freeze_duration), Some(pause_count), Some(pause_duration)) = (
            metrics.freeze_count_delta,
            metrics.freeze_duration_ms_delta,
            metrics.pause_count_delta,
            metrics.pause_duration_ms_delta,
        ) else {
            return RouteQualityEvidenceResult::Accepted;
        };
        let observation = self
            .quality_observations
            .get_mut(&input.child_peer_id)
            .unwrap();
        observation.last_accepted_at_ms = input.accepted_at_ms;
        if self.quality_baselines_pending.remove(&input.child_peer_id) {
            return RouteQualityEvidenceResult::Accepted;
        }
        observation.eligible_windows = safe_add(observation.eligible_windows, 1.0);
        observation.eligible_duration_ms =
            safe_add(observation.eligible_duration_ms, input.window_ms as f64);
        if freeze_count > 0 {
            observation.freeze_windows = safe_add(observation.freeze_windows, 1.0);
        }
        observation.freeze_count = safe_add(observation.freeze_count, freeze_count as f64);
        observation.freeze_duration_ms = safe_add(observation.freeze_duration_ms, freeze_duration);
        observation.pause_count = safe_add(observation.pause_count, pause_count as f64);
        observation.pause_duration_ms = safe_add(observation.pause_duration_ms, pause_duration);
        RouteQualityEvidenceResult::Observed
    }

    /// `accepted: true, committed: false`.
    pub(super) fn sender_accepted(
        &self,
        released: Vec<ResourceRef>,
    ) -> SenderQualityEvidenceResult {
        SettleResult {
            accepted: true,
            committed: false,
            failed_peer_ids: Vec::new(),
            active_revision: self.revision,
            released,
        }
    }

    pub(super) fn sender_rejected(&self) -> SenderQualityEvidenceResult {
        SettleResult {
            accepted: false,
            committed: false,
            failed_peer_ids: Vec::new(),
            active_revision: self.revision,
            released: Vec::new(),
        }
    }

    /// Records a parent's health sample for a child, settling the live
    /// quality/root candidate when the sample names it. Requires quality
    /// convergence.
    pub fn observe_sender_quality_evidence(
        &mut self,
        input: SenderQualityEvidenceInput,
        commit_reservation: CommitReservation<'_>,
    ) -> SenderQualityEvidenceResult {
        if !self.quality_convergence_enabled
            || self.paused
            || !self.video_work_enabled()
            || !is_safe_integer(input.accepted_at_ms)
            || input.accepted_at_ms < 0
        {
            return self.sender_rejected();
        }
        let (Some(parent), Some(child)) = (
            self.participant(&input.parent_peer_id),
            self.participant(&input.child_peer_id),
        ) else {
            return self.sender_rejected();
        };
        if parent.session_id.is_empty()
            || !parent.session_is(&input.parent_session_id)
            || child.session_id.is_empty()
            || child.departure_confirmed
            || parent.departure_confirmed
        {
            return self.sender_rejected();
        }
        let child_session_id = child.session_id.clone();

        let names_candidate = self.operation.as_ref().is_some_and(|op| {
            (op.reason == DemandReason::RootConvergence
                || op.reason == DemandReason::QualityConvergence)
                && op.current.as_ref().is_some_and(|att| {
                    att.tuple.kind == UpstreamKind::Peer
                        && op.child_peer_id == input.child_peer_id
                        && op.child_session_id == child_session_id
                        && att.tuple.parent_peer_id == input.parent_peer_id
                        && att.parent_session_id == input.parent_session_id
                        && att.connection_id == input.connection_id
                        && att.revision == input.route_revision
                })
        });
        if names_candidate {
            let sender_identity = input.sender_identity.clone().unwrap_or_default();
            return self.settle_candidate_quality(
                input.state,
                input.accepted_at_ms,
                sender_identity,
                input.sample_timestamp_ms,
                commit_reservation,
            );
        }

        let edge_ok = input.route_revision == self.revision
            && self.edge(&input.child_peer_id).is_some_and(|edge| {
                edge.kind == UpstreamKind::Peer
                    && edge.usable
                    && edge.physical_active
                    && edge.parent_peer_id == input.parent_peer_id
                    && edge.parent_session_id == input.parent_session_id
                    && edge.child_session_id == child_session_id
                    && self.quality_connection_id(edge) == input.connection_id
            });
        if !edge_ok {
            return self.sender_rejected();
        }
        let previous = self
            .sender_quality_observations
            .remove(&input.child_peer_id);
        let same_connection_identity = previous.as_ref().is_some_and(|previous| {
            previous.child_session_id == child_session_id
                && previous.parent_peer_id == input.parent_peer_id
                && previous.parent_session_id == input.parent_session_id
                && previous.connection_id == input.connection_id
        });
        if input.state == SenderQualityState::Unknown {
            if previous
                .as_ref()
                .is_some_and(|previous| previous.state != SenderQualityState::Unknown)
            {
                route_debug!(self, "sender-quality-reset",
                    "parent" => self.debug_peer(&input.parent_peer_id),
                    "child" => self.debug_peer(&input.child_peer_id));
            }
            if let Some(mut reset) = previous.filter(|_| same_connection_identity) {
                // The reset keeps the same consumed ledger.
                reset.state = SenderQualityState::Unknown;
                reset.consecutive_degraded_windows = 0;
                reset.last_accepted_at_ms = input.accepted_at_ms;
                self.sender_quality_observations
                    .insert(input.child_peer_id.clone(), reset);
            }
            return self.sender_accepted(Vec::new());
        }
        let (Some(sender_identity), Some(sample_timestamp_ms)) = (
            input
                .sender_identity
                .clone()
                .filter(|identity| !identity.is_empty()),
            input.sample_timestamp_ms,
        ) else {
            if let Some(previous) = previous {
                self.sender_quality_observations
                    .insert(input.child_peer_id.clone(), previous);
            }
            return self.sender_rejected();
        };
        let same_sender_identity = same_connection_identity
            && previous
                .as_ref()
                .is_some_and(|previous| previous.sender_identity == sender_identity);
        if same_sender_identity
            && sample_timestamp_ms <= previous.as_ref().unwrap().last_sample_timestamp_ms
        {
            self.sender_quality_observations
                .insert(input.child_peer_id.clone(), previous.unwrap());
            return self.sender_accepted(Vec::new());
        }
        if self
            .sender_quality_regeneration_blocks
            .get(&input.child_peer_id)
            .is_some_and(|block| {
                !sender_quality_regeneration_block_matches(
                    block,
                    &child_session_id,
                    &input,
                    &sender_identity,
                )
            })
        {
            // A new connection or sender identity is a new generation. Do not
            // carry a same-edge regeneration latch across that identity fence.
            self.sender_quality_regeneration_blocks
                .remove(&input.child_peer_id);
        }
        if input.state == SenderQualityState::Healthy {
            self.sender_quality_regeneration_blocks
                .remove(&input.child_peer_id);
        }
        let same_fresh_sender_identity = same_sender_identity
            && input.accepted_at_ms
                < previous.as_ref().unwrap().last_accepted_at_ms + QUALITY_EVIDENCE_EXPIRY_MS;
        if input.state == SenderQualityState::Healthy {
            self.sender_quality_baselines.remove(&input.child_peer_id);
        }
        let mut consecutive_degraded_windows = 0;
        if input.state == SenderQualityState::Degraded {
            consecutive_degraded_windows = match &previous {
                Some(previous)
                    if same_fresh_sender_identity
                        && previous.state == SenderQualityState::Degraded =>
                {
                    safe_add(previous.consecutive_degraded_windows, 1.0)
                }
                _ => 1,
            };
        }
        let previous_state = previous.as_ref().map(|previous| previous.state);
        let consumed = match previous {
            Some(previous)
                if same_sender_identity && input.state != SenderQualityState::Healthy =>
            {
                previous.consumed_quality_candidates
            }
            _ => Map::new(),
        };
        self.sender_quality_observations.insert(
            input.child_peer_id.clone(),
            SenderQualityObservation {
                child_session_id,
                parent_peer_id: input.parent_peer_id.clone(),
                parent_session_id: input.parent_session_id.clone(),
                connection_id: input.connection_id.clone(),
                sender_identity,
                state: input.state,
                consecutive_degraded_windows,
                last_sample_timestamp_ms: sample_timestamp_ms,
                last_accepted_at_ms: input.accepted_at_ms,
                consumed_quality_candidates: consumed,
            },
        );
        if !same_fresh_sender_identity
            || previous_state != Some(input.state)
            || consecutive_degraded_windows == PERSISTENT_DEGRADED_WINDOWS
        {
            route_debug!(self, "sender-quality-observed",
                "parent" => self.debug_peer(&input.parent_peer_id),
                "child" => self.debug_peer(&input.child_peer_id),
                "state" => input.state,
                "consecutiveDegradedWindows" => consecutive_degraded_windows);
        }
        if input.state == SenderQualityState::Healthy
            && self.operation.as_ref().is_some_and(|op| {
                op.reason == DemandReason::QualityConvergence
                    && op.child_peer_id == input.child_peer_id
            })
        {
            let released =
                self.abort_operation(Some(input.accepted_at_ms), RejectionBucket::Aborted);
            return self.sender_accepted(released);
        }
        self.sender_accepted(Vec::new())
    }

    /// Records a Host-to-SFU ingress sample, settling a live SFU reuse
    /// quality candidate. Requires quality convergence. Unlike the other two
    /// observers it validates no numbers.
    pub fn observe_sfu_publisher_quality_evidence(
        &mut self,
        input: SfuPublisherQualityEvidenceInput,
        commit_reservation: CommitReservation<'_>,
    ) -> SenderQualityEvidenceResult {
        let host_ok = self.participant(&self.host_peer_id).is_some_and(|host| {
            !host.session_id.is_empty() && host.session_is(&input.host_session_id)
        });
        if !self.quality_convergence_enabled
            || self.paused
            || !self.video_work_enabled()
            || input.host_peer_id != self.host_peer_id
            || !host_ok
        {
            return self.sender_rejected();
        }
        let names_candidate = self.operation.as_ref().is_some_and(|op| {
            op.reason == DemandReason::QualityConvergence
                && op.current.as_ref().is_some_and(|att| {
                    att.tuple.kind == UpstreamKind::Sfu
                        && att.host_session_id == input.host_session_id
                        && att.publication_generation == input.publication_generation
                        && att.revision == input.route_revision
                })
        });
        if names_candidate {
            return self.settle_candidate_quality(
                input.state,
                input.accepted_at_ms,
                input.publication_generation.clone(),
                input.sample_timestamp_ms,
                commit_reservation,
            );
        }
        let publication_ok = input.route_revision == self.revision
            && self.host_publication.as_ref().is_some_and(|publication| {
                publication.usable
                    && publication.physical_active
                    && publication.host_session_id == input.host_session_id
                    && publication.generation == input.publication_generation
            });
        if !publication_ok {
            return self.sender_rejected();
        }
        if input.state == SenderQualityState::Unknown {
            if self.sfu_publisher_quality_observation.is_some() {
                route_debug!(self, "sfu-publisher-quality-reset");
            }
            self.sfu_publisher_quality_observation = None;
            return self.sender_accepted(Vec::new());
        }
        let Some(sample_timestamp_ms) = input.sample_timestamp_ms else {
            return self.sender_rejected();
        };
        let previous = self.sfu_publisher_quality_observation.clone();
        let same_publication = previous.as_ref().is_some_and(|previous| {
            previous.host_session_id == input.host_session_id
                && previous.publication_generation == input.publication_generation
        });
        if same_publication
            && sample_timestamp_ms <= previous.as_ref().unwrap().last_sample_timestamp_ms
        {
            return self.sender_accepted(Vec::new());
        }
        let same_fresh_publication = same_publication
            && input.accepted_at_ms
                < previous.as_ref().unwrap().last_accepted_at_ms + QUALITY_EVIDENCE_EXPIRY_MS;
        let mut consecutive_healthy_windows = 0;
        if input.state == SenderQualityState::Healthy {
            consecutive_healthy_windows = match &previous {
                Some(previous)
                    if same_fresh_publication && previous.state == SenderQualityState::Healthy =>
                {
                    safe_add(previous.consecutive_healthy_windows, 1.0)
                }
                _ => 1,
            };
        }
        let was_persistently_healthy = same_fresh_publication
            && previous.as_ref().is_some_and(|previous| {
                previous.state == SenderQualityState::Healthy
                    && previous.consecutive_healthy_windows >= PERSISTENT_DEGRADED_WINDOWS
            });
        self.sfu_publisher_quality_observation = Some(SfuPublisherQualityObservation {
            host_session_id: input.host_session_id.clone(),
            publication_generation: input.publication_generation.clone(),
            state: input.state,
            consecutive_healthy_windows,
            last_sample_timestamp_ms: sample_timestamp_ms,
            last_accepted_at_ms: input.accepted_at_ms,
        });
        if !same_publication
            || previous
                .as_ref()
                .is_some_and(|previous| previous.state != input.state)
            || consecutive_healthy_windows == PERSISTENT_DEGRADED_WINDOWS
        {
            route_debug!(self, "sfu-publisher-quality-observed",
                "state" => input.state,
                "consecutiveHealthyWindows" => consecutive_healthy_windows);
        }
        if input.state == SenderQualityState::Healthy
            && consecutive_healthy_windows >= PERSISTENT_DEGRADED_WINDOWS
            && !was_persistently_healthy
        {
            self.touch_facts();
        }
        let settle = self.operation.as_ref().and_then(|op| {
            let att = op.current.as_ref()?;
            (op.reason == DemandReason::QualityConvergence
                && att.tuple.kind == UpstreamKind::Sfu
                && att.tuple.publication == Some(PublicationPlan::Reuse)
                && att.host_session_id == input.host_session_id
                && att.publication_generation == input.publication_generation
                && att.media_ready)
                .then(|| {
                    (
                        CandidateGuard {
                            child_peer_id: op.child_peer_id.clone(),
                            child_session_id: op.child_session_id.clone(),
                            revision: att.revision,
                            connection_id: att.connection_id.clone(),
                        },
                        op.video_generation.clone(),
                    )
                })
        });
        if let Some((guard, video_generation)) = settle {
            if input.state == SenderQualityState::Degraded {
                return self.candidate_failed(&guard, input.accepted_at_ms);
            }
            if consecutive_healthy_windows >= PERSISTENT_DEGRADED_WINDOWS {
                return self.candidate_ready(
                    &guard,
                    input.accepted_at_ms,
                    commit_reservation,
                    CandidateProof {
                        relative_quality_approved: false,
                        video_generation,
                    },
                );
            }
        }
        self.sender_accepted(Vec::new())
    }

    /// Folds a sender sample into the live candidate and fails or commits it.
    /// An empty `sender_identity` means unknown.
    fn settle_candidate_quality(
        &mut self,
        state: SenderQualityState,
        accepted_at_ms: i64,
        sender_identity: String,
        sample_timestamp_ms: Option<i64>,
        commit_reservation: CommitReservation<'_>,
    ) -> SenderQualityEvidenceResult {
        let op = self.op();
        let att = op.current.as_ref().unwrap();
        if att.sender_quality_state != Some(state) {
            route_debug!(self, "candidate-quality-observed",
                "child" => self.debug_peer(&op.child_peer_id),
                "candidate" => self.debug_tuple(&att.tuple),
                "state" => state);
        }
        if state == SenderQualityState::Unknown {
            let att = self.op_mut().current.as_mut().unwrap();
            att.sender_quality_state = None;
            att.sender_quality_accepted_at_ms = None;
            att.sender_quality_consecutive_healthy_windows = 0;
            att.sender_quality_consecutive_degraded_windows = 0;
            att.sender_quality_sample_timestamp_ms = None;
            return self.sender_accepted(Vec::new());
        }
        let Some(sample_timestamp_ms) = sample_timestamp_ms.filter(|_| !sender_identity.is_empty())
        else {
            return self.sender_rejected();
        };
        let guard = CandidateGuard {
            child_peer_id: op.child_peer_id.clone(),
            child_session_id: op.child_session_id.clone(),
            revision: att.revision,
            connection_id: att.connection_id.clone(),
        };
        if !att.sender_quality_identity.is_empty() && att.sender_quality_identity != sender_identity
        {
            return self.candidate_failed(&guard, accepted_at_ms);
        }
        if att
            .sender_quality_sample_timestamp_ms
            .is_some_and(|previous| sample_timestamp_ms <= previous)
        {
            return self.sender_accepted(Vec::new());
        }
        let same_fresh_state = att
            .sender_quality_accepted_at_ms
            .is_some_and(|at| accepted_at_ms < at + QUALITY_EVIDENCE_EXPIRY_MS);
        let mut consecutive_healthy_windows = 0;
        if att.tuple.kind == UpstreamKind::Sfu && state == SenderQualityState::Healthy {
            consecutive_healthy_windows = if att.sender_quality_state
                == Some(SenderQualityState::Healthy)
                && same_fresh_state
            {
                safe_add(att.sender_quality_consecutive_healthy_windows, 1.0)
            } else {
                1
            };
        } else if state == SenderQualityState::Healthy {
            consecutive_healthy_windows = 1;
        }
        let mut consecutive_degraded_windows = 0;
        if state == SenderQualityState::Degraded {
            consecutive_degraded_windows = if att.sender_quality_state
                == Some(SenderQualityState::Degraded)
                && same_fresh_state
            {
                safe_add(att.sender_quality_consecutive_degraded_windows, 1.0)
            } else {
                1
            };
        }
        let reason = op.reason;
        let video_generation = op.video_generation.clone();
        let att = self.op_mut().current.as_mut().unwrap();
        att.sender_quality_state = Some(state);
        att.sender_quality_accepted_at_ms = Some(accepted_at_ms);
        att.sender_quality_consecutive_healthy_windows = consecutive_healthy_windows;
        att.sender_quality_consecutive_degraded_windows = consecutive_degraded_windows;
        att.sender_quality_sample_timestamp_ms = Some(sample_timestamp_ms);
        att.sender_quality_identity = sender_identity;
        let kind = att.tuple.kind;
        let media_ready = att.media_ready;
        let persistent_quality_peer_degradation = reason == DemandReason::QualityConvergence
            && kind == UpstreamKind::Peer
            && consecutive_degraded_windows >= PERSISTENT_DEGRADED_WINDOWS;
        let settled = if state == SenderQualityState::Degraded
            && (kind != UpstreamKind::Peer
                || reason != DemandReason::QualityConvergence
                || persistent_quality_peer_degradation)
        {
            self.candidate_failed(&guard, accepted_at_ms)
        } else if media_ready
            && (kind != UpstreamKind::Sfu
                || consecutive_healthy_windows >= PERSISTENT_DEGRADED_WINDOWS)
        {
            self.candidate_ready(
                &guard,
                accepted_at_ms,
                commit_reservation,
                CandidateProof {
                    relative_quality_approved: false,
                    video_generation,
                },
            )
        } else {
            return self.sender_accepted(Vec::new());
        };
        SettleResult {
            accepted: true,
            ..settled
        }
    }
}
