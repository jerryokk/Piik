//! Synchronous decisions over one room's media route graph, its single
//! in-flight route operation, the quality/opportunity ledgers and the
//! privacy-safe diagnostic snapshot.
//!
//! The controller starts no timers, never locks and returns resource effects
//! to its caller. Optional diagnostics invoke the injected sink synchronously.
//! Every method is a state transition over the `now_ms` the caller passes in;
//! the caller serializes every call and releases exactly the resources a
//! method returns, in order.
//!
//! Resources are compared by `Arc` identity. Invalid graphs, reservations or
//! constructor options are programming errors and panic; ordinary unavailable
//! routes remain decision outcomes.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use piik_ordered::Map;

mod api;
mod candidates;
mod controller;
mod debug;
mod operation;
mod opportunities;
mod policy;
mod publication;
mod quality;
mod relay_volunteer;
mod root_convergence;
mod sfu_bootstrap;
mod timing;
mod voice;

#[cfg(test)]
mod tests;

pub use api::*;
pub use debug::DebugSink;
pub use operation::CommitReservation;
pub use relay_volunteer::MAX_RELAY_VOLUNTEER_CHILDREN;

use controller::{
    DirectContinuation, Operation, Participant, RouteQualityObservation, RouteTimingRecord,
    SenderQualityIdentity, SenderQualityObservation, SfuBootstrapIntent,
    SfuPublisherQualityObservation,
};

/// One room's route controller. It never locks: the caller serializes every
/// call. Ledgers whose order never reaches a decision use `HashMap`; every
/// order-bearing collection is an insertion-ordered [`Map`].
pub struct Controller {
    // `host_peer_id` is mutable because `rebind_host_identity` reassigns it;
    // the other options are copied once (the route policy may change between
    // shares).
    host_peer_id: String,
    debug_room_id: String,
    debug_log: Option<DebugSink>,
    endpoint_media_copy_capacity: i64,
    operation_timeout_ms: i64,
    sfu_enabled: bool,
    sfu_only: bool,
    quality_convergence_enabled: bool,
    nat_prediction_enabled: bool,

    // Insertion order is load-bearing for participants, upstream_by_viewer and
    // direct_continuations; the opportunity ledgers and the retiring set are
    // ordered for the same delete-while-iterating semantics.
    participants: Map<String, Participant>,
    upstream_by_viewer: Map<String, CommittedEdge>,
    host_publication: Option<HostPublication>,
    operation: Option<Operation>,
    revision: i64,
    revisions: Arc<RevisionSequence>,
    fact_version: i64,
    next_join_order: i64,
    paused: bool,
    room_media: bool,
    voice_listeners: HashSet<String>,
    video_generation: String,
    video_paused: bool,
    route_timings: HashMap<String, RouteTimingRecord>,
    quality_observations: HashMap<String, RouteQualityObservation>,
    sender_quality_observations: HashMap<String, SenderQualityObservation>,
    /// Tri-state: absent / false / true.
    sender_quality_baselines: HashMap<String, bool>,
    sender_quality_regeneration_blocks: HashMap<String, SenderQualityIdentity>,
    sfu_publisher_quality_observation: Option<SfuPublisherQualityObservation>,
    quality_baselines_pending: HashSet<String>,
    direct_continuations: Map<String, DirectContinuation>,
    retiring_publication_generations: Map<String, ()>,
    sfu_bootstrap_intent: Option<SfuBootstrapIntent>,
    consumed_sfu_bootstrap_opportunities: Map<String, i64>,
    root_convergence_root_peer_id: String,
}
