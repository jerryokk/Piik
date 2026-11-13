//! Route policy tests.

use piik_protocol::{RoutePolicy, ViewerRoutePreference};

use super::*;
use crate::*;

fn policy(peer_only: bool, topology_optimization: bool, nat_prediction: bool) -> RoutePolicy {
    RoutePolicy {
        peer_only,
        topology_optimization,
        nat_prediction,
    }
}

#[test]
fn room_policy_peer_only_repairs_retained_sfu_before_video_attachment() {
    let mut routes = controller(
        2,
        Options {
            sfu_enabled: true,
            quality_convergence_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_host_publication("room-publication", res("publication"), "old-publisher");
    routes.hydrate_edge(A, sfu_edge("room-publication", "room-a", "subscription"));
    routes.set_viewer_route_preference(A, &session_of(A), ViewerRoutePreference::Sfu, ms(0));
    routes.set_video_publication("", false, 1);
    routes.set_voice_listeners(&[HOST, A], 2);
    assert!(
        routes
            .set_route_policy(policy(true, false, true), 3)
            .is_empty()
    );
    assert_eq!(
        routes.participant(A).unwrap().route_preference,
        Some(ViewerRoutePreference::Auto)
    );
    assert!(!routes.quality_convergence_enabled);
    assert!(routes.nat_prediction_enabled);
    routes.set_video_publication("peer-only-share", false, 4);
    let op = must(routes.reconcile(5).operation);
    assert_eq!(op.reason, DemandReason::EdgeUnavailable);
    assert_eq!(op.candidates[0].tuple.kind, UpstreamKind::Peer);
    assert_eq!(edge_of(&routes, A).connection_id, "room-a");
    assert!(edge_of(&routes, A).usable);
    let prepared = begin_operation(&mut routes, begin(6, "peer-a", direct()));
    assert_eq!(current(&prepared).connection_attempt.unwrap().current, 1);
    let guard = guard_for(A, current(&prepared).revision, "peer-a");
    let proof = CandidateProof {
        video_generation: "peer-only-share".to_owned(),
        ..CandidateProof::default()
    };
    let settled = routes.candidate_ready(&guard, 7, None, proof);
    assert!(settled.committed);
    eq_labels(&settled.released, &["subscription", "publication"]);
    assert_eq!(edge_of(&routes, A).kind, UpstreamKind::Peer);
    assert_eq!(edge_of(&routes, A).video_generation, "peer-only-share");
    assert!(routes.snapshot().host_publication.is_none());
    no_operation(routes.reconcile(8).operation);
}

#[test]
fn room_policy_change_cancels_old_attachment_and_same_policy_does_not_cancel_work() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    routes.hydrate_edge(A, sfu_edge("publication", "room-a", "subscription"));
    routes.set_video_publication("", false, 1);
    routes.set_video_publication("share", false, 2);
    let late = begin_video_attach(&mut routes, 3, A, "sfu-video-a");
    routes.set_route_policy(policy(true, false, false), 4);
    let proof = CandidateProof {
        video_generation: "share".to_owned(),
        ..CandidateProof::default()
    };
    assert!(!routes.candidate_ready(&late, 5, None, proof).accepted);
    let op = must(routes.reconcile(6).operation);
    assert_eq!(op.candidates[0].tuple.kind, UpstreamKind::Peer);
    let prepared = begin_operation(&mut routes, begin(7, "peer-a", direct()));
    routes.set_route_policy(policy(true, false, false), 8);
    assert_eq!(
        current(&must(routes.operation())).connection_id,
        current(&prepared).connection_id
    );
    assert_eq!(edge_of(&routes, A).connection_id, "room-a");
}

#[test]
fn room_policy_hybrid_reopens_existing_availability_and_sfu_bootstrap() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "room-a"));
    routes.set_video_publication("", false, 1);
    routes.set_voice_listeners(&[HOST, A, B], 2);
    no_operation(routes.reconcile(3).operation);
    assert!(routes.participant(B).unwrap().availability_exhausted);
    routes.set_route_policy(policy(false, true, false), 4);
    assert!(!routes.participant(B).unwrap().availability_exhausted);
    assert!(routes.quality_convergence_enabled);
    assert!(!routes.nat_prediction_enabled);
    let op = must(routes.reconcile(5).operation);
    assert_eq!(op.reason, DemandReason::SfuBootstrap);
    assert_eq!(op.child_peer_id, A);
    assert_eq!(op.demand_peer_id, B);
    assert_eq!(edge_of(&routes, A).connection_id, "room-a");
}
