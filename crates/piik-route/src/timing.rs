//! Per-demand route timing and diagnostic projection.

use std::collections::HashMap;

use piik_protocol::{RouteDiagnosticParent, RouteDiagnosticQuality};

use crate::controller::{QUALITY_EVIDENCE_EXPIRY_MS, RouteTimingRecord};
use crate::quality::quality_observation_matches_edge;
use crate::{CommittedEdge, Controller, DemandReason, FinalRoute, RejectionBucket, UpstreamKind};

impl Controller {
    /// Replaces the peer's timing record wholesale.
    pub(crate) fn record_demand(
        &mut self,
        child_peer_id: &str,
        now_ms: i64,
        _reason: DemandReason,
    ) {
        self.route_timings.insert(
            child_peer_id.to_owned(),
            RouteTimingRecord {
                demand_at_ms: now_ms,
                operation_started_at_ms: None,
                candidate_started_at_ms: None,
                first_decoded_frame_at_ms: None,
                final_at_ms: None,
                final_route: FinalRoute::Waiting,
                rejection_bucket: RejectionBucket::None,
            },
        );
    }

    pub(crate) fn ensure_demand(&mut self, child_peer_id: &str, now_ms: i64, reason: DemandReason) {
        if !self.route_timings.contains_key(child_peer_id) {
            self.record_demand(child_peer_id, now_ms, reason);
        }
    }

    pub(crate) fn start_operation_timing(&mut self, child_peer_id: &str, now_ms: i64) {
        if let Some(record) = self.route_timings.get_mut(child_peer_id) {
            record.operation_started_at_ms = Some(now_ms);
            record.candidate_started_at_ms = None;
            record.first_decoded_frame_at_ms = None;
            record.final_at_ms = None;
            record.final_route = FinalRoute::Waiting;
            record.rejection_bucket = RejectionBucket::None;
        }
    }

    pub(crate) fn start_candidate_timing(&mut self, child_peer_id: &str, now_ms: i64) {
        if let Some(record) = self.route_timings.get_mut(child_peer_id) {
            record.candidate_started_at_ms = Some(now_ms);
            record.first_decoded_frame_at_ms = None;
            record.final_at_ms = None;
            record.final_route = FinalRoute::Waiting;
            record.rejection_bucket = RejectionBucket::None;
        }
    }

    pub(crate) fn note_rejection(&mut self, child_peer_id: &str, bucket: RejectionBucket) {
        if let Some(record) = self.route_timings.get_mut(child_peer_id) {
            record.rejection_bucket = bucket;
        }
    }

    pub(crate) fn clear_candidate_timing(&mut self, child_peer_id: &str) {
        if let Some(record) = self.route_timings.get_mut(child_peer_id) {
            record.candidate_started_at_ms = None;
            record.first_decoded_frame_at_ms = None;
            record.final_at_ms = None;
            record.final_route = FinalRoute::Waiting;
        }
    }

    pub(crate) fn finish_timing(
        &mut self,
        child_peer_id: &str,
        now_ms: i64,
        final_route: FinalRoute,
        rejection_bucket: RejectionBucket,
        first_decoded_frame: bool,
    ) {
        if let Some(record) = self.route_timings.get_mut(child_peer_id) {
            if first_decoded_frame {
                record.first_decoded_frame_at_ms = Some(now_ms);
            }
            record.final_at_ms = Some(now_ms);
            record.final_route = final_route;
            record.rejection_bucket = rejection_bucket;
        }
    }

    pub(crate) fn usable_route(&self, child_peer_id: &str) -> bool {
        self.current_final_route(child_peer_id) != FinalRoute::Waiting
    }

    pub(crate) fn current_final_route(&self, child_peer_id: &str) -> FinalRoute {
        let Some(edge) = self.edge(child_peer_id) else {
            return FinalRoute::Waiting;
        };
        if !edge.usable || !edge.physical_active {
            return FinalRoute::Waiting;
        }
        if edge.kind == UpstreamKind::Peer {
            return if self.source_usable(&edge.parent_peer_id) {
                FinalRoute::Direct
            } else {
                FinalRoute::Waiting
            };
        }
        if self.publication_serves(edge) {
            FinalRoute::Sfu
        } else {
            FinalRoute::Waiting
        }
    }

    pub(crate) fn route_demand_reason(&self, child_peer_id: &str) -> DemandReason {
        let Some(edge) = self.edge(child_peer_id) else {
            return DemandReason::Join;
        };
        if edge.kind == UpstreamKind::Peer {
            if self
                .participant(&edge.parent_peer_id)
                .is_some_and(|parent| parent.departure_confirmed)
            {
                return DemandReason::ParentDeparted;
            }
            if edge.physical_active && !self.overflow_peer_children(&edge.parent_peer_id).is_empty()
            {
                return DemandReason::CapacityReduction;
            }
        }
        DemandReason::EdgeUnavailable
    }

    pub(crate) fn diagnostic_parent(
        &self,
        edge: Option<&CommittedEdge>,
        ordinals: &HashMap<String, i64>,
    ) -> RouteDiagnosticParent {
        let Some(edge) = edge.filter(|edge| edge.usable && edge.physical_active) else {
            return RouteDiagnosticParent::None;
        };
        if edge.kind == UpstreamKind::Sfu {
            return if self.publication_serves(edge) {
                RouteDiagnosticParent::Sfu
            } else {
                RouteDiagnosticParent::None
            };
        }
        if edge.parent_peer_id == self.host_peer_id {
            return RouteDiagnosticParent::Host;
        }
        match ordinals.get(&edge.parent_peer_id) {
            Some(ordinal) => RouteDiagnosticParent::Viewer { ordinal: *ordinal },
            None => RouteDiagnosticParent::None,
        }
    }

    /// `None` unless the observation is fresh, non-empty and still describes
    /// the current edge.
    pub(crate) fn diagnostic_quality(
        &self,
        child_peer_id: &str,
        edge: Option<&CommittedEdge>,
        now_ms: i64,
    ) -> Option<RouteDiagnosticQuality> {
        let observation = self.quality_observations.get(child_peer_id)?;
        let current = self.participant(child_peer_id)?;
        let edge = edge?;
        if self.paused
            || !self.video_work_enabled()
            || observation.eligible_windows == 0
            || now_ms >= observation.last_accepted_at_ms + QUALITY_EVIDENCE_EXPIRY_MS
            || current.session_id.is_empty()
            || !current.session_is(&observation.child_session_id)
            || !edge.usable
            || !edge.physical_active
            || self.quality_connection_id(edge) != observation.connection_id
            || !quality_observation_matches_edge(observation, edge)
            || !self.source_usable_for_quality(edge)
        {
            return None;
        }
        Some(RouteDiagnosticQuality {
            eligible_windows: observation.eligible_windows,
            eligible_duration_ms: observation.eligible_duration_ms,
            freeze_windows: observation.freeze_windows,
            freeze_count: observation.freeze_count,
            freeze_duration_ms: observation.freeze_duration_ms,
            pause_count: observation.pause_count,
            pause_duration_ms: observation.pause_duration_ms,
        })
    }
}
