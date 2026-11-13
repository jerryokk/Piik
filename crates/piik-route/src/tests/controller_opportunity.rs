//! Controller tests: opportunity ledgers, transitions and SFU reopening.

use super::controller_bootstrap::{
    invalidate_active_sfu_edge, retire_last_sfu_publication_after_reuse_deadline,
};
use super::*;
use crate::*;

// TS 4399
#[test]
fn retries_bootstrap_carrier_only_when_transition_improves() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "failed_direct"));
    assert!(routes.invalidate_edge(edge_guard(A, "host_session", 0, "failed_direct"), None));
    let operation = must(routes.reconcile(0).operation);
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            1,
            "failed_sfu",
            "failed_publication",
            "failed_publication_connection",
            sfu_create_overlap(
                "failed_subscription_resource",
                "failed_publication_resource",
                "failed_overlap",
            ),
        ),
    );
    let guard = CandidateGuard {
        child_peer_id: operation.child_peer_id.clone(),
        child_session_id: operation.child_session_id.clone(),
        revision: current(&prepared).revision,
        connection_id: "failed_sfu".to_owned(),
    };
    let failed = routes.candidate_failed(&guard, 2);
    assert!(failed.accepted);
    assert_eq!(failed.failed_peer_ids, [A]);
    let improved = must(routes.reconcile(3).operation);
    assert_eq!(
        improved.candidates[0].tuple,
        sfu_tuple(PublicationPlan::Create)
    );
    expect_transition(
        &improved.candidates[0].endpoint_transition,
        TransitionKind::None,
        HOST,
    );
    let attempt = begin_operation(
        &mut routes,
        begin_publication(
            4,
            "improved_sfu",
            "improved_publication",
            "improved_publication_connection",
            sfu_create(
                "improved_subscription_resource",
                "improved_publication_resource",
            ),
        ),
    );
    let guard = CandidateGuard {
        child_peer_id: improved.child_peer_id.clone(),
        child_session_id: improved.child_session_id.clone(),
        revision: current(&attempt).revision,
        connection_id: "improved_sfu".to_owned(),
    };
    let failed_again = routes.candidate_failed(&guard, 5);
    assert!(failed_again.accepted);
    assert_eq!(failed_again.failed_peer_ids, [A]);
    let fact_version = routes.snapshot().fact_version;
    no_operation(routes.reconcile(6).operation);
    assert!(routes.set_effective_capacity(A, &session_of(A), 0, None));
    assert_eq!(routes.snapshot().fact_version, fact_version);
    no_operation(routes.reconcile(7).operation);
    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(A, "host_session", revision, "failed_direct"),
        None
    ));
    assert_eq!(routes.snapshot().fact_version, fact_version);
    no_operation(routes.reconcile(8).operation);

    add_viewer(&mut routes, B, 0, ms(9));
    assert_eq!(must(routes.reconcile(9).operation).child_peer_id, B);
    let mut rounds = 0;
    while routes.snapshot().operation.is_some() {
        assert!(rounds <= 100, "skipping never exhausted the operation");
        skip_candidate(&mut routes, 10);
        rounds += 1;
    }
    no_operation(routes.reconcile(11).operation);
}

// TS 4472
#[test]
fn requires_one_overlap_slot_for_sfu_to_host_transition() {
    for capacity in [1, 2] {
        let mut routes = controller(capacity, sfu_opts());
        add_viewer(&mut routes, A, 0, None);
        if capacity == 2 {
            add_viewer(&mut routes, E, 0, None);
            routes.hydrate_edge(E, peer_edge(HOST, "e_direct"));
        }
        routes.hydrate_host_publication("publication_1", res("publication_resource_1"), "");
        routes.hydrate_edge(A, sfu_edge("publication_1", "a_sfu", "a_subscription"));
        routes.invalidate_edge(edge_guard(A, "", 0, "a_sfu"), None);
        let operation = must(routes.reconcile(0).operation);
        assert_eq!(operation.candidates[0].tuple, peer_tuple(HOST));
        expect_transition(
            &operation.candidates[0].endpoint_transition,
            TransitionKind::Overlap,
            HOST,
        );
        expect_panic("endpoint overlap reservation", || {
            begin_candidate(&mut routes, begin(1, "a_host_direct", direct()));
        });
    }
}

// TS 4510
#[test]
fn uses_one_bounded_gap_instead_of_fourth_host_copy_at_capacity_three() {
    let mut routes = controller(3, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    for peer_id in [E, F] {
        add_viewer(&mut routes, peer_id, 0, None);
        routes.hydrate_edge(peer_id, peer_edge(HOST, &format!("{peer_id}_direct")));
    }
    routes.hydrate_host_publication("publication_1", res("publication_resource_1"), "");
    routes.hydrate_edge(A, sfu_edge("publication_1", "a_sfu", "a_subscription"));
    routes.invalidate_edge(edge_guard(A, "", 0, "a_sfu"), None);
    let operation = must(routes.reconcile(10).operation);
    let deadline = operation.deadline_at_ms;
    assert_eq!(operation.candidates[0].tuple, peer_tuple(HOST));
    let transition = &operation.candidates[0].endpoint_transition;
    expect_transition(transition, TransitionKind::BoundedGap, HOST);
    let retire = transition.retire.as_ref().expect("expected a retirement");
    assert_eq!(
        (retire.kind, retire.generation.as_str()),
        (RetirementKind::Publication, "publication_1")
    );
    let gap = routes.retire_current_candidate_producer(&cursor_guard(&operation), 11);
    assert!(gap.accepted);
    eq_labels(&gap.released, &["a_subscription", "publication_resource_1"]);
    let replanned = must(gap.operation);
    assert_eq!(replanned.deadline_at_ms, deadline);
    assert_eq!(replanned.candidates.len(), 1);
    expect_transition(
        &replanned.candidates[0].endpoint_transition,
        TransitionKind::None,
        HOST,
    );
    let prepared = begin_operation(&mut routes, begin(12, "a_host_direct", direct()));
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&prepared).revision, "a_host_direct"),
                13,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    assert!(routes.snapshot().host_publication.is_none());
}

// TS 4564
#[test]
fn does_not_reopen_consumed_none_transitions_after_capacity_reduction() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_host_publication("publication", res("publication_resource"), "");
    routes.hydrate_edge(A, sfu_edge("publication", "a_from_sfu", "a_subscription"));
    assert!(routes.invalidate_edge(edge_guard(A, "", 0, "a_from_sfu"), None));
    let operation = must(routes.reconcile(0).operation);
    assert_eq!(operation.candidates[0].tuple, peer_tuple(HOST));
    expect_transition(
        &operation.candidates[0].endpoint_transition,
        TransitionKind::None,
        HOST,
    );
    assert!(routes.set_effective_capacity(HOST, "host_session", 1, None));
    assert_eq!(
        routes
            .operation_expired(operation.deadline_at_ms)
            .failed_peer_ids,
        [A]
    );
    no_operation(routes.reconcile(operation.deadline_at_ms + 1).operation);
}

// TS 4596
#[test]
fn does_not_unblock_exhausted_demand_for_unrelated_join() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 0, ms(0));
    assert_eq!(routes.reconcile(0).failed_peer_ids, [A]);
    assert!(routes.set_effective_capacity(A, &session_of(A), 1, None));
    no_operation(routes.reconcile(1).operation);
    add_viewer(&mut routes, C, 0, ms(2));
    assert_eq!(routes.reconcile(2).failed_peer_ids, [C]);
    no_operation(routes.snapshot().operation);
    assert!(routes.set_effective_capacity(B, &session_of(B), 1, None));
    let operation = must(routes.reconcile(3).operation);
    assert_eq!(operation.child_peer_id, A);
    assert_eq!(tuples(&operation), [peer_tuple(B)]);
}

fn failed_host_edge(capacity: i64, connection: &str) -> Controller {
    let mut routes = controller(capacity, opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, connection));
    assert!(routes.invalidate_edge(edge_guard(A, "host_session", 0, connection), None));
    no_operation(routes.reconcile(0).operation);
    routes
}

// TS 4621
#[test]
fn does_not_revive_exact_failed_host_tuple_when_new_parent_joins() {
    let mut routes = failed_host_edge(1, "failed_direct");
    add_viewer(&mut routes, B, 1, ms(1));
    let join = must(routes.reconcile(2).operation);
    assert_eq!(
        (join.child_peer_id.as_str(), join.reason),
        (B, DemandReason::Join)
    );
    commit_current(&mut routes, 2, "b_from_host");
    let retry = must(routes.reconcile(4).operation);
    assert_eq!(retry.child_peer_id, A);
    assert_eq!(tuples(&retry), [peer_tuple(B)]);
    assert!(!has_tuple(&retry, |tuple| is_peer(tuple)
        && tuple.parent_peer_id == HOST));
}

// TS 4660
#[test]
fn does_not_unblock_exhausted_viewer_when_candidate_set_shrinks() {
    let mut routes = failed_host_edge(1, "failed_host");
    add_viewer(&mut routes, B, 1, ms(1));
    routes.reconcile(1);
    commit_current(&mut routes, 2, "b_from_host");
    let candidate = must(routes.reconcile(3).operation);
    let attempt = begin_operation(&mut routes, begin(4, "a_from_b", direct()));
    assert_eq!(
        routes
            .candidate_failed(&guard_for(A, current(&attempt).revision, "a_from_b"), 5)
            .failed_peer_ids,
        [A]
    );
    assert_eq!(candidate.candidates.len(), 1);
    assert!(routes.disconnect_session(B, &session_of(B)));
    no_operation(routes.reconcile(6).operation);
}

// TS 4694
#[test]
fn treats_replacement_child_session_as_new_exact_opportunity() {
    let mut routes = failed_host_edge(1, "failed_host");
    routes.upsert_participant(viewer_input(A, "replacement_session", 0), None);
    let operation = must(routes.reconcile(1).operation);
    assert_eq!(
        (
            operation.child_peer_id.as_str(),
            operation.child_session_id.as_str()
        ),
        (A, "replacement_session")
    );
    assert_eq!(tuples(&operation), [peer_tuple(HOST)]);
}

// TS 4724
#[test]
fn does_not_retry_failed_bootstrap_carrier_when_another_carrier_leaves() {
    let mut routes = controller(2, sfu_opts());
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 1, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    add_viewer(&mut routes, A, 0, ms(0));
    let acquisition = must(routes.reconcile(0).operation);
    let wake = acquisition.wake_at_ms;
    begin_candidate(&mut routes, begin(1, "a_head_start", direct()));
    routes.operation_expired(wake);
    let bootstrap = must(routes.reconcile(wake + 1).operation);
    assert_eq!(bootstrap.reason, DemandReason::SfuBootstrap);
    let failed_carrier = bootstrap.child_peer_id.clone();
    let other_carrier = if failed_carrier == B { C } else { B };
    let attempt = begin_operation(
        &mut routes,
        begin_publication(
            wake + 2,
            "failed_bootstrap",
            "failed_publication",
            "failed_publication_connection",
            sfu_create_overlap(
                "failed_subscription",
                "failed_publication_resource",
                "failed_overlap",
            ),
        ),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(
                    &failed_carrier,
                    current(&attempt).revision,
                    "failed_bootstrap"
                ),
                wake + 3
            )
            .accepted
    );
    assert!(routes.disconnect_session(other_carrier, &session_of(other_carrier)));
    if let Some(next) = routes.reconcile(wake + 4).operation {
        assert!(
            !(next.reason == DemandReason::SfuBootstrap && next.child_peer_id == failed_carrier),
            "retried the failed carrier"
        );
    }
}

// TS 4775
#[test]
fn does_not_bootstrap_new_sfu_generation_after_last_reuse_opportunity_expires() {
    let (mut routes, now) = retire_last_sfu_publication_after_reuse_deadline();
    let reconciled = routes.reconcile(now);
    no_operation(reconciled.operation);
    assert!(reconciled.failed_peer_ids.is_empty());
}

// TS 4783
#[test]
fn reopens_exhausted_sfu_opportunity_for_new_authority() {
    for authority in ["child-session", "host-session", "external-resource"] {
        let (mut routes, now) = retire_last_sfu_publication_after_reuse_deadline();
        match authority {
            "child-session" => drop(
                routes.upsert_participant(viewer_input(A, "replacement_child_session", 0), None),
            ),
            "host-session" => {
                drop(routes.upsert_participant(host_input("replacement_host_session", 2), None))
            }
            _ => routes.touch_external_facts(),
        }
        let operation = must(routes.reconcile(now).operation);
        assert_eq!(
            (operation.demand_peer_id.as_str(), operation.reason),
            (A, DemandReason::SfuBootstrap),
            "{authority}"
        );
        assert_eq!(tuples(&operation), [sfu_tuple(PublicationPlan::Create)]);
    }
}

// TS 4817
#[test]
fn consumes_canonical_sfu_opportunity_when_active_edge_fails() {
    let mut routes = invalidate_active_sfu_edge();
    assert!(!has_tuple(&must(routes.reconcile(0).operation), is_sfu));
}

// TS 4826
#[test]
fn reopens_active_edge_sfu_opportunity_for_new_authority() {
    for authority in ["child-session", "host-session", "external-resource"] {
        let mut routes = invalidate_active_sfu_edge();
        match authority {
            "child-session" => drop(
                routes
                    .upsert_participant(viewer_input(A, "active_edge_child_replacement", 0), None),
            ),
            "host-session" => {
                drop(routes.upsert_participant(host_input("active_edge_host_replacement", 1), None))
            }
            _ => routes.touch_external_facts(),
        }
        let operation = must(routes.reconcile(0).operation);
        assert!(
            has_tuple(&operation, |tuple| is_sfu(tuple)
                && tuple.publication == Some(PublicationPlan::Reuse)),
            "{authority}"
        );
    }
}

// TS 4860
#[test]
fn does_not_turn_failed_same_rank_sfu_replace_into_create_retry() {
    let mut routes = controller(2, sfu_opts());
    routes.hydrate_host_publication("failed_publication", res("failed_publication_resource"), "");
    let guard = PublicationGuard {
        host_session_id: "host_session".to_owned(),
        route_revision: routes.revision(),
        generation: "failed_publication".to_owned(),
        connection_id: "publication:failed_publication".to_owned(),
    };
    assert!(routes.invalidate_host_publication(guard, None));
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    assert_eq!(operation.candidates[0].tuple.kind, UpstreamKind::Peer);
    skip_candidate(&mut routes, 1);
    let replace = must(routes.snapshot().operation).candidates[1].clone();
    assert_eq!(replace.tuple, sfu_tuple(PublicationPlan::Replace));
    expect_transition(&replace.endpoint_transition, TransitionKind::None, HOST);
    let attempt = begin_operation(
        &mut routes,
        begin_publication(
            2,
            "failed_replace",
            "replacement_publication",
            "replacement_ingress",
            sfu_create(
                "replacement_subscription",
                "replacement_publication_resource",
            ),
        ),
    );
    assert_eq!(
        routes
            .candidate_failed(
                &guard_for(A, current(&attempt).revision, "failed_replace"),
                3
            )
            .failed_peer_ids,
        [A]
    );
    add_viewer(&mut routes, B, 0, None);
    assert!(routes.confirm_departure(B, None));
    let reconciled = routes.reconcile(4);
    assert!(routes.snapshot().host_publication.is_none());
    no_operation(reconciled.operation);
    assert!(reconciled.failed_peer_ids.is_empty());
}

// TS 4913
#[test]
fn reopens_only_sfu_opportunities_for_new_external_fact() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, ms(0));
    routes.reconcile(0);
    let direct_attempt = begin_operation(&mut routes, begin(1, "failed_direct", direct()));
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&direct_attempt).revision, "failed_direct"),
                2
            )
            .accepted
    );
    let sfu_attempt = begin_operation(
        &mut routes,
        begin_publication(
            3,
            "failed_sfu",
            "failed_publication",
            "failed_publication_connection",
            sfu_create("failed_subscription", "failed_publication_resource"),
        ),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&sfu_attempt).revision, "failed_sfu"),
                4
            )
            .accepted
    );
    no_operation(routes.snapshot().operation);
    no_operation(routes.reconcile(5).operation);
    routes.touch_external_facts();
    let retried = must(routes.reconcile(6).operation);
    assert_eq!(tuples(&retried), [sfu_tuple(PublicationPlan::Create)]);
    expect_transition(
        &retried.candidates[0].endpoint_transition,
        TransitionKind::None,
        HOST,
    );
    assert!(!has_tuple(&retried, is_peer));
}

// TS 4963
#[test]
fn fails_all_waiting_demands_without_cutting_capacity_three_host_children() {
    let mut routes = controller(3, sfu_opts());
    let seeds = [(A, "a_direct"), (B, "b_direct"), (C, "c_direct")];
    for (peer, connection) in seeds {
        add_viewer(&mut routes, peer, 0, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    add_viewer(&mut routes, D, 0, None);
    add_viewer(&mut routes, E, 0, None);
    let reconciled = routes.reconcile(0);
    no_operation(reconciled.operation);
    assert_eq!(reconciled.failed_peer_ids, [D, E]);
    for (peer, connection) in seeds {
        let edge = edge_of(&routes, peer);
        assert!(edge.connection_id == connection && edge.usable && edge.physical_active);
    }
}

// TS 4991
#[test]
fn does_not_promote_when_synchronous_admission_commit_fails() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    let prepared = begin_operation(&mut routes, begin(1, "candidate_connection", direct()));
    let revision = current(&prepared).revision;
    let guard = CandidateGuard {
        child_peer_id: operation.child_peer_id.clone(),
        child_session_id: operation.child_session_id.clone(),
        revision,
        connection_id: "candidate_connection".to_owned(),
    };
    let reject = |_: &CandidateReservation| false;
    let settled = routes.candidate_ready(&guard, 2, Some(&reject), CandidateProof::default());
    assert!(!settled.accepted);
    assert!(settled.active_revision > revision);
    assert!(!has_edge(&routes, A));
}
