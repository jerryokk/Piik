//! Route quality evidence: sample identity, copy-context projection, and the
//! sender and SFU publisher evidence observers.

use piik_protocol::{Role, SenderQualityEvidenceMessage, SfuPublisherQualityEvidenceMessage};
use piik_route::{
    DemandReason, PublicationPlan, RouteSnapshot, SenderQualityEvidenceInput,
    SfuPublisherQualityEvidenceInput, TransitionKind, UpstreamKind,
};
use serde_json::{Value, json};

use crate::router::RouteParticipant;
use crate::server::Core;

/// QualitySampleReference; `kind` selects the peer or SFU fields.
pub(crate) struct QualitySample {
    pub kind: UpstreamKind,
    pub child_peer_id: String,
    pub route_revision: i64,
    pub connection_id: String,
    pub publication_generation: String,
    pub host_session_id: String,
}

/// Copy demand attributed to committed and candidate work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct QualityCopyContext {
    pub committed_copies: i64,
    pub candidate_reserved_copies: i64,
    pub endpoint_capacity: i64,
    pub operation_reason: Option<DemandReason>,
    pub candidate_transition: Option<TransitionKind>,
    pub sample_role: &'static str,
}

impl QualityCopyContext {
    fn kv(&self) -> Vec<(&'static str, Value)> {
        vec![
            ("committedCopies", json!(self.committed_copies)),
            (
                "candidateReservedCopies",
                json!(self.candidate_reserved_copies),
            ),
            (
                "possibleCopies",
                json!(self.committed_copies + self.candidate_reserved_copies),
            ),
            ("endpointCapacity", json!(self.endpoint_capacity)),
            (
                "operationReason",
                json!(self.operation_reason.map(|r| r.as_str())),
            ),
            (
                "candidateTransition",
                json!(self.candidate_transition.map(|t| t.as_str())),
            ),
            ("sampleRole", json!(self.sample_role)),
        ]
    }
}

pub(crate) fn quality_copy_context(
    snapshot: &RouteSnapshot,
    host_peer_id: &str,
    observed_peer_id: &str,
    endpoint_capacity: i64,
    sample: &QualitySample,
) -> QualityCopyContext {
    let mut committed_copies = snapshot
        .upstream_by_viewer
        .iter()
        .filter(|(_, edge)| {
            edge.kind == UpstreamKind::Peer
                && edge.parent_peer_id == observed_peer_id
                && edge.physical_active
        })
        .count() as i64;
    if observed_peer_id == host_peer_id
        && snapshot
            .host_publication
            .as_ref()
            .is_some_and(|p| p.physical_active)
    {
        committed_copies += 1;
    }
    let operation = snapshot.operation.as_ref();
    let current = operation.and_then(|op| op.current.as_ref());
    let current_plan = operation
        .filter(|op| op.current.is_some())
        .and_then(|op| op.candidates.get(op.cursor));
    let mut candidate_reserved_copies = 0;
    if let Some(current) = current {
        let peer_copy = current.tuple.kind == UpstreamKind::Peer
            && current.tuple.parent_peer_id == observed_peer_id;
        let sfu_copy = current.tuple.kind == UpstreamKind::Sfu
            && observed_peer_id == host_peer_id
            && current.tuple.publication != Some(PublicationPlan::Reuse);
        if peer_copy || sfu_copy {
            candidate_reserved_copies = 1;
        }
    }
    let candidate_sample = match (operation, current) {
        (Some(op), Some(current)) => {
            if sample.kind == UpstreamKind::Peer {
                current.tuple.kind == UpstreamKind::Peer
                    && op.child_peer_id == sample.child_peer_id
                    && current.tuple.parent_peer_id == observed_peer_id
                    && current.revision == sample.route_revision
                    && current.connection_id == sample.connection_id
            } else {
                current.tuple.kind == UpstreamKind::Sfu
                    && observed_peer_id == host_peer_id
                    && current.revision == sample.route_revision
            }
        }
        _ => false,
    };
    let active_sample = if sample.kind == UpstreamKind::Peer {
        sample.route_revision == snapshot.revision
            && snapshot
                .upstream_by_viewer
                .get(&sample.child_peer_id)
                .is_some_and(|edge| {
                    edge.kind == UpstreamKind::Peer
                        && edge.physical_active
                        && edge.usable
                        && edge.parent_peer_id == observed_peer_id
                        && edge.connection_id == sample.connection_id
                })
    } else {
        sample.route_revision == snapshot.revision
            && snapshot.host_publication.as_ref().is_some_and(|p| {
                p.physical_active
                    && p.usable
                    && p.generation == sample.publication_generation
                    && p.host_session_id == sample.host_session_id
            })
    };
    QualityCopyContext {
        committed_copies,
        candidate_reserved_copies,
        endpoint_capacity,
        operation_reason: operation.map(|op| op.reason),
        candidate_transition: current_plan.map(|plan| plan.endpoint_transition.kind),
        sample_role: if candidate_sample {
            "candidate"
        } else if active_sample {
            "active"
        } else {
            "unknown"
        },
    }
}

fn sender_quality_state(
    state: piik_protocol::SenderQualityState,
) -> piik_route::SenderQualityState {
    match state {
        piik_protocol::SenderQualityState::Unknown => piik_route::SenderQualityState::Unknown,
        piik_protocol::SenderQualityState::Healthy => piik_route::SenderQualityState::Healthy,
        piik_protocol::SenderQualityState::Degraded => piik_route::SenderQualityState::Degraded,
    }
}

fn sample_timestamp(value: Option<f64>) -> Option<i64> {
    value.map(|timestamp| timestamp as i64)
}

fn nullable_json<T: serde::Serialize>(value: &piik_protocol::Nullable<T>) -> Value {
    value.value().map_or(Value::Null, |v| json!(v))
}

impl Core {
    pub(crate) fn router_observe_sender_quality_evidence(
        &mut self,
        participant: &RouteParticipant,
        message: &SenderQualityEvidenceMessage,
    ) -> bool {
        let room_id = participant.room_id.clone();
        let Some(snapshot) = self.snapshot(&room_id) else {
            return false;
        };
        let before = snapshot.revision;
        let endpoint_capacity = if participant.role == Role::Host {
            self.router.capacity
        } else {
            self.viewer_settings(&room_id, &participant.peer_id)
                .capacity
                .min(self.router.capacity)
        };
        let copy_context = quality_copy_context(
            &snapshot,
            &self.host_peer_of(&room_id),
            &participant.peer_id,
            endpoint_capacity,
            &QualitySample {
                kind: UpstreamKind::Peer,
                child_peer_id: message.child_peer_id.clone(),
                route_revision: message.route_revision,
                connection_id: message.connection_id.clone(),
                publication_generation: String::new(),
                host_session_id: String::new(),
            },
        );
        let sender_identity = match (&message.rtp_stats_id, &message.track_identifier) {
            (Some(stats), Some(track)) if !stats.is_empty() && !track.is_empty() => {
                Some(format!("{stats}\0{track}"))
            }
            _ => None,
        };
        let input = SenderQualityEvidenceInput {
            parent_peer_id: participant.peer_id.clone(),
            parent_session_id: participant.session_id.clone(),
            child_peer_id: message.child_peer_id.clone(),
            route_revision: message.route_revision,
            connection_id: message.connection_id.clone(),
            sender_identity,
            sample_timestamp_ms: sample_timestamp(message.sample_timestamp_ms),
            state: sender_quality_state(message.state),
            accepted_at_ms: self.now(),
        };
        let Some(result) = self.with_commit(&room_id, |c, commit| {
            c.observe_sender_quality_evidence(input, commit)
        }) else {
            return false;
        };
        if result.accepted && self.debug_enabled() {
            let d = &message.diagnostics;
            let mut details = vec![
                (
                    "parent",
                    json!(self.debug_peer(&room_id, &participant.peer_id)),
                ),
                (
                    "child",
                    json!(self.debug_peer(&room_id, &message.child_peer_id)),
                ),
                ("routeRevision", json!(message.route_revision)),
            ];
            details.extend(copy_context.kv());
            details.extend([
                ("state", json!(message.state.as_str())),
                ("natTraversalPath", json!(d.nat_traversal_path.as_str())),
                ("reason", json!(d.reason.map(|r| r.as_str()))),
                ("framesPerSecond", json!(d.frames_per_second)),
                ("bitrateKbps", json!(d.bitrate_kbps)),
                (
                    "captureFramesPerSecond",
                    nullable_json(&d.capture_frames_per_second),
                ),
                (
                    "mediaSourceFramesPerSecond",
                    nullable_json(&d.media_source_frames_per_second),
                ),
                ("width", nullable_json(&d.width)),
                ("height", nullable_json(&d.height)),
                (
                    "availableOutgoingKbps",
                    nullable_json(&d.available_outgoing_kbps),
                ),
                ("rttMs", nullable_json(&d.rtt_ms)),
                ("packetLossPercent", nullable_json(&d.packet_loss_percent)),
            ]);
            self.emit_debug(&room_id, "sender-quality-evidence", details);
        }
        self.release_resources(result.released);
        if result.committed {
            self.router.resource_waiters.delete(&room_id);
        }
        if self.controller_revision(&room_id) != before {
            self.broadcast_active(&room_id);
        }
        let revision = self.controller_revision(&room_id);
        self.send_route_failures(&room_id, &result.failed_peer_ids, revision);
        if result.accepted {
            self.request_pump(&room_id);
        }
        result.accepted
    }

    pub(crate) fn router_observe_sfu_publisher_quality_evidence(
        &mut self,
        participant: &RouteParticipant,
        message: &SfuPublisherQualityEvidenceMessage,
    ) -> bool {
        let room_id = participant.room_id.clone();
        if participant.role != Role::Host {
            return false;
        }
        let Some(snapshot) = self.snapshot(&room_id) else {
            return false;
        };
        let operation = snapshot.operation.as_ref();
        let candidate_child = operation
            .map(|op| op.child_peer_id.clone())
            .unwrap_or_default();
        let candidate_connection = operation
            .and_then(|op| op.current.as_ref())
            .map(|c| c.connection_id.clone())
            .unwrap_or_default();
        let before = snapshot.revision;
        let copy_context = quality_copy_context(
            &snapshot,
            &participant.peer_id,
            &participant.peer_id,
            self.router.capacity,
            &QualitySample {
                kind: UpstreamKind::Sfu,
                child_peer_id: String::new(),
                route_revision: message.route_revision,
                connection_id: String::new(),
                publication_generation: message.publication_generation.clone(),
                host_session_id: participant.session_id.clone(),
            },
        );
        let input = SfuPublisherQualityEvidenceInput {
            host_peer_id: participant.peer_id.clone(),
            host_session_id: participant.session_id.clone(),
            publication_generation: message.publication_generation.clone(),
            route_revision: message.route_revision,
            state: sender_quality_state(message.state),
            sample_timestamp_ms: sample_timestamp(message.sample_timestamp_ms),
            accepted_at_ms: self.now(),
        };
        let Some(result) = self.with_commit(&room_id, |c, commit| {
            c.observe_sfu_publisher_quality_evidence(input, commit)
        }) else {
            return false;
        };
        if result.accepted && self.debug_enabled() {
            let d = &message.diagnostics;
            let mut details = vec![("routeRevision", json!(message.route_revision))];
            details.extend(copy_context.kv());
            details.extend([
                ("state", json!(message.state.as_str())),
                ("reason", json!(d.reason.map(|r| r.as_str()))),
                ("framesPerSecond", json!(d.frames_per_second)),
                ("bitrateKbps", json!(d.bitrate_kbps)),
                (
                    "captureFramesPerSecond",
                    nullable_json(&d.capture_frames_per_second),
                ),
                (
                    "mediaSourceFramesPerSecond",
                    nullable_json(&d.media_source_frames_per_second),
                ),
                ("width", nullable_json(&d.width)),
                ("height", nullable_json(&d.height)),
                ("videoEncodingCount", nullable_json(&d.video_encoding_count)),
                (
                    "activeVideoEncodingCount",
                    nullable_json(&d.active_video_encoding_count),
                ),
                (
                    "availableOutgoingKbps",
                    nullable_json(&d.available_outgoing_kbps),
                ),
                ("rttMs", nullable_json(&d.rtt_ms)),
                ("packetLossPercent", nullable_json(&d.packet_loss_percent)),
            ]);
            self.emit_debug(&room_id, "sfu-publisher-quality-evidence", details);
        }
        self.release_resources(result.released);
        if result.committed && !candidate_child.is_empty() && !candidate_connection.is_empty() {
            self.router.resource_waiters.delete(&room_id);
        }
        if self.controller_revision(&room_id) != before {
            self.broadcast_active(&room_id);
        }
        let revision = self.controller_revision(&room_id);
        self.send_route_failures(&room_id, &result.failed_peer_ids, revision);
        if result.accepted {
            self.request_pump(&room_id);
        }
        result.accepted
    }

    pub(crate) fn router_reset_sender_quality(&mut self, participant: &RouteParticipant) {
        let room_id = &participant.room_id;
        if !self.has_controller(room_id) {
            return;
        }
        let before = self.controller_revision(room_id);
        let now = self.now();
        let released = self
            .ctl(room_id, |c| {
                c.reset_sender_quality(&participant.peer_id, &participant.session_id, now)
            })
            .unwrap_or_default();
        self.release_resources(released);
        if self.controller_revision(room_id) != before {
            self.broadcast_active(room_id);
        }
        self.request_pump(room_id);
    }
}
