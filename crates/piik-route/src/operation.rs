//! The single serial route operation: scheduling, candidate cursor,
//! deadlines, commit and rollback.
//!
//! The shared types and helpers live here; admission, settle, reconcile and
//! scheduling methods sit in the sibling modules.

use crate::candidates::{assert_reservation, candidate_plan_equals, tuple_key};
use crate::controller::{
    Attempt, DirectContinuation, MAX_DIRECT_HEAD_START_MS, Operation, PERSISTENT_DEGRADED_WINDOWS,
    QUALITY_EVIDENCE_EXPIRY_MS, ResourceSet, SenderQualityIdentity, dedupe_strings,
    reservation_resources,
};
use crate::debug::route_debug;
use crate::{
    BeginInput, BeginResult, CandidateCursorGuard, CandidateGuard, CandidatePlan, CandidateProof,
    CandidateReservation, CandidateTuple, CommittedEdge, Controller, CurrentAttempt, DemandReason,
    FinalRoute, HostPublication, OperationSnapshot, PublicationPlan, ReconcileResult,
    RejectionBucket, ResourceRef, RetirementKind, SenderQualityState, SettleResult, TransitionKind,
    Transport, UpstreamKind,
};

mod admission;
mod reconcile;
mod schedule;
mod settle;

/// Accepts or rejects the admission commit of a candidate's reservation;
/// `None` always accepts.
pub type CommitReservation<'a> = Option<&'a dyn Fn(&CandidateReservation) -> bool>;

/// The private `validate_or_advance` result.
#[derive(Default)]
pub(crate) struct Validation {
    pub(crate) released: Vec<ResourceRef>,
    pub(crate) exhausted_peer_ids: Vec<String>,
    pub(crate) expired: bool,
    pub(crate) consumed_guard: bool,
}

/// Deduplicates while preserving argument order.
fn failed_peer_ids_from(results: &[&Validation]) -> Vec<String> {
    dedupe_strings(
        results
            .iter()
            .flat_map(|result| result.exhausted_peer_ids.iter().cloned()),
    )
}

fn concat(
    mut first: Vec<ResourceRef>,
    second: impl IntoIterator<Item = ResourceRef>,
) -> Vec<ResourceRef> {
    first.extend(second);
    first
}

pub(crate) fn is_background_convergence(reason: DemandReason) -> bool {
    matches!(
        reason,
        DemandReason::DirectConvergence
            | DemandReason::QualityConvergence
            | DemandReason::RootConvergence
    )
}

pub(crate) fn is_availability_operation(reason: DemandReason) -> bool {
    reason != DemandReason::PublicationAttach
        && reason != DemandReason::SfuBootstrap
        && !is_background_convergence(reason)
}

pub(crate) fn operation_requires_native_candidate_proof(op: Option<&Operation>) -> bool {
    op.is_some_and(|op| {
        op.reason == DemandReason::RootConvergence
            || (op.reason == DemandReason::QualityConvergence
                && op
                    .current
                    .as_ref()
                    .is_some_and(|current| current.tuple.kind == UpstreamKind::Sfu))
    })
}

struct DirectContinuationPick {
    child_peer_id: String,
    plan: CandidatePlan,
}

impl Controller {
    /// Half the operation timeout bounded to 1..5000 ms.
    pub(crate) fn direct_head_start_ms(&self) -> i64 {
        MAX_DIRECT_HEAD_START_MS.min((self.operation_timeout_ms / 2).max(1))
    }

    fn settle(
        &self,
        accepted: bool,
        failed_peer_ids: Vec<String>,
        released: Vec<ResourceRef>,
    ) -> SettleResult {
        SettleResult {
            accepted,
            committed: false,
            failed_peer_ids,
            active_revision: self.revision,
            released,
        }
    }

    /// A deep copy plus the derived wake time.
    pub(crate) fn operation_snapshot(&self) -> Option<OperationSnapshot> {
        let op = self.operation.as_ref()?;
        Some(OperationSnapshot {
            video_generation: op.video_generation.clone(),
            attach_connection_id: op.attach_connection_id.clone(),
            child_peer_id: op.child_peer_id.clone(),
            child_session_id: op.child_session_id.clone(),
            demand_peer_id: op.demand_peer_id.clone(),
            reason: op.reason,
            base_revision: op.base_revision,
            fact_version: op.built_at_fact_version,
            candidates: op.candidates.clone(),
            cursor: op.cursor,
            deadline_at_ms: op.deadline_at_ms,
            wake_at_ms: self.operation_wake_at(op),
            current: op.current.as_ref().map(|current| CurrentAttempt {
                tuple: current.tuple.clone(),
                revision: current.revision,
                connection_id: current.connection_id.clone(),
                connection_attempt: current.connection_attempt,
            }),
        })
    }
}

/// Reports whether the live candidate's own sender is persistently limited.
fn candidate_sender_persistently_degraded(att: &Attempt, now_ms: i64) -> bool {
    att.sender_quality_state == Some(SenderQualityState::Degraded)
        && att
            .sender_quality_accepted_at_ms
            .is_some_and(|at| now_ms < at + QUALITY_EVIDENCE_EXPIRY_MS)
        && att.sender_quality_consecutive_degraded_windows >= PERSISTENT_DEGRADED_WINDOWS
}
