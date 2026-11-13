//! Publication tests (part two).

use super::publication::{begin_video_attach, commit_room_peer, proof, voice_tree};
use super::*;
use crate::*;

#[test]
fn voice_opt_in_retains_already_proven_video() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 0, ms(1));
    must(routes.reconcile(1).operation);
    commit_current(&mut routes, 2, "existing-video");
    routes.set_video_publication("live-share", false, 4);
    let edge = edge_of(&routes, A);
    assert_eq!(edge.video_generation, "live-share");
    assert_eq!(edge.video_connection_id, "existing-video");
    assert!(edge.video_ready);
    no_operation(routes.reconcile(5).operation);
}

#[test]
fn paused_video_still_allows_room_join_and_resume_attaches() {
    let mut routes = controller(1, opts());
    routes.set_video_publication("", false, 0);
    routes.set_video_publication("share", true, 1);
    add_viewer(&mut routes, A, 0, ms(2));
    commit_room_peer(&mut routes, 3, "voice-a");
    assert!(!routes.snapshot().paused);
    assert!(routes.snapshot().video_paused);
    assert!(!edge_of(&routes, A).video_ready);
    routes.set_paused(false, ms(6));
    begin_video_attach(&mut routes, 7, A, "video-a");
}

#[test]
fn room_sfu_attachment_keeps_its_admitted_room_resources() {
    let mut routes = controller(
        1,
        Options {
            sfu_enabled: true,
            sfu_only: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 0, ms(1));
    routes.hydrate_host_publication("room-ingress", res("ingress"), "ingress-pc");
    routes.hydrate_edge(A, sfu_edge("room-ingress", "voice-a", "subscription"));
    routes.set_video_publication("", false, 2);
    let before = routes.snapshot();
    routes.set_video_publication("share", false, 3);
    let guard = begin_video_attach(&mut routes, 4, A, "video-a");
    assert!(
        routes
            .candidate_ready(&guard, 5, None, proof("share"))
            .committed
    );
    assert!(routes.set_video_publication("", false, 6).is_empty());
    let same = |left: &Option<ResourceRef>, right: &Option<ResourceRef>| {
        std::sync::Arc::ptr_eq(left.as_ref().unwrap(), right.as_ref().unwrap())
    };
    assert!(same(
        &routes.publication().unwrap().resource,
        &before.host_publication.unwrap().resource
    ));
    let old = before.upstream_by_viewer.get(&A.to_owned()).unwrap();
    assert!(same(&edge_of(&routes, A).resource, &old.resource));
    assert_eq!(edge_of(&routes, A).connection_id, old.connection_id);
}

#[test]
fn attached_video_quality_uses_physical_video_identity_and_native_proof() {
    let mut routes = controller(2, quality_opts());
    routes.set_video_publication("", false, 0);
    add_viewer(&mut routes, A, 1, ms(1));
    commit_room_peer(&mut routes, 2, "voice-a");
    routes.set_video_publication("share", false, 5);
    let guard = begin_video_attach(&mut routes, 6, A, "video-a");
    assert!(
        routes
            .candidate_ready(&guard, 7, None, proof("share"))
            .committed
    );
    assert!(!observe(&mut routes, A, "voice-a", SenderQualityState::Degraded, 8).accepted);
    observe_persistent_degraded(&mut routes, A, "video-a", 10);
    let op = must(routes.reconcile(14).operation);
    assert_eq!(op.reason, DemandReason::QualityConvergence);
    let begun = begin_operation(&mut routes, begin(15, "replacement", direct()));
    let candidate = guard_for(A, current(&begun).revision, "replacement");
    let approved = CandidateProof {
        relative_quality_approved: true,
        video_generation: "share".to_owned(),
    };
    assert!(
        !routes
            .candidate_ready(&candidate, 16, None, approved)
            .committed
    );
    let sample = sender_sample(
        HOST,
        A,
        current(&begun).revision,
        "replacement",
        "replacement-sender",
        SenderQualityState::Healthy,
        17,
    );
    assert!(
        routes
            .observe_sender_quality_evidence(sample, None)
            .committed
    );
    assert_eq!(edge_of(&routes, A).video_connection_id, "replacement");
    routes.set_video_publication("", false, 18);
    assert!(
        !observe(
            &mut routes,
            A,
            "replacement",
            SenderQualityState::Healthy,
            19
        )
        .accepted
    );
}

#[test]
fn failed_optional_picture_reopens_only_for_new_endpoint_session() {
    for endpoint in ["child", "parent", "host"] {
        let mut routes = voice_tree();
        routes.set_video_publication("share", false, 10);
        let failed_child = if endpoint == "host" { A } else { B };
        if endpoint != "host" {
            let a = begin_video_attach(&mut routes, 11, A, "video-a");
            assert!(
                routes
                    .candidate_ready(&a, 12, None, proof("share"))
                    .committed
            );
        }
        let failed = begin_video_attach(&mut routes, 13, failed_child, "failed-video");
        routes.candidate_failed(&failed, 14);
        let before = routes.snapshot();
        let changed_peer = match endpoint {
            "child" => B,
            "host" => HOST,
            _ => A,
        };
        let session_id = routes.participant(changed_peer).unwrap().session_id.clone();
        let mut input = if endpoint == "host" {
            host_input(&session_id, 1)
        } else {
            viewer_input(changed_peer, &session_id, 1)
        };
        routes.upsert_participant(input.clone(), ms(15));
        no_operation(routes.reconcile(16).operation);
        assert_eq!(edge_of(&routes, failed_child).video_generation, "share");
        input.session_id = "new_endpoint_session".to_owned();
        routes.upsert_participant(input.clone(), ms(17));
        let mut retry = begin_video_attach(&mut routes, 18, failed_child, "recovered-video");
        retry.child_session_id = edge_of(&routes, failed_child).child_session_id;
        assert!(
            routes
                .candidate_ready(&retry, 19, None, proof("share"))
                .committed
        );
        if endpoint == "host" {
            let b = begin_video_attach(&mut routes, 20, B, "video-b");
            assert!(
                routes
                    .candidate_ready(&b, 21, None, proof("share"))
                    .committed
            );
        } else {
            assert_eq!(edge_of(&routes, A).video_connection_id, "video-a");
        }
        for peer_id in [A, B] {
            let previous = before.upstream_by_viewer.get(&peer_id.to_owned()).unwrap();
            assert_eq!(
                edge_of(&routes, peer_id).connection_id,
                previous.connection_id
            );
            assert!(edge_of(&routes, peer_id).usable);
        }
        // Further endpoint sessions retain proven physical video without another attach.
        input.session_id = "healthy_endpoint_session".to_owned();
        routes.upsert_participant(input, ms(22));
        no_operation(routes.reconcile(23).operation);
        assert_eq!(
            edge_of(&routes, failed_child).video_connection_id,
            "recovered-video"
        );
    }
}

#[test]
fn host_session_reopens_failed_sfu_picture_without_replacing_healthy_picture() {
    let mut routes = controller(
        1,
        Options {
            sfu_enabled: true,
            sfu_only: true,
            ..opts()
        },
    );
    routes.set_video_publication("", false, 0);
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    routes.hydrate_edge(A, sfu_edge("publication", "voice-a", "subscription-a"));
    routes.hydrate_edge(B, sfu_edge("publication", "voice-b", "subscription-b"));
    routes.set_video_publication("share", false, 1);
    let a = begin_video_attach(&mut routes, 2, A, "failed-a");
    routes.candidate_failed(&a, 3);
    let b = begin_video_attach(&mut routes, 4, B, "healthy-b");
    assert!(
        routes
            .candidate_ready(&b, 5, None, proof("share"))
            .committed
    );
    let host_session = routes.participant(HOST).unwrap().session_id.clone();
    routes.upsert_participant(host_input(&host_session, 1), ms(6));
    no_operation(routes.reconcile(7).operation);
    routes.upsert_participant(host_input("replacement-host", 1), ms(8));
    let a = begin_video_attach(&mut routes, 9, A, "recovered-a");
    assert!(
        routes
            .candidate_ready(&a, 10, None, proof("share"))
            .committed
    );
    no_operation(routes.reconcile(11).operation);
    assert_eq!(edge_of(&routes, A).connection_id, "voice-a");
    assert_eq!(edge_of(&routes, B).video_connection_id, "healthy-b");
    assert_eq!(
        routes.publication().unwrap().host_session_id,
        "replacement-host"
    );
}
