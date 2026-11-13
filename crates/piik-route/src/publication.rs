//! Room-media video publication attachment over retained room edges.

use crate::controller::{Attempt, Operation, reservation_resources};
use crate::operation::Validation;
use crate::{
    BeginInput, BeginResult, CandidateGuard, CandidatePlan, CandidateReservation, CandidateTuple,
    CommittedEdge, Controller, DemandReason, EndpointTransition, PublicationPlan, RejectionBucket,
    ReservationKind, ResourceRef, TransitionKind, UpstreamKind,
};

impl Controller {
    /// Replaces only the physical video facet. The logical publication
    /// generation/resource continue to own room reachability and voice.
    pub fn set_host_video_connection_id(&mut self, id: &str) -> bool {
        if !self.room_media || id.is_empty() {
            return false;
        }
        let Some(publication) = self.host_publication.as_mut() else {
            return false;
        };
        if publication.connection_id != id {
            publication.connection_id = id.to_owned();
            self.touch_facts();
        }
        true
    }

    /// Opts this controller into room media. Room edges survive publication
    /// replacement, stop and pause. The returned resources belong only to the
    /// canceled operation; the caller retires video facets on stop or
    /// replacement. An empty generation establishes an audio-only room.
    pub fn set_video_publication(
        &mut self,
        generation: &str,
        paused: bool,
        now_ms: i64,
    ) -> Vec<ResourceRef> {
        let paused = paused && !generation.is_empty();
        if self.room_media && self.video_generation == generation && self.video_paused == paused {
            return Vec::new();
        }
        let released = self.abort_operation(Some(now_ms), RejectionBucket::Aborted);
        let first = !self.room_media;
        let changed = first || self.video_generation != generation;
        self.room_media = true;
        self.video_generation = generation.to_owned();
        self.video_paused = paused;
        self.paused = false;
        if changed {
            for key in self.upstream_by_viewer.keys() {
                let edge = self.upstream_by_viewer.get_mut(&key).unwrap();
                edge.video_generation.clear();
                edge.video_connection_id.clear();
                edge.video_ready = false;
                // Enabling voice during an accepted share must not
                // reconstruct its already-proven video transports.
                if first && !generation.is_empty() && edge.usable && edge.physical_active {
                    edge.video_generation = generation.to_owned();
                    edge.video_connection_id = edge.connection_id.clone();
                    edge.video_ready = true;
                }
            }
        }
        self.clear_all_quality();
        self.root_convergence_root_peer_id.clear();
        self.touch_facts();
        released
    }

    pub(crate) fn video_work_enabled(&self) -> bool {
        !self.room_media || (!self.video_generation.is_empty() && !self.video_paused)
    }

    pub(crate) fn required_video_generation(&self) -> String {
        if self.room_media && self.video_work_enabled() {
            return self.video_generation.clone();
        }
        String::new()
    }

    pub(crate) fn quality_connection_id(&self, edge: &CommittedEdge) -> String {
        if self.room_media {
            if edge.video_ready && edge.video_generation == self.video_generation {
                return edge.video_connection_id.clone();
            }
            return String::new();
        }
        edge.connection_id.clone()
    }

    /// Additional publication evidence, never room reachability.
    pub(crate) fn source_video_ready(&self, peer_id: &str) -> bool {
        if self.required_video_generation().is_empty() || peer_id == self.host_peer_id {
            return true;
        }
        self.edge(peer_id)
            .is_some_and(|edge| edge.video_generation == self.video_generation && edge.video_ready)
    }

    pub(crate) fn start_publication_attach(&mut self, now_ms: i64) -> bool {
        if !self.room_media || !self.video_work_enabled() {
            return false;
        }
        for (child_id, edge) in self.upstream_by_viewer.iter() {
            let Some(child) = self.participant(child_id) else {
                continue;
            };
            if !child.session_is(&edge.child_session_id)
                || child.departure_confirmed
                || !edge.usable
                || !edge.physical_active
                || !self.allows_incoming(child_id, edge.kind)
                || edge.video_generation == self.video_generation
            {
                continue;
            }
            let mut tuple = CandidateTuple {
                kind: edge.kind,
                parent_peer_id: String::new(),
                transport: Some(edge.transport),
                regenerate: false,
                publication: None,
            };
            if edge.kind == UpstreamKind::Peer {
                if !self.source_usable(&edge.parent_peer_id)
                    || !self.source_video_ready(&edge.parent_peer_id)
                {
                    continue;
                }
                tuple.parent_peer_id = edge.parent_peer_id.clone();
            } else {
                if !self.publication_serves(edge) {
                    continue;
                }
                tuple.publication = Some(PublicationPlan::Reuse);
            }
            let operation = Operation {
                video_generation: self.video_generation.clone(),
                attach_connection_id: edge.connection_id.clone(),
                child_peer_id: child_id.clone(),
                child_session_id: child.session_id.clone(),
                demand_peer_id: child_id.clone(),
                demand_session_id: child.session_id.clone(),
                reason: DemandReason::PublicationAttach,
                base_revision: self.revision,
                candidates: vec![CandidatePlan {
                    tuple,
                    endpoint_transition: EndpointTransition {
                        kind: TransitionKind::None,
                        producer_peer_id: None,
                        retire: None,
                    },
                }],
                cursor: 0,
                deadline_at_ms: now_ms + self.operation_timeout_ms,
                built_at_fact_version: self.fact_version,
                deferred_parent_peer_ids: Vec::new(),
                peer_attempt_started: false,
                quality_source: None,
                current: None,
            };
            // A retained physical path may outlive its parent's signaling
            // session. Select only attachments that can pass the same
            // ownership check at begin.
            if !self.publication_attach_owned(&operation) {
                continue;
            }
            self.operation = Some(operation);
            return true;
        }
        false
    }

    pub(crate) fn publication_attach_owned(&self, op: &Operation) -> bool {
        if !self.room_media
            || !self.video_work_enabled()
            || op.video_generation != self.video_generation
        {
            return false;
        }
        let (Some(child), Some(edge)) = (
            self.participant(&op.child_peer_id),
            self.edge(&op.child_peer_id),
        ) else {
            return false;
        };
        if !child.session_is(&op.child_session_id)
            || child.departure_confirmed
            || edge.child_session_id != op.child_session_id
            || edge.connection_id != op.attach_connection_id
            || !edge.usable
            || !edge.physical_active
            || !self.allows_incoming(&op.child_peer_id, edge.kind)
        {
            return false;
        }
        let tuple = &op.candidates[0].tuple;
        if tuple.kind == UpstreamKind::Peer {
            return edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == tuple.parent_peer_id
                && self
                    .participant(&tuple.parent_peer_id)
                    .is_some_and(|parent| parent.session_is(&edge.parent_session_id))
                && self.source_usable(&tuple.parent_peer_id)
                && self.source_video_ready(&tuple.parent_peer_id);
        }
        edge.kind == UpstreamKind::Sfu
            && self.host_publication.as_ref().is_some_and(|publication| {
                publication.generation == edge.publication_generation
                    && publication.physical_active
                    && publication.usable
            })
    }

    pub(crate) fn begin_publication_attach(
        &mut self,
        input: BeginInput,
        checked: Validation,
    ) -> BeginResult {
        let reservation = &input.reservation;
        if input.connection_id.is_empty()
            || reservation.kind.is_some()
            || reservation.edge.is_some()
            || reservation.publication.is_some()
            || reservation.overlap.is_some()
            || !self.publication_attach_owned(self.op())
        {
            let mut released = checked.released;
            released.extend(reservation_resources(reservation));
            return BeginResult {
                released,
                ..BeginResult::default()
            };
        }
        let edge = self.edge(&self.op().child_peer_id).unwrap();
        let attempt = Attempt {
            tuple: self.op().candidates[0].tuple.clone(),
            revision: self.allocate_revision(),
            connection_id: input.connection_id,
            child_session_id: self.op().child_session_id.clone(),
            parent_session_id: edge.parent_session_id.clone(),
            host_session_id: String::new(),
            publication_generation: edge.publication_generation.clone(),
            publication_connection_id: String::new(),
            reservation: CandidateReservation {
                kind: Some(ReservationKind::Direct),
                ..CandidateReservation::default()
            },
            started_at_ms: input.now_ms,
            transport_connected: false,
            media_ready: false,
            connection_attempt: None,
            sender_quality_state: None,
            sender_quality_accepted_at_ms: None,
            sender_quality_consecutive_healthy_windows: 0,
            sender_quality_consecutive_degraded_windows: 0,
            sender_quality_sample_timestamp_ms: None,
            sender_quality_identity: String::new(),
            relative_quality_approved_at_ms: None,
        };
        self.op_mut().current = Some(attempt);
        BeginResult {
            accepted: true,
            operation: self.operation_snapshot(),
            released: checked.released,
            failed_peer_ids: Vec::new(),
        }
    }

    /// Optional attachment gets one attempt per edge, publication and
    /// endpoint sessions. Failure records an unavailable publication without
    /// consuming or resetting the healthy room transport's NAT opportunity.
    pub(crate) fn finish_publication_attach(&mut self, ready: bool) -> Vec<String> {
        let op = self.op().clone();
        let edge = self.edge_mut(&op.child_peer_id).unwrap();
        edge.video_generation = op.video_generation.clone();
        edge.video_connection_id.clear();
        edge.video_ready = ready;
        if ready {
            edge.video_connection_id = op.current.as_ref().unwrap().connection_id.clone();
            self.revision = op.current.as_ref().unwrap().revision;
        } else {
            self.advance_active_revision();
        }
        self.operation = None;
        self.touch_facts();
        if ready {
            return Vec::new();
        }
        // Dependents cannot attach this picture until their upstream
        // recovers. Report that terminal result without retiring voice or
        // spending their own untried attachment.
        let mut failed = vec![op.child_peer_id];
        let mut index = 0;
        while index < failed.len() {
            for peer_id in self.children_of(&failed[index]) {
                let (Some(child), Some(edge)) = (self.participant(&peer_id), self.edge(&peer_id))
                else {
                    continue;
                };
                if child.session_is(&edge.child_session_id)
                    && !child.departure_confirmed
                    && edge.usable
                    && edge.physical_active
                    && !self.source_video_ready(&peer_id)
                {
                    failed.push(peer_id);
                }
            }
            index += 1;
        }
        failed
    }

    pub(crate) fn validate_publication_attach(
        &mut self,
        now_ms: i64,
        failed: Option<&CandidateGuard>,
    ) -> Validation {
        let mut result = Validation::default();
        if !self.publication_attach_owned(self.op()) {
            self.advance_active_revision();
            self.operation = None;
            return result;
        }
        let op = self.op();
        result.consumed_guard = failed.is_some_and(|guard| {
            op.current
                .as_ref()
                .is_some_and(|current| self.guard_matches(guard, op, current))
        });
        result.expired = now_ms >= op.deadline_at_ms;
        if result.expired || result.consumed_guard {
            result.exhausted_peer_ids = self.finish_publication_attach(false);
        }
        result
    }
}
