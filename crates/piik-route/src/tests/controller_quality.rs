//! Controller tests: sender-quality runs and same-parent regeneration.

use super::*;
use crate::*;

use SenderQualityState::{Degraded, Healthy};

fn approved() -> CandidateProof {
    CandidateProof {
        relative_quality_approved: true,
        ..CandidateProof::default()
    }
}

fn with_identity<'a>(
    child: &'a str,
    connection: &'a str,
    identity: &'a str,
    state: SenderQualityState,
    at: i64,
) -> SenderState<'a> {
    SenderState {
        child_peer_id: child,
        connection_id: connection,
        sender_identity: identity,
        state: Some(state),
        accepted_at_ms: at,
        ..SenderState::default()
    }
}

// TS 359
#[test]
fn keeps_one_diagnostic_label_until_departed_relay_is_pruned() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(A, "b_from_a"));
    let label = routes.diagnostic_participant_label(A);
    let digits = label.strip_prefix("viewer-").expect("viewer label");
    assert!(
        !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()),
        "label {label:?}"
    );

    routes.upsert_participant(viewer_input(A, "a_replacement_session", 1), None);
    assert_eq!(routes.diagnostic_participant_label(A), label);
    assert!(routes.disconnect_session(A, "a_replacement_session"));
    assert_eq!(routes.diagnostic_participant_label(A), label);
    assert!(routes.confirm_departure(A, ms(0)));
    assert_eq!(routes.diagnostic_participant_label(A), label);

    let repair = must(routes.reconcile(1).operation);
    assert_eq!(repair.child_peer_id, B);
    assert_eq!(repair.reason, DemandReason::ParentDeparted);
    commit_current(&mut routes, 2, "b_from_host");
    assert!(routes.reconcile(4).removed_peer_ids.contains(&A.to_owned()));
    assert_eq!(routes.diagnostic_participant_label(A), "viewer-unknown");

    add_viewer(&mut routes, C, 0, None);
    assert_ne!(routes.diagnostic_participant_label(C), label);
}

// TS 394
#[test]
fn clears_unknown_sender_run_without_rearming_healthy_baseline() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    for (child, connection) in [(A, "a_from_host"), (B, "b_from_host")] {
        assert!(observe(&mut routes, child, connection, Healthy, 0).accepted);
    }
    assert!(observe(&mut routes, A, "a_from_host", Degraded, 1).accepted);
    let revision = routes.revision();
    assert!(
        routes
            .observe_sender_quality_evidence(
                unknown_sample(HOST, A, revision, "a_from_host", 2),
                None
            )
            .accepted
    );
    for at in [3, 4, 5] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    let operation = must(routes.reconcile(6).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
}

// TS 448
#[test]
fn consumes_quality_candidate_rejected_at_preparation() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    observe_persistent_degraded(&mut routes, A, "a_from_host", 0);
    let first = must(routes.reconcile(10).operation);
    assert_eq!(first.child_peer_id, A);
    assert_eq!(first.reason, DemandReason::QualityConvergence);
    let rejected = first.candidates[first.cursor].tuple.clone();

    let mut operation = Some(first);
    let mut round = 0;
    while round < 4 {
        let Some(current) = operation.as_ref() else {
            break;
        };
        if round > 0 {
            assert!(
                !tuples_from(current, current.cursor).contains(&rejected),
                "round {round} rebuilt the rejected tuple"
            );
        }
        assert!(
            routes
                .skip_current_candidate(&cursor_guard(current), 11 + round, RejectionBucket::Stale)
                .accepted
        );
        operation = routes.reconcile(12 + round).operation;
        round += 1;
    }
    no_operation(operation);
}

// TS 480
#[test]
fn newly_committed_edge_reports_persistent_degradation_before_healthy() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 2, ms(0));
    assert_eq!(must(routes.reconcile(0).operation).child_peer_id, A);
    commit_current(&mut routes, 1, "a_from_host");
    for at in [2, 3, 4] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    let operation = must(routes.reconcile(5).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
}

// TS 506
#[test]
fn starts_fresh_degradation_run_when_sender_identity_changes() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 2, ms(0));
    routes.reconcile(0);
    commit_current(&mut routes, 1, "a_from_host");
    let sample = |routes: &mut Controller, identity, at| {
        assert!(
            observe_sender_state(
                routes,
                with_identity(A, "a_from_host", identity, Degraded, at)
            )
            .accepted
        );
    };
    sample(&mut routes, "old-rtp\0track", 2);
    sample(&mut routes, "old-rtp\0track", 3);
    sample(&mut routes, "new-rtp\0track", 4);
    no_operation(routes.reconcile(4).operation);
    sample(&mut routes, "new-rtp\0track", 5);
    sample(&mut routes, "new-rtp\0track", 6);
    let operation = must(routes.reconcile(7).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
}

// TS 566
#[test]
fn reset_sender_begins_new_degradation_run_without_healthy_sample() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 2, ms(0));
    routes.reconcile(0);
    commit_current(&mut routes, 1, "a_from_host");
    assert!(observe(&mut routes, A, "a_from_host", Healthy, 2).accepted);
    routes.reset_sender_quality(HOST, "host_session", 3);
    for at in [4, 5, 6] {
        assert!(observe(&mut routes, A, "a_from_host", Degraded, at).accepted);
    }
    let operation = must(routes.reconcile(7).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(operation.reason, DemandReason::QualityConvergence);
}

// TS 599
#[test]
fn regenerates_persistently_degraded_edge_on_same_parent() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    observe_persistent_degraded(&mut routes, A, "a_from_host", 10);

    let operation = must(routes.reconcile(14).operation);
    assert_eq!(operation.candidates.len(), 1);
    assert_eq!(operation.candidates[0].tuple, regenerate_tuple(HOST));
    expect_transition(
        &operation.candidates[0].endpoint_transition,
        TransitionKind::None,
        HOST,
    );

    let started = begin_operation(&mut routes, begin(15, "a_regenerated", direct()));
    let edge = edge_of(&routes, A);
    assert_eq!(edge.parent_peer_id, HOST);
    assert_eq!(edge.connection_id, "a_from_host");
    assert!(edge.usable && edge.physical_active);
    let revision = current(&started).revision;
    let ready = routes.candidate_ready(
        &guard_for(A, revision, "a_regenerated"),
        16,
        None,
        approved(),
    );
    assert!(ready.accepted && !ready.committed);

    let sample = sender_sample(
        HOST,
        A,
        revision,
        "a_regenerated",
        "a-regenerated-rtp\0track",
        Healthy,
        17,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );
    let edge = edge_of(&routes, A);
    assert_eq!(edge.kind, UpstreamKind::Peer);
    assert_eq!(edge.parent_peer_id, HOST);
    assert_eq!(edge.connection_id, "a_regenerated");
    assert!(
        observe_sender_state(
            &mut routes,
            with_identity(A, "a_regenerated", "a-regenerated-rtp\0track", Healthy, 18)
        )
        .accepted
    );
    for at in [19, 20, 21] {
        assert!(
            observe_sender_state(
                &mut routes,
                with_identity(A, "a_regenerated", "a-regenerated-rtp\0track", Degraded, at)
            )
            .accepted
        );
    }
    assert_eq!(
        must(routes.reconcile(22).operation).candidates[0].tuple,
        regenerate_tuple(HOST)
    );
}

// TS 686
#[test]
fn keeps_same_edge_regeneration_damped_while_allowing_different_parent() {
    let mut routes = controller(2, quality_opts());
    add_viewer(&mut routes, A, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    observe_persistent_degraded(&mut routes, A, "a_from_host", 10);

    routes.reconcile(14);
    let started = begin_operation(&mut routes, begin(15, "a_regenerated", direct()));
    let revision = current(&started).revision;
    routes.candidate_ready(
        &guard_for(A, revision, "a_regenerated"),
        16,
        None,
        approved(),
    );
    let sample = sender_sample(
        HOST,
        A,
        revision,
        "a_regenerated",
        "a-regenerated-rtp\0track",
        Healthy,
        17,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );

    for at in [18, 19, 20] {
        assert!(
            observe_sender_state(
                &mut routes,
                with_identity(A, "a_regenerated", "a-regenerated-rtp\0track", Degraded, at)
            )
            .accepted
        );
    }
    no_operation(routes.reconcile(21).operation);

    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    assert!(observe(&mut routes, B, "b_from_host", Healthy, 22).accepted);
    let with_alternate = must(routes.reconcile(23).operation);
    assert_eq!(with_alternate.child_peer_id, A);
    assert_eq!(with_alternate.reason, DemandReason::QualityConvergence);
    assert_eq!(tuples(&with_alternate), [peer_tuple(B)]);
}

// TS 754
#[test]
fn uses_existing_one_slot_overlap_for_same_parent_regeneration() {
    let mut routes = controller(1, quality_opts());
    add_viewer(&mut routes, A, 1, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    observe_persistent_degraded(&mut routes, A, "a_from_host", 10);

    let operation = must(routes.reconcile(14).operation);
    expect_transition(
        &operation.candidates[0].endpoint_transition,
        TransitionKind::Overlap,
        HOST,
    );
    let started = begin_operation(
        &mut routes,
        begin(15, "a_regenerated", direct_overlap("a_overlap")),
    );
    let revision = current(&started).revision;
    routes.candidate_ready(
        &guard_for(A, revision, "a_regenerated"),
        16,
        None,
        approved(),
    );
    let sample = sender_sample(
        HOST,
        A,
        revision,
        "a_regenerated",
        "a-regenerated-rtp\0track",
        Healthy,
        17,
    );
    let committed = routes.observe_sender_quality_evidence(sample, None);
    assert!(committed.committed);
    contains_labels(&committed.released, &["a_overlap"]);
}
