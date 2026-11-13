//! Controller state, participant lifecycle and the committed-graph API.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use piik_ordered::Map;
use piik_protocol::{
    MAX_MEDIA_ROUTE_REVISION, MAX_SAFE_INTEGER, PERSISTENT_NATIVE_EDGE_DEGRADED_WINDOWS, Role,
    RouteDiagnosticChild, RouteDiagnosticOperation, RouteDiagnosticSnapshot, RouteDiagnosticStage,
    VIEWER_QUALITY_EVIDENCE_EXPIRY_MS, ViewerRoutePreference, assert_endpoint_media_copy_capacity,
};
use sha2::{Digest, Sha256};

use crate::debug::route_debug;
use crate::relay_volunteer::MAX_RELAY_VOLUNTEER_CHILDREN;
use crate::{
    AdoptDirectConnectionInput, CandidatePlan, CandidateReservation, CandidateTuple, CommittedEdge,
    ConnectionAttemptProgress, Controller, DemandReason, EdgeGuard, FinalRoute, HostPublication,
    Options, ParentEdgeGuard, ParticipantInput, PublicationGuard, RejectionBucket, ResourceRef,
    RevisionSequence, RouteSnapshot, SenderQualityState, SettleResult, Transport, UpstreamKind,
};

mod lifecycle;
mod lifecycle_ops;
mod retire;

pub(crate) const MAX_DIRECT_HEAD_START_MS: i64 = 5_000;
pub(crate) const MAX_NAT_CONNECTION_ATTEMPTS: i64 = 4;
pub(crate) const SFU_OPPORTUNITY_PREFIX: &str = "sfu\0";
pub(crate) const QUALITY_EVIDENCE_EXPIRY_MS: i64 = VIEWER_QUALITY_EVIDENCE_EXPIRY_MS;
pub(crate) const PERSISTENT_DEGRADED_WINDOWS: i64 = PERSISTENT_NATIVE_EDGE_DEGRADED_WINDOWS;

/// The attempts consumed at one candidate rank.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct CandidateOpportunity {
    pub(crate) rank: i64,
    pub(crate) started_attempts: i64,
    pub(crate) exhausted: bool,
}

/// A known peer. An empty `session_id` means disconnected but in grace, or
/// departed; validated wire IDs never name a live session with "".
#[derive(Debug, Clone)]
pub(crate) struct Participant {
    pub(crate) peer_id: String,
    pub(crate) role: Role,
    pub(crate) session_id: String,
    pub(crate) departure_confirmed: bool,
    pub(crate) join_order: i64,
    pub(crate) effective_downstream_capacity: i64,
    pub(crate) reported_downstream_capacity: i64,
    pub(crate) relay_volunteer: bool,
    pub(crate) availability_exhausted: bool,
    pub(crate) consumed_candidate_opportunities: Map<String, CandidateOpportunity>,
    pub(crate) bootstrap_failure_reported: bool,
    pub(crate) sfu_first_at_next_route: bool,
    pub(crate) route_preference: Option<ViewerRoutePreference>,
}

impl Participant {
    /// Never matches a disconnected participant, including an empty id.
    pub(crate) fn session_is(&self, id: &str) -> bool {
        !self.session_id.is_empty() && self.session_id == id
    }
}

/// Scopes quality evidence to one physical sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SenderQualityIdentity {
    pub(crate) child_session_id: String,
    pub(crate) parent_peer_id: String,
    pub(crate) parent_session_id: String,
    pub(crate) connection_id: String,
    pub(crate) sender_identity: String,
}

/// The live candidate. Optional identities use ""; optional timestamps use
/// `Option` because zero is a valid accepted/sample time.
#[derive(Debug, Clone)]
pub(crate) struct Attempt {
    pub(crate) tuple: CandidateTuple,
    pub(crate) revision: i64,
    pub(crate) connection_id: String,
    pub(crate) child_session_id: String,
    pub(crate) parent_session_id: String,
    pub(crate) host_session_id: String,
    pub(crate) publication_generation: String,
    pub(crate) publication_connection_id: String,
    pub(crate) reservation: CandidateReservation,
    pub(crate) started_at_ms: i64,
    pub(crate) transport_connected: bool,
    pub(crate) media_ready: bool,
    pub(crate) connection_attempt: Option<ConnectionAttemptProgress>,
    /// `None` means no sender-quality state yet.
    pub(crate) sender_quality_state: Option<SenderQualityState>,
    pub(crate) sender_quality_accepted_at_ms: Option<i64>,
    pub(crate) sender_quality_consecutive_healthy_windows: i64,
    pub(crate) sender_quality_consecutive_degraded_windows: i64,
    pub(crate) sender_quality_sample_timestamp_ms: Option<i64>,
    pub(crate) sender_quality_identity: String,
    pub(crate) relative_quality_approved_at_ms: Option<i64>,
}

/// The single in-flight route operation.
#[derive(Debug, Clone)]
pub(crate) struct Operation {
    pub(crate) video_generation: String,
    pub(crate) attach_connection_id: String,
    pub(crate) child_peer_id: String,
    pub(crate) child_session_id: String,
    pub(crate) demand_peer_id: String,
    pub(crate) demand_session_id: String,
    pub(crate) reason: DemandReason,
    pub(crate) base_revision: i64,
    pub(crate) candidates: Vec<CandidatePlan>,
    pub(crate) cursor: usize,
    pub(crate) deadline_at_ms: i64,
    pub(crate) built_at_fact_version: i64,
    pub(crate) deferred_parent_peer_ids: Vec<String>,
    pub(crate) peer_attempt_started: bool,
    pub(crate) quality_source: Option<SenderQualityIdentity>,
    pub(crate) current: Option<Attempt>,
}

impl Operation {
    /// Copies the plan at `index`, or `None` when out of range.
    pub(crate) fn plan_at(&self, index: usize) -> Option<CandidatePlan> {
        self.candidates.get(index).cloned()
    }

    /// The cursor's plan, the final plan if exhausted, or `None` without
    /// candidates.
    pub(crate) fn plan_at_or_last(&self) -> Option<CandidatePlan> {
        if self.candidates.is_empty() {
            return None;
        }
        self.plan_at(self.cursor.min(self.candidates.len() - 1))
    }

    /// Prefers the live attempt over the next planned candidate.
    pub(crate) fn current_tuple(&self) -> Option<CandidateTuple> {
        if let Some(current) = &self.current {
            return Some(current.tuple.clone());
        }
        self.plan_at(self.cursor).map(|plan| plan.tuple)
    }
}

/// The remaining parent retry order behind an SFU edge.
#[derive(Debug, Clone)]
pub(crate) struct DirectContinuation {
    pub(crate) child_session_id: String,
    pub(crate) sfu_connection_id: String,
    pub(crate) publication_generation: String,
    pub(crate) parent_peer_ids: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct SfuBootstrapIntent {
    pub(crate) demand_peer_id: String,
    pub(crate) demand_session_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct SfuBootstrapCarrier {
    pub(crate) demand_peer_id: String,
    pub(crate) carrier_peer_id: String,
}

/// `None` timestamps are events that have not happened; zero is a valid time.
#[derive(Debug, Clone)]
pub(crate) struct RouteTimingRecord {
    pub(crate) demand_at_ms: i64,
    pub(crate) operation_started_at_ms: Option<i64>,
    pub(crate) candidate_started_at_ms: Option<i64>,
    pub(crate) first_decoded_frame_at_ms: Option<i64>,
    pub(crate) final_at_ms: Option<i64>,
    pub(crate) final_route: FinalRoute,
    pub(crate) rejection_bucket: RejectionBucket,
}

/// Belongs to one receiver edge and presentation epoch. `upstream_peer_id`
/// is empty for SFU.
#[derive(Debug, Clone)]
pub(crate) struct RouteQualityObservation {
    pub(crate) child_session_id: String,
    pub(crate) upstream_kind: UpstreamKind,
    pub(crate) upstream_peer_id: String,
    pub(crate) connection_id: String,
    pub(crate) presentation_epoch: i64,
    pub(crate) eligible_windows: i64,
    pub(crate) eligible_duration_ms: i64,
    pub(crate) freeze_windows: i64,
    pub(crate) freeze_count: i64,
    pub(crate) freeze_duration_ms: i64,
    pub(crate) pause_count: i64,
    pub(crate) pause_duration_ms: i64,
    pub(crate) last_accepted_at_ms: i64,
    pub(crate) last_decoded_progress_at_ms: Option<i64>,
}

/// A replacement sample on the same physical sender inherits the consumed
/// candidate ledger, so it cannot reopen an exhausted opportunity.
#[derive(Debug, Clone)]
pub(crate) struct SenderQualityObservation {
    pub(crate) child_session_id: String,
    pub(crate) parent_peer_id: String,
    pub(crate) parent_session_id: String,
    pub(crate) connection_id: String,
    pub(crate) sender_identity: String,
    pub(crate) state: SenderQualityState,
    pub(crate) consecutive_degraded_windows: i64,
    pub(crate) last_sample_timestamp_ms: i64,
    pub(crate) last_accepted_at_ms: i64,
    pub(crate) consumed_quality_candidates: Map<String, i64>,
}

#[derive(Debug, Clone)]
pub(crate) struct SfuPublisherQualityObservation {
    pub(crate) host_session_id: String,
    pub(crate) publication_generation: String,
    pub(crate) state: SenderQualityState,
    pub(crate) consecutive_healthy_windows: i64,
    pub(crate) last_sample_timestamp_ms: i64,
    pub(crate) last_accepted_at_ms: i64,
}

/// Deduplicates by `Arc` identity and preserves insertion order.
#[derive(Default)]
pub(crate) struct ResourceSet {
    list: Vec<ResourceRef>,
}

impl ResourceSet {
    pub(crate) fn add(&mut self, resources: impl IntoIterator<Item = ResourceRef>) {
        for resource in resources {
            if !self.has(&resource) {
                self.list.push(resource);
            }
        }
    }

    pub(crate) fn has(&self, resource: &ResourceRef) -> bool {
        self.list.iter().any(|known| Arc::ptr_eq(known, resource))
    }

    pub(crate) fn into_vec(self) -> Vec<ResourceRef> {
        self.list
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &ResourceRef> {
        self.list.iter()
    }
}

/// Retains each value's first occurrence.
pub(crate) fn dedupe_strings(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

/// Keeps values exactly representable by the Browser's number type.
pub(crate) fn is_safe_integer(value: i64) -> bool {
    (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value)
}

/// Preserves join order, including across Host rebinding; peer ID breaks
/// ties in synthetic inputs.
pub(crate) fn compare_participant(left: &Participant, right: &Participant) -> Ordering {
    left.join_order
        .cmp(&right.join_order)
        .then_with(|| left.peer_id.cmp(&right.peer_id))
}

/// The first 6 bytes, big-endian, of `sha256(child + "\0" + parent)`.
/// Byte-exact: it breaks parent ties.
pub(crate) fn stable_pair_rank(child_peer_id: &str, parent_peer_id: &str) -> i64 {
    let mut hasher = Sha256::new();
    hasher.update(child_peer_id.as_bytes());
    hasher.update([0]);
    hasher.update(parent_peer_id.as_bytes());
    let sum = hasher.finalize();
    sum[..6]
        .iter()
        .fold(0, |rank, byte| (rank << 8) | i64::from(*byte))
}

/// Ignores invalid rounded deltas and saturates at the wire integer bound.
pub(crate) fn safe_add(total: i64, delta: f64) -> i64 {
    // Rounds half away from zero.
    let rounded = delta.round();
    if rounded.is_nan()
        || rounded.is_infinite()
        || rounded < 0.0
        || rounded > MAX_SAFE_INTEGER as f64
    {
        return total;
    }
    let value = rounded as i64;
    if value > MAX_SAFE_INTEGER - total {
        return MAX_SAFE_INTEGER;
    }
    total + value
}

impl Controller {
    /// Constructs a controller; panics on invalid capacity, timeout or SFU policy.
    pub fn new(options: Options) -> Self {
        if let Err(error) =
            assert_endpoint_media_copy_capacity(options.endpoint_media_copy_capacity)
        {
            panic!("{error}");
        }
        if !is_safe_integer(options.operation_timeout_ms) || options.operation_timeout_ms <= 0 {
            panic!("Route operation timeout must be a positive integer");
        }
        if options.sfu_only && !options.sfu_enabled {
            panic!("SFU-only routing requires SFU availability");
        }
        let controller = Self {
            host_peer_id: options.host_peer_id,
            debug_room_id: options.debug_room_id,
            debug_log: options.debug_log,
            endpoint_media_copy_capacity: options.endpoint_media_copy_capacity,
            operation_timeout_ms: options.operation_timeout_ms,
            sfu_enabled: options.sfu_enabled,
            sfu_only: options.sfu_only,
            quality_convergence_enabled: options.quality_convergence_enabled && !options.sfu_only,
            nat_prediction_enabled: options.nat_prediction_enabled && !options.sfu_only,
            participants: Map::new(),
            upstream_by_viewer: Map::new(),
            host_publication: None,
            operation: None,
            revision: 0,
            revisions: options
                .revisions
                .unwrap_or_else(|| Arc::new(RevisionSequence::default())),
            fact_version: 0,
            next_join_order: 0,
            paused: false,
            room_media: false,
            voice_listeners: HashSet::new(),
            video_generation: String::new(),
            video_paused: false,
            route_timings: HashMap::new(),
            quality_observations: HashMap::new(),
            sender_quality_observations: HashMap::new(),
            sender_quality_baselines: HashMap::new(),
            sender_quality_regeneration_blocks: HashMap::new(),
            sfu_publisher_quality_observation: None,
            quality_baselines_pending: HashSet::new(),
            direct_continuations: Map::new(),
            retiring_publication_generations: Map::new(),
            sfu_bootstrap_intent: None,
            consumed_sfu_bootstrap_opportunities: Map::new(),
            root_convergence_root_peer_id: String::new(),
        };
        route_debug!(controller, "controller-created",
            "endpointCapacity" => options.endpoint_media_copy_capacity,
            "operationTimeoutMs" => options.operation_timeout_ms,
            "sfuEnabled" => options.sfu_enabled,
            "sfuOnly" => options.sfu_only,
            "qualityConvergenceEnabled" => options.quality_convergence_enabled);
        controller
    }

    // --- keyed access helpers -------------------------------------------------

    pub(crate) fn participant(&self, peer_id: &str) -> Option<&Participant> {
        self.participants.get(&peer_id.to_owned())
    }

    pub(crate) fn participant_mut(&mut self, peer_id: &str) -> Option<&mut Participant> {
        self.participants.get_mut(&peer_id.to_owned())
    }

    pub(crate) fn edge(&self, child_peer_id: &str) -> Option<&CommittedEdge> {
        self.upstream_by_viewer.get(&child_peer_id.to_owned())
    }

    pub(crate) fn edge_mut(&mut self, child_peer_id: &str) -> Option<&mut CommittedEdge> {
        self.upstream_by_viewer.get_mut(&child_peer_id.to_owned())
    }

    pub(crate) fn has_edge(&self, child_peer_id: &str) -> bool {
        self.upstream_by_viewer.has(&child_peer_id.to_owned())
    }

    pub(crate) fn host_session_id(&self) -> String {
        self.participant(&self.host_peer_id)
            .map(|host| host.session_id.clone())
            .unwrap_or_default()
    }

    pub(crate) fn op(&self) -> &Operation {
        self.operation.as_ref().expect("route operation is active")
    }

    pub(crate) fn op_mut(&mut self) -> &mut Operation {
        self.operation.as_mut().expect("route operation is active")
    }

    // --- read-only views -------------------------------------------------------

    /// Returns `host`, `viewer-<joinOrder>` or `viewer-unknown` without
    /// exposing the ID.
    pub fn diagnostic_participant_label(&self, peer_id: &str) -> String {
        self.debug_peer(peer_id)
    }

    /// `snapshot().revision` without the graph copy.
    pub fn revision(&self) -> i64 {
        self.revision
    }

    /// A deep copy of the in-flight operation, or `None` when idle.
    pub fn operation(&self) -> Option<crate::OperationSnapshot> {
        self.operation_snapshot()
    }

    /// A copy of the single live SFU ingress, or `None`.
    pub fn publication(&self) -> Option<HostPublication> {
        self.host_publication.clone()
    }

    /// A copy of the committed graph, publication and operation; mutates nothing.
    pub fn snapshot(&self) -> RouteSnapshot {
        let mut upstream = Map::new();
        for (id, edge) in self.upstream_by_viewer.iter() {
            upstream.set(id.clone(), edge.clone());
        }
        RouteSnapshot {
            room_media: self.room_media,
            video_generation: self.video_generation.clone(),
            video_paused: self.video_paused,
            revision: self.revision,
            paused: self.paused,
            fact_version: self.fact_version,
            upstream_by_viewer: upstream,
            host_publication: self.host_publication.clone(),
            operation: self.operation_snapshot(),
        }
    }

    /// The privacy-safe snapshot: connected viewers sorted by join order with
    /// snapshot-local ordinals and relative durations; the operation is
    /// omitted unless its demand peer has an ordinal.
    pub fn route_diagnostic_snapshot(&self, now_ms: i64) -> RouteDiagnosticSnapshot {
        let mut viewers: Vec<&Participant> = self
            .participants
            .iter()
            .map(|(_, current)| current)
            .filter(|current| {
                current.role == Role::Viewer
                    && !current.session_id.is_empty()
                    && !current.departure_confirmed
            })
            .collect();
        viewers.sort_by(|left, right| compare_participant(left, right));
        let ordinals: HashMap<String, i64> = viewers
            .iter()
            .enumerate()
            .map(|(index, viewer)| (viewer.peer_id.clone(), index as i64 + 1))
            .collect();
        let mut children = Vec::with_capacity(viewers.len());
        for viewer in &viewers {
            let edge = self.edge(&viewer.peer_id);
            let record = self.route_timings.get(&viewer.peer_id);
            let final_route = self.current_final_route(&viewer.peer_id);
            let child_count = self
                .children_of(&viewer.peer_id)
                .iter()
                .filter(|child| {
                    self.edge(child).is_some_and(|child_edge| {
                        child_edge.kind == UpstreamKind::Peer && child_edge.physical_active
                    })
                })
                .count();
            let mut child = RouteDiagnosticChild {
                ordinal: ordinals[&viewer.peer_id],
                parent: self.diagnostic_parent(edge, &ordinals),
                effective_capacity: viewer.effective_downstream_capacity,
                child_count: child_count as i64,
                demand_age_ms: None,
                queue_wait_ms: None,
                candidate_start_ms: None,
                first_decoded_frame_ms: None,
                final_ms: None,
                final_route,
                rejection_bucket: RejectionBucket::None,
                quality: self.diagnostic_quality(&viewer.peer_id, edge, now_ms),
            };
            if let Some(record) = record {
                child.demand_age_ms = elapsed_ms(record.demand_at_ms, now_ms);
                child.queue_wait_ms = record
                    .operation_started_at_ms
                    .and_then(|at| elapsed_ms(record.demand_at_ms, at));
                child.candidate_start_ms = record
                    .candidate_started_at_ms
                    .and_then(|at| elapsed_ms(record.demand_at_ms, at));
                child.first_decoded_frame_ms = record
                    .first_decoded_frame_at_ms
                    .and_then(|at| elapsed_ms(record.demand_at_ms, at));
                child.final_ms = record
                    .final_at_ms
                    .and_then(|at| elapsed_ms(record.demand_at_ms, at));
                child.rejection_bucket = record.rejection_bucket;
            }
            if final_route == FinalRoute::Waiting {
                child.final_route = FinalRoute::Waiting;
                if record.is_some_and(|record| record.final_route == FinalRoute::Failed) {
                    child.final_route = FinalRoute::Failed;
                }
            }
            children.push(child);
        }
        let operation = self.operation.as_ref().and_then(|op| {
            let child_ordinal = *ordinals.get(&op.demand_peer_id)?;
            let stage = match &op.current {
                None => RouteDiagnosticStage::Admission,
                Some(current) if current.media_ready => RouteDiagnosticStage::QualityProof,
                Some(_) => RouteDiagnosticStage::FirstFrame,
            };
            Some(RouteDiagnosticOperation {
                child_ordinal,
                reason: op.reason,
                stage,
                cursor: op.cursor as i64,
                candidate_count: op.candidates.len() as i64,
            })
        });
        RouteDiagnosticSnapshot {
            children,
            operation,
        }
    }
}

/// `None` when the end precedes the start or the difference exceeds the
/// wire integer bound.
pub(crate) fn elapsed_ms(started_at_ms: i64, ended_at_ms: i64) -> Option<i64> {
    let elapsed = ended_at_ms.checked_sub(started_at_ms)?;
    (is_safe_integer(elapsed) && elapsed >= 0).then_some(elapsed)
}

pub(crate) use crate::candidates::reservation_resources;
pub(crate) use crate::operation::is_availability_operation;

impl CandidateTuple {
    /// A tuple with only kind and parent set.
    pub(crate) fn bare(kind: UpstreamKind, parent_peer_id: &str) -> Self {
        Self {
            kind,
            parent_peer_id: parent_peer_id.to_owned(),
            transport: None,
            regenerate: false,
            publication: None,
        }
    }

    /// A direct Peer tuple.
    pub fn peer(parent_peer_id: &str) -> Self {
        Self {
            kind: UpstreamKind::Peer,
            parent_peer_id: parent_peer_id.to_owned(),
            transport: Some(Transport::Direct),
            regenerate: false,
            publication: None,
        }
    }

    /// An SFU tuple.
    pub fn sfu(publication: crate::PublicationPlan) -> Self {
        Self {
            kind: UpstreamKind::Sfu,
            parent_peer_id: String::new(),
            transport: None,
            regenerate: false,
            publication: Some(publication),
        }
    }
}

impl CandidatePlan {
    /// A plan with no endpoint transition, used only for keys.
    pub(crate) fn bare(tuple: CandidateTuple) -> Self {
        Self {
            tuple,
            endpoint_transition: crate::EndpointTransition {
                kind: crate::TransitionKind::None,
                producer_peer_id: None,
                retire: None,
            },
        }
    }
}
