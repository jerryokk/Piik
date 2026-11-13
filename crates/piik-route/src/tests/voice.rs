//! Voice route tests.

use super::publication::proof;
use super::*;
use crate::*;

fn video_tree_for_voice(capacity: i64, options: Options) -> Controller {
    let mut routes = controller(capacity, options);
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 1, None);
    routes.hydrate_edge(A, peer_edge(HOST, "video-a"));
    routes.hydrate_edge(B, peer_edge(A, "video-b"));
    routes.set_video_publication("share", false, 1);
    routes
}

#[test]
fn voice_approval_reparents_through_the_existing_operation() {
    let mut routes = video_tree_for_voice(2, opts());
    routes.set_voice_listeners(&[HOST, B], 2);
    let op = must(routes.reconcile(3).operation);
    assert_eq!(op.child_peer_id, B);
    assert_eq!(op.reason, DemandReason::EdgeUnavailable);
    assert_eq!(op.candidates[0].tuple.parent_peer_id, HOST);
    assert_eq!(edge_of(&routes, B).connection_id, "video-b");
    assert!(edge_of(&routes, B).video_ready);
    let prepared = begin_operation(&mut routes, begin(4, "permitted-b", direct()));
    let guard = guard_for(B, current(&prepared).revision, "permitted-b");
    assert!(
        routes
            .candidate_ready(&guard, 5, None, proof("share"))
            .committed
    );
    assert_eq!(edge_of(&routes, B).parent_peer_id, HOST);
    assert!(!routes.voice_route_missing(B));
    no_operation(routes.reconcile(6).operation);
}

#[test]
fn voice_reparent_failure_retains_video_and_permission_delta_reopens_only_demand() {
    let mut routes = video_tree_for_voice(2, opts());
    routes.set_voice_listeners(&[HOST, A, B], 2);
    no_operation(routes.reconcile(3).operation);
    routes.set_voice_listeners(&[HOST, B], 4);
    must(routes.reconcile(5).operation);
    let prepared = begin_operation(&mut routes, begin(5, "failed-b", direct()));
    assert!(
        routes
            .candidate_failed(&guard_for(B, current(&prepared).revision, "failed-b"), 6)
            .accepted
    );
    for now in [7, 20_000, 40_000] {
        no_operation(routes.reconcile(now).operation);
        assert_eq!(edge_of(&routes, B).connection_id, "video-b");
        assert!(edge_of(&routes, B).usable && edge_of(&routes, B).video_ready);
    }
    assert!(!routes.set_voice_listeners(&[B, HOST], 40_001));
    no_operation(routes.reconcile(40_002).operation);
    // A newly permitted relay is an actual new opportunity. Its existing
    // healthy route already restores the listener without replacement.
    routes.set_voice_listeners(&[HOST, A, B], 40_003);
    assert!(!routes.voice_route_missing(B));
    no_operation(routes.reconcile(40_004).operation);
    routes.set_voice_listeners(&[HOST, B], 40_005);
    assert_eq!(must(routes.reconcile(40_006).operation).child_peer_id, B);
}

#[test]
fn voice_parent_requires_permitted_ancestors_and_legacy_routes_remain_unchanged() {
    let mut routes = video_tree_for_voice(2, opts());
    add_viewer(&mut routes, C, 0, None);
    routes.hydrate_edge(C, peer_edge(B, "video-c"));
    // Hydrated test edge follows opt-in; mark its existing video evidence.
    let edge = routes.edge_for_test(C);
    edge.video_ready = true;
    edge.video_generation = "share".to_owned();
    edge.video_connection_id = "video-c".to_owned();
    routes.set_voice_listeners(&[HOST, B, C], 2);
    for plan in routes.build_candidates(C, false) {
        assert!(
            !(plan.tuple.kind == UpstreamKind::Peer
                && (plan.tuple.parent_peer_id == A || plan.tuple.parent_peer_id == B)),
            "candidate path traversed an unapproved listener"
        );
    }
    let mut legacy = controller(1, opts());
    add_viewer(&mut legacy, A, 1, None);
    add_viewer(&mut legacy, B, 0, None);
    legacy.hydrate_edge(A, peer_edge(HOST, "legacy-a"));
    legacy.hydrate_edge(B, peer_edge(A, "legacy-b"));
    assert!(!legacy.set_voice_listeners(&[HOST, B], 2));
    no_operation(legacy.reconcile(3).operation);
}

#[test]
fn voice_permission_repair_can_use_existing_sfu_bootstrap() {
    let mut routes = video_tree_for_voice(1, sfu_opts());
    routes.set_voice_listeners(&[HOST, B], 2);
    // Host's only copy is occupied by video-only A. The ordinary bootstrap
    // operation moves A onto SFU while retaining its healthy video.
    let op = must(routes.reconcile(3).operation);
    assert_eq!(op.reason, DemandReason::SfuBootstrap);
    assert_eq!(op.child_peer_id, A);
    assert_eq!(op.demand_peer_id, B);
    assert!(edge_of(&routes, A).usable && edge_of(&routes, B).usable);
}
