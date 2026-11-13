//! Publication failure tests.

use super::publication::{begin_video_attach, proof, voice_tree};
use super::*;
use crate::*;

#[test]
fn failed_publication_attachment_reports_its_dependent_viewers() {
    for failure in ["transport", "timeout", "rejected"] {
        let mut routes = voice_tree();
        let before = routes.snapshot();
        routes.set_video_publication("share", false, 10);
        let failed = if failure == "rejected" {
            let op = must(routes.reconcile(11).operation);
            routes
                .skip_current_candidate(&cursor_guard(&op), 12, RejectionBucket::CandidateFailed)
                .failed_peer_ids
        } else {
            let guard = begin_video_attach(&mut routes, 11, A, "video-a");
            if failure == "transport" {
                routes.candidate_failed(&guard, 12).failed_peer_ids
            } else {
                let deadline = routes.operation().unwrap().deadline_at_ms;
                routes.operation_expired(deadline).failed_peer_ids
            }
        };
        assert_eq!(failed, [A, B]);
        for child in [A, B] {
            let previous = before.upstream_by_viewer.get(&child.to_owned()).unwrap();
            let edge = edge_of(&routes, child);
            assert_eq!(edge.connection_id, previous.connection_id);
            assert!(edge.usable && edge.physical_active);
            assert!(!edge.video_ready);
        }
        let settled = routes.reconcile(40_000);
        no_operation(settled.operation);
        assert!(settled.failed_peer_ids.is_empty());
        // A failed picture does not poison the voice graph or a later share.
        routes.set_video_publication("replacement", false, 40_001);
        let a = begin_video_attach(&mut routes, 40_002, A, "replacement-video-a");
        assert!(
            routes
                .candidate_ready(&a, 40_003, None, proof("replacement"))
                .committed
        );
        let b = begin_video_attach(&mut routes, 40_004, B, "replacement-video-b");
        assert!(
            routes
                .candidate_ready(&b, 40_005, None, proof("replacement"))
                .committed
        );
    }
}

#[test]
fn publication_attachment_failure_does_not_fail_independent_branches() {
    let mut routes = controller(2, opts());
    routes.set_video_publication("", false, 0);
    for peer_id in [A, B, C, D] {
        add_viewer(&mut routes, peer_id, 1, None);
    }
    routes.hydrate_edge(A, peer_edge(HOST, "voice-a"));
    routes.hydrate_edge(B, peer_edge(A, "voice-b"));
    routes.hydrate_edge(C, peer_edge(B, "voice-c"));
    routes.hydrate_edge(D, peer_edge(HOST, "voice-d"));
    routes.set_video_publication("share", false, 1);
    let a = begin_video_attach(&mut routes, 2, A, "video-a");
    assert_eq!(routes.candidate_failed(&a, 3).failed_peer_ids, [A, B, C]);
    let d = begin_video_attach(&mut routes, 4, D, "video-d");
    assert!(
        routes
            .candidate_ready(&d, 5, None, proof("share"))
            .committed
    );
    no_operation(routes.reconcile(6).operation);
    assert!(edge_of(&routes, D).video_ready);
}
