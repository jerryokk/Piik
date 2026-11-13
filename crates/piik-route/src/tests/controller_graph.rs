//! Controller tests: graph construction, repair, departure and rebinding.

use super::*;
use crate::*;

// TS 2828
#[test]
fn admits_twenty_viewers_through_one_bounded_graph() {
    for capacity in [1, 2, 3] {
        let mut routes = controller(capacity, opts());
        let viewers: Vec<String> = (0..20)
            .map(|index| format!("viewer_{index:02}_12345678"))
            .collect();
        for (index, peer_id) in viewers.iter().enumerate() {
            add_viewer(&mut routes, peer_id, capacity, None);
            let operation = must(routes.reconcile(index as i64 * 2).operation);
            assert_eq!(&operation.child_peer_id, peer_id);
            commit_current(
                &mut routes,
                index as i64 * 2 + 1,
                &format!("{peer_id}_connection"),
            );
        }
        let snapshot = routes.snapshot();
        assert_eq!(snapshot.upstream_by_viewer.len(), 20);
        for parent in std::iter::once(HOST.to_owned()).chain(viewers) {
            let child_count = snapshot
                .upstream_by_viewer
                .iter()
                .filter(|(_, edge)| {
                    edge.kind == UpstreamKind::Peer
                        && edge.physical_active
                        && edge.parent_peer_id == parent
                })
                .count() as i64;
            assert!(
                child_count <= capacity,
                "parent {parent:?} has {child_count} children"
            );
        }
    }
}

// TS 2856
#[test]
fn admits_waiting_viewers_through_existing_sfu_before_direct_convergence() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_host_publication("publication_generation", res("publication_resource"), "");
    routes.hydrate_edge(
        B,
        sfu_edge("publication_generation", "b_sfu", "b_subscription"),
    );
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(HOST, "c_from_host"));
    let waiting: Vec<String> = (0..18)
        .map(|index| format!("silent_{index:02}_12345678"))
        .collect();
    for peer_id in &waiting {
        add_viewer(&mut routes, peer_id, 0, None);
    }
    let mut now = 0;
    for (index, peer_id) in waiting.iter().enumerate() {
        let direct_operation = must(routes.reconcile(now).operation);
        assert_eq!(&direct_operation.child_peer_id, peer_id);
        assert_eq!(
            direct_operation.candidates[0].tuple.kind,
            UpstreamKind::Peer
        );
        begin_candidate(
            &mut routes,
            begin(now + 1, &format!("{peer_id}_direct"), direct()),
        );
        routes.operation_expired(direct_operation.wake_at_ms);
        let sfu_operation = must(routes.snapshot().operation);
        assert_eq!(
            sfu_operation.candidates[sfu_operation.cursor].tuple,
            sfu_tuple(PublicationPlan::Reuse)
        );
        let connection = format!("{peer_id}_sfu");
        let prepared = begin_operation(
            &mut routes,
            begin(
                direct_operation.wake_at_ms + 1,
                &connection,
                sfu_reuse(&format!("{peer_id}_subscription")),
            ),
        );
        assert!(
            routes
                .candidate_ready(
                    &guard_for(peer_id, current(&prepared).revision, &connection),
                    direct_operation.wake_at_ms + 2,
                    None,
                    CandidateProof::default()
                )
                .accepted
        );
        now = direct_operation.wake_at_ms + 3;
        let b_edge = edge_of(&routes, B);
        assert_eq!(
            (b_edge.kind, b_edge.connection_id.as_str()),
            (UpstreamKind::Sfu, "b_sfu")
        );
        let c_edge = edge_of(&routes, C);
        assert_eq!(
            (
                c_edge.kind,
                c_edge.parent_peer_id.as_str(),
                c_edge.connection_id.as_str()
            ),
            (UpstreamKind::Peer, HOST, "c_from_host")
        );
        let admitted = edge_of(&routes, peer_id);
        assert!(admitted.kind == UpstreamKind::Sfu && admitted.usable);
        assert_eq!(routes.snapshot().upstream_by_viewer.len(), index + 3);
    }
    assert_eq!(routes.snapshot().upstream_by_viewer.len(), 20);
    let publication = routes
        .snapshot()
        .host_publication
        .expect("expected a host publication");
    assert!(publication.generation == "publication_generation" && publication.usable);
    assert_eq!(
        must(routes.reconcile(now).operation).reason,
        DemandReason::DirectConvergence
    );
}

// TS 2943
#[test]
fn builds_deterministic_acyclic_route_without_depth_cap() {
    for capacity in [1, 2, 3] {
        let mut routes = controller(capacity, opts());
        for peer_id in [A, B, C, D] {
            add_viewer(&mut routes, peer_id, capacity, None);
            assert_eq!(must(routes.reconcile(0).operation).child_peer_id, peer_id);
            commit_current(&mut routes, 1, &format!("{peer_id}_connection"));
        }
        let snapshot = routes.snapshot();
        assert_eq!(snapshot.upstream_by_viewer.len(), 4);
        for (child, edge) in snapshot.upstream_by_viewer.iter() {
            assert_eq!(edge.kind, UpstreamKind::Peer);
            assert_ne!(&edge.parent_peer_id, child);
        }
        if capacity == 1 {
            // Pins the stable pair rank / depth ordering: at C=1 D hangs below C.
            let d_edge = edge_of(&routes, D);
            assert_eq!(
                (d_edge.kind, d_edge.parent_peer_id.as_str()),
                (UpstreamKind::Peer, C)
            );
        }
    }
}

// TS 2969
#[test]
fn keeps_old_edge_until_exact_candidate_first_frame_ready() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    routes.reconcile(0);
    commit_current(&mut routes, 1, "old_connection");
    routes.reconcile(2);
    commit_current(&mut routes, 3, "b_connection");
    let revision = routes.revision();
    assert!(routes.invalidate_edge(
        edge_guard(A, "host_session", revision, "old_connection"),
        None
    ));
    routes.reconcile(100);
    let prepared = begin_operation(&mut routes, begin(101, "new_connection", direct()));
    assert_eq!(edge_of(&routes, A).connection_id, "old_connection");
    let attempt = current(&prepared).revision;
    assert!(
        !routes
            .candidate_ready(
                &guard_for(A, attempt - 1, "new_connection"),
                102,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, attempt, "new_connection"),
                102,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let edge = edge_of(&routes, A);
    assert!(edge.connection_id == "new_connection" && edge.parent_peer_id == B && edge.usable);
}

// TS 3020
#[test]
fn invalidates_one_exact_active_peer_edge_from_parent_idempotently() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 1, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    let report = ParentEdgeGuard {
        parent_peer_id: HOST.to_owned(),
        parent_session_id: "host_session".to_owned(),
        route_revision: routes.revision(),
        connection_id: "a_from_host".to_owned(),
    };
    assert!(routes.invalidate_direct_edge_from_parent(report.clone(), ms(10)));
    let after_first = routes.snapshot();
    let a_edge = after_first.upstream_by_viewer.get(&A.to_owned()).unwrap();
    assert!(!a_edge.usable && a_edge.connection_id == "a_from_host");
    let b_edge = after_first.upstream_by_viewer.get(&B.to_owned()).unwrap();
    assert!(b_edge.usable && b_edge.connection_id == "b_from_host");
    assert!(routes.invalidate_direct_edge_from_parent(report.clone(), ms(11)));
    assert_eq!(routes.snapshot().fact_version, after_first.fact_version);
    let unknown = ParentEdgeGuard {
        connection_id: "unknown_connection".to_owned(),
        ..report
    };
    assert!(!routes.invalidate_direct_edge_from_parent(unknown, None));
    let operation = must(routes.reconcile(12).operation);
    assert_eq!(
        (operation.child_peer_id.as_str(), operation.reason),
        (A, DemandReason::EdgeUnavailable)
    );
    assert_eq!(tuples(&operation), [peer_tuple(B)]);
}

// TS 3819
#[test]
fn replans_when_current_candidate_parent_fails() {
    let mut routes = controller(3, opts());
    for (peer, connection) in [(A, "a_old"), (B, "b_old"), (C, "c_old")] {
        add_viewer(&mut routes, peer, 1, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    let revision = routes.revision();
    assert!(routes.invalidate_edge(edge_guard(A, "host_session", revision, "a_old"), None));
    routes.reconcile(0);
    let attempt = begin_operation(&mut routes, begin(1, "a_new", direct()));
    let tuple = current(&attempt).tuple.clone();
    assert_eq!(tuple.kind, UpstreamKind::Peer);
    let failed_parent = tuple.parent_peer_id.clone();
    let failed_connection = if failed_parent == B { "b_old" } else { "c_old" };
    assert!(routes.invalidate_edge(
        edge_guard(&failed_parent, "host_session", revision, failed_connection),
        None
    ));
    assert!(
        !routes
            .candidate_ready(
                &guard_for(A, current(&attempt).revision, "a_new"),
                2,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    let replacement = begin_operation(&mut routes, begin(2, "a_new_2", direct()));
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&replacement).revision, "a_new_2"),
                3,
                None,
                CandidateProof::default()
            )
            .accepted
    );
    assert_eq!(
        must(routes.reconcile(3).operation).child_peer_id,
        failed_parent
    );
}

// TS 3886
#[test]
fn keeps_active_candidate_when_unrelated_session_is_replaced() {
    let mut routes = controller(2, opts());
    for (peer, connection) in [(B, "b_from_host"), (C, "c_from_host")] {
        add_viewer(&mut routes, peer, 1, None);
        routes.hydrate_edge(peer, peer_edge(HOST, connection));
    }
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    let active = begin_operation(&mut routes, begin(1, "a_active_candidate", direct()));
    let tuple = current(&active).tuple.clone();
    assert_eq!(tuple.kind, UpstreamKind::Peer);
    let unrelated = if tuple.parent_peer_id == B { C } else { B };
    eq_labels(
        &routes.upsert_participant(
            viewer_input(unrelated, &format!("{unrelated}_replacement"), 1),
            ms(2),
        ),
        &[],
    );
    let snapshot = must(routes.snapshot().operation);
    let live = current(&snapshot);
    assert_eq!(live.revision, operation.base_revision + 1);
    assert_eq!(live.connection_id, "a_active_candidate");
}

#[test]
fn uses_departed_relays_freed_host_slot_without_moving_healthy_descendants() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 2, None);
    add_viewer(&mut routes, C, 0, None);
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_root"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_root"));
    routes.hydrate_edge(C, peer_edge(A, "c_from_a"));
    routes.hydrate_edge(D, peer_edge(B, "d_from_b"));
    assert!(routes.confirm_departure(B, None));
    let repair = must(routes.reconcile(0).operation);
    assert_eq!(
        (repair.child_peer_id.as_str(), &repair.candidates[0].tuple),
        (D, &peer_tuple(HOST))
    );
    commit_current(&mut routes, 1, "d_from_host");
    no_operation(routes.reconcile(2).operation);
    assert!(!has_edge(&routes, B));
    add_viewer(&mut routes, E, 0, None);
    let join = must(routes.reconcile(3).operation);
    assert_eq!(
        (join.child_peer_id.as_str(), &join.candidates[0].tuple),
        (E, &peer_tuple(A))
    );
    commit_current(&mut routes, 4, "e_from_a");
    for (peer, parent, connection) in [
        (C, A, "c_from_a"),
        (D, HOST, "d_from_host"),
        (E, A, "e_from_a"),
    ] {
        let edge = edge_of(&routes, peer);
        assert_eq!(
            (edge.parent_peer_id.as_str(), edge.connection_id.as_str()),
            (parent, connection)
        );
    }
}

// TS 3967
#[test]
fn keeps_grace_media_then_reparents_confirmed_relay_departure() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 2, None);
    add_viewer(&mut routes, B, 1, None);
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_connection"));
    routes.hydrate_edge(B, peer_edge(A, "b_connection"));
    routes.hydrate_edge(C, peer_edge(B, "c_connection"));
    assert!(routes.disconnect_session(A, &session_of(A)));
    no_operation(routes.reconcile(0).operation);
    assert!(routes.confirm_departure(A, None));
    assert_eq!(must(routes.reconcile(1).operation).child_peer_id, B);
    commit_current(&mut routes, 2, "b_reparented");
    let before_prune = routes.revision();
    let after = routes.reconcile(3);
    assert!(after.removed_peer_ids.contains(&A.to_owned()));
    assert!(routes.revision() > before_prune);
    let c_edge = edge_of(&routes, C);
    assert_eq!(
        (
            c_edge.parent_peer_id.as_str(),
            c_edge.connection_id.as_str()
        ),
        (B, "c_connection")
    );
}

// TS 3991
#[test]
fn moves_newest_overflow_children_and_supports_effective_capacity_zero() {
    let mut routes = controller(3, opts());
    add_viewer(&mut routes, A, 3, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_connection"));
    for peer_id in [B, C, D] {
        add_viewer(&mut routes, peer_id, 0, None);
        routes.hydrate_edge(peer_id, peer_edge(A, &format!("{peer_id}_connection")));
    }
    assert!(routes.set_effective_capacity(A, &session_of(A), 1, None));
    assert_eq!(must(routes.reconcile(0).operation).child_peer_id, D);
    eq_labels(&routes.set_paused(true, None), &[]);
    assert!(routes.set_effective_capacity(A, &session_of(A), 0, None));
    eq_labels(&routes.set_paused(false, None), &[]);
    assert_eq!(must(routes.reconcile(1).operation).child_peer_id, D);

    let mut blocked = controller(1, opts());
    add_viewer(&mut blocked, A, 0, None);
    add_viewer(&mut blocked, B, 0, None);
    blocked.hydrate_edge(A, peer_edge(HOST, "blocked_a"));
    blocked.hydrate_edge(B, peer_edge(A, "blocked_b"));
    no_operation(blocked.reconcile(0).operation);
    no_operation(blocked.reconcile(1).operation);
}

/// A capacity reduction must move a child that still has a usable path before
/// retiring the relay branch that carries it. The blocked child keeps the old
/// newest-overflow fallback, so both outcomes stay covered.
#[test]
fn prefers_movable_overflow_children_over_blocked_fallback() {
    let mut routes = controller(3, opts());
    add_viewer(&mut routes, A, 3, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_connection"));
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(B, peer_edge(A, "b_connection"));
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(B, "c_connection"));

    // B's only parent is A; exhausting A leaves B without a candidate.
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_edge(D, peer_edge(HOST, "d_connection"));

    assert!(routes.set_effective_capacity(A, &session_of(A), 0, None));
    let operation = must(routes.reconcile(0).operation);
    // B is movable (it can reach HOST directly), so it is selected before any
    // blocked overflowing child is treated as failure work.
    assert_eq!(operation.child_peer_id, B);
    assert_eq!(operation.reason, DemandReason::CapacityReduction);
}

// TS 4199
#[test]
fn binds_reserve_and_skip_results_to_exact_candidate_cursor() {
    let mut reserve = controller(2, opts());
    add_viewer(&mut reserve, A, 2, None);
    reserve.hydrate_edge(A, peer_edge(HOST, "a_active"));
    add_viewer(&mut reserve, B, 0, None);
    let first = must(reserve.reconcile(0).operation);
    assert_eq!(first.candidates[0].tuple, peer_tuple(HOST));
    reserve.set_effective_capacity(HOST, "host_session", 0, None);
    let stale = reserve.begin_current_candidate(BeginInput {
        guard: cursor_guard(&first),
        ..begin(
            1,
            "reserved_for_old_cursor",
            direct_overlap("stale_reservation"),
        )
    });
    assert!(!stale.accepted);
    eq_labels(&stale.released, &["stale_reservation"]);
    let rebuilt = must(reserve.snapshot().operation);
    assert_eq!(rebuilt.cursor, 0);
    assert_eq!(tuples(&rebuilt), [peer_tuple(A)]);
    assert!(rebuilt.current.is_none());

    let mut skip = controller(2, opts());
    add_viewer(&mut skip, A, 2, None);
    skip.hydrate_edge(A, peer_edge(HOST, "a_active"));
    add_viewer(&mut skip, B, 0, None);
    let stale_cursor = must(skip.reconcile(0).operation);
    skip.set_effective_capacity(HOST, "host_session", 0, None);
    assert!(
        !skip
            .skip_current_candidate(&cursor_guard(&stale_cursor), 1, RejectionBucket::Stale)
            .accepted
    );
    let pending = must(skip.snapshot().operation);
    assert_eq!(pending.cursor, 0);
    assert_eq!(pending.candidates[0].tuple, peer_tuple(A));
}

// TS 4243
#[test]
fn keeps_only_old_sfu_roots_that_still_anchor_peer_descendants() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 1, None);
    add_viewer(&mut routes, C, 0, None);
    add_viewer(&mut routes, D, 0, None);
    routes.hydrate_host_publication("publication_1", res("publication_resource_1"), "");
    for (peer, subscription) in [
        (A, "subscription_a"),
        (B, "subscription_b"),
        (D, "subscription_d"),
    ] {
        routes.hydrate_edge(
            peer,
            sfu_edge("publication_1", &format!("{peer}_sfu_1"), subscription),
        );
    }
    routes.hydrate_edge(C, peer_edge(B, "c_from_b"));
    let guard = PublicationGuard {
        host_session_id: "host_session".to_owned(),
        route_revision: 0,
        generation: "publication_1".to_owned(),
        connection_id: "publication:publication_1".to_owned(),
    };
    assert!(routes.invalidate_host_publication(guard, ms(10)));
    routes.touch_external_facts();
    let operation = must(routes.reconcile(20).operation);
    assert_eq!(
        operation.candidates[0].tuple,
        sfu_tuple(PublicationPlan::Replace)
    );
    expect_transition(
        &operation.candidates[0].endpoint_transition,
        TransitionKind::Overlap,
        HOST,
    );
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            21,
            "a_sfu_2",
            "publication_2",
            "publication_connection_2",
            sfu_create_overlap(
                "subscription_a_2",
                "publication_resource_2",
                "publication_overlap",
            ),
        ),
    );
    let settled = routes.candidate_ready(
        &guard_for(A, current(&prepared).revision, "a_sfu_2"),
        22,
        None,
        CandidateProof::default(),
    );
    assert!(settled.accepted);
    contains_labels(
        &settled.released,
        &[
            "publication_overlap",
            "subscription_a",
            "subscription_b",
            "subscription_d",
            "publication_resource_1",
        ],
    );
    assert_eq!(edge_of(&routes, A).publication_generation, "publication_2");
    let b_edge = edge_of(&routes, B);
    assert!(b_edge.kind == UpstreamKind::Sfu && b_edge.publication_generation == "publication_1");
    assert!(!b_edge.usable && !b_edge.physical_active);
    let c_edge = edge_of(&routes, C);
    assert_eq!(
        (c_edge.kind, c_edge.parent_peer_id.as_str()),
        (UpstreamKind::Peer, B)
    );
    assert!(!has_edge(&routes, D));
    let child = routes.route_diagnostic_snapshot(22).children[1].clone();
    assert_eq!(child.demand_age_ms, Some(12));
    assert_eq!(
        child.final_route,
        piik_protocol::RouteDiagnosticFinalRoute::Waiting
    );
    let next = must(routes.reconcile(23).operation);
    assert_eq!(
        (next.child_peer_id.as_str(), next.reason),
        (B, DemandReason::EdgeUnavailable)
    );
}

// TS 4328
#[test]
fn rebinds_direct_and_sfu_media_to_replacement_sessions() {
    let mut peers = controller(2, opts());
    add_viewer(&mut peers, A, 1, None);
    add_viewer(&mut peers, B, 0, None);
    peers.hydrate_edge(A, peer_edge(HOST, "a_direct"));
    peers.hydrate_edge(B, peer_edge(A, "b_direct"));
    eq_labels(
        &peers.upsert_participant(viewer_input(A, "a_replacement", 1), None),
        &[],
    );
    let a_edge = edge_of(&peers, A);
    assert!(a_edge.child_session_id == "a_replacement" && a_edge.usable && a_edge.physical_active);
    assert_eq!(edge_of(&peers, B).parent_session_id, "a_replacement");
    eq_labels(
        &peers.upsert_participant(viewer_input(B, "b_replacement", 0), None),
        &[],
    );
    let b_edge = edge_of(&peers, B);
    assert!(b_edge.child_session_id == "b_replacement" && b_edge.usable && b_edge.physical_active);

    let mut sfu = controller(1, sfu_opts());
    add_viewer(&mut sfu, A, 0, None);
    sfu.hydrate_host_publication(
        "publication",
        res("publication_resource"),
        "publication_connection",
    );
    sfu.hydrate_edge(
        A,
        sfu_edge(
            "publication",
            "subscription_connection",
            "subscription_resource",
        ),
    );
    eq_labels(
        &sfu.upsert_participant(host_input("host_replacement", 1), None),
        &[],
    );
    let publication = sfu
        .snapshot()
        .host_publication
        .expect("expected a host publication");
    assert!(
        publication.host_session_id == "host_replacement"
            && publication.usable
            && publication.physical_active
    );
    eq_labels(
        &sfu.upsert_participant(viewer_input(A, "a_replacement", 0), None),
        &[],
    );
    let edge = edge_of(&sfu, A);
    assert!(edge.child_session_id == "a_replacement" && edge.physical_active);
}
