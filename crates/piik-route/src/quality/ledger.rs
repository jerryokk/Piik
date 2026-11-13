//! The quality ledgers behind convergence: sender observations, baselines,
//! regeneration latches and per-participant clearing.

use super::*;

impl Controller {
    /// Drops the parent's sender observations, baselines and regeneration
    /// latches and aborts an operation whose live candidate that parent owns;
    /// empty unless the parent's session matches.
    pub fn reset_sender_quality(
        &mut self,
        parent_peer_id: &str,
        parent_session_id: &str,
        now_ms: i64,
    ) -> Vec<ResourceRef> {
        if self
            .participant(parent_peer_id)
            .is_none_or(|parent| !parent.session_is(parent_session_id))
        {
            return Vec::new();
        }
        // Membership only: every affected child is deleted.
        let mut children: HashSet<String> = self.children_of(parent_peer_id).into_iter().collect();
        for (child_peer_id, observation) in &self.sender_quality_observations {
            if observation.parent_peer_id == parent_peer_id
                && observation.parent_session_id == parent_session_id
            {
                children.insert(child_peer_id.clone());
            }
        }
        for child_peer_id in children {
            self.sender_quality_observations.remove(&child_peer_id);
            // Reset means the next exact sender sequence is a new observation
            // window. A quality/root commit still installs the explicit rearm
            // latch; a visibility or source reset must not deadlock a
            // persistently limited edge waiting for a healthy sample.
            self.sender_quality_baselines.remove(&child_peer_id);
            self.sender_quality_regeneration_blocks
                .remove(&child_peer_id);
        }
        if parent_peer_id == self.host_peer_id {
            self.sfu_publisher_quality_observation = None;
        }
        let owns_candidate = self.operation.as_ref().is_some_and(|op| {
            op.current.as_ref().is_some_and(|att| {
                if att.tuple.kind == UpstreamKind::Peer {
                    (op.reason == DemandReason::RootConvergence
                        || op.reason == DemandReason::QualityConvergence)
                        && att.tuple.parent_peer_id == parent_peer_id
                        && att.parent_session_id == parent_session_id
                } else {
                    operation_requires_native_candidate_proof(Some(op))
                        && parent_peer_id == self.host_peer_id
                        && att.host_session_id == parent_session_id
                }
            })
        });
        if owns_candidate {
            return self.abort_operation(Some(now_ms), RejectionBucket::Aborted);
        }
        Vec::new()
    }

    pub(super) fn sender_quality_state(
        &self,
        child_peer_id: &str,
        edge: &CommittedEdge,
        now_ms: i64,
    ) -> SenderQualityState {
        if edge.kind != UpstreamKind::Peer {
            return if self.source_usable_for_quality(edge) {
                SenderQualityState::Healthy
            } else {
                SenderQualityState::Unknown
            };
        }
        let Some(observation) = self.sender_quality_observations.get(child_peer_id) else {
            return SenderQualityState::Unknown;
        };
        let child_ok = self
            .participant(child_peer_id)
            .is_some_and(|child| child.session_is(&observation.child_session_id));
        if now_ms < observation.last_accepted_at_ms + QUALITY_EVIDENCE_EXPIRY_MS
            && child_ok
            && edge.child_session_id == observation.child_session_id
            && edge.parent_peer_id == observation.parent_peer_id
            && edge.parent_session_id == observation.parent_session_id
            && self.quality_connection_id(edge) == observation.connection_id
            && self.source_usable_for_quality(edge)
        {
            return observation.state;
        }
        SenderQualityState::Unknown
    }

    pub(crate) fn current_quality_operation_source(
        &self,
        child_peer_id: &str,
    ) -> Option<SenderQualityIdentity> {
        let child = self.participant(child_peer_id)?;
        let edge = self.edge(child_peer_id)?;
        let observation = self.sender_quality_observations.get(child_peer_id)?;
        if child.session_id.is_empty()
            || edge.kind != UpstreamKind::Peer
            || observation.child_session_id != child.session_id
            || observation.child_session_id != edge.child_session_id
            || observation.parent_peer_id != edge.parent_peer_id
            || observation.parent_session_id != edge.parent_session_id
            || observation.connection_id != self.quality_connection_id(edge)
        {
            return None;
        }
        Some(SenderQualityIdentity {
            child_session_id: observation.child_session_id.clone(),
            parent_peer_id: observation.parent_peer_id.clone(),
            parent_session_id: observation.parent_session_id.clone(),
            connection_id: observation.connection_id.clone(),
            sender_identity: observation.sender_identity.clone(),
        })
    }

    pub(crate) fn current_quality_operation_observation(
        &self,
        op: &Operation,
    ) -> Option<&SenderQualityObservation> {
        let source = op.quality_source.as_ref()?;
        let current = self.current_quality_operation_source(&op.child_peer_id)?;
        if *source != current {
            return None;
        }
        self.sender_quality_observations.get(&op.child_peer_id)
    }

    pub(super) fn sender_quality_regeneration_blocked(&mut self, child_peer_id: &str) -> bool {
        let Some(block) = self.sender_quality_regeneration_blocks.get(child_peer_id) else {
            return false;
        };
        if self
            .sender_quality_observations
            .get(child_peer_id)
            .is_some_and(|observation| observation.sender_identity != block.sender_identity)
        {
            self.sender_quality_regeneration_blocks
                .remove(child_peer_id);
            return false;
        }
        let edge = self.edge(child_peer_id).unwrap();
        if block.child_session_id != edge.child_session_id
            || block.parent_peer_id != edge.parent_peer_id
            || block.parent_session_id != edge.parent_session_id
            || block.connection_id != self.quality_connection_id(edge)
        {
            self.sender_quality_regeneration_blocks
                .remove(child_peer_id);
            return false;
        }
        true
    }

    pub(crate) fn sfu_publisher_quality_state(
        &self,
        publication_generation: &str,
        now_ms: i64,
    ) -> SenderQualityState {
        let Some(observation) = &self.sfu_publisher_quality_observation else {
            return SenderQualityState::Unknown;
        };
        if observation.publication_generation == publication_generation
            && self.host_publication.as_ref().is_some_and(|publication| {
                observation.host_session_id == publication.host_session_id
            })
            && now_ms < observation.last_accepted_at_ms + QUALITY_EVIDENCE_EXPIRY_MS
        {
            if observation.state == SenderQualityState::Healthy
                && observation.consecutive_healthy_windows < PERSISTENT_DEGRADED_WINDOWS
            {
                return SenderQualityState::Unknown;
            }
            return observation.state;
        }
        SenderQualityState::Unknown
    }

    pub(super) fn quality_source_path_healthy(&self, peer_id: &str, now_ms: i64) -> bool {
        if peer_id == self.host_peer_id {
            return true;
        }
        let mut seen = HashSet::new();
        let mut peer_id = peer_id.to_owned();
        loop {
            if !seen.insert(peer_id.clone()) {
                return false;
            }
            let Some(edge) = self
                .edge(&peer_id)
                .filter(|edge| edge.usable && edge.physical_active)
            else {
                return false;
            };
            if edge.kind == UpstreamKind::Sfu {
                return self.source_usable_for_quality(edge)
                    && self.decoded_progress_fresh(&peer_id, edge, now_ms)
                    && self.sfu_publisher_quality_state(&edge.publication_generation, now_ms)
                        == SenderQualityState::Healthy;
            }
            if self.sender_quality_state(&peer_id, edge, now_ms) != SenderQualityState::Healthy {
                return false;
            }
            if edge.parent_peer_id == self.host_peer_id {
                return true;
            }
            peer_id = edge.parent_peer_id.clone();
        }
    }

    pub(super) fn decoded_progress_fresh(
        &self,
        child_peer_id: &str,
        edge: &CommittedEdge,
        now_ms: i64,
    ) -> bool {
        let Some(observation) = self.quality_observations.get(child_peer_id) else {
            return false;
        };
        observation
            .last_decoded_progress_at_ms
            .is_some_and(|at| now_ms < at + QUALITY_EVIDENCE_EXPIRY_MS)
            && self
                .participant(child_peer_id)
                .is_some_and(|current| current.session_is(&observation.child_session_id))
            && edge.child_session_id == observation.child_session_id
            && self.quality_connection_id(edge) == observation.connection_id
            && quality_observation_matches_edge(observation, edge)
            && self.source_usable_for_quality(edge)
    }

    pub(crate) fn source_usable_for_quality(&self, edge: &CommittedEdge) -> bool {
        if edge.kind == UpstreamKind::Peer {
            return self
                .participant(&edge.parent_peer_id)
                .is_some_and(|parent| parent.session_is(&edge.parent_session_id))
                && self.source_usable(&edge.parent_peer_id);
        }
        self.publication_serves(edge)
    }

    /// Clears every child for the Host; a viewer clears its subtree plus itself.
    pub(crate) fn clear_quality_for_participant(&mut self, peer_id: &str) {
        let affected = if peer_id == self.host_peer_id {
            self.upstream_by_viewer.keys()
        } else {
            with_self(self.descendants_of(peer_id), peer_id)
        };
        for child_peer_id in affected {
            self.quality_observations.remove(&child_peer_id);
            self.sender_quality_observations.remove(&child_peer_id);
            self.sender_quality_baselines.remove(&child_peer_id);
            self.sender_quality_regeneration_blocks
                .remove(&child_peer_id);
            if self.has_edge(&child_peer_id) {
                self.quality_baselines_pending.insert(child_peer_id.clone());
                self.sender_quality_baselines.insert(child_peer_id, true);
            } else {
                self.quality_baselines_pending.remove(&child_peer_id);
                self.sender_quality_baselines.remove(&child_peer_id);
            }
        }
    }

    /// A missing baseline reads as false.
    pub(crate) fn require_sender_quality_baseline(&mut self, peer_id: &str, suppress_fact: bool) {
        for child_peer_id in with_self(self.descendants_of(peer_id), peer_id) {
            if self.has_edge(&child_peer_id) {
                self.sender_quality_observations.remove(&child_peer_id);
                let previous = self.sender_quality_baselines.get(&child_peer_id) == Some(&true);
                self.sender_quality_baselines
                    .insert(child_peer_id, suppress_fact || previous);
            }
        }
    }

    pub(crate) fn clear_sfu_quality(&mut self) {
        self.sfu_publisher_quality_observation = None;
        for child_peer_id in self.upstream_by_viewer.keys() {
            if self
                .edge(&child_peer_id)
                .is_some_and(|edge| edge.kind == UpstreamKind::Sfu)
            {
                self.clear_quality_for_participant(&child_peer_id);
            }
        }
    }
}
