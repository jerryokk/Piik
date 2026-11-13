//! Controller tests: quality aggregates and the diagnostic snapshot.

use piik_protocol::{
    RouteDiagnosticBucket, RouteDiagnosticChild, RouteDiagnosticFinalRoute,
    RouteDiagnosticOperation, RouteDiagnosticParent, RouteDiagnosticQuality,
    RouteDiagnosticSnapshot, RouteDiagnosticStage, ToWire,
};

use super::*;
use crate::*;

// TS 1305
#[test]
fn keeps_only_fresh_exact_edge_quality_shadow_aggregates() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 1, ms(0));
    routes.reconcile(10);
    commit_current(&mut routes, 20, "a_quality_connection");
    let active = routes.snapshot();
    let window = |revision, epoch, at, metrics| {
        quality_window(
            A,
            revision,
            "a_quality_connection",
            peer_upstream(HOST),
            epoch,
            at,
            metrics,
        )
    };

    assert_eq!(
        routes.observe_quality_evidence(window(active.revision, 1, 1_000, quality_metrics())),
        RouteQualityEvidenceResult::Observed
    );
    assert_eq!(routes.snapshot().revision, active.revision);
    assert_eq!(routes.snapshot().fact_version, active.fact_version);
    add_viewer(&mut routes, B, 0, ms(1_100));
    routes.reconcile(1_200);
    commit_current(&mut routes, 1_210, "b_quality_connection");
    let quality = routes.route_diagnostic_snapshot(1_300).children[0]
        .quality
        .expect("expected quality aggregates");
    assert_eq!(
        quality,
        RouteDiagnosticQuality {
            eligible_windows: 1,
            eligible_duration_ms: 2_000,
            freeze_windows: 1,
            freeze_count: 1,
            freeze_duration_ms: 250,
            pause_count: 0,
            pause_duration_ms: 0,
        }
    );
    assert!(
        routes.route_diagnostic_snapshot(5_999).children[0]
            .quality
            .is_some(),
        "quality expired before 6_000"
    );
    assert!(
        routes.route_diagnostic_snapshot(6_000).children[0]
            .quality
            .is_none(),
        "quality did not expire at 6_000"
    );

    let next = routes.revision();
    let mut null_decoded = quality_metrics();
    null_decoded.frames_decoded_delta = None;
    assert_eq!(
        routes.observe_quality_evidence(window(next, 2, 7_000, null_decoded)),
        RouteQualityEvidenceResult::Accepted
    );
    assert!(
        routes.route_diagnostic_snapshot(7_100).children[0]
            .quality
            .is_none()
    );
    assert_eq!(
        routes.observe_quality_evidence(window(next, 2, 7_100, quality_metrics())),
        RouteQualityEvidenceResult::Observed
    );
    assert_eq!(
        routes.observe_quality_evidence(window(next, 1, 7_200, quality_metrics())),
        RouteQualityEvidenceResult::Rejected
    );

    routes.set_paused(true, ms(7_300));
    assert!(
        routes.route_diagnostic_snapshot(7_301).children[0]
            .quality
            .is_none(),
        "expected no quality while paused"
    );
}

// TS 2489
#[test]
fn clears_quality_shadow_for_relay_subtree_when_source_changes() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(A, "b_from_a"));
    routes.hydrate_edge(C, peer_edge(B, "c_from_b"));
    let revision = routes.revision();
    let relays = [(B, A, "b_from_a"), (C, B, "c_from_b")];
    for (child, parent, connection) in relays {
        let window = quality_window(
            child,
            revision,
            connection,
            peer_upstream(parent),
            1,
            1_000,
            quality_metrics(),
        );
        assert_eq!(
            routes.observe_quality_evidence(window),
            RouteQualityEvidenceResult::Observed
        );
    }
    let with_quality = |routes: &Controller, now| {
        routes
            .route_diagnostic_snapshot(now)
            .children
            .iter()
            .filter(|child| child.quality.is_some())
            .count()
    };
    assert_eq!(with_quality(&routes, 1_001), 2);

    let adopt = AdoptDirectConnectionInput {
        edge: edge_guard(A, "host_session", revision, "a_from_host"),
        new_connection_id: "a_from_host_recovered".to_owned(),
    };
    assert!(routes.adopt_direct_connection(adopt, 1_002).accepted);
    assert_eq!(with_quality(&routes, 1_002), 0);

    for (at, expected) in [
        (1_100, RouteQualityEvidenceResult::Accepted),
        (3_100, RouteQualityEvidenceResult::Observed),
    ] {
        for (child, parent, connection) in relays {
            let window = quality_window(
                child,
                revision,
                connection,
                peer_upstream(parent),
                1,
                at,
                quality_metrics(),
            );
            assert_eq!(routes.observe_quality_evidence(window), expected);
        }
    }
    assert_eq!(with_quality(&routes, 3_101), 2);
}

// TS 2569
#[test]
fn retains_one_latest_timing_sample_for_twenty_viewer_burst() {
    let mut routes = controller(2, opts());
    let viewers: Vec<String> = (0..20)
        .map(|index| format!("burst_{index:02}_12345678"))
        .collect();
    for peer_id in &viewers {
        add_viewer(&mut routes, peer_id, 2, ms(0));
    }
    for (index, peer_id) in viewers.iter().enumerate() {
        let started = (index as i64 + 1) * 10;
        assert_eq!(
            &must(routes.reconcile(started).operation).child_peer_id,
            peer_id
        );
        commit_current(&mut routes, started + 1, &format!("{peer_id}_connection"));
    }
    let snapshot = routes.route_diagnostic_snapshot(250);
    assert_eq!(snapshot.children.len(), 20);
    for (index, child) in snapshot.children.iter().enumerate() {
        assert_eq!(child.queue_wait_ms, Some((index as i64 + 1) * 10));
        assert_eq!(child.final_route, RouteDiagnosticFinalRoute::Direct);
    }
}

// TS 2598
#[test]
fn projects_latest_route_timing_through_snapshot_local_ordinals() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 1, ms(100));
    assert_eq!(
        routes.route_diagnostic_snapshot(150),
        RouteDiagnosticSnapshot {
            children: vec![RouteDiagnosticChild {
                ordinal: 1,
                parent: RouteDiagnosticParent::None,
                effective_capacity: 1,
                child_count: 0,
                demand_age_ms: Some(50),
                queue_wait_ms: None,
                candidate_start_ms: None,
                first_decoded_frame_ms: None,
                final_ms: None,
                final_route: RouteDiagnosticFinalRoute::Waiting,
                rejection_bucket: RouteDiagnosticBucket::None,
                quality: None,
            }],
            operation: None,
        }
    );

    let operation = must(routes.reconcile(160).operation);
    assert_eq!(
        routes.route_diagnostic_snapshot(165).operation,
        Some(RouteDiagnosticOperation {
            child_ordinal: 1,
            reason: DemandReason::Join,
            stage: RouteDiagnosticStage::Admission,
            cursor: 0,
            candidate_count: operation.candidates.len() as i64,
        })
    );
    let prepared = begin_operation(&mut routes, begin(170, "a_connection", direct()));
    let pending = routes.route_diagnostic_snapshot(180);
    assert_eq!(pending.children.len(), 1);
    assert_eq!(pending.children[0].queue_wait_ms, Some(60));
    assert_eq!(pending.children[0].candidate_start_ms, Some(70));
    assert_eq!(pending.children[0].first_decoded_frame_ms, None);
    assert_eq!(pending.children[0].final_ms, None);
    let pending_operation = pending.operation.expect("expected a diagnostic operation");
    assert_eq!(pending_operation.child_ordinal, 1);
    assert_eq!(pending_operation.stage, RouteDiagnosticStage::FirstFrame);
    let guard = guard_for(A, current(&prepared).revision, "a_connection");
    assert!(
        routes
            .candidate_ready(&guard, 230, None, CandidateProof::default())
            .accepted
    );
    let settled = routes.route_diagnostic_snapshot(240);
    assert_eq!(settled.children.len(), 1);
    let child = &settled.children[0];
    assert_eq!(child.parent, RouteDiagnosticParent::Host);
    assert_eq!(child.queue_wait_ms, Some(60));
    assert_eq!(child.candidate_start_ms, Some(70));
    assert_eq!(child.first_decoded_frame_ms, Some(130));
    assert_eq!(child.final_ms, Some(130));
    assert_eq!(child.final_route, RouteDiagnosticFinalRoute::Direct);
    assert_eq!(child.rejection_bucket, RouteDiagnosticBucket::None);
    assert!(settled.operation.is_none());
    let serialized = settled.to_wire().to_string();
    assert!(
        !serialized.contains(A),
        "diagnostic snapshot leaks the peer ID: {serialized}"
    );
    assert!(
        !serialized.contains("a_connection"),
        "diagnostic snapshot leaks the connection ID: {serialized}"
    );

    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(A, "host_session", revision, "a_connection"),
        ms(300)
    ));
    let waiting = routes.route_diagnostic_snapshot(320).children[0].clone();
    assert_eq!(waiting.demand_age_ms, Some(20));
    assert_eq!(
        (
            waiting.queue_wait_ms,
            waiting.candidate_start_ms,
            waiting.first_decoded_frame_ms,
            waiting.final_ms
        ),
        (None, None, None, None)
    );
    assert_eq!(waiting.final_route, RouteDiagnosticFinalRoute::Waiting);
    assert_eq!(waiting.rejection_bucket, RouteDiagnosticBucket::None);

    assert!(routes.confirm_departure(A, ms(330)));
    let departed = routes.route_diagnostic_snapshot(330);
    assert!(departed.children.is_empty());
    assert!(departed.operation.is_none());
}

// TS 2704
#[test]
fn records_bounded_candidate_failure_without_retaining_route_identity() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 0, ms(0));
    routes.reconcile(10);
    let prepared = begin_operation(&mut routes, begin(20, "failed_candidate", direct()));
    let failed = routes.candidate_failed(
        &guard_for(A, current(&prepared).revision, "failed_candidate"),
        30,
    );
    assert!(failed.accepted);
    assert_eq!(failed.failed_peer_ids, [A]);
    let child = routes.route_diagnostic_snapshot(30).children[0].clone();
    assert_eq!(child.final_ms, Some(30));
    assert_eq!(child.final_route, RouteDiagnosticFinalRoute::Failed);
    assert_eq!(
        child.rejection_bucket,
        RouteDiagnosticBucket::CandidateFailed
    );

    routes.dispose();
    let disposed = routes.route_diagnostic_snapshot(40);
    assert!(disposed.children.is_empty());
    assert!(disposed.operation.is_none());
}

// TS 2737
#[test]
fn reports_current_child_that_has_no_usable_candidate() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_connection"));
    add_viewer(&mut routes, B, 0, ms(0));
    let result = routes.reconcile(10);
    no_operation(result.operation);
    assert_eq!(result.failed_peer_ids, [B]);
    let child = routes.route_diagnostic_snapshot(10).children[1].clone();
    assert_eq!(child.final_ms, Some(10));
    assert_eq!(child.final_route, RouteDiagnosticFinalRoute::Failed);
    assert_eq!(
        child.rejection_bucket,
        RouteDiagnosticBucket::CandidateFailed
    );
}

// TS 2753
#[test]
fn records_sfu_admission_waiting_without_advancing_route_cursor() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, ms(0));
    let direct_op = must(routes.reconcile(10).operation);
    assert_eq!(
        tuple_kinds(&direct_op),
        [UpstreamKind::Peer, UpstreamKind::Sfu]
    );
    assert!(
        routes
            .skip_current_candidate(
                &cursor_guard(&direct_op),
                20,
                RejectionBucket::CandidateFailed
            )
            .accepted
    );
    let sfu = must(routes.snapshot().operation);
    assert!(
        routes.note_current_candidate_rejection(&cursor_guard(&sfu), RejectionBucket::SfuAdmission)
    );
    let snapshot = routes.route_diagnostic_snapshot(30);
    assert_eq!(snapshot.children.len(), 1);
    assert_eq!(
        snapshot.children[0].final_route,
        RouteDiagnosticFinalRoute::Waiting
    );
    assert_eq!(
        snapshot.children[0].rejection_bucket,
        RouteDiagnosticBucket::SfuAdmission
    );
    assert_eq!(
        snapshot.operation,
        Some(RouteDiagnosticOperation {
            child_ordinal: 1,
            reason: DemandReason::Join,
            stage: RouteDiagnosticStage::Admission,
            cursor: 1,
            candidate_count: 2,
        })
    );
}

// TS 2791
#[test]
fn distinguishes_first_frame_timeout_from_total_operation_deadline() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, ms(0));
    let direct_op = must(routes.reconcile(10).operation);
    begin_candidate(&mut routes, begin(20, "silent_direct", direct()));
    assert!(routes.operation_expired(direct_op.wake_at_ms).accepted);
    let snapshot = routes.route_diagnostic_snapshot(direct_op.wake_at_ms);
    assert_eq!(snapshot.children.len(), 1);
    assert_eq!(snapshot.children[0].candidate_start_ms, None);
    assert_eq!(snapshot.children[0].final_ms, None);
    assert_eq!(
        snapshot.children[0].final_route,
        RouteDiagnosticFinalRoute::Waiting
    );
    assert_eq!(
        snapshot.children[0].rejection_bucket,
        RouteDiagnosticBucket::FirstFrameTimeout
    );
    let operation = snapshot.operation.expect("expected a diagnostic operation");
    assert_eq!(operation.cursor, 1);
    assert_eq!(operation.stage, RouteDiagnosticStage::Admission);

    let deadline = must(routes.snapshot().operation).deadline_at_ms;
    let expired = routes.operation_expired(deadline);
    assert!(expired.accepted);
    assert_eq!(expired.failed_peer_ids, [A]);
    let child = routes.route_diagnostic_snapshot(deadline).children[0].clone();
    assert_eq!(child.final_ms, Some(deadline));
    assert_eq!(child.final_route, RouteDiagnosticFinalRoute::Failed);
    assert_eq!(
        child.rejection_bucket,
        RouteDiagnosticBucket::OperationDeadline
    );
}
