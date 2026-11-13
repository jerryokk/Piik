//! Controller tests: quality candidate ordering, Host relief and root convergence.

use super::controller_quality_proof::{approved, degraded_pair};
use super::*;
use crate::*;

use SenderQualityState::{Degraded, Healthy};

// TS 1010
#[test]
fn never_treats_one_degraded_host_edge_as_sfu_fanout_pressure() {
    let mut routes = controller(
        2,
        Options {
            sfu_enabled: true,
            quality_convergence_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_host_publication(
        "publication_generation_12345678",
        res("publication_resource"),
        "",
    );
    routes.hydrate_edge(
        B,
        sfu_edge(
            "publication_generation_12345678",
            "b_from_sfu",
            "b_subscription",
        ),
    );
    // Isolate quality convergence: this established SFU relay already tried
    // its own Peer opportunities while remaining eligible to supply A.
    for plan in routes.build_candidates(B, false) {
        if plan.tuple.kind == UpstreamKind::Peer {
            routes.consume_candidate_opportunity(B, &plan);
        }
    }
    observe_persistent_degraded(&mut routes, A, "a_from_host", 99);
    let revision = routes.revision();
    observe_persistent_sfu_healthy(
        &mut routes,
        "publication_generation_12345678",
        revision,
        103,
    );
    let operation = must(routes.reconcile(104).operation);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
    assert_eq!(tuples(&operation), [regenerate_tuple(HOST)]);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.parent_peer_id.as_str()),
        (UpstreamKind::Peer, HOST)
    );
}

// TS 1147
#[test]
fn uses_one_healthy_serial_operation_to_distribute_new_host_root() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, B, 2, None);
    add_viewer(&mut routes, C, 0, None);
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    routes.hydrate_edge(C, peer_edge(B, "c_from_b"));
    routes.hydrate_edge(D, peer_edge(B, "d_from_b"));
    add_viewer(&mut routes, A, 2, ms(10));

    let joined = must(routes.reconcile(10).operation);
    assert_eq!(joined.candidates[0].tuple, peer_tuple(HOST));
    commit_current(&mut routes, 11, "a_from_host");

    let convergence = must(routes.reconcile(13).operation);
    assert_eq!(convergence.reason, DemandReason::RootConvergence);
    assert_eq!(tuples(&convergence), [peer_tuple(A)]);
    assert_eq!(
        edge_of(&routes, &convergence.child_peer_id).parent_peer_id,
        B
    );

    let prepared = begin_operation(
        &mut routes,
        begin(14, "root_convergence_candidate", direct()),
    );
    let revision = current(&prepared).revision;
    let guard = CandidateGuard {
        child_peer_id: convergence.child_peer_id.clone(),
        child_session_id: convergence.child_session_id.clone(),
        revision,
        connection_id: "root_convergence_candidate".to_owned(),
    };
    let ready = routes.candidate_ready(&guard, 15, None, CandidateProof::default());
    assert!(ready.accepted && !ready.committed);
    let child = convergence.child_peer_id.clone();
    let sample = sender_sample(
        A,
        &child,
        revision,
        "root_convergence_candidate",
        "root-rtp\0track",
        Healthy,
        16,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );
    assert_eq!(edge_of(&routes, &child).parent_peer_id, A);
    let migrated = |routes: &mut Controller, state, at| {
        observe_sender_state(
            routes,
            SenderState {
                child_peer_id: &child,
                connection_id: "root_convergence_candidate",
                parent_peer_id: A,
                state: Some(state),
                accepted_at_ms: at,
                ..SenderState::default()
            },
        )
    };
    for at in [17, 18, 19] {
        assert!(migrated(&mut routes, Degraded, at).accepted);
    }
    no_operation(routes.reconcile(20).operation);
    assert!(migrated(&mut routes, Healthy, 21).accepted);
    for at in [22, 23, 24] {
        assert!(migrated(&mut routes, Degraded, at).accepted);
    }
    assert_eq!(
        must(routes.reconcile(25).operation).reason,
        DemandReason::QualityConvergence
    );
}

// TS 1582
#[test]
fn persists_relative_proof_while_candidate_sender_evidence_is_unknown() {
    let mut routes = degraded_pair(Some(99), 99);
    routes.reconcile(103);
    let started = must(begin_candidate(&mut routes, begin(104, "a_from_b", direct())).operation);
    let revision = current(&started).revision;
    assert!(
        routes
            .observe_sender_quality_evidence(unknown_sample(B, A, revision, "a_from_b", 105), None)
            .accepted
    );
    assert!(
        !routes
            .candidate_ready(&guard_for(A, revision, "a_from_b"), 106, None, approved())
            .committed
    );
    let sample = sender_sample(
        B,
        A,
        revision,
        "a_from_b",
        "a-from-b-rtp\0track",
        Healthy,
        107,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
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

// TS 1648
#[test]
fn advances_past_persistently_limited_quality_candidate() {
    let mut routes = controller(3, quality_opts());
    for (peer, connection) in [(A, "a_from_host"), (B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 3, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    observe(&mut routes, B, "b_from_host", Healthy, 99);
    observe(&mut routes, C, "c_from_host", Healthy, 99);
    observe_persistent_degraded(&mut routes, A, "a_from_host", 99);

    let operation = must(routes.reconcile(103).operation);
    let deadline = operation.deadline_at_ms;
    assert_eq!(operation.candidates[0].tuple.kind, UpstreamKind::Peer);
    let first_parent = operation.candidates[0].tuple.parent_peer_id.clone();
    let first = begin_operation(&mut routes, begin(104, "first_quality_candidate", direct()));
    let first_revision = current(&first).revision;
    let mut last = SettleResult::default();
    for at in [105, 106, 107] {
        let sample = sender_sample(
            &first_parent,
            A,
            first_revision,
            "first_quality_candidate",
            "first-quality-rtp\0track",
            Degraded,
            at,
        );
        last = routes.observe_sender_quality_evidence(sample, None);
        assert!(last.accepted);
    }
    assert!(!last.committed);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (HOST, "a_from_host")
    );
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.cursor, 1);
    assert_eq!(pending.deadline_at_ms, deadline);
    assert!(pending.current.is_none());

    let second_parent = pending.candidates[1].tuple.parent_peer_id.clone();
    assert_ne!(second_parent, first_parent);
    let second = begin_operation(
        &mut routes,
        begin(108, "second_quality_candidate", direct()),
    );
    let revision = current(&second).revision;
    assert!(
        routes
            .observe_sender_quality_evidence(
                unknown_sample(&second_parent, A, revision, "second_quality_candidate", 109),
                None
            )
            .accepted
    );
    assert!(
        !routes
            .candidate_ready(
                &guard_for(A, revision, "second_quality_candidate"),
                110,
                None,
                approved()
            )
            .committed
    );
    let sample = sender_sample(
        &second_parent,
        A,
        revision,
        "second_quality_candidate",
        "second-quality-rtp\0track",
        Healthy,
        111,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (second_parent.as_str(), "second_quality_candidate")
    );
}

// TS 1768
#[test]
fn preempts_quality_work_for_viewer_without_media() {
    let mut routes = degraded_pair(Some(100), 99);
    assert_eq!(
        must(routes.reconcile(103).operation).reason,
        DemandReason::QualityConvergence
    );
    add_viewer(&mut routes, C, 0, ms(104));
    let join = must(routes.reconcile(105).operation);
    assert_eq!(
        (join.child_peer_id.as_str(), join.reason),
        (C, DemandReason::Join)
    );
    commit_current(&mut routes, 106, "c_join_connection");
    observe(&mut routes, B, "b_from_host", Healthy, 107);
    observe_persistent_degraded(&mut routes, A, "a_from_host", 107);
    assert_eq!(
        must(routes.reconcile(111).operation).reason,
        DemandReason::QualityConvergence
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (HOST, "a_from_host")
    );
}

// TS 1806
#[test]
fn keeps_quality_operation_reason_immutable_until_availability_preempts() {
    let mut routes = degraded_pair(Some(99), 99);
    assert_eq!(
        must(routes.reconcile(103).operation).reason,
        DemandReason::QualityConvergence
    );
    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(A, "host_session", revision, "a_from_host"),
        ms(104)
    ));
    assert_eq!(
        must(routes.snapshot().operation).reason,
        DemandReason::QualityConvergence
    );
    let preempted = must(routes.reconcile(105).operation);
    assert_eq!(
        (preempted.child_peer_id.as_str(), preempted.reason),
        (A, DemandReason::EdgeUnavailable)
    );
}

// TS 1893
#[test]
fn keeps_root_convergence_reason_immutable_until_availability_preempts() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, B, 2, None);
    add_viewer(&mut routes, C, 0, None);
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    routes.hydrate_edge(C, peer_edge(B, "c_from_b"));
    routes.hydrate_edge(D, peer_edge(B, "d_from_b"));
    add_viewer(&mut routes, A, 2, ms(10));
    routes.reconcile(10);
    commit_current(&mut routes, 11, "a_from_host");
    let convergence = must(routes.reconcile(13).operation);
    assert_eq!(convergence.reason, DemandReason::RootConvergence);
    let old = edge_of(&routes, &convergence.child_peer_id);
    assert_eq!(old.kind, UpstreamKind::Peer);
    let guard = EdgeGuard {
        child_peer_id: convergence.child_peer_id.clone(),
        child_session_id: convergence.child_session_id.clone(),
        parent_session_id: old.parent_session_id,
        route_revision: routes.revision(),
        connection_id: old.connection_id,
    };
    assert!(routes.invalidate_edge(guard, ms(14)));
    assert_eq!(
        must(routes.snapshot().operation).reason,
        DemandReason::RootConvergence
    );
    let preempted = must(routes.reconcile(15).operation);
    assert_eq!(preempted.child_peer_id, convergence.child_peer_id);
    assert_eq!(preempted.reason, DemandReason::EdgeUnavailable);
}

// TS 1931
#[test]
fn keeps_usable_parent_eligible_before_native_state_becomes_clear() {
    let mut routes = degraded_pair(None, 99);
    let operation = must(routes.reconcile(103).operation);
    assert_eq!(
        (operation.child_peer_id.as_str(), operation.reason),
        (A, DemandReason::QualityConvergence)
    );
    assert_eq!(tuples(&operation), [peer_tuple(B), regenerate_tuple(HOST)]);
}

// TS 1957
#[test]
fn waits_for_fresh_candidate_sender_after_prior_evidence_expires() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    assert!(observe(&mut routes, B, "b_from_host", Healthy, 100).accepted);
    assert!(observe(&mut routes, A, "a_from_host", Healthy, 99).accepted);
    for at in [100, 101, 102] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    routes.reconcile(103);
    let started =
        must(begin_candidate(&mut routes, begin(104, "a_from_b_stale", direct())).operation);
    let revision = current(&started).revision;
    assert!(
        routes
            .candidate_transport_connected(&guard_for(A, revision, "a_from_b_stale"), 105)
            .accepted
    );
    let stale = sender_sample(
        B,
        A,
        revision,
        "a_from_b_stale",
        "a-stale-rtp\0a-track",
        Healthy,
        104,
    );
    assert!(routes.observe_sender_quality_evidence(stale, None).accepted);
    for at in [5_101, 5_102, 5_103] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    assert!(
        !routes
            .candidate_ready(
                &guard_for(A, revision, "a_from_b_stale"),
                5_104,
                None,
                approved()
            )
            .committed
    );
    let fresh = sender_sample(
        B,
        A,
        revision,
        "a_from_b_stale",
        "a-stale-rtp\0a-track",
        Healthy,
        5_105,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(fresh, None)
            .committed
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (B, "a_from_b_stale")
    );
}

// TS 2063
#[test]
fn keeps_usable_candidate_when_native_source_category_expires() {
    let mut routes = degraded_pair(Some(0), 0);
    assert_eq!(
        must(routes.reconcile(4).operation).reason,
        DemandReason::QualityConvergence
    );
    let started = begin_candidate(&mut routes, begin(5_001, "expired_candidate", direct()));
    assert!(started.accepted);
    assert_eq!(
        current(&must(started.operation)).connection_id,
        "expired_candidate"
    );
}
