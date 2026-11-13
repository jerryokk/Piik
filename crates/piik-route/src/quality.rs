//! Receiver and native sender quality evidence and quality convergence.

use std::collections::HashSet;

use piik_ordered::Map;

use crate::candidates::{edge_tuple_key, tuple_key};
use crate::controller::{
    Operation, PERSISTENT_DEGRADED_WINDOWS, QUALITY_EVIDENCE_EXPIRY_MS, RouteQualityObservation,
    SenderQualityIdentity, SenderQualityObservation, SfuPublisherQualityObservation,
    compare_participant, is_safe_integer, safe_add,
};
use crate::debug::route_debug;
use crate::operation::{CommitReservation, operation_requires_native_candidate_proof};
use crate::{
    CandidateGuard, CandidatePlan, CandidateProof, CandidateTuple, CommittedEdge, Controller,
    DemandReason, PublicationPlan, QualityUpstream, RejectionBucket, ResourceRef,
    RouteQualityEvidenceInput, RouteQualityEvidenceResult, SenderQualityEvidenceInput,
    SenderQualityEvidenceResult, SenderQualityState, SettleResult,
    SfuPublisherQualityEvidenceInput, TransitionKind, UpstreamKind,
};

mod convergence;
mod evidence;
mod ledger;

fn sender_quality_regeneration_block_matches(
    block: &SenderQualityIdentity,
    child_session_id: &str,
    input: &SenderQualityEvidenceInput,
    sender_identity: &str,
) -> bool {
    block.child_session_id == child_session_id
        && block.parent_peer_id == input.parent_peer_id
        && block.parent_session_id == input.parent_session_id
        && block.connection_id == input.connection_id
        && block.sender_identity == sender_identity
}

fn quality_upstream_matches(edge: &CommittedEdge, upstream: &QualityUpstream) -> bool {
    if edge.kind == UpstreamKind::Peer {
        return upstream.kind == UpstreamKind::Peer && upstream.peer_id == edge.parent_peer_id;
    }
    upstream.kind == UpstreamKind::Sfu
}

/// Requires an empty upstream peer ID for SFU.
pub(crate) fn quality_observation_matches_edge(
    observation: &RouteQualityObservation,
    edge: &CommittedEdge,
) -> bool {
    if edge.kind == UpstreamKind::Peer {
        return observation.upstream_kind == UpstreamKind::Peer
            && observation.upstream_peer_id == edge.parent_peer_id;
    }
    observation.upstream_kind == UpstreamKind::Sfu && observation.upstream_peer_id.is_empty()
}

/// Appends the peer itself unless the descendant walk already reached it.
fn with_self(mut descendants: Vec<String>, peer_id: &str) -> Vec<String> {
    if !descendants.iter().any(|id| id == peer_id) {
        descendants.push(peer_id.to_owned());
    }
    descendants
}
