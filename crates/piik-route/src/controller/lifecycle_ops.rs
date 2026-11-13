//! Graph-facing mutations: edge and publication invalidation, direct
//! connection adoption, pause, dispose and shared operation bookkeeping.

use super::*;

impl Controller {
    /// Marks the exactly identified edge unusable and records an
    /// `edge-unavailable` demand; true when the guard matched (idempotent).
    pub fn invalidate_edge(&mut self, guard: EdgeGuard, now_ms: Option<i64>) -> bool {
        let child_matches = self
            .participant(&guard.child_peer_id)
            .is_some_and(|child| child.session_is(&guard.child_session_id));
        let Some(edge) = self.edge(&guard.child_peer_id).cloned() else {
            return false;
        };
        if !child_matches
            || self.revision != guard.route_revision
            || edge.connection_id != guard.connection_id
            || edge.child_session_id != guard.child_session_id
            || (edge.kind == UpstreamKind::Peer
                && edge.parent_session_id != guard.parent_session_id)
        {
            return false;
        }
        if edge.usable {
            self.consume_active_edge_opportunity(&guard.child_peer_id, &edge);
            self.clear_quality_for_participant(&guard.child_peer_id);
            self.edge_mut(&guard.child_peer_id).unwrap().usable = false;
            self.direct_continuations.delete(&guard.child_peer_id);
            let child = self.participant_mut(&guard.child_peer_id).unwrap();
            child.availability_exhausted = false;
            child.bootstrap_failure_reported = false;
            self.touch_facts();
            if let Some(now) = now_ms {
                self.record_demand(&guard.child_peer_id, now, DemandReason::EdgeUnavailable);
            }
        }
        true
    }

    /// Invalidates the first committed direct edge (insertion order) matching
    /// the parent-side guard.
    pub fn invalidate_direct_edge_from_parent(
        &mut self,
        input: ParentEdgeGuard,
        now_ms: Option<i64>,
    ) -> bool {
        // First match in insertion order wins.
        let found = self
            .upstream_by_viewer
            .iter()
            .find_map(|(child_peer_id, edge)| {
                (edge.kind == UpstreamKind::Peer
                    && edge.parent_peer_id == input.parent_peer_id
                    && edge.parent_session_id == input.parent_session_id
                    && edge.connection_id == input.connection_id)
                    .then(|| (child_peer_id.clone(), edge.child_session_id.clone()))
            });
        let Some((child_peer_id, child_session_id)) = found else {
            return false;
        };
        self.invalidate_edge(
            EdgeGuard {
                child_peer_id,
                child_session_id,
                parent_session_id: input.parent_session_id,
                route_revision: input.route_revision,
                connection_id: input.connection_id,
            },
            now_ms,
        )
    }

    /// Marks the identified publication unusable and records demand for every
    /// SFU child; true when the guard matched (idempotent).
    pub fn invalidate_host_publication(
        &mut self,
        guard: PublicationGuard,
        now_ms: Option<i64>,
    ) -> bool {
        let Some(publication) = &self.host_publication else {
            return false;
        };
        if self.revision != guard.route_revision
            || publication.host_session_id != guard.host_session_id
            || publication.generation != guard.generation
            || publication.connection_id != guard.connection_id
        {
            return false;
        }
        if publication.usable {
            self.clear_sfu_quality();
            self.host_publication.as_mut().unwrap().usable = false;
            self.direct_continuations.clear();
            self.sfu_bootstrap_intent = None;
            if let Some(now) = now_ms {
                let sfu_children: Vec<String> = self
                    .upstream_by_viewer
                    .iter()
                    .filter(|(_, edge)| edge.kind == UpstreamKind::Sfu)
                    .map(|(child, _)| child.clone())
                    .collect();
                for child_peer_id in sfu_children {
                    self.record_demand(&child_peer_id, now, DemandReason::EdgeUnavailable);
                }
            }
            self.touch_facts();
        }
        true
    }

    /// Accepts a replacement for the exact active edge. Recovery supersedes
    /// optional preparation for that child; required availability work,
    /// including capacity reduction, keeps priority.
    pub fn adopt_direct_connection(
        &mut self,
        input: AdoptDirectConnectionInput,
        now_ms: i64,
    ) -> SettleResult {
        let mut result = SettleResult {
            active_revision: self.revision,
            ..SettleResult::default()
        };
        let guard = &input.edge;
        let Some(edge) = self.edge(&guard.child_peer_id).cloned() else {
            return result;
        };
        if self.revision != guard.route_revision
            || edge.kind != UpstreamKind::Peer
            || edge.transport != Transport::Direct
            || !edge.usable
            || !edge.physical_active
            || edge.child_session_id != guard.child_session_id
            || edge.parent_session_id != guard.parent_session_id
            || edge.connection_id != guard.connection_id
            || input.new_connection_id.is_empty()
        {
            return result;
        }
        let mut active_connection_id = edge.connection_id.clone();
        if self.room_media {
            if !edge.video_ready
                || edge.video_generation != self.video_generation
                || self.video_generation.is_empty()
            {
                return result;
            }
            active_connection_id = edge.video_connection_id.clone();
        }
        if input.new_connection_id == active_connection_id {
            result.accepted = true;
            return result;
        }
        if let Some(op) = &self.operation
            && op.child_peer_id == guard.child_peer_id
        {
            if is_availability_operation(op.reason) {
                return result;
            }
            result.released = self.abort_operation(Some(now_ms), RejectionBucket::Aborted);
        }
        let child_peer_id = guard.child_peer_id.clone();
        self.clear_quality_for_participant(&child_peer_id);
        if self.room_media {
            self.edge_mut(&child_peer_id).unwrap().video_connection_id = input.new_connection_id;
        } else {
            let key = self.candidate_opportunity_base(
                &child_peer_id,
                &CandidatePlan::bare(CandidateTuple::bare(edge.kind, &edge.parent_peer_id)),
            );
            let edge = self.edge_mut(&child_peer_id).unwrap();
            edge.connection_id = input.new_connection_id;
            edge.opportunity_key = key;
        }
        if let Some(child) = self.participant_mut(&child_peer_id) {
            child.availability_exhausted = false;
        }
        self.touch_facts();
        result.accepted = true;
        result.active_revision = self.revision;
        result
    }

    /// Retires the physically active publication and its subscriptions when
    /// no attempt is live and the guard matches, returning the subscriptions
    /// then the publication resource; empty otherwise.
    pub fn retire_host_publication(&mut self, guard: PublicationGuard) -> Vec<ResourceRef> {
        let live_attempt = self
            .operation
            .as_ref()
            .is_some_and(|op| op.current.is_some());
        let Some(publication) = self.host_publication.clone() else {
            return Vec::new();
        };
        if live_attempt
            || !publication.physical_active
            || self.revision != guard.route_revision
            || publication.host_session_id != guard.host_session_id
            || publication.generation != guard.generation
            || publication.connection_id != guard.connection_id
        {
            return Vec::new();
        }
        // Subscriptions in map order, then the publication.
        let mut released = Vec::new();
        for key in self.upstream_by_viewer.keys() {
            let Some(edge) = self.edge(&key) else {
                continue;
            };
            if edge.kind == UpstreamKind::Sfu
                && edge.publication_generation == publication.generation
            {
                let retain = self.retains_anchor(&key);
                self.retire_sfu_edge(&key, &mut released, retain);
            }
        }
        self.host_publication = None;
        self.clear_sfu_quality();
        self.revision = self.allocate_revision();
        if let Some(op) = self.operation.as_mut() {
            op.base_revision = self.revision;
        }
        self.touch_facts();
        released.extend(publication.resource);
        self.prune_retiring_sfu_anchors();
        released
    }

    /// Pauses (aborting the operation and clearing quality state) or resumes;
    /// returns the aborted reservation's resources.
    pub fn set_paused(&mut self, paused: bool, now_ms: Option<i64>) -> Vec<ResourceRef> {
        if self.room_media {
            let generation = self.video_generation.clone();
            return self.set_video_publication(&generation, paused, now_ms.unwrap_or(0));
        }
        if self.paused == paused {
            return Vec::new();
        }
        self.paused = paused;
        if paused {
            self.clear_all_quality();
        }
        self.touch_facts();
        if paused {
            return self.abort_operation(now_ms, RejectionBucket::Aborted);
        }
        Vec::new()
    }

    /// Clears all state, pauses the controller and returns the live
    /// reservation's resources then every committed resource, deduplicated.
    pub fn dispose(&mut self) -> Vec<ResourceRef> {
        let mut resources = ResourceSet::default();
        if let Some(current) = self.operation.as_ref().and_then(|op| op.current.as_ref()) {
            resources.add(reservation_resources(&current.reservation));
        }
        resources.add(self.committed_resources());
        self.operation = None;
        self.upstream_by_viewer.clear();
        self.clear_all_quality();
        self.host_publication = None;
        self.participants.clear();
        self.route_timings.clear();
        self.direct_continuations.clear();
        self.retiring_publication_generations.clear();
        self.sfu_bootstrap_intent = None;
        self.consumed_sfu_bootstrap_opportunities.clear();
        self.root_convergence_root_peer_id.clear();
        // next_join_order, revision, the room's revision sequence and
        // fact_version survive.
        self.paused = true;
        resources.into_vec()
    }

    /// Clears every receiver/sender quality ledger.
    pub(crate) fn clear_all_quality(&mut self) {
        self.quality_observations.clear();
        self.quality_baselines_pending.clear();
        self.sender_quality_observations.clear();
        self.sender_quality_baselines.clear();
        self.sender_quality_regeneration_blocks.clear();
        self.sfu_publisher_quality_observation = None;
    }

    /// Finishes demand timing when `now_ms` is set, releases the live
    /// reservation and clears the operation.
    pub(crate) fn abort_operation(
        &mut self,
        now_ms: Option<i64>,
        bucket: RejectionBucket,
    ) -> Vec<ResourceRef> {
        let Some(demand_peer_id) = self.operation.as_ref().map(|op| op.demand_peer_id.clone())
        else {
            return Vec::new();
        };
        if let Some(now) = now_ms {
            let final_route = self.current_final_route(&demand_peer_id);
            self.finish_timing(&demand_peer_id, now, final_route, bucket, false);
        }
        let mut released = Vec::new();
        if let Some(current) = self.op().current.as_ref() {
            released = reservation_resources(&current.reservation);
            self.advance_active_revision();
        }
        self.operation = None;
        released
    }

    pub(crate) fn operation_uses_participant_session(&self, peer_id: &str) -> bool {
        let Some(op) = &self.operation else {
            return false;
        };
        if op.child_peer_id == peer_id || op.demand_peer_id == peer_id {
            return true;
        }
        match op.current_tuple() {
            Some(tuple) if tuple.kind == UpstreamKind::Peer => tuple.parent_peer_id == peer_id,
            _ => peer_id == self.host_peer_id,
        }
    }

    /// Panics `Effective capacity is invalid` for a negative or unsafe value
    /// and clamps to the endpoint capacity.
    pub(crate) fn effective_capacity(&self, value: i64, volunteer: bool) -> i64 {
        if !is_safe_integer(value) || value < 0 {
            panic!("Effective capacity is invalid");
        }
        let limit = if volunteer {
            MAX_RELAY_VOLUNTEER_CHILDREN
        } else {
            self.endpoint_media_copy_capacity
        };
        value.min(limit)
    }

    pub(crate) fn touch_facts(&mut self) {
        self.fact_version += 1;
    }

    /// Bounds revisions by the wire revision limit.
    pub(crate) fn allocate_revision(&self) -> i64 {
        let latest = self.revisions.latest();
        if latest >= MAX_MEDIA_ROUTE_REVISION {
            panic!("Media route revision space exhausted");
        }
        self.revisions.set(latest + 1);
        latest + 1
    }
}
