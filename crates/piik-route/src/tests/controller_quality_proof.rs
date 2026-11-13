//! Controller tests: bounded quality experiments and relative proof.

use super::*;
use crate::*;

use SenderQualityState::{Degraded, Healthy};

pub(crate) fn approved() -> CandidateProof {
    CandidateProof {
        relative_quality_approved: true,
        ..CandidateProof::default()
    }
}

/// Two Host roots with A persistently degraded at `at`; B optionally healthy first.
pub(crate) fn degraded_pair(b_healthy_at: Option<i64>, at: i64) -> Controller {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    if let Some(b_at) = b_healthy_at {
        observe(&mut routes, B, "b_from_host", Healthy, b_at);
    }
    observe_persistent_degraded(&mut routes, A, "a_from_host", at);
    routes
}

// TS 794
#[test]
fn keeps_one_quality_experiment_through_inconclusive_windows() {
    let mut routes = degraded_pair(Some(9), 10);
    let operation = must(routes.reconcile(14).operation);
    assert_eq!(tuples(&operation), [peer_tuple(B), regenerate_tuple(HOST)]);
    begin_candidate(&mut routes, begin(15, "a_from_b_failed", direct()));

    let revision = routes.revision();
    assert!(
        routes
            .observe_sender_quality_evidence(
                unknown_sample(HOST, A, revision, "a_from_host", 16),
                None
            )
            .accepted
    );
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.reason, DemandReason::QualityConvergence);
    assert_eq!(pending.cursor, 0);

    let after_first = routes.operation_expired(pending.wake_at_ms);
    assert!(after_first.accepted);
    let pending = must(routes.snapshot().operation);
    assert_eq!(pending.reason, DemandReason::QualityConvergence);
    assert_eq!(pending.cursor, 1);
    let regeneration = begin_operation(
        &mut routes,
        begin(
            20,
            "a_regeneration_failed",
            direct_overlap("a_regeneration_overlap"),
        ),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&regeneration).revision, "a_regeneration_failed"),
                21
            )
            .accepted
    );
    no_operation(routes.snapshot().operation);

    for at in [22, 23, 24] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    no_operation(routes.reconcile(25).operation);
    routes.touch_external_facts();
    no_operation(routes.reconcile(26).operation);

    add_viewer(&mut routes, C, 2, None);
    routes.hydrate_edge(C, peer_edge(B, "c_from_b"));
    observe_sender_state(
        &mut routes,
        SenderState {
            child_peer_id: C,
            connection_id: "c_from_b",
            parent_peer_id: B,
            state: Some(Healthy),
            accepted_at_ms: 27,
            ..SenderState::default()
        },
    );
    let new_opportunity = must(routes.reconcile(28).operation);
    assert_eq!(new_opportunity.child_peer_id, A);
    assert_eq!(new_opportunity.reason, DemandReason::QualityConvergence);
    assert_eq!(tuples(&new_opportunity), [peer_tuple(C)]);
}

// TS 902
#[test]
fn does_not_restart_quality_candidate_set_after_total_deadline() {
    let mut routes = degraded_pair(Some(9), 10);
    let operation = must(routes.reconcile(14).operation);
    begin_candidate(&mut routes, begin(15, "a_from_b_pending", direct()));
    assert!(routes.operation_expired(operation.deadline_at_ms).accepted);
    no_operation(routes.snapshot().operation);
    no_operation(routes.reconcile(operation.deadline_at_ms + 1).operation);
    routes.touch_external_facts();
    no_operation(routes.reconcile(operation.deadline_at_ms + 2).operation);

    observe(
        &mut routes,
        A,
        "a_from_host",
        Healthy,
        operation.deadline_at_ms + 3,
    );
    for offset in [4, 5, 6] {
        observe(
            &mut routes,
            A,
            "a_from_host",
            Degraded,
            operation.deadline_at_ms + offset,
        );
    }
    assert_eq!(
        must(routes.reconcile(operation.deadline_at_ms + 7).operation).reason,
        DemandReason::QualityConvergence
    );
}

// TS 951
#[test]
fn keeps_isolated_limitation_windows_diagnostic() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    observe(&mut routes, B, "b_from_host", Healthy, 99);
    observe(&mut routes, A, "a_from_host", Healthy, 99);
    observe(&mut routes, A, "a_from_host", Degraded, 100);
    let mut repeated = sender_sample(
        HOST,
        A,
        routes.revision(),
        "a_from_host",
        "a_from_host-rtp\0track",
        Degraded,
        101,
    );
    repeated.sample_timestamp_ms = Some(100);
    assert!(
        routes
            .observe_sender_quality_evidence(repeated, None)
            .accepted
    );
    no_operation(routes.reconcile(101).operation);
    observe(&mut routes, A, "a_from_host", Degraded, 102);
    no_operation(routes.reconcile(102).operation);
    observe(&mut routes, A, "a_from_host", Degraded, 103);
    let operation = must(routes.reconcile(104).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
}

// TS 1052
#[test]
fn commits_only_after_viewer_approves_p2p_receive_improvement() {
    let mut routes = degraded_pair(Some(100), 99);
    let operation = must(routes.reconcile(103).operation);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
    assert_eq!(operation.candidates[0].tuple, peer_tuple(B));
    let started = must(begin_candidate(&mut routes, begin(104, "a_from_b", direct())).operation);
    let revision = current(&started).revision;
    let edge = edge_of(&routes, A);
    assert_eq!(
        (
            edge.kind,
            edge.parent_peer_id.as_str(),
            edge.connection_id.as_str()
        ),
        (UpstreamKind::Peer, HOST, "a_from_host")
    );
    let ready = routes.candidate_ready(&guard_for(A, revision, "a_from_b"), 105, None, approved());
    assert!(ready.accepted && !ready.committed);
    let sample = sender_sample(
        B,
        A,
        revision,
        "a_from_b",
        "a-from-b-rtp\0a-track",
        Healthy,
        106,
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

    let migrated = routes.revision();
    let sample = |routes: &mut Controller, state, at| {
        routes.observe_sender_quality_evidence(
            sender_sample(
                B,
                A,
                migrated,
                "a_from_b",
                "a-from-b-rtp\0a-track",
                state,
                at,
            ),
            None,
        )
    };
    for at in [107, 108, 109] {
        assert!(sample(&mut routes, Degraded, at).accepted);
    }
    no_operation(routes.reconcile(110).operation);
    routes.touch_external_facts();
    no_operation(routes.reconcile(110).operation);
    assert!(sample(&mut routes, Healthy, 111).accepted);
    for at in [112, 113, 114] {
        assert!(sample(&mut routes, Degraded, at).accepted);
    }
    assert_eq!(
        must(routes.reconcile(115).operation).reason,
        DemandReason::QualityConvergence
    );
}

// TS 1390
#[test]
fn waits_for_fresh_healthy_candidate_sender_after_relative_proof() {
    let mut routes = degraded_pair(Some(100), 99);
    routes.reconcile(103);
    let started = must(begin_candidate(&mut routes, begin(104, "a_from_b", direct())).operation);
    let revision = current(&started).revision;
    assert!(
        routes
            .candidate_transport_connected(&guard_for(A, revision, "a_from_b"), 105)
            .accepted
    );
    let degraded = sender_sample(
        B,
        A,
        revision,
        "a_from_b",
        "a-from-b-rtp\0track",
        Degraded,
        105,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(degraded, None)
            .accepted
    );
    assert!(
        !routes
            .candidate_ready(&guard_for(A, revision, "a_from_b"), 106, None, approved())
            .committed
    );
    let healthy = sender_sample(
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
            .observe_sender_quality_evidence(healthy, None)
            .committed
    );
}

// TS 1464
#[test]
fn does_not_commit_relative_proof_after_freshness_window_expires() {
    let mut routes = degraded_pair(None, 99);
    routes.reconcile(103);
    let started =
        must(begin_candidate(&mut routes, begin(104, "a_from_b_stale_proof", direct())).operation);
    let revision = current(&started).revision;
    let guard = guard_for(A, revision, "a_from_b_stale_proof");
    assert!(routes.candidate_transport_connected(&guard, 105).accepted);

    assert!(
        !routes
            .candidate_ready(&guard, 105, None, approved())
            .committed
    );
    assert!(
        !routes
            .candidate_ready(&guard, 109, None, approved())
            .committed
    );
    for at in [5_105, 5_106, 5_107] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    let sample = sender_sample(
        B,
        A,
        revision,
        &guard.connection_id,
        "stale-proof-rtp\0track",
        Healthy,
        5_111,
    );
    let settled = routes.observe_sender_quality_evidence(sample, None);
    assert!(settled.accepted && !settled.committed);
    let operation = must(routes.snapshot().operation);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
    assert_eq!(operation.cursor, 1);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (HOST, "a_from_host")
    );
}

// TS 1528
#[test]
fn abandons_pending_quality_move_when_incumbent_recovers() {
    let mut routes = degraded_pair(None, 99);
    routes.reconcile(103);
    let started = must(
        begin_candidate(
            &mut routes,
            begin(104, "a_from_b_incumbent_recovery", direct()),
        )
        .operation,
    );
    let revision = current(&started).revision;
    let guard = guard_for(A, revision, "a_from_b_incumbent_recovery");
    assert!(
        !routes
            .candidate_ready(&guard, 105, None, approved())
            .committed
    );
    assert!(observe(&mut routes, A, "a_from_host", Healthy, 106).accepted);
    no_operation(routes.snapshot().operation);
    let late = sender_sample(
        B,
        A,
        revision,
        &guard.connection_id,
        "late-healthy-rtp\0track",
        Healthy,
        107,
    );
    assert!(!routes.observe_sender_quality_evidence(late, None).accepted);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
        (HOST, "a_from_host")
    );
}

#[test]
fn quality_readiness_keeps_inconclusive_incumbent_within_the_same_operation() {
    for unknown_first in [true, false] {
        let mut routes = degraded_pair(None, 99);
        let operation = must(routes.reconcile(103).operation);
        let started = begin_operation(&mut routes, begin(104, "a_from_b", direct()));
        let guard = guard_for(A, current(&started).revision, "a_from_b");
        let unknown = |routes: &mut Controller, at| {
            let revision = routes.revision();
            assert!(
                routes
                    .observe_sender_quality_evidence(
                        unknown_sample(HOST, A, revision, "a_from_host", at),
                        None
                    )
                    .accepted
            );
        };
        let ready = |routes: &mut Controller, at| {
            let settled = routes.candidate_ready(&guard, at, None, approved());
            assert!(settled.accepted && !settled.committed);
        };
        if unknown_first {
            unknown(&mut routes, 105);
            ready(&mut routes, 106);
        } else {
            ready(&mut routes, 105);
            unknown(&mut routes, 106);
        }
        let pending = must(routes.snapshot().operation);
        assert_eq!(current(&pending).revision, guard.revision);
        assert_eq!(pending.deadline_at_ms, operation.deadline_at_ms);
        assert_eq!(edge_of(&routes, A).connection_id, "a_from_host");
        let sample = sender_sample(
            B,
            A,
            guard.revision,
            &guard.connection_id,
            "candidate-rtp\0track",
            Healthy,
            107,
        );
        assert!(
            routes
                .observe_sender_quality_evidence(sample, None)
                .committed
        );
        no_operation(routes.snapshot().operation);
        assert_eq!(edge_of(&routes, A).connection_id, "a_from_b");
    }
}

#[test]
fn inconclusive_quality_operation_still_requires_current_authority() {
    for cause in ["healthy", "reset", "sender-replaced", "pause", "deadline"] {
        let mut routes = degraded_pair(None, 99);
        let operation = must(routes.reconcile(103).operation);
        let started = begin_operation(&mut routes, begin(104, "a_from_b", direct()));
        let guard = guard_for(A, current(&started).revision, "a_from_b");
        let revision = routes.revision();
        assert!(
            routes
                .observe_sender_quality_evidence(
                    unknown_sample(HOST, A, revision, "a_from_host", 105),
                    None
                )
                .accepted
        );
        assert!(
            routes
                .candidate_ready(&guard, 106, None, approved())
                .accepted
        );
        let mut at = 107;
        match cause {
            "healthy" => drop(observe(&mut routes, A, "a_from_host", Healthy, at)),
            "reset" => drop(routes.reset_sender_quality(HOST, "host_session", at)),
            "sender-replaced" => drop(observe_sender_state(
                &mut routes,
                SenderState {
                    child_peer_id: A,
                    connection_id: "a_from_host",
                    sender_identity: "replacement-rtp\0track",
                    state: Some(Degraded),
                    accepted_at_ms: at,
                    ..SenderState::default()
                },
            )),
            "pause" => drop(routes.set_paused(true, Some(at))),
            _ => at = operation.deadline_at_ms,
        }
        assert!(
            !routes
                .candidate_ready(&guard, at, None, approved())
                .accepted,
            "{cause}"
        );
        no_operation(routes.snapshot().operation);
        assert_eq!(edge_of(&routes, A).connection_id, "a_from_host");
    }
}
