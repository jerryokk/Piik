//! Builders for begin inputs, reservations, guards and fixture edges.

use super::harness::{must, res, session_of};
use crate::*;

/// A `BeginInput` whose guard `begin_candidate` derives from the current operation.
pub(crate) fn begin(
    now_ms: i64,
    connection_id: &str,
    reservation: CandidateReservation,
) -> BeginInput {
    BeginInput {
        guard: CandidateCursorGuard {
            child_peer_id: String::new(),
            child_session_id: String::new(),
            base_revision: 0,
            fact_version: 0,
            cursor: 0,
            plan: CandidatePlan::bare(CandidateTuple::peer("")),
        },
        now_ms,
        connection_id: connection_id.to_owned(),
        reservation,
        publication_generation: String::new(),
        publication_connection_id: String::new(),
        host_session_id: String::new(),
    }
}

/// `begin` for an SFU create/replace tuple.
pub(crate) fn begin_publication(
    now_ms: i64,
    connection_id: &str,
    generation: &str,
    publication_connection_id: &str,
    reservation: CandidateReservation,
) -> BeginInput {
    BeginInput {
        publication_generation: generation.to_owned(),
        publication_connection_id: publication_connection_id.to_owned(),
        ..begin(now_ms, connection_id, reservation)
    }
}

/// Ports `beginCandidate(routes, input)`: non-direct reservations default
/// the Host session to `host_session`.
#[track_caller]
pub(crate) fn begin_candidate(routes: &mut Controller, mut input: BeginInput) -> BeginResult {
    input.guard = super::harness::cursor_guard(&must(routes.snapshot().operation));
    if input.reservation.kind != Some(ReservationKind::Direct) && input.host_session_id.is_empty() {
        input.host_session_id = "host_session".to_owned();
    }
    routes.begin_current_candidate(input)
}

/// Ports `beginCandidate(...).operation!`.
#[track_caller]
pub(crate) fn begin_operation(routes: &mut Controller, input: BeginInput) -> OperationSnapshot {
    must(begin_candidate(routes, input).operation)
}

#[track_caller]
pub(crate) fn skip_candidate(routes: &mut Controller, now_ms: i64) -> BeginResult {
    let guard = super::harness::cursor_guard(&must(routes.snapshot().operation));
    routes.skip_current_candidate(&guard, now_ms, RejectionBucket::Stale)
}

#[track_caller]
pub(crate) fn current(operation: &OperationSnapshot) -> &CurrentAttempt {
    operation.current.as_ref().expect("expected a live attempt")
}

#[track_caller]
pub(crate) fn commit_current(
    routes: &mut Controller,
    now_ms: i64,
    connection_id: &str,
) -> CandidateGuard {
    let prepared = begin_operation(routes, begin(now_ms, connection_id, direct()));
    let guard = CandidateGuard {
        child_peer_id: prepared.child_peer_id.clone(),
        child_session_id: prepared.child_session_id.clone(),
        revision: current(&prepared).revision,
        connection_id: connection_id.to_owned(),
    };
    assert!(
        routes
            .candidate_ready(&guard, now_ms + 1, None, CandidateProof::default())
            .accepted
    );
    guard
}

pub(crate) fn guard_for(peer_id: &str, revision: i64, connection_id: &str) -> CandidateGuard {
    CandidateGuard {
        child_peer_id: peer_id.to_owned(),
        child_session_id: session_of(peer_id),
        revision,
        connection_id: connection_id.to_owned(),
    }
}

pub(crate) fn edge_guard(
    child_peer_id: &str,
    parent_session_id: &str,
    revision: i64,
    connection_id: &str,
) -> EdgeGuard {
    EdgeGuard {
        child_peer_id: child_peer_id.to_owned(),
        child_session_id: session_of(child_peer_id),
        parent_session_id: parent_session_id.to_owned(),
        route_revision: revision,
        connection_id: connection_id.to_owned(),
    }
}

pub(crate) fn direct() -> CandidateReservation {
    CandidateReservation {
        kind: Some(ReservationKind::Direct),
        ..CandidateReservation::default()
    }
}

pub(crate) fn direct_overlap(overlap: &str) -> CandidateReservation {
    CandidateReservation {
        overlap: Some(res(overlap)),
        ..direct()
    }
}

pub(crate) fn sfu_reuse(edge: &str) -> CandidateReservation {
    CandidateReservation {
        kind: Some(ReservationKind::SfuReuse),
        edge: Some(res(edge)),
        ..CandidateReservation::default()
    }
}

pub(crate) fn sfu_create(edge: &str, publication: &str) -> CandidateReservation {
    CandidateReservation {
        kind: Some(ReservationKind::SfuCreate),
        edge: Some(res(edge)),
        publication: Some(res(publication)),
        ..CandidateReservation::default()
    }
}

pub(crate) fn sfu_create_overlap(
    edge: &str,
    publication: &str,
    overlap: &str,
) -> CandidateReservation {
    CandidateReservation {
        overlap: Some(res(overlap)),
        ..sfu_create(edge, publication)
    }
}
