//! Route controller smoke tests.

use piik_protocol::{Role, RouteDiagnosticFinalRoute, RouteDiagnosticParent};

use crate::*;

/// A Host and two Viewers join, reconcile starts one join operation for the
/// first Viewer with the Host first, and begin + ready commit that edge at
/// the attempt's revision.
#[test]
fn smoke_commits_one_direct_candidate() {
    let host = "smoke_host_1234";
    let first = "smoke_viewer_a_1";
    let second = "smoke_viewer_b_1";
    let mut routes = Controller::new(Options {
        host_peer_id: host.to_owned(),
        endpoint_media_copy_capacity: 2,
        operation_timeout_ms: 10_000,
        ..Options::default()
    });
    let input = |peer_id: &str, role, session_id: String| ParticipantInput {
        peer_id: peer_id.to_owned(),
        role,
        session_id,
        effective_downstream_capacity: 2,
        route_preference: None,
    };
    routes.upsert_participant(input(host, Role::Host, "host_s".to_owned()), None);
    let now = 1_000;
    for viewer in [first, second] {
        routes.upsert_participant(
            input(viewer, Role::Viewer, format!("{viewer}_s")),
            Some(now),
        );
    }

    let operation = routes
        .reconcile(now)
        .operation
        .expect("expected reconcile to start an operation");
    assert_eq!(operation.child_peer_id, first);
    assert_eq!(operation.reason, DemandReason::Join);
    assert_eq!(operation.candidates[0].tuple, CandidateTuple::peer(host));
    assert_eq!(operation.wake_at_ms, operation.deadline_at_ms);
    assert_eq!(operation.deadline_at_ms, now + 10_000);

    let begin = routes.begin_current_candidate(BeginInput {
        guard: CandidateCursorGuard {
            child_peer_id: operation.child_peer_id.clone(),
            child_session_id: operation.child_session_id.clone(),
            base_revision: operation.base_revision,
            fact_version: operation.fact_version,
            cursor: operation.cursor,
            plan: operation.candidates[operation.cursor].clone(),
        },
        now_ms: now + 1,
        connection_id: "conn_a".to_owned(),
        reservation: CandidateReservation {
            kind: Some(ReservationKind::Direct),
            ..CandidateReservation::default()
        },
        publication_generation: String::new(),
        publication_connection_id: String::new(),
        host_session_id: String::new(),
    });
    assert!(begin.accepted);
    let revision = begin
        .operation
        .as_ref()
        .unwrap()
        .current
        .as_ref()
        .unwrap()
        .revision;
    assert_eq!(
        routes.snapshot().revision,
        operation.base_revision,
        "begin must not advance the advertised revision"
    );

    let settled = routes.candidate_ready(
        &CandidateGuard {
            child_peer_id: operation.child_peer_id.clone(),
            child_session_id: operation.child_session_id.clone(),
            revision,
            connection_id: "conn_a".to_owned(),
        },
        now + 2,
        None,
        CandidateProof::default(),
    );
    assert!(settled.accepted && settled.committed && settled.released.is_empty());
    let snapshot = routes.snapshot();
    assert_eq!(snapshot.revision, revision);
    assert!(snapshot.operation.is_none());
    let edge = snapshot.upstream_by_viewer.get(&first.to_owned()).unwrap();
    assert!(
        edge.kind == UpstreamKind::Peer
            && edge.parent_peer_id == host
            && edge.connection_id == "conn_a"
    );
    assert!(edge.usable && edge.physical_active);
    assert_eq!(
        routes.reconcile(now + 3).operation.unwrap().child_peer_id,
        second
    );
    let diagnostic = routes.route_diagnostic_snapshot(now + 3);
    assert_eq!(diagnostic.children.len(), 2);
    assert_eq!(diagnostic.children[0].parent, RouteDiagnosticParent::Host);
    assert_eq!(
        diagnostic.children[0].final_route,
        RouteDiagnosticFinalRoute::Direct
    );
}
