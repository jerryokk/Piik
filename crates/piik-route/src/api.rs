//! Public inputs, outputs and identities of the route controller.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use piik_ordered::Map;
use piik_protocol::{Role, ViewerQualityEvidenceMetrics, ViewerRoutePreference};

pub use piik_protocol::{
    RouteDemandReason as DemandReason, RouteDiagnosticBucket as RejectionBucket,
    RouteDiagnosticFinalRoute as FinalRoute,
};

use crate::debug::DebugSink;

macro_rules! string_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            /// The wire spelling.
            pub const fn as_str(self) -> &'static str {
                match self {
                    $($name::$variant => $wire),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }
    };
}

string_enum! {
    /// The allocation a [`Resource`] represents.
    ResourceKind { Overlap = "overlap", SfuSubscription = "sfu-subscription", SfuPublication = "sfu-publication" }
}

string_enum! {
    /// Discriminates candidate tuples, committed edges and quality upstreams.
    UpstreamKind { Peer = "peer", Sfu = "sfu" }
}

string_enum! {
    /// The edge transport.
    Transport { Direct = "direct", Sfu = "sfu" }
}

string_enum! {
    /// The SFU tuple action.
    PublicationPlan { Reuse = "reuse", Create = "create", Replace = "replace" }
}

string_enum! {
    /// Discriminates [`CandidateReservation`].
    ReservationKind { Direct = "direct", SfuReuse = "sfu-reuse", SfuCreate = "sfu-create" }
}

string_enum! {
    /// Discriminates [`EndpointRetirement`].
    RetirementKind { Edge = "edge", Publication = "publication" }
}

string_enum! {
    /// Discriminates [`EndpointTransition`].
    TransitionKind { None = "none", Overlap = "overlap", BoundedGap = "bounded-gap" }
}

string_enum! {
    /// The sender-side health sample state.
    SenderQualityState { Unknown = "unknown", Healthy = "healthy", Degraded = "degraded" }
}

string_enum! {
    /// The `observe_quality_evidence` outcome.
    RouteQualityEvidenceResult { Rejected = "rejected", Accepted = "accepted", Observed = "observed" }
}

/// The caller-owned SFU/overlap allocation the controller stores, moves
/// between collections and compares by `Arc` identity. The controller never
/// reads its fields; the router owns their meaning and release.
#[derive(Debug)]
pub struct Resource {
    pub kind: ResourceKind,
    /// Set for [`ResourceKind::Overlap`].
    pub endpoint_peer_id: String,
    /// Fence fields for SFU publication and subscription; `viewer_peer_id`
    /// only for the subscription. All are opaque IDs.
    pub room_id: String,
    pub share_generation: String,
    pub publication_generation: String,
    pub viewer_peer_id: String,
    pub connection_id: String,
    /// Flipped by the router once the allocation is freed.
    pub released: AtomicBool,
}

impl Resource {
    /// An allocation of `kind` with empty fences.
    pub fn new(kind: ResourceKind) -> Self {
        Self {
            kind,
            endpoint_peer_id: String::new(),
            room_id: String::new(),
            share_generation: String::new(),
            publication_generation: String::new(),
            viewer_peer_id: String::new(),
            connection_id: String::new(),
            released: AtomicBool::new(false),
        }
    }
}

/// Shared handle; identity is [`Arc::ptr_eq`].
pub type ResourceRef = Arc<Resource>;

/// One route candidate: a direct edge from `parent_peer_id` (`Peer`,
/// transport direct, `regenerate` rebuilds the same edge) or an SFU edge
/// (`Sfu`, `publication` set). `None` fields are unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateTuple {
    pub kind: UpstreamKind,
    pub parent_peer_id: String,
    pub transport: Option<Transport>,
    pub regenerate: bool,
    pub publication: Option<PublicationPlan>,
}

/// What the caller allocated before `begin_current_candidate`. A `None` kind
/// is the empty reservation of a publication attachment.
#[derive(Debug, Clone, Default)]
pub struct CandidateReservation {
    pub kind: Option<ReservationKind>,
    pub edge: Option<ResourceRef>,
    pub publication: Option<ResourceRef>,
    /// Another holder's live subscription that must not be released.
    pub borrowed: bool,
    pub overlap: Option<ResourceRef>,
}

/// The producer retirement a bounded-gap transition requires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointRetirement {
    pub kind: RetirementKind,
    pub child_peer_id: String,
    pub child_session_id: String,
    pub parent_peer_id: String,
    pub parent_session_id: String,
    pub transport: Option<Transport>,
    pub connection_id: String,
    pub host_session_id: String,
    pub generation: String,
}

/// How the producer's copy budget absorbs the candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointTransition {
    pub kind: TransitionKind,
    /// `None` only for `TransitionKind::None` on an SFU reuse plan.
    pub producer_peer_id: Option<String>,
    /// Set only for `TransitionKind::BoundedGap`.
    pub retire: Option<EndpointRetirement>,
}

/// A tuple with its endpoint transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidatePlan {
    pub tuple: CandidateTuple,
    pub endpoint_transition: EndpointTransition,
}

/// A child's one upstream edge in the committed graph.
#[derive(Debug, Clone)]
pub struct CommittedEdge {
    pub kind: UpstreamKind,
    pub child_session_id: String,
    pub parent_peer_id: String,
    pub parent_session_id: String,
    pub publication_generation: String,
    pub transport: Transport,
    pub connection_id: String,
    pub usable: bool,
    pub physical_active: bool,
    /// Video attachment belongs to a publication, independently of room transport.
    pub video_generation: String,
    pub video_connection_id: String,
    pub video_ready: bool,
    pub resource: Option<ResourceRef>,
    /// The acquisition's session key stays fixed when signaling ownership rebinds.
    pub(crate) opportunity_key: String,
}

/// The single live SFU ingress.
#[derive(Debug, Clone)]
pub struct HostPublication {
    pub generation: String,
    pub host_session_id: String,
    pub connection_id: String,
    pub usable: bool,
    pub physical_active: bool,
    pub resource: Option<ResourceRef>,
}

/// The NAT-prediction attempt counter (initial attempt plus retries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionAttemptProgress {
    pub current: i64,
    pub total: i64,
}

/// The operation's live candidate attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentAttempt {
    pub tuple: CandidateTuple,
    pub revision: i64,
    pub connection_id: String,
    pub connection_attempt: Option<ConnectionAttemptProgress>,
}

/// A deep copy of the in-flight operation plus the derived wake time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationSnapshot {
    /// Fences decoded-frame proof.
    pub video_generation: String,
    /// The retained room edge, only for publication attachment.
    pub attach_connection_id: String,
    pub child_peer_id: String,
    pub child_session_id: String,
    pub demand_peer_id: String,
    pub reason: DemandReason,
    pub base_revision: i64,
    pub fact_version: i64,
    pub candidates: Vec<CandidatePlan>,
    pub cursor: usize,
    pub deadline_at_ms: i64,
    pub wake_at_ms: i64,
    pub current: Option<CurrentAttempt>,
}

/// The committed graph as the router reads it.
#[derive(Debug, Clone)]
pub struct RouteSnapshot {
    pub room_media: bool,
    pub video_generation: String,
    pub video_paused: bool,
    pub revision: i64,
    pub paused: bool,
    pub fact_version: i64,
    /// Insertion-ordered copy (child peer ID to edge).
    pub upstream_by_viewer: Map<String, CommittedEdge>,
    pub host_publication: Option<HostPublication>,
    pub operation: Option<OperationSnapshot>,
}

/// The scheduler outcome.
#[derive(Debug, Clone, Default)]
pub struct ReconcileResult {
    pub operation: Option<OperationSnapshot>,
    pub removed_peer_ids: Vec<String>,
    pub failed_peer_ids: Vec<String>,
    pub released: Vec<ResourceRef>,
}

/// The outcome of candidate settlement or active replacement.
#[derive(Debug, Clone, Default)]
pub struct SettleResult {
    pub accepted: bool,
    /// True only when the attempt committed.
    pub committed: bool,
    pub failed_peer_ids: Vec<String>,
    pub active_revision: i64,
    pub released: Vec<ResourceRef>,
}

/// The settlement outcome of sender-side evidence.
pub type SenderQualityEvidenceResult = SettleResult;

/// The outcome of the cursor-guarded candidate calls.
#[derive(Debug, Clone, Default)]
pub struct BeginResult {
    pub accepted: bool,
    pub operation: Option<OperationSnapshot>,
    pub failed_peer_ids: Vec<String>,
    pub released: Vec<ResourceRef>,
}

/// A connected participant. `route_preference: None` means no preference
/// and behaves like Auto.
#[derive(Debug, Clone)]
pub struct ParticipantInput {
    pub peer_id: String,
    pub role: Role,
    pub session_id: String,
    pub effective_downstream_capacity: i64,
    pub route_preference: Option<ViewerRoutePreference>,
}

/// Identifies one committed edge exactly. `parent_session_id` is compared
/// only for peer edges.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EdgeGuard {
    pub child_peer_id: String,
    pub child_session_id: String,
    pub route_revision: i64,
    pub connection_id: String,
    pub parent_session_id: String,
}

/// Identifies a direct edge from the parent's side.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParentEdgeGuard {
    pub parent_peer_id: String,
    pub parent_session_id: String,
    pub route_revision: i64,
    pub connection_id: String,
}

/// Identifies the Host publication exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicationGuard {
    pub host_session_id: String,
    pub route_revision: i64,
    pub generation: String,
    pub connection_id: String,
}

/// An [`EdgeGuard`] plus the replacement connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AdoptDirectConnectionInput {
    pub edge: EdgeGuard,
    pub new_connection_id: String,
}

/// Identifies the live attempt exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateGuard {
    pub child_peer_id: String,
    pub child_session_id: String,
    pub revision: i64,
    pub connection_id: String,
}

/// Identifies one candidate slot: still at `cursor` with an equal plan,
/// built at `fact_version` on `base_revision`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateCursorGuard {
    pub child_peer_id: String,
    pub child_session_id: String,
    pub base_revision: i64,
    pub fact_version: i64,
    pub cursor: usize,
    pub plan: CandidatePlan,
}

/// The `begin_current_candidate` input. Publication fields are required for
/// SFU create/replace tuples; `host_session_id` for every SFU tuple.
#[derive(Debug, Clone)]
pub struct BeginInput {
    pub guard: CandidateCursorGuard,
    pub now_ms: i64,
    pub connection_id: String,
    pub reservation: CandidateReservation,
    pub publication_generation: String,
    pub publication_connection_id: String,
    pub host_session_id: String,
}

/// The decoded publication identity in room-media mode and the Viewer's
/// one-shot relative quality approval when required.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CandidateProof {
    pub relative_quality_approved: bool,
    pub video_generation: String,
}

/// The upstream a receiver-side window was measured on; `peer_id` is set
/// only for peer upstreams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QualityUpstream {
    pub kind: UpstreamKind,
    pub peer_id: String,
}

/// One receiver-side quality window. Only the decoded, freeze and pause
/// deltas of `metrics` are read.
#[derive(Debug, Clone)]
pub struct RouteQualityEvidenceInput {
    pub child_peer_id: String,
    pub child_session_id: String,
    pub route_revision: i64,
    pub connection_id: String,
    pub upstream: QualityUpstream,
    pub presentation_epoch: i64,
    pub window_ms: i64,
    pub metrics: ViewerQualityEvidenceMetrics,
    pub accepted_at_ms: i64,
}

/// One sender-side health sample from a parent about one child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SenderQualityEvidenceInput {
    pub parent_peer_id: String,
    pub parent_session_id: String,
    pub child_peer_id: String,
    pub route_revision: i64,
    pub connection_id: String,
    pub sender_identity: Option<String>,
    pub sample_timestamp_ms: Option<i64>,
    pub state: SenderQualityState,
    pub accepted_at_ms: i64,
}

/// One Host-to-SFU ingress health sample.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfuPublisherQualityEvidenceInput {
    pub host_peer_id: String,
    pub host_session_id: String,
    pub publication_generation: String,
    pub route_revision: i64,
    pub state: SenderQualityState,
    pub sample_timestamp_ms: Option<i64>,
    pub accepted_at_ms: i64,
}

/// Belongs to the room, including across controller replacement. The caller
/// serializes access with controller calls.
#[derive(Debug, Default)]
pub struct RevisionSequence {
    latest: AtomicI64,
}

impl RevisionSequence {
    /// The latest allocated revision.
    pub fn latest(&self) -> i64 {
        self.latest.load(Ordering::Relaxed)
    }

    pub(crate) fn set(&self, value: i64) {
        self.latest.store(value, Ordering::Relaxed);
    }
}

/// Configures a controller. An empty `debug_room_id` disables the sanitized
/// route debug events.
#[derive(Clone, Default)]
pub struct Options {
    pub host_peer_id: String,
    pub debug_room_id: String,
    /// May be shared by successive controllers for the same room; `None`
    /// gives this controller its own sequence.
    pub revisions: Option<Arc<RevisionSequence>>,
    /// Receives sanitized route events; `None` disables them.
    pub debug_log: Option<DebugSink>,
    pub endpoint_media_copy_capacity: i64,
    pub operation_timeout_ms: i64,
    pub sfu_enabled: bool,
    pub sfu_only: bool,
    pub quality_convergence_enabled: bool,
    pub nat_prediction_enabled: bool,
}
