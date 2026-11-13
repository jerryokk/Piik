//! Relay volunteer tests.

use super::publication::commit_room_peer;
use super::*;
use crate::*;

#[test]
fn relay_volunteer_uses_reported_capability_and_exact_session() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 20, None);
    let capacity =
        |routes: &Controller| routes.participant(A).unwrap().effective_downstream_capacity;
    assert_eq!(capacity(&routes), 2);
    assert!(!routes.set_relay_volunteer(A, "stale", true, None));
    assert!(routes.set_relay_volunteer(A, &session_of(A), true, None));
    assert_eq!(capacity(&routes), 20);
    routes.upsert_participant(viewer_input(A, &session_of(A), 100), None);
    assert_eq!(capacity(&routes), 20);
    assert!(routes.set_effective_capacity(A, &session_of(A), 3, None));
    assert_eq!(capacity(&routes), 3);
    // An older App cannot reserve a fourth copy.
    assert_eq!(
        routes.transition_copy_limit(routes.participant(A).unwrap()),
        3
    );
    routes.set_effective_capacity(A, &session_of(A), 20, None);
    assert_eq!(
        routes.transition_copy_limit(routes.participant(A).unwrap()),
        21
    );
    routes.upsert_participant(viewer_input(A, "new-session", 20), None);
    assert!(!routes.participant(A).unwrap().relay_volunteer);
    assert_eq!(capacity(&routes), 2);
    assert!(!routes.set_relay_volunteer(A, &session_of(A), true, None));
}

#[test]
fn relay_volunteer_fanout_and_withdrawal_keep_single_route_operation() {
    let mut routes = controller(1, opts());
    routes.set_video_publication("", false, 0);
    add_viewer(&mut routes, A, 20, ms(0));
    routes.reconcile(0);
    commit_room_peer(&mut routes, 0, "host-to-relay");
    routes.set_relay_volunteer(A, &session_of(A), true, ms(1));
    // A relay is itself one of the twenty Viewers, so nineteen children is
    // the full room through an ordinary volunteering Viewer.
    for index in 0..19 {
        let child = format!("child_{index:02}");
        let at = index + 2;
        add_viewer(&mut routes, &child, 1, ms(at));
        let op = must(routes.reconcile(at).operation);
        assert_eq!(op.candidates[0].tuple.parent_peer_id, A);
        commit_room_peer(&mut routes, at, &format!("{child}-edge"));
    }
    assert_eq!(routes.physical_copies(A), 19);
    routes.set_relay_volunteer(A, &session_of(A), false, ms(100));
    assert_eq!(routes.overflow_peer_children(A).len(), 18);
    // Each handoff preserves the incoming relay and remaining physical media;
    // no second planner or bulk teardown is needed to reduce its fanout.
    for index in 0..18 {
        let op = must(routes.reconcile(101 + index).operation);
        assert_eq!(op.reason, DemandReason::CapacityReduction);
        assert_ne!(
            op.candidates[0].tuple.parent_peer_id, A,
            "withdrawn relay admitted a new child"
        );
        commit_room_peer(&mut routes, 101 + index, &format!("handoff-{index}"));
    }
    assert_eq!(routes.physical_copies(A), 1);
    assert_eq!(edge_of(&routes, A).connection_id, "host-to-relay");
}

#[test]
fn relay_volunteer_twenty_child_physical_overlap_requires_opt_in() {
    let mut routes = controller(2, opts());
    routes.set_video_publication("", false, 0);
    routes.set_effective_capacity(HOST, "host_session", 20, None);
    routes.set_relay_volunteer(HOST, "host_session", true, None);
    for index in 0..20 {
        let child = format!("viewer_{index:02}");
        add_viewer(&mut routes, &child, 0, ms(index));
        commit_room_peer(&mut routes, index, &format!("{child}-edge"));
    }
    let plan = routes.plan_candidate("viewer_00", &regenerate_tuple(HOST));
    assert_eq!(
        plan.map(|plan| plan.endpoint_transition.kind),
        Some(TransitionKind::Overlap)
    );
    routes.set_relay_volunteer(HOST, "host_session", false, ms(100));
    assert!(
        routes
            .plan_candidate("viewer_00", &regenerate_tuple(HOST))
            .is_none(),
        "withdrawn volunteer offered another copy"
    );
}

#[test]
fn host_video_connection_identity_preserves_room_publication() {
    let mut routes = controller(2, opts());
    routes.hydrate_host_publication("logical-publication", res("publication"), "old-video");
    assert!(!routes.set_host_video_connection_id("new-video"));
    routes.set_video_publication("share", false, 0);
    let publication = routes.publication().unwrap();
    assert!(!routes.set_host_video_connection_id(""));
    assert!(routes.set_host_video_connection_id("new-video"));
    let current = routes.publication().unwrap();
    assert_eq!(current.generation, publication.generation);
    assert!(std::sync::Arc::ptr_eq(
        current.resource.as_ref().unwrap(),
        publication.resource.as_ref().unwrap()
    ));
    assert_eq!(current.usable, publication.usable);
    assert_eq!(current.physical_active, publication.physical_active);
    assert_eq!(current.connection_id, "new-video");
}
