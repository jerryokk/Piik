//! Router failure handling and generation fencing tests.

use piik_protocol::{ErrorCode, MediaRouteUpstream, Role, ServerMessage};
use piik_room::ConnectParticipantInput;
use piik_route::{BeginInput, CandidateGuard, CandidateProof, DemandReason};

use super::router_harness::*;
use crate::router::pump::cursor_guard;
use crate::router::{ActiveViewerMediaEdge, RouteParticipant};
use piik_sfu::runtime::Usage;

fn options(capacity: i64, with_sfu: bool, fake_timers: bool) -> RouterOptions {
    RouterOptions {
        capacity,
        with_sfu,
        fake_timers,
        ..Default::default()
    }
}

#[test]
fn cancel_prepared_video_attachment_does_not_fail_room() {
    let h = RouterHarness::new(options(2, false, false));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "cancel-video-attachment");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.route_ready(&viewer, prepared.revision);
    h.settle();
    h.locked(|core| {
        let now = core.now();
        let runtime = core.room_rt_mut(&room).expect("room");
        runtime.room_media = true;
        runtime.media_generation = "room_media_12345678".to_owned();
        let controller = runtime.controller.as_mut().expect("controller");
        controller.set_video_publication(TEST_SHARE_GENERATION, false, now);
        controller.set_video_publication("", false, now);
        controller.set_video_publication(TEST_SHARE_GENERATION, false, now);
        core.request_pump(&room);
        let operation = core.controller(&room).and_then(|c| c.operation());
        assert!(
            operation.is_some_and(|op| !op.attach_connection_id.is_empty() && op.current.is_none()),
            "expected admitted attachment waiting for the pump continuation"
        );
        // A stop can overtake the asynchronous begin step.
        core.set_video_publication(&room, "", false);
    });
    h.settle();
    assert!(
        !h.messages(&host.session_id).iter().any(|message| matches!(
            message,
            ServerMessage::Error {
                code: ErrorCode::ServerError,
                ..
            }
        )),
        "canceling a video attachment surfaced a media routing error"
    );
}

#[test]
fn room_video_failures_cannot_invalidate_another_physical_facet() {
    let h = RouterHarness::new(options(2, false, false));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "room-video-replacement");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.locked(|core| {
        do_ready(core, &viewer, prepared.revision);
        let now = core.now();
        let edge_of = |core: &crate::server::Core| {
            core.snapshot(&room)
                .and_then(|snapshot| snapshot.upstream_by_viewer.get(&viewer.peer_id).cloned())
        };
        let old = edge_of(core).expect("old edge");
        let runtime = core.room_rt_mut(&room).expect("room");
        runtime.room_media = true;
        let controller = runtime.controller.as_mut().expect("controller");
        controller.set_video_publication(TEST_SHARE_GENERATION, false, now);
        controller.set_video_publication("", false, now);
        controller.set_video_publication("room_video_replacement_123", false, now);
        let op = controller.reconcile(now).operation.expect("operation");
        assert_eq!(
            op.reason,
            DemandReason::PublicationAttach,
            "replacement did not prepare a video attachment"
        );
        let begun = controller.begin_current_candidate(BeginInput {
            guard: cursor_guard(&op, &op.candidates[op.cursor]),
            now_ms: now,
            connection_id: "replacement_video_connection_123".to_owned(),
            reservation: Default::default(),
            publication_generation: String::new(),
            publication_connection_id: String::new(),
            host_session_id: String::new(),
        });
        assert!(begun.accepted, "replacement attachment did not begin");
        let current = begun.operation.and_then(|op| op.current).expect("current");
        let guard = CandidateGuard {
            child_peer_id: viewer.peer_id.clone(),
            child_session_id: viewer.session_id.clone(),
            revision: current.revision,
            connection_id: current.connection_id.clone(),
        };
        let settled = controller.candidate_ready(
            &guard,
            now,
            None,
            CandidateProof {
                relative_quality_approved: false,
                video_generation: "room_video_replacement_123".to_owned(),
            },
        );
        assert!(settled.committed, "replacement attachment did not commit");
        for participant in [&host, &viewer] {
            do_failed(
                core,
                participant,
                prepared.revision,
                "active",
                Some(&old.connection_id),
            );
            let edge = edge_of(core).expect("edge");
            assert!(
                edge.usable && edge.video_ready && edge.video_connection_id == guard.connection_id,
                "a retired video failure affected its replacement or room voice"
            );
        }
        let revision = core.controller_revision(&room);
        do_failed(core, &host, revision, "active", Some(&guard.connection_id));
        assert!(
            !edge_of(core).expect("edge").usable,
            "the current parent video failure did not trigger normal route repair"
        );
    });
}

#[test]
fn router_prepares_sfu_for_new_and_reauthenticated_viewers() {
    for reauthenticate in [false, true] {
        let h = RouterHarness::new(options(1, true, true));
        let created = h.create_room();
        let (_, first, sibling) = h.establish_sfu_room(&created);
        let viewer = if reauthenticate {
            let mut viewer = first.clone();
            viewer.session_id = "replacement_viewer_session_12345678".to_owned();
            let connected = h.locked(|core| {
                core.store.connect_participant(&ConnectParticipantInput {
                    room_id: created.room_id.clone(),
                    role: Role::Viewer,
                    token: String::new(),
                    viewer_grant: created.viewer_grant.clone(),
                    client_id: format!("viewer_client_sfu-root-{}_12345678", created.room_id),
                    session_id: viewer.session_id.clone(),
                })
            });
            assert_eq!(
                connected.expect("reconnect").peer_id,
                first.peer_id,
                "viewer replacement changed identity"
            );
            viewer
        } else {
            h.connect_viewer(&created, "late-viewer")
        };
        let retained = h.locked(|core| {
            let retained =
                reauthenticate.then(|| do_must_edge(core, &created.room_id, &viewer.peer_id));
            let state = do_complete(core, &viewer);
            let expected = if reauthenticate {
                MediaRouteUpstream::Sfu
            } else {
                MediaRouteUpstream::None
            };
            assert_eq!(
                state.route_assignment.upstream, expected,
                "authentication upstream"
            );
            retained
        });
        if let Some(retained) = &retained {
            h.settle();
            assert_eq!(
                sfu_config_count(&h.messages(&viewer.session_id)),
                1,
                "healthy retained media must receive its current configuration"
            );
            assert!(
                !h.messages(&viewer.session_id)
                    .iter()
                    .any(|message| as_prepare(message).is_some()),
                "reauthentication alone must not replace healthy media"
            );
            h.route_failed(
                &viewer,
                retained.revision,
                "active",
                &retained.connection_id,
            );
        }
        let prepared = h.wait_prepared_transport(&viewer.session_id, "sfu");
        assert_ne!(
            Some(&prepared.candidate.connection_id),
            retained
                .as_ref()
                .map(|edge: &ActiveViewerMediaEdge| &edge.connection_id),
            "replacement must allocate a new physical connection"
        );
        let mut prepared_seen = false;
        let mut configured = false;
        for message in h.messages(&viewer.session_id) {
            if let Some(prepare) = as_prepare(&message) {
                prepared_seen = prepare.candidate.connection_id == prepared.candidate.connection_id;
            } else if let ServerMessage::SfuConfig { connection_id, .. } = &message
                && *connection_id == prepared.candidate.connection_id
            {
                assert!(
                    prepared_seen,
                    "new subscription configuration preceded its preparation"
                );
                configured = true;
            }
        }
        assert!(configured, "prepared subscription has no configuration");
        h.route_ready(&viewer, prepared.revision);
        assert_eq!(
            h.must_active_edge(&created.room_id, &sibling.peer_id)
                .upstream,
            MediaRouteUpstream::Sfu,
            "healthy sibling lost its SFU route"
        );
        let egress = if reauthenticate { 2 } else { 3 };
        h.wait_usage(Usage { ingress: 1, egress });
    }
}

#[test]
fn router_fences_delayed_active_failure_by_physical_connection() {
    for transport in ["peer", "sfu"] {
        for role in ["host", "viewer"] {
            for report in [
                "exact",
                "no-identity",
                "other-connection",
                "old-session",
                "future",
            ] {
                let label = format!("{transport}/{role}/{report}");
                let h = RouterHarness::new(options(2, transport == "sfu", true));
                let created = h.create_room();
                let room = created.room_id.clone();
                let host = h.connect_host(&created, None, "", "");
                let viewer = h.connect_viewer(&created, "delayed-failure");
                h.locked(|core| {
                    do_complete(core, &host);
                    do_complete(core, &viewer);
                });
                let mut prepared = h.wait_prepared_transport(&viewer.session_id, "direct");
                if transport == "sfu" {
                    h.route_failed(
                        &viewer,
                        prepared.revision,
                        "prepare",
                        &prepared.candidate.connection_id,
                    );
                    prepared = h.wait_prepared_transport(&viewer.session_id, "sfu");
                }
                h.route_ready(&viewer, prepared.revision);
                let (mut revision, mut connection_id) = h.locked(|core| {
                    let edge = do_must_edge(core, &room, &viewer.peer_id);
                    let id = if transport == "sfu" && role == "host" {
                        core.snapshot(&room)
                            .and_then(|snapshot| snapshot.host_publication)
                            .expect("publication")
                            .connection_id
                    } else {
                        edge.connection_id
                    };
                    (edge.revision, Some(id))
                });
                let other = h.connect_viewer(&created, "unrelated-join");
                h.complete(&other);
                let candidate = h.wait_prepared_transport(&other.session_id, "direct");
                h.route_ready(&other, candidate.revision);
                h.locked(|core| {
                    let before = core.snapshot(&room).expect("snapshot");
                    assert!(
                        before.revision > revision,
                        "{label}: another Viewer must advance the room revision"
                    );
                    let mut participant: RouteParticipant = if role == "host" {
                        host.clone()
                    } else {
                        viewer.clone()
                    };
                    match report {
                        "no-identity" => connection_id = None,
                        "other-connection" => {
                            connection_id = Some("retired_connection_12345678".to_owned())
                        }
                        "old-session" => {
                            participant.session_id = "retired_session_12345678".to_owned()
                        }
                        "future" => revision = before.revision + 1,
                        _ => {}
                    }
                    do_failed(
                        core,
                        &participant,
                        revision,
                        "active",
                        connection_id.as_deref(),
                    );
                    let after = core.snapshot(&room).expect("snapshot");
                    let usable = if transport == "sfu" && role == "host" {
                        after
                            .host_publication
                            .as_ref()
                            .is_some_and(|publication| publication.usable)
                    } else {
                        after
                            .upstream_by_viewer
                            .get(&viewer.peer_id)
                            .is_some_and(|edge| edge.usable)
                    };
                    assert_eq!(
                        usable,
                        report != "exact",
                        "{label}: active usable after report"
                    );
                    assert!(
                        after
                            .upstream_by_viewer
                            .get(&other.peer_id)
                            .is_some_and(|edge| edge.usable),
                        "{label}: unrelated direct Viewer must remain usable"
                    );
                });
            }
        }
    }
}

#[test]
fn router_active_failure_during_peer_preparation_keeps_its_subject() {
    for role in ["host", "viewer"] {
        let capacity = if role == "viewer" { 1 } else { 2 };
        let h = RouterHarness::new(options(capacity, true, true));
        let created = h.create_room();
        let room = created.room_id.clone();
        let host = h.connect_host(&created, None, "", "");
        let viewer = h.connect_viewer(&created, "sfu-ingress");
        h.locked(|core| {
            do_complete(core, &host);
            do_complete(core, &viewer);
        });
        let direct = h.wait_prepared_transport(&viewer.session_id, "direct");
        h.route_failed(
            &viewer,
            direct.revision,
            "prepare",
            &direct.candidate.connection_id,
        );
        let sfu = h.wait_prepared_transport(&viewer.session_id, "sfu");
        h.route_ready(&viewer, sfu.revision);
        if role == "viewer" {
            h.locked(|core| do_relay(core, &viewer, 1));
        }
        let child = h.connect_viewer(&created, "preparing-child");
        h.complete(&child);
        let candidate = h.wait_prepared_transport(&child.session_id, "direct");
        h.locked(|core| {
            let before = core.snapshot(&room).expect("snapshot");
            let (participant, id) = if role == "host" {
                (
                    &host,
                    before
                        .host_publication
                        .as_ref()
                        .expect("publication")
                        .connection_id
                        .clone(),
                )
            } else {
                (
                    &viewer,
                    before
                        .upstream_by_viewer
                        .get(&viewer.peer_id)
                        .expect("edge")
                        .connection_id
                        .clone(),
                )
            };
            let current_parent = before
                .operation
                .as_ref()
                .and_then(|op| op.current.as_ref())
                .map(|current| current.tuple.parent_peer_id.clone());
            assert_eq!(
                current_parent.as_deref(),
                Some(participant.peer_id.as_str()),
                "preparation must belong to the reporting parent"
            );
            do_failed(core, participant, before.revision, "active", Some(&id));
            let after = core.snapshot(&room).expect("snapshot");
            if role == "host" {
                assert!(
                    !after
                        .host_publication
                        .as_ref()
                        .is_some_and(|publication| publication.usable),
                    "failed publication remains usable"
                );
                assert_eq!(
                    after
                        .operation
                        .and_then(|op| op.current)
                        .map(|current| current.connection_id),
                    Some(candidate.candidate.connection_id.clone()),
                    "independent direct candidate was retired"
                );
            } else {
                assert!(
                    !after
                        .upstream_by_viewer
                        .get(&viewer.peer_id)
                        .is_some_and(|edge| edge.usable),
                    "failed subscriber remains usable"
                );
            }
        });
    }
}

#[test]
fn stopped_share_ready_cannot_commit_replacement_share() {
    let h = RouterHarness::new(options(2, false, true));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "share-ready");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let old = h.wait_prepared(&viewer.session_id);
    h.settle();
    h.locked(|core| {
        core.stop_room(&created.room_id);
        clear_sent(core, &viewer.session_id);
        core.detached.as_mut().expect("detached").share_generation =
            "replacement_generation_12345678".to_owned();
        do_complete(core, &host);
    });
    let current = h.wait_prepared(&viewer.session_id);
    assert_ne!(
        current.candidate.connection_id, old.candidate.connection_id,
        "replacement reused the physical connection"
    );
    h.route_ready(&viewer, old.revision);
    assert!(
        h.active_edge(&created.room_id, &viewer.peer_id).is_none(),
        "stopped share ready at revision {} committed replacement revision {}",
        old.revision,
        current.revision
    );
    h.route_ready(&viewer, current.revision);
    assert!(
        h.active_edge(&created.room_id, &viewer.peer_id).is_some(),
        "replacement's own first-frame proof did not commit"
    );
}
