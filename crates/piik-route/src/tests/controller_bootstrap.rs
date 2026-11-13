//! Controller tests: SFU bootstrap and publication repair.

use super::*;
use crate::*;

pub(crate) fn retire_last_sfu_publication_after_reuse_deadline() -> (Controller, i64) {
    let mut routes = controller(2, sfu_opts());
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 0, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    add_viewer(&mut routes, A, 0, ms(0));

    let bootstrap = must(routes.reconcile(0).operation);
    assert_eq!(bootstrap.demand_peer_id, A);
    assert_eq!(bootstrap.reason, DemandReason::SfuBootstrap);
    assert_eq!(tuples(&bootstrap), [sfu_tuple(PublicationPlan::Create)]);
    let carrier_peer_id = bootstrap.child_peer_id.clone();
    let carrier = begin_operation(
        &mut routes,
        begin_publication(
            1,
            "carrier_sfu",
            "publication_generation",
            "publication_connection",
            sfu_create_overlap(
                "carrier_subscription",
                "publication_resource",
                "publication_overlap",
            ),
        ),
    );
    let guard = CandidateGuard {
        child_peer_id: carrier.child_peer_id.clone(),
        child_session_id: carrier.child_session_id.clone(),
        revision: current(&carrier).revision,
        connection_id: "carrier_sfu".to_owned(),
    };
    assert!(
        routes
            .candidate_ready(&guard, 2, None, CandidateProof::default())
            .accepted
    );

    let demand = must(routes.reconcile(3).operation);
    assert_eq!(
        (
            demand.child_peer_id.as_str(),
            demand.demand_peer_id.as_str()
        ),
        (A, A)
    );
    assert_eq!(tuples(&demand), [sfu_tuple(PublicationPlan::Reuse)]);
    begin_candidate(
        &mut routes,
        begin(4, "demand_sfu_reuse", sfu_reuse("demand_subscription")),
    );
    assert_eq!(
        routes
            .operation_expired(demand.deadline_at_ms)
            .failed_peer_ids,
        [A]
    );

    let convergence = must(routes.reconcile(demand.deadline_at_ms + 1).operation);
    assert_eq!(convergence.child_peer_id, carrier_peer_id);
    assert_eq!(convergence.reason, DemandReason::DirectConvergence);
    assert_eq!(tuples(&convergence), [peer_tuple(HOST)]);
    let direct_op = begin_operation(
        &mut routes,
        begin(
            demand.deadline_at_ms + 2,
            "carrier_direct",
            direct_overlap("carrier_direct_overlap"),
        ),
    );
    let guard = CandidateGuard {
        child_peer_id: direct_op.child_peer_id.clone(),
        child_session_id: direct_op.child_session_id.clone(),
        revision: current(&direct_op).revision,
        connection_id: "carrier_direct".to_owned(),
    };
    let committed = routes.candidate_ready(
        &guard,
        demand.deadline_at_ms + 3,
        None,
        CandidateProof::default(),
    );
    assert!(committed.accepted);
    contains_labels(
        &committed.released,
        &["carrier_subscription", "publication_resource"],
    );
    assert!(routes.snapshot().host_publication.is_none());
    (routes, demand.deadline_at_ms + 4)
}

pub(crate) fn invalidate_active_sfu_edge() -> Controller {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_host_publication("active_publication", res("publication_resource"), "");
    routes.hydrate_edge(
        A,
        sfu_edge(
            "active_publication",
            "active_sfu_edge",
            "active_subscription",
        ),
    );
    let revision = routes.revision();
    assert!(routes.invalidate_edge(edge_guard(A, "", revision, "active_sfu_edge"), None));
    routes
}

#[test]
fn active_rebuild_retires_bootstrap_reservation_without_losing_waiting_demand() {
    let mut routes = controller(2, sfu_opts());
    for child in [B, C] {
        add_viewer(&mut routes, child, 0, None);
        routes.hydrate_edge(child, peer_edge(HOST, &format!("{child}_from_host")));
    }
    add_viewer(&mut routes, A, 0, ms(0));
    let bootstrap = must(routes.reconcile(0).operation);
    assert_eq!(bootstrap.reason, DemandReason::SfuBootstrap);
    let carrier = bootstrap.child_peer_id.clone();
    let retained = edge_of(&routes, &carrier);
    let attempt = begin_operation(
        &mut routes,
        begin_publication(
            1,
            "carrier_sfu",
            "publication",
            "publication_connection",
            sfu_create_overlap("subscription", "publication", "overlap"),
        ),
    );
    let adopt = AdoptDirectConnectionInput {
        edge: edge_guard(
            &carrier,
            "host_session",
            routes.revision(),
            &retained.connection_id,
        ),
        new_connection_id: "recovered_connection".to_owned(),
    };
    let replaced = routes.adopt_direct_connection(adopt, 2);
    assert!(replaced.accepted);
    eq_labels(
        &replaced.released,
        &["subscription", "publication", "overlap"],
    );
    no_operation(routes.snapshot().operation);
    assert_eq!(
        edge_of(&routes, &carrier).connection_id,
        "recovered_connection"
    );
    assert!(
        !routes
            .candidate_ready(
                &guard_for(&carrier, current(&attempt).revision, "carrier_sfu"),
                3,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let next = must(routes.reconcile(4).operation);
    assert_eq!(
        (next.reason, next.demand_peer_id.as_str()),
        (DemandReason::SfuBootstrap, A)
    );
}

// TS 3579
#[test]
fn owns_host_full_sfu_bootstrap_failure_by_waiting_demand() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(HOST, "c_from_host"));
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, D, 0, None);

    let operation = must(routes.reconcile(0).operation);
    assert!(!has_tuple(&operation, is_sfu));
    assert_eq!(operation.wake_at_ms, 5_000);
    begin_candidate(&mut routes, begin(1, "a_silent_direct", direct()));
    let blocked = routes.operation_expired(operation.wake_at_ms);
    assert!(blocked.accepted);
    assert!(blocked.failed_peer_ids.is_empty());
    eq_labels(&blocked.released, &[]);
    no_operation(routes.snapshot().operation);

    let wake = operation.wake_at_ms;
    let bootstrap = must(routes.reconcile(wake + 1).operation);
    assert_eq!(
        (
            bootstrap.child_peer_id.as_str(),
            bootstrap.demand_peer_id.as_str()
        ),
        (C, A)
    );
    assert_eq!(
        bootstrap.candidates[0].tuple,
        sfu_tuple(PublicationPlan::Create)
    );
    assert_eq!(
        bootstrap.candidates[0].endpoint_transition.kind,
        TransitionKind::Overlap
    );
    let first = begin_operation(
        &mut routes,
        begin_publication(
            wake + 2,
            "c_bootstrap_sfu",
            "c_bootstrap_publication",
            "c_bootstrap_ingress",
            sfu_create_overlap(
                "c_bootstrap_subscription",
                "c_bootstrap_publication_resource",
                "c_bootstrap_overlap",
            ),
        ),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(C, current(&first).revision, "c_bootstrap_sfu"),
                wake + 3
            )
            .failed_peer_ids
            .is_empty()
    );
    let c_edge = edge_of(&routes, C);
    assert!(c_edge.connection_id == "c_from_host" && c_edge.usable && c_edge.physical_active);

    let second = must(routes.reconcile(wake + 4).operation);
    assert_eq!(
        (
            second.child_peer_id.as_str(),
            second.demand_peer_id.as_str(),
            second.reason
        ),
        (B, A, DemandReason::SfuBootstrap)
    );
    let carrier = begin_operation(
        &mut routes,
        begin_publication(
            wake + 5,
            "b_bootstrap_sfu",
            "b_bootstrap_publication",
            "b_bootstrap_ingress",
            sfu_create_overlap(
                "b_bootstrap_subscription",
                "b_bootstrap_publication_resource",
                "b_bootstrap_overlap",
            ),
        ),
    );
    assert_eq!(
        routes
            .candidate_failed(
                &guard_for(B, current(&carrier).revision, "b_bootstrap_sfu"),
                wake + 6
            )
            .failed_peer_ids,
        [A]
    );
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        let edge = edge_of(&routes, peer);
        assert!(edge.connection_id == connection && edge.usable && edge.physical_active);
    }
    let next = must(routes.reconcile(wake + 7).operation);
    assert_eq!(
        (next.child_peer_id.as_str(), next.reason),
        (D, DemandReason::Join)
    );
    begin_candidate(&mut routes, begin(wake + 8, "d_direct", direct()));
    assert_eq!(
        routes
            .operation_expired(next.deadline_at_ms)
            .failed_peer_ids,
        [D]
    );
    no_operation(routes.reconcile(next.deadline_at_ms + 1).operation);
}

// TS 3680
#[test]
fn abandons_bootstrap_when_host_slot_becomes_available() {
    let mut routes = controller(2, sfu_opts());
    assert!(routes.set_effective_capacity(HOST, "host_session", 1, None));
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    add_viewer(&mut routes, A, 0, None);
    let bootstrap = must(routes.reconcile(0).operation);
    assert_eq!(
        (
            bootstrap.child_peer_id.as_str(),
            bootstrap.demand_peer_id.as_str(),
            bootstrap.reason
        ),
        (B, A, DemandReason::SfuBootstrap)
    );
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            1,
            "stale_bootstrap",
            "stale_publication",
            "stale_ingress",
            sfu_create_overlap(
                "stale_subscription",
                "stale_publication_resource",
                "stale_overlap",
            ),
        ),
    );
    assert!(routes.set_effective_capacity(HOST, "host_session", 2, None));
    let stale = routes.candidate_ready(
        &guard_for(B, current(&prepared).revision, "stale_bootstrap"),
        2,
        None,
        CandidateProof::default(),
    );
    assert!(!stale.accepted);
    contains_labels(
        &stale.released,
        &[
            "stale_subscription",
            "stale_publication_resource",
            "stale_overlap",
        ],
    );
    let b_edge = edge_of(&routes, B);
    assert!(b_edge.connection_id == "b_from_host" && b_edge.usable && b_edge.physical_active);
    let join = must(routes.reconcile(3).operation);
    assert_eq!(
        (
            join.child_peer_id.as_str(),
            join.demand_peer_id.as_str(),
            join.reason
        ),
        (A, A, DemandReason::Join)
    );
}

// TS 3731
#[test]
fn tries_sfu_first_after_bootstrap_and_retains_direct_suffix() {
    let mut routes = controller(2, sfu_opts());
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 1, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    add_viewer(&mut routes, A, 0, None);
    let acquisition = must(routes.reconcile(0).operation);
    let wake = acquisition.wake_at_ms;
    begin_candidate(&mut routes, begin(1, "a_direct_head_start", direct()));
    routes.operation_expired(wake);
    let bootstrap = must(routes.reconcile(wake + 1).operation);
    let carrier = begin_operation(
        &mut routes,
        begin_publication(
            wake + 2,
            "bootstrap_sfu",
            "bootstrap_publication",
            "bootstrap_ingress",
            sfu_create_overlap(
                "carrier_subscription",
                "bootstrap_publication_resource",
                "bootstrap_overlap",
            ),
        ),
    );
    let guard = CandidateGuard {
        child_peer_id: bootstrap.child_peer_id.clone(),
        child_session_id: bootstrap.child_session_id.clone(),
        revision: current(&carrier).revision,
        connection_id: "bootstrap_sfu".to_owned(),
    };
    assert!(
        routes
            .candidate_ready(&guard, wake + 3, None, CandidateProof::default())
            .accepted
    );

    let demand = must(routes.reconcile(wake + 4).operation);
    assert_eq!(
        (
            demand.child_peer_id.as_str(),
            demand.demand_peer_id.as_str(),
            demand.reason
        ),
        (A, A, DemandReason::Join)
    );
    assert_eq!(
        demand.candidates[0].tuple,
        sfu_tuple(PublicationPlan::Reuse)
    );
    assert!(tuples_from(&demand, 1).iter().all(is_peer));
    let subscription = begin_operation(
        &mut routes,
        begin(wake + 5, "a_sfu", sfu_reuse("a_subscription")),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&subscription).revision, "a_sfu"),
                wake + 6
            )
            .accepted
    );
    let pending = must(routes.snapshot().operation);
    assert_eq!(
        pending.candidates[pending.cursor].tuple.kind,
        UpstreamKind::Peer
    );
    let direct_op = begin_operation(&mut routes, begin(wake + 7, "a_deferred_direct", direct()));
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&direct_op).revision, "a_deferred_direct"),
                wake + 8,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Peer, "a_deferred_direct")
    );
    let convergence = must(routes.reconcile(wake + 9).operation);
    assert_eq!(
        (convergence.child_peer_id.as_str(), convergence.reason),
        (
            bootstrap.child_peer_id.as_str(),
            DemandReason::DirectConvergence
        )
    );
}

// TS 4015
#[test]
fn aborts_pending_sfu_resources_on_pause_and_repairs_publication() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_host_publication("publication_1", res("publication_resource"), "");
    routes.hydrate_edge(A, sfu_edge("publication_1", "a_sfu", "a_subscription"));
    let operation = must(routes.reconcile(0).operation);
    assert_eq!(operation.candidates[0].tuple, peer_tuple(A));
    begin_candidate(&mut routes, begin(1, "b_direct_head_start", direct()));
    routes.operation_expired(operation.wake_at_ms);
    begin_candidate(
        &mut routes,
        begin(
            operation.wake_at_ms + 1,
            "b_candidate",
            sfu_reuse("b_subscription"),
        ),
    );
    eq_labels(&routes.set_paused(true, None), &["b_subscription"]);
    no_operation(routes.snapshot().operation);
    assert_eq!(edge_of(&routes, A).connection_id, "a_sfu");
    routes.set_paused(false, None);
    let guard = PublicationGuard {
        host_session_id: "host_session".to_owned(),
        route_revision: routes.revision(),
        generation: "publication_1".to_owned(),
        connection_id: "publication:publication_1".to_owned(),
    };
    assert!(routes.invalidate_host_publication(guard, None));
    routes.touch_external_facts();
    let repair = must(routes.reconcile(2).operation);
    assert_eq!(
        (repair.child_peer_id.as_str(), &repair.candidates[0].tuple),
        (A, &peer_tuple(HOST))
    );
    skip_candidate(&mut routes, 3);
    assert_eq!(
        must(routes.snapshot().operation).candidates[1].tuple,
        sfu_tuple(PublicationPlan::Replace)
    );
    let replace = |reservation| {
        begin_publication(
            3,
            "a_sfu_2",
            "publication_2",
            "publication_connection_2",
            reservation,
        )
    };
    expect_panic("endpoint overlap reservation", || {
        begin_candidate(
            &mut routes,
            replace(sfu_create("a_subscription_2", "publication_resource_2")),
        );
    });
    let repair = begin_operation(
        &mut routes,
        replace(sfu_create_overlap(
            "a_subscription_2",
            "publication_resource_2",
            "publication_overlap",
        )),
    );
    let repaired = routes.candidate_ready(
        &guard_for(A, current(&repair).revision, "a_sfu_2"),
        4,
        None,
        CandidateProof::default(),
    );
    assert!(repaired.accepted);
    assert_eq!(repaired.released.len(), 3);
    contains_labels(
        &repaired.released,
        &[
            "publication_overlap",
            "a_subscription",
            "publication_resource",
        ],
    );
    let publication = routes
        .snapshot()
        .host_publication
        .expect("expected a repaired publication");
    assert!(
        publication.generation == "publication_2"
            && publication.usable
            && publication.physical_active
    );

    let mut bootstrap = controller(1, sfu_opts());
    add_viewer(&mut bootstrap, A, 0, None);
    add_viewer(&mut bootstrap, B, 0, None);
    bootstrap.hydrate_edge(A, peer_edge(HOST, "bootstrap_a"));
    let operation = must(bootstrap.reconcile(0).operation);
    assert_eq!(
        (
            operation.child_peer_id.as_str(),
            &operation.candidates[0].tuple
        ),
        (A, &sfu_tuple(PublicationPlan::Create))
    );
    let input = |reservation| {
        begin_publication(
            2,
            "bootstrap_sfu",
            "bootstrap_publication",
            "bootstrap_ingress_connection",
            reservation,
        )
    };
    expect_panic("endpoint overlap reservation", || {
        begin_candidate(
            &mut bootstrap,
            input(sfu_create("bootstrap_subscription", "bootstrap_ingress")),
        );
    });
    let prepared = begin_operation(
        &mut bootstrap,
        input(sfu_create_overlap(
            "bootstrap_subscription",
            "bootstrap_ingress",
            "bootstrap_overlap",
        )),
    );
    assert!(
        bootstrap
            .candidate_ready(
                &guard_for(A, current(&prepared).revision, "bootstrap_sfu"),
                3,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    assert_eq!(must(bootstrap.reconcile(4).operation).child_peer_id, B);
}

// TS 4153
#[test]
fn reopens_sfu_bootstrap_after_physical_publication_retires() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_host_publication("retired_publication", res("publication_resource"), "");
    routes.hydrate_edge(
        A,
        sfu_edge("retired_publication", "a_from_sfu", "a_subscription"),
    );
    let publication = routes.snapshot().host_publication.unwrap();
    let released = routes.retire_host_publication(PublicationGuard {
        host_session_id: publication.host_session_id,
        route_revision: routes.revision(),
        generation: publication.generation,
        connection_id: publication.connection_id,
    });
    contains_labels(&released, &["a_subscription", "publication_resource"]);
    assert!(routes.snapshot().host_publication.is_none());
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 0, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    let reconciled = routes.reconcile(1);
    assert!(reconciled.failed_peer_ids.is_empty());
    let operation = must(reconciled.operation);
    assert_eq!(
        (operation.demand_peer_id.as_str(), operation.reason),
        (A, DemandReason::SfuBootstrap)
    );
    assert_eq!(tuples(&operation), [sfu_tuple(PublicationPlan::Create)]);
}
