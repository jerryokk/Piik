//! Quality convergence scheduling: candidate building, target selection and
//! operation eligibility.

use super::*;

impl Controller {
    /// Orders clear peers, remaining peers, SFU (only under Host fanout
    /// relief), same-parent regeneration, filtered by the per-observation
    /// consumed ledger.
    pub(crate) fn build_quality_candidates(
        &mut self,
        child_peer_id: &str,
        now_ms: i64,
    ) -> Vec<CandidatePlan> {
        let Some(current) = self
            .edge(child_peer_id)
            .filter(|edge| edge.kind == UpstreamKind::Peer)
            .cloned()
        else {
            return Vec::new();
        };
        let current_key = edge_tuple_key(&current);
        let publication_healthy = self.host_publication.as_ref().is_some_and(|publication| {
            self.sfu_publisher_quality_state(&publication.generation, now_ms)
                == SenderQualityState::Healthy
        });
        let candidates: Vec<CandidatePlan> = self
            .build_candidate_plans(child_peer_id, false, false)
            .into_iter()
            .filter(|candidate| {
                candidate.endpoint_transition.kind != TransitionKind::BoundedGap
                    && tuple_key(&candidate.tuple) != current_key
            })
            .filter(|candidate| {
                candidate.tuple.kind == UpstreamKind::Peer
                    || candidate.tuple.publication != Some(PublicationPlan::Reuse)
                    || publication_healthy
            })
            .collect();
        let mut clear_peers = Vec::new();
        let mut remaining_peers = Vec::new();
        let mut sfu = Vec::new();
        for candidate in candidates {
            if candidate.tuple.kind != UpstreamKind::Peer {
                sfu.push(candidate);
            } else if self.quality_source_path_healthy(&candidate.tuple.parent_peer_id, now_ms) {
                clear_peers.push(candidate);
            } else {
                remaining_peers.push(candidate);
            }
        }
        if !(current.parent_peer_id == self.host_peer_id
            && self.host_fanout_needs_sfu_relief(now_ms))
        {
            sfu.clear();
        }
        let mut regeneration = Vec::new();
        if !self.sender_quality_regeneration_blocked(child_peer_id) {
            let mut tuple = CandidateTuple::peer(&current.parent_peer_id);
            tuple.regenerate = true;
            if let Some(plan) = self.plan_candidate(child_peer_id, &tuple)
                && plan.endpoint_transition.kind != TransitionKind::BoundedGap
                && self.candidate_valid(child_peer_id, &plan, None)
            {
                regeneration.push(plan);
            }
        }
        clear_peers
            .into_iter()
            .chain(remaining_peers)
            .chain(sfu)
            .chain(regeneration)
            .filter(|candidate| {
                self.quality_candidate_opportunity_available(child_peer_id, candidate)
            })
            .collect()
    }

    pub(crate) fn select_quality_child(&mut self, now_ms: i64) -> String {
        if !self.quality_convergence_enabled {
            return String::new();
        }
        let mut targets = Vec::new();
        for viewer in self.available_viewers(false) {
            let Some(edge) = self
                .edge(&viewer)
                .filter(|edge| edge.kind == UpstreamKind::Peer)
                .cloned()
            else {
                continue;
            };
            if self.sender_quality_baselines.get(&viewer) != Some(&true)
                && self.sender_quality_persistently_degraded(&viewer, &edge, now_ms)
                && !self.build_quality_candidates(&viewer, now_ms).is_empty()
            {
                targets.push(viewer);
            }
        }
        targets.sort_by(|left, right| {
            self.depth(left).cmp(&self.depth(right)).then_with(|| {
                compare_participant(
                    self.participant(left).unwrap(),
                    self.participant(right).unwrap(),
                )
            })
        });
        targets.into_iter().next().unwrap_or_default()
    }

    pub(super) fn sender_quality_persistently_degraded(
        &self,
        child_peer_id: &str,
        edge: &CommittedEdge,
        now_ms: i64,
    ) -> bool {
        self.sender_quality_state(child_peer_id, edge, now_ms) == SenderQualityState::Degraded
            && self
                .sender_quality_observations
                .get(child_peer_id)
                .is_some_and(|observation| {
                    observation.consecutive_degraded_windows >= PERSISTENT_DEGRADED_WINDOWS
                })
    }

    pub(crate) fn quality_operation_still_eligible(&self, op: &Operation, now_ms: i64) -> bool {
        let Some(current_edge) = self
            .edge(&op.child_peer_id)
            .filter(|edge| edge.kind == UpstreamKind::Peer)
        else {
            return false;
        };
        let Some(tuple) = op.current_tuple() else {
            return false;
        };
        if self.current_quality_operation_observation(op).is_none() {
            return false;
        }
        let state = self.sender_quality_state(&op.child_peer_id, current_edge, now_ms);
        if state == SenderQualityState::Healthy {
            return false;
        }
        tuple.kind == UpstreamKind::Peer
            || (state == SenderQualityState::Degraded && self.host_fanout_needs_sfu_relief(now_ms))
    }

    pub(super) fn host_fanout_needs_sfu_relief(&self, now_ms: i64) -> bool {
        let mut count = 0;
        for (child_peer_id, edge) in self.upstream_by_viewer.iter() {
            if edge.kind != UpstreamKind::Peer
                || edge.parent_peer_id != self.host_peer_id
                || !edge.usable
                || !edge.physical_active
            {
                continue;
            }
            count += 1;
            if !self.sender_quality_persistently_degraded(child_peer_id, edge, now_ms) {
                return false;
            }
        }
        count >= 2
    }
}
