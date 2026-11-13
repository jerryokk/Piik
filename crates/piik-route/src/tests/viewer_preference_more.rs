//! Viewer route preference tests (part two).

use piik_protocol::ViewerRoutePreference;

use super::*;
use crate::*;

#[test]
fn viewer_route_preference_sfu_bootstrap_does_not_queue_peer_continuation() {
    let mut routes = controller(2, sfu_opts());
    for peer in [B, C] {
        add_viewer(&mut routes, peer, 0, None);
        routes.hydrate_edge(peer, peer_edge(HOST, &format!("{peer}_host")));
    }
    add_viewer(&mut routes, A, 0, None);
    let op = must(routes.reconcile(0).operation);
    assert_eq!(op.reason, DemandReason::SfuBootstrap);
    assert_eq!(op.child_peer_id, C);
    // The carrier selects SFU while its existing bootstrap is being prepared.
    routes.set_viewer_route_preference(C, &session_of(C), ViewerRoutePreference::Sfu, ms(1));
    let op = must(routes.reconcile(2).operation);
    assert_eq!(op.reason, DemandReason::SfuBootstrap);
    assert_eq!(op.child_peer_id, C);
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            3,
            "carrier_sfu",
            "publication",
            "publisher",
            sfu_create_overlap("carrier_subscription", "publication", "publication_overlap"),
        ),
    );
    assert!(
        routes
            .candidate_ready(
                &guard_for(C, current(&prepared).revision, "carrier_sfu"),
                4,
                None,
                CandidateProof::default()
            )
            .committed
    );
    assert!(!routes.direct_continuations.has(&C.to_owned()));
    let op = must(routes.reconcile(5).operation);
    assert_eq!(op.child_peer_id, A);
    assert_eq!(tuples(&op), [sfu_tuple(PublicationPlan::Reuse)]);
    let prepared = begin_operation(&mut routes, begin(6, "a_sfu", sfu_reuse("a_subscription")));
    assert!(
        routes
            .candidate_ready(
                &guard_for(A, current(&prepared).revision, "a_sfu"),
                7,
                None,
                CandidateProof::default()
            )
            .committed
    );
    no_operation(routes.reconcile(8).operation);
    no_operation(routes.reconcile(9).operation);
}

#[test]
fn viewer_route_preference_exhaustion_retains_sfu_anchor_for_child_repair() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 1, None);
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    routes.hydrate_edge(A, sfu_edge("publication", "a_sfu", "subscription"));
    routes.hydrate_edge(B, peer_edge(A, "a_b"));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Peer, ms(0));
    let op = must(routes.reconcile(1).operation);
    assert_eq!(tuples(&op), [peer_tuple(HOST)]);
    assert!(routes.source_usable(B));
    routes.skip_current_candidate(&cursor_guard(&op), 2, RejectionBucket::CandidateFailed);
    assert!(!edge_of(&routes, A).physical_active);
    assert!(routes.publication().is_none());
    routes.assert_graph();
    let op = must(routes.reconcile(3).operation);
    // Retiring the last subscription frees the Host's publication copy. The
    // existing ledger permits that less disruptive Peer opportunity once.
    assert_eq!(op.child_peer_id, A);
    assert_eq!(
        op.candidates[0].endpoint_transition.kind,
        TransitionKind::None
    );
    routes.skip_current_candidate(&cursor_guard(&op), 4, RejectionBucket::CandidateFailed);
    let op = must(routes.reconcile(5).operation);
    assert_eq!(op.child_peer_id, B);
    assert_eq!(op.candidates[0].tuple, peer_tuple(HOST));
    commit_current(&mut routes, 6, "host_b");
    assert!(routes.source_usable(B));
    routes.assert_graph();
}

#[test]
fn viewer_route_preference_p2p_exhaustion_then_auto() {
    let mut routes = controller(1, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    routes.hydrate_edge(A, sfu_edge("publication", "a_sfu", "a_subscription"));
    routes.hydrate_edge(B, sfu_edge("publication", "b_sfu", "b_subscription"));
    routes.set_viewer_route_preference(B, &session_of(B), ViewerRoutePreference::Sfu, ms(0));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Peer, ms(0));
    let result = routes.reconcile(1);
    no_operation(result.operation);
    assert_eq!(result.failed_peer_ids, [A]);
    assert!(!has_edge(&routes, A));
    no_operation(routes.reconcile(2).operation);
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Auto, ms(3));
    let op = must(routes.reconcile(4).operation);
    assert_eq!(op.child_peer_id, A);
    assert_eq!(tuples(&op), [sfu_tuple(PublicationPlan::Reuse)]);
}

#[test]
fn fresh_relay_respects_sfu_policy() {
    for site_only in [false, true] {
        let mut routes = controller(
            1,
            Options {
                sfu_enabled: true,
                sfu_only: site_only,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 0, None);
        add_viewer(&mut routes, B, 0, None);
        routes.hydrate_host_publication("publication", res("publication"), "publisher");
        routes.hydrate_edge(A, sfu_edge("publication", "a_sfu", "subscription_a"));
        routes.hydrate_edge(B, sfu_edge("publication", "b_sfu", "subscription_b"));
        if !site_only {
            routes.set_viewer_route_preference(
                A,
                &session_of(A),
                ViewerRoutePreference::Sfu,
                ms(0),
            );
        }
        routes.set_effective_capacity(B, &session_of(B), 1, ms(1));
        no_operation(routes.reconcile(2).operation);
        assert_eq!(edge_of(&routes, A).connection_id, "a_sfu");
    }
}
