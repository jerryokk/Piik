//! Viewer route preference tests (part one).

use piik_protocol::ViewerRoutePreference;

use super::*;
use crate::*;

#[test]
fn viewer_route_preference_auto_restores_direct_continuation() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 2, None);
    routes.hydrate_host_publication("publication", res("publication"), "publication_connection");
    routes.hydrate_edge(A, sfu_edge("publication", "sfu_connection", "subscription"));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Sfu, ms(0));
    no_operation(routes.reconcile(1).operation);
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Auto, ms(2));
    let op = must(routes.reconcile(3).operation);
    assert_eq!(op.reason, DemandReason::DirectConvergence);
    assert_eq!(op.candidates[0].tuple, peer_tuple(HOST));
    assert_eq!(edge_of(&routes, A).connection_id, "sfu_connection");
    let prepared = begin_operation(&mut routes, begin(4, "peer_connection", direct()));
    let late = guard_for(A, current(&prepared).revision, "peer_connection");
    // Selecting SFU again aborts preparation without disturbing playback.
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Sfu, ms(5));
    assert!(
        !routes
            .candidate_ready(&late, 6, None, CandidateProof::default())
            .accepted
    );
    assert_eq!(edge_of(&routes, A).connection_id, "sfu_connection");
    no_operation(routes.reconcile(7).operation);
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Auto, ms(8));
    assert_eq!(
        must(routes.reconcile(9).operation).reason,
        DemandReason::DirectConvergence
    );
    commit_current(&mut routes, 10, "restored_peer");
    assert_eq!(edge_of(&routes, A).kind, UpstreamKind::Peer);
    no_operation(routes.reconcile(12).operation);
}

#[test]
fn viewer_route_preference_exhaustion_repairs_relay_children() {
    let mut routes = controller(2, sfu_opts());
    for peer in [A, B, C] {
        add_viewer(&mut routes, peer, 2, None);
    }
    routes.hydrate_edge(A, peer_edge(HOST, "host_a"));
    routes.hydrate_edge(B, peer_edge(A, "a_b"));
    routes.hydrate_edge(C, peer_edge(B, "b_c"));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Sfu, ms(0));
    let op = must(routes.reconcile(1).operation);
    assert_eq!(op.child_peer_id, A);
    assert_eq!(tuple_kinds(&op), [UpstreamKind::Sfu]);
    assert!(routes.source_usable(C));
    // The retained ingress and entire subtree survive candidate preparation.
    let prepared = begin_operation(
        &mut routes,
        begin_publication(
            2,
            "a_sfu",
            "publication",
            "publisher",
            sfu_create("subscription", "publication"),
        ),
    );
    assert!(routes.source_usable(C));
    routes.candidate_failed(&guard_for(A, current(&prepared).revision, "a_sfu"), 3);
    assert!(!edge_of(&routes, A).physical_active);
    assert!(edge_of(&routes, B).usable);
    let op = must(routes.reconcile(4).operation);
    assert_eq!(op.child_peer_id, B);
    assert_eq!(op.candidates[0].tuple, peer_tuple(HOST));
    commit_current(&mut routes, 5, "host_b");
    assert!(routes.source_usable(C));
    assert_eq!(edge_of(&routes, C).connection_id, "b_c");
    // The exhausted strict choice cannot spin when another branch commits.
    no_operation(routes.reconcile(7).operation);
    no_operation(routes.reconcile(8).operation);
}

#[test]
fn viewer_route_preference_p2p_accepts_sfu_fed_parent() {
    let mut routes = controller(
        1,
        Options {
            sfu_enabled: true,
            quality_convergence_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 1, None);
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    routes.hydrate_edge(A, sfu_edge("publication", "a_sfu", "subscription"));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Sfu, ms(0));
    routes.set_viewer_route_preference(B, &session_of(B), ViewerRoutePreference::Peer, ms(0));
    let op = must(routes.reconcile(1).operation);
    assert_eq!(op.child_peer_id, B);
    assert_eq!(tuples(&op), [peer_tuple(A)]);
    commit_current(&mut routes, 2, "a_b");
    assert!(routes.source_usable(B));
    no_operation(routes.reconcile(4).operation);
    // A fixed incoming kind still permits quality repair of the P2P child.
    let states = [
        SenderQualityState::Healthy,
        SenderQualityState::Degraded,
        SenderQualityState::Degraded,
        SenderQualityState::Degraded,
    ];
    for (index, state) in states.into_iter().enumerate() {
        let sample = SenderState {
            parent_peer_id: A,
            child_peer_id: B,
            connection_id: "a_b",
            state: Some(state),
            accepted_at_ms: 10 + index as i64,
            ..SenderState::default()
        };
        assert!(observe_sender_state(&mut routes, sample).accepted);
    }
    let op = must(routes.reconcile(14).operation);
    assert_eq!(op.reason, DemandReason::QualityConvergence);
    assert_eq!(op.child_peer_id, B);
    assert_eq!(tuples(&op), [regenerate_tuple(A)]);
    let prepared = begin_operation(
        &mut routes,
        begin(
            15,
            "a_b_regenerated",
            direct_overlap("regeneration_overlap"),
        ),
    );
    let revision = current(&prepared).revision;
    let guard = guard_for(B, revision, "a_b_regenerated");
    let approved = CandidateProof {
        relative_quality_approved: true,
        ..CandidateProof::default()
    };
    assert!(!routes.candidate_ready(&guard, 16, None, approved).committed);
    let sample = sender_sample(
        A,
        B,
        revision,
        "a_b_regenerated",
        "regenerated-rtp\0track",
        SenderQualityState::Healthy,
        17,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );
    assert_eq!(edge_of(&routes, B).connection_id, "a_b_regenerated");
    assert_eq!(edge_of(&routes, A).kind, UpstreamKind::Sfu);
    assert_eq!(edge_of(&routes, A).connection_id, "a_sfu");
    no_operation(routes.reconcile(18).operation);
}
