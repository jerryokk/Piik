//! Publication tests (part one) and their room-media helpers.

use super::*;
use crate::*;

pub(crate) fn proof(generation: &str) -> CandidateProof {
    CandidateProof {
        video_generation: generation.to_owned(),
        ..CandidateProof::default()
    }
}

#[track_caller]
pub(crate) fn commit_room_peer(
    routes: &mut Controller,
    at: i64,
    connection: &str,
) -> CandidateGuard {
    must(routes.reconcile(at).operation);
    let op = begin_operation(routes, begin(at, connection, direct()));
    let guard = guard_for(&op.child_peer_id, current(&op).revision, connection);
    assert!(
        !routes
            .candidate_ready(&guard, at + 1, None, CandidateProof::default())
            .accepted
    );
    assert!(
        routes
            .candidate_transport_connected(&guard, at + 1)
            .accepted
    );
    assert!(
        routes
            .candidate_ready(&guard, at + 2, None, CandidateProof::default())
            .committed
    );
    guard
}

#[track_caller]
pub(crate) fn voice_tree() -> Controller {
    let mut routes = controller(1, opts());
    routes.set_video_publication("", false, 0);
    add_viewer(&mut routes, A, 1, ms(1));
    commit_room_peer(&mut routes, 2, "voice-a");
    add_viewer(&mut routes, B, 0, ms(5));
    commit_room_peer(&mut routes, 6, "voice-b");
    assert_eq!(edge_of(&routes, B).parent_peer_id, A);
    routes
}

#[track_caller]
pub(crate) fn begin_video_attach(
    routes: &mut Controller,
    at: i64,
    child: &str,
    connection: &str,
) -> CandidateGuard {
    let op = must(routes.reconcile(at).operation);
    assert_eq!(op.reason, DemandReason::PublicationAttach);
    assert_eq!(op.child_peer_id, child);
    assert_eq!(
        op.attach_connection_id,
        edge_of(routes, child).connection_id
    );
    assert_eq!(
        op.candidates[0].endpoint_transition.kind,
        TransitionKind::None
    );
    let begun = routes.begin_current_candidate(BeginInput {
        guard: cursor_guard(&op),
        reservation: CandidateReservation::default(),
        ..begin(at, connection, CandidateReservation::default())
    });
    assert!(begun.accepted);
    assert!(begun.released.is_empty());
    guard_for(
        child,
        current(begun.operation.as_ref().unwrap()).revision,
        connection,
    )
}

fn adopt(
    routes: &Controller,
    child: &str,
    connection: &str,
    new_connection: &str,
) -> AdoptDirectConnectionInput {
    AdoptDirectConnectionInput {
        edge: edge_guard(child, "host_session", routes.revision(), connection),
        new_connection_id: new_connection.to_owned(),
    }
}

#[test]
fn room_video_recovery_preserves_logical_connection() {
    let mut routes = voice_tree();
    routes.set_video_publication("share", false, 10);
    let a = begin_video_attach(&mut routes, 11, A, "video-a");
    assert!(
        routes
            .candidate_ready(&a, 12, None, proof("share"))
            .committed
    );
    // Optional downstream video attachment may remain pending while recovery
    // replaces the parent's video. Neither mutation replaces voice identity.
    let before = edge_of(&routes, A);
    let input = adopt(&routes, A, &before.connection_id, "recovered-video");
    assert!(routes.adopt_direct_connection(input, 13).accepted);
    let after = edge_of(&routes, A);
    assert_eq!(after.connection_id, before.connection_id);
    assert_eq!(after.video_connection_id, "recovered-video");
    assert_eq!(after.video_generation, "share");
    assert!(after.video_ready);
    assert_eq!(after.parent_session_id, before.parent_session_id);
    assert_eq!(after.opportunity_key, before.opportunity_key);
    routes.set_video_publication("", false, 14);
    let input = adopt(&routes, A, &before.connection_id, "late-video");
    assert!(!routes.adopt_direct_connection(input, 15).accepted);
    assert_eq!(edge_of(&routes, A).connection_id, before.connection_id);
}

#[test]
fn room_voice_tree_attaches_video_serially_and_stop_preserves_graph() {
    let mut routes = voice_tree();
    let before = routes.snapshot();
    routes.set_video_publication("share-one", false, 10);
    let a = begin_video_attach(&mut routes, 11, A, "video-a");
    assert!(
        !routes
            .candidate_ready(&a, 12, None, proof("old-share"))
            .accepted
    );
    assert!(!edge_of(&routes, A).video_ready);
    // A connected transport alone does not complete video attachment.
    assert!(routes.candidate_transport_connected(&a, 12).accepted);
    assert_eq!(routes.reconcile(13).operation.unwrap().child_peer_id, A);
    assert!(
        routes
            .candidate_ready(&a, 14, None, proof("share-one"))
            .committed
    );
    let b = begin_video_attach(&mut routes, 15, B, "video-b");
    assert!(
        routes
            .candidate_ready(&b, 16, None, proof("share-one"))
            .committed
    );
    no_operation(routes.reconcile(17).operation);
    for child in [A, B] {
        let edge = edge_of(&routes, child);
        let previous = before.upstream_by_viewer.get(&child.to_owned()).unwrap();
        assert_eq!(edge.connection_id, previous.connection_id);
        assert_eq!(edge.parent_peer_id, previous.parent_peer_id);
        assert_eq!(edge.video_generation, "share-one");
        assert!(edge.video_ready);
    }
    assert!(routes.set_video_publication("", false, 18).is_empty());
    for child in [A, B] {
        let edge = edge_of(&routes, child);
        assert!(edge.usable && edge.physical_active);
        assert_eq!(edge.video_generation, "");
        assert_eq!(edge.video_connection_id, "");
        assert!(!edge.video_ready);
    }
    no_operation(routes.reconcile(19).operation);
    routes.set_video_publication("share-two", false, 20);
    let a2 = begin_video_attach(&mut routes, 21, A, "video-a2");
    assert!(
        !routes
            .candidate_ready(&a, 22, None, proof("share-one"))
            .accepted
    );
    assert!(
        routes
            .candidate_ready(&a2, 22, None, proof("share-two"))
            .committed
    );
}

#[test]
fn failed_publication_attachment_does_not_retry_or_consume_voice_opportunity() {
    for failure in ["failure", "timeout", "skip"] {
        let mut routes = voice_tree();
        let before = edge_of(&routes, A);
        routes.set_video_publication("share", false, 10);
        if failure == "skip" {
            let op = must(routes.reconcile(11).operation);
            assert!(
                routes
                    .skip_current_candidate(
                        &cursor_guard(&op),
                        12,
                        RejectionBucket::CandidateFailed
                    )
                    .accepted
            );
        } else {
            let guard = begin_video_attach(&mut routes, 11, A, "video-a");
            if failure == "failure" {
                assert!(routes.candidate_failed(&guard, 12).accepted);
            } else {
                let deadline = routes.operation().unwrap().deadline_at_ms;
                assert!(routes.operation_expired(deadline).accepted);
            }
        }
        let edge = edge_of(&routes, A);
        assert_eq!(edge.connection_id, before.connection_id);
        assert!(edge.usable && edge.physical_active);
        assert_eq!(edge.video_generation, "share");
        assert!(!edge.video_ready);
        assert_eq!(edge.video_connection_id, "");
        assert!(!routes.participant(A).unwrap().availability_exhausted);
        for at in [20_000, 40_000, 80_000] {
            no_operation(routes.reconcile(at).operation);
        }
        routes.set_video_publication("replacement", false, 80_001);
        begin_video_attach(&mut routes, 80_002, A, "replacement-a");
    }
}

#[test]
fn stop_cancels_attachment_and_rejects_late_frame() {
    let mut routes = voice_tree();
    routes.set_video_publication("share", false, 10);
    let guard = begin_video_attach(&mut routes, 11, A, "video-a");
    assert!(routes.set_video_publication("", false, 12).is_empty());
    no_operation(routes.operation());
    assert!(
        !routes
            .candidate_ready(&guard, 13, None, proof("share"))
            .accepted
    );
    assert_eq!(edge_of(&routes, A).connection_id, "voice-a");
    assert_eq!(edge_of(&routes, B).connection_id, "voice-b");
}

#[test]
fn canceled_publication_preparation_releases_no_room_resources() {
    for change in [
        "stop",
        "replacement",
        "pause",
        "child-session",
        "parent-session",
    ] {
        let mut routes = voice_tree();
        routes.set_video_publication("share", false, 10);
        let op = must(routes.reconcile(11).operation);
        assert_eq!(op.reason, DemandReason::PublicationAttach);
        // The effect layer prepares this optional facet without a
        // reservation, then yields before handing the result back.
        let prepared = BeginInput {
            guard: cursor_guard(&op),
            ..begin(13, "late-video", CandidateReservation::default())
        };
        match change {
            "stop" => drop(routes.set_video_publication("", false, 12)),
            "replacement" => drop(routes.set_video_publication("replacement-share", false, 12)),
            "pause" => drop(routes.set_video_publication("share", true, 12)),
            "child-session" => {
                drop(routes.upsert_participant(viewer_input(A, "replacement-child", 1), ms(12)))
            }
            _ => drop(routes.upsert_participant(host_input("replacement-parent", 1), ms(12))),
        }
        let result = routes.begin_current_candidate(prepared);
        assert!(!result.accepted, "{change}");
        assert!(result.released.is_empty());
        assert_eq!(edge_of(&routes, A).connection_id, "voice-a");
        assert_eq!(edge_of(&routes, B).connection_id, "voice-b");
        assert!(edge_of(&routes, A).usable && edge_of(&routes, B).usable);
    }
}
