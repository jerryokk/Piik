//! Controller tests: direct head start, deadlines and direct convergence.

use super::*;
use crate::*;

/// Head-start a direct candidate, then commit SFU reuse for `peer_id`.
fn acquire_via_sfu(routes: &mut Controller, peer_id: &str, now: i64) -> i64 {
    let acquisition = must(routes.reconcile(now).operation);
    begin_candidate(
        routes,
        begin(now + 1, &format!("{peer_id}_head_start"), direct()),
    );
    routes.operation_expired(acquisition.wake_at_ms);
    let sfu = begin_operation(
        routes,
        begin(
            acquisition.wake_at_ms + 1,
            &format!("{peer_id}_sfu"),
            sfu_reuse(&format!("{peer_id}_subscription")),
        ),
    );
    let guard = guard_for(peer_id, current(&sfu).revision, &format!("{peer_id}_sfu"));
    assert!(
        routes
            .candidate_ready(
                &guard,
                acquisition.wake_at_ms + 2,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    acquisition.wake_at_ms
}

fn exhaust_relay_peers(routes: &mut Controller, peers: &[&str]) {
    for peer_id in peers {
        for plan in routes.build_candidates(peer_id, false) {
            if plan.tuple.kind == UpstreamKind::Peer {
                routes.consume_candidate_opportunity(peer_id, &plan);
            }
        }
    }
}

// TS 1247
#[test]
fn advances_silent_peer_only_when_another_bounded_candidate_remains() {
    let mut routes = controller(
        2,
        Options {
            operation_timeout_ms: 20_000,
            ..opts()
        },
    );
    add_viewer(&mut routes, B, 2, None);
    add_viewer(&mut routes, C, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    routes.hydrate_edge(C, peer_edge(HOST, "c_from_host"));
    add_viewer(&mut routes, A, 0, ms(0));
    let operation = must(routes.reconcile(0).operation);
    begin_candidate(&mut routes, begin(100, "first_silent_candidate", direct()));
    assert_eq!(must(routes.snapshot().operation).wake_at_ms, 5_100);
    assert!(routes.operation_expired(5_100).accepted);
    begin_candidate(
        &mut routes,
        begin(5_200, "second_silent_candidate", direct()),
    );
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.wake_at_ms, operation.deadline_at_ms);
    assert_eq!(pending.deadline_at_ms, operation.deadline_at_ms);
    assert!(!routes.operation_expired(10_200).accepted);
    assert_eq!(
        current(&must(routes.snapshot().operation)).connection_id,
        "second_silent_candidate"
    );
}

// TS 1281
#[test]
fn retains_peer_only_direct_attempt_through_total_deadline() {
    let mut routes = controller(
        1,
        Options {
            operation_timeout_ms: 20_000,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 0, ms(0));
    let operation = must(routes.reconcile(0).operation);
    begin_candidate(&mut routes, begin(100, "only_direct_candidate", direct()));
    assert_eq!(
        must(routes.snapshot().operation).wake_at_ms,
        operation.deadline_at_ms
    );
    assert!(!routes.operation_expired(5_100).accepted);
    assert_eq!(
        current(&must(routes.snapshot().operation)).connection_id,
        "only_direct_candidate"
    );
    let expired = routes.operation_expired(operation.deadline_at_ms);
    assert!(expired.accepted);
    assert_eq!(expired.failed_peer_ids, [A]);
}

// TS 1842
#[test]
fn keeps_direct_convergence_reason_immutable_until_availability_preempts() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    routes.hydrate_host_publication("publication", res("publication_resource"), "");
    add_viewer(&mut routes, A, 0, ms(0));
    let acquisition = must(routes.reconcile(0).operation);
    begin_candidate(&mut routes, begin(1, "a_head_start", direct()));
    routes.operation_expired(acquisition.wake_at_ms);
    let sfu = begin_operation(
        &mut routes,
        begin(
            acquisition.wake_at_ms + 1,
            "a_sfu",
            sfu_reuse("a_subscription"),
        ),
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&sfu).revision, "a_sfu"),
                acquisition.wake_at_ms + 2,
                None,
                CandidateProof::default()
            )
            .committed
    );
    assert_eq!(
        must(routes.reconcile(acquisition.wake_at_ms + 3).operation).reason,
        DemandReason::DirectConvergence
    );
    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(A, "", revision, "a_sfu"),
        ms(acquisition.wake_at_ms + 4)
    ));
    assert_eq!(
        must(routes.snapshot().operation).reason,
        DemandReason::DirectConvergence
    );
    let preempted = must(routes.reconcile(acquisition.wake_at_ms + 5).operation);
    assert_eq!(
        (preempted.child_peer_id.as_str(), preempted.reason),
        (A, DemandReason::EdgeUnavailable)
    );
}

// TS 3062
#[test]
fn advances_one_candidate_cursor_without_resetting_operation_deadline() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 2, None);
    let operation = must(routes.reconcile(1_000).operation);
    let deadline = operation.deadline_at_ms;
    let first = begin_operation(&mut routes, begin(1_001, "candidate_1", direct()));
    let first_revision = current(&first).revision;
    let failed = routes.candidate_failed(&guard_for(A, first_revision, "candidate_1"), 1_001);
    assert!(failed.accepted);
    assert!(failed.active_revision > first_revision);
    assert!(failed.failed_peer_ids.is_empty());
    eq_labels(&failed.released, &[]);
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.cursor, 1);
    assert_eq!(pending.deadline_at_ms, deadline);

    let second = begin_operation(
        &mut routes,
        begin_publication(
            1_002,
            "candidate_2",
            "publication_2",
            "publication_connection_2",
            sfu_create("candidate_2_edge", "candidate_2_publication"),
        ),
    );
    let guard = guard_for(A, current(&second).revision, "candidate_2");
    let late = routes.candidate_ready(&guard, deadline, None, CandidateProof::default());
    assert!(!late.accepted);
    assert_eq!(late.failed_peer_ids, [A]);
    eq_labels(
        &late.released,
        &["candidate_2_edge", "candidate_2_publication"],
    );
    no_operation(routes.snapshot().operation);
    assert!(
        !routes
            .candidate_ready(&guard, deadline + 1, None, CandidateProof::default())
            .accepted
    );
    no_operation(routes.reconcile(deadline + 1).operation);
    routes.touch_external_facts();
    assert_eq!(
        must(routes.reconcile(deadline + 2).operation).child_peer_id,
        A
    );
}

// TS 3123
#[test]
fn reserves_one_derived_deadline_stage_for_direct_and_sfu() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    assert_eq!(operation.deadline_at_ms, 10_000);
    assert_eq!(operation.wake_at_ms, 5_000);
    for want in [peer_tuple(B), sfu_tuple(PublicationPlan::Create)] {
        assert!(tuples(&operation).contains(&want));
    }
    begin_candidate(&mut routes, begin(1, "silent_direct", direct()));
    let expired = routes.operation_expired(operation.wake_at_ms);
    assert!(expired.accepted);
    assert!(expired.failed_peer_ids.is_empty());
    let pending = must(routes.snapshot().operation);
    assert_eq!(
        (pending.deadline_at_ms, pending.wake_at_ms),
        (10_000, 10_000)
    );
    assert!(pending.current.is_none());
    assert_eq!(
        pending.candidates[pending.cursor].tuple.kind,
        UpstreamKind::Sfu
    );
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            7_000,
            "sfu_subscription",
            "publication_generation",
            "publication_connection",
            sfu_create("sfu_edge", "sfu_publication"),
        ),
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&prepared).revision, "sfu_subscription"),
                9_000,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let edge = edge_of(&routes, A);
    assert!(edge.kind == UpstreamKind::Sfu && edge.usable);
}

fn host_full_two_sfu_relays(relay_capacity: i64, relays: &[&str]) -> Controller {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(D, peer_edge(HOST, "host_full"));
    routes.hydrate_host_publication("publication_1", res("publication_resource"), "");
    for peer_id in relays {
        add_viewer(&mut routes, peer_id, relay_capacity, None);
        routes.hydrate_edge(
            peer_id,
            sfu_edge(
                "publication_1",
                &format!("{peer_id}_sfu"),
                &format!("{peer_id}_subscription"),
            ),
        );
    }
    routes
}

// TS 3186
#[test]
fn uses_same_direct_head_start_after_early_hard_failure() {
    let mut routes = host_full_two_sfu_relays(1, &[B, C]);
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    assert_eq!(
        tuple_kinds(&operation),
        [UpstreamKind::Peer, UpstreamKind::Sfu, UpstreamKind::Peer]
    );
    let first = begin_operation(&mut routes, begin(1, "first_direct", direct()));
    assert!(
        routes
            .candidate_failed(&guard_for(A, current(&first).revision, "first_direct"), 2)
            .accepted
    );
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.cursor, 1);
    assert_eq!(pending.wake_at_ms, operation.wake_at_ms);
    assert_eq!(pending.candidates[1].tuple.kind, UpstreamKind::Peer);
}

// TS 3231
#[test]
fn keeps_transport_connected_direct_candidate_until_total_deadline() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(D, peer_edge(HOST, "host_full"));
    routes.hydrate_host_publication("publication", res("publication_resource"), "");
    add_viewer(&mut routes, B, 1, None);
    routes.hydrate_edge(B, sfu_edge("publication", "b_sfu", "b_subscription"));
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    let direct_op = begin_operation(&mut routes, begin(1, "a_direct", direct()));
    let revision = current(&direct_op).revision;
    assert!(
        routes
            .candidate_transport_connected(&guard_for(A, revision, "a_direct"), 1_000)
            .accepted
    );
    assert!(
        routes
            .candidate_transport_connected(&guard_for(A, revision, "a_direct"), 1_001)
            .accepted
    );
    assert!(
        !routes
            .candidate_transport_connected(&guard_for(A, revision, "stale_direct"), 1_002)
            .accepted
    );
    assert_eq!(routes.snapshot().revision, revision - 1);
    assert_eq!(
        must(routes.snapshot().operation).wake_at_ms,
        operation.deadline_at_ms
    );
    assert!(!routes.operation_expired(operation.wake_at_ms).accepted);
    assert_eq!(
        current(&must(routes.snapshot().operation)).connection_id,
        "a_direct"
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, revision, "a_direct"),
                operation.wake_at_ms + 1,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Peer, "a_direct")
    );
}

// TS 3288
#[test]
fn keeps_sfu_active_while_converging_through_sfu_fed_parent() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_host_publication("publication_generation", res("publication_resource"), "");
    routes.hydrate_edge(
        B,
        sfu_edge("publication_generation", "b_sfu", "b_subscription"),
    );
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(HOST, "c_from_host"));
    add_viewer(&mut routes, A, 0, None);
    let acquisition = must(routes.reconcile(0).operation);
    assert_eq!(acquisition.candidates[0].tuple, peer_tuple(B));
    begin_candidate(&mut routes, begin(1, "a_direct_head_start", direct()));
    routes.operation_expired(acquisition.wake_at_ms);
    let prepared = begin_operation(
        &mut routes,
        begin(
            acquisition.wake_at_ms + 1,
            "a_sfu_reuse",
            sfu_reuse("a_subscription"),
        ),
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&prepared).revision, "a_sfu_reuse"),
                acquisition.wake_at_ms + 2,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let publication = routes
        .snapshot()
        .host_publication
        .expect("expected a host publication");
    assert!(publication.generation == "publication_generation" && publication.usable);
    let edge = edge_of(&routes, A);
    assert!(
        edge.kind == UpstreamKind::Sfu
            && edge.publication_generation == "publication_generation"
            && edge.usable
    );
    let convergence = must(routes.reconcile(acquisition.wake_at_ms + 3).operation);
    assert_eq!(
        (convergence.child_peer_id.as_str(), convergence.reason),
        (A, DemandReason::DirectConvergence)
    );
    assert_eq!(convergence.candidates[0].tuple, peer_tuple(B));
    let direct_op = begin_operation(
        &mut routes,
        begin(acquisition.wake_at_ms + 4, "a_from_b", direct()),
    );
    assert_eq!(
        must(routes.snapshot().operation).wake_at_ms,
        convergence.deadline_at_ms
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Sfu, "a_sfu_reuse")
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&direct_op).revision, "a_from_b"),
                acquisition.wake_at_ms + 5,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (
            edge.kind,
            edge.parent_peer_id.as_str(),
            edge.connection_id.as_str()
        ),
        (UpstreamKind::Peer, B, "a_from_b")
    );
}

// TS 3380
#[test]
fn advances_exact_direct_convergence_failures_without_failing_sfu() {
    let mut routes = host_full_two_sfu_relays(1, &[B, C]);
    // These fixture SFU relays already exhausted their own upstream opportunities.
    exhaust_relay_peers(&mut routes, &[B, C]);
    add_viewer(&mut routes, A, 0, ms(0));
    let acquisition = must(routes.reconcile(0).operation);
    let head_start = acquisition.candidates[0].tuple.clone();
    assert_eq!(head_start.kind, UpstreamKind::Peer);
    let other_parent = if head_start.parent_peer_id == B { C } else { B };
    begin_candidate(&mut routes, begin(1, "a_direct_head_start", direct()));
    routes.operation_expired(acquisition.wake_at_ms);
    let sfu = begin_operation(
        &mut routes,
        begin(
            acquisition.wake_at_ms + 1,
            "a_sfu",
            sfu_reuse("a_subscription"),
        ),
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&sfu).revision, "a_sfu"),
                acquisition.wake_at_ms + 2,
                None,
                CandidateProof::default()
            )
            .accepted
    );

    let first = must(routes.reconcile(acquisition.wake_at_ms + 3).operation);
    assert_eq!(first.candidates[0].tuple, peer_tuple(other_parent));
    let failed = begin_operation(
        &mut routes,
        begin(acquisition.wake_at_ms + 4, "a_direct_followup", direct()),
    );
    let result = routes.candidate_failed(
        &guard_for(A, current(&failed).revision, "a_direct_followup"),
        acquisition.wake_at_ms + 5,
    );
    assert!(result.failed_peer_ids.is_empty());
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Sfu, "a_sfu")
    );
    let retry = must(routes.reconcile(acquisition.wake_at_ms + 6).operation);
    assert_eq!(
        retry.candidates[0].tuple,
        peer_tuple(&head_start.parent_peer_id)
    );
}

// TS 3451
#[test]
fn round_robins_background_direct_convergence_between_sfu_viewers() {
    let mut routes = controller(2, sfu_opts());
    routes.hydrate_host_publication("publication", res("publication_resource"), "");
    for peer_id in [B, C] {
        add_viewer(&mut routes, peer_id, 2, None);
        routes.hydrate_edge(
            peer_id,
            sfu_edge(
                "publication",
                &format!("{peer_id}_sfu"),
                &format!("{peer_id}_subscription"),
            ),
        );
    }
    exhaust_relay_peers(&mut routes, &[B, C]);
    let mut now = 0;
    for peer_id in [A, D] {
        add_viewer(&mut routes, peer_id, 0, ms(now));
        now = acquire_via_sfu(&mut routes, peer_id, now) + 3;
    }
    let first = must(routes.reconcile(now).operation);
    assert_eq!(
        (first.child_peer_id.as_str(), first.reason),
        (A, DemandReason::DirectConvergence)
    );
    let attempt = begin_operation(&mut routes, begin(now + 1, "a_background_direct", direct()));
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&attempt).revision, "a_background_direct"),
                now + 2
            )
            .accepted
    );
    let next = must(routes.reconcile(now + 3).operation);
    assert_eq!(
        (next.child_peer_id.as_str(), next.reason),
        (D, DemandReason::DirectConvergence)
    );
}

// TS 3516
#[test]
fn preempts_direct_convergence_when_another_viewer_needs_route() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_host_publication("publication_1", res("publication_resource"), "");
    routes.hydrate_edge(B, sfu_edge("publication_1", "b_sfu", "b_subscription"));
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(HOST, "c_from_host"));
    add_viewer(&mut routes, A, 0, ms(0));
    let acquisition = must(routes.reconcile(0).operation);
    begin_candidate(&mut routes, begin(1, "a_direct_head_start", direct()));
    routes.operation_expired(acquisition.wake_at_ms);
    let sfu = begin_operation(
        &mut routes,
        begin(
            acquisition.wake_at_ms + 1,
            "a_sfu",
            sfu_reuse("a_subscription"),
        ),
    );
    routes.candidate_ready(
        &guard_for(A, current(&sfu).revision, "a_sfu"),
        acquisition.wake_at_ms + 2,
        None,
        CandidateProof::default(),
    );
    assert_eq!(
        must(routes.reconcile(acquisition.wake_at_ms + 3).operation).reason,
        DemandReason::DirectConvergence
    );
    begin_candidate(
        &mut routes,
        begin(
            acquisition.wake_at_ms + 4,
            "a_direct_pending",
            direct_overlap("a_overlap"),
        ),
    );
    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(C, "host_session", revision, "c_from_host"),
        ms(acquisition.wake_at_ms + 5)
    ));
    let reconciled = routes.reconcile(acquisition.wake_at_ms + 6);
    eq_labels(&reconciled.released, &["a_overlap"]);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Sfu, "a_sfu")
    );
    let preempted = must(reconciled.operation);
    assert_eq!(
        (preempted.child_peer_id.as_str(), preempted.reason),
        (C, DemandReason::EdgeUnavailable)
    );
}
