//! Router-level SFU tests: room media publication, subscription and SFU
//! signaling.

use std::sync::Arc;
use std::time::{Duration, Instant};

use piik_protocol::{
    IceCandidate, Nullable, SdpType as WireSdpType, ServerMessage,
    SessionDescription as WireDescription, SfuCodec, SfuLayer, SfuMedia, SfuSignalKind,
    SfuSignalMessage,
};
use piik_route::{BeginInput, CandidateGuard, CandidateProof, DemandReason};
use serde_json::json;

use super::fake_media::{Barrier, FakeMedia, HookedMedia};
use super::router_harness::*;
use crate::router::RouteParticipant;
use crate::router::pump::{cursor_guard, resource_fence, subscription_fence};
use crate::router::sfu_signal::sfu_connection_phase;
use crate::server::Core;
use piik_sfu::runtime::{
    MediaEvent, PeerConnectionState, PublicationMedia, ResourceFence, Runtime, SubscriptionFence,
    Usage,
};

const WAIT: Duration = Duration::from_secs(5);

fn sfu(capacity: i64) -> RouterOptions {
    RouterOptions {
        capacity,
        with_sfu: true,
        ..Default::default()
    }
}

fn signal(
    kind: SfuSignalKind,
    revision: i64,
    generation: &str,
    connection_id: &str,
) -> SfuSignalMessage {
    SfuSignalMessage {
        revision,
        publication_generation: generation.to_owned(),
        connection_id: connection_id.to_owned(),
        kind,
        description: None,
        candidate: None,
        media: None,
        active_count: None,
    }
}

fn offer(mut message: SfuSignalMessage, sdp: &str) -> SfuSignalMessage {
    message.kind = SfuSignalKind::Description;
    message.description = Some(WireDescription {
        kind: WireSdpType::Offer,
        sdp: sdp.to_owned(),
    });
    message.media = Some(SfuMedia {
        codec: SfuCodec::Vp8,
        layers: vec![SfuLayer {
            rid: String::new(),
            width: 640,
            height: 360,
            bitrate: 500_000,
        }],
        audio: false,
        audio_bitrate: 0,
    });
    message
}

fn media_event(
    fence: ResourceFence,
    connection_id: &str,
    state: PeerConnectionState,
) -> MediaEvent {
    MediaEvent {
        fence,
        viewer_peer_id: String::new(),
        connection_id: connection_id.to_owned(),
        candidate: None,
        state,
        active_count: None,
    }
}

fn publication_fence(core: &Core, room_id: &str) -> (ResourceFence, String, String) {
    let publication = core
        .controller(room_id)
        .and_then(|c| c.publication())
        .expect("publication");
    let fence = resource_fence(publication.resource.as_ref().expect("resource"));
    (fence, publication.connection_id, publication.generation)
}

fn enable_room_media(core: &mut Core, room_id: &str, media_generation: Option<&str>) {
    let now = core.now();
    let runtime = core.room_rt_mut(room_id).expect("room");
    runtime.room_media = true;
    if let Some(generation) = media_generation {
        runtime.media_generation = generation.to_owned();
    }
    runtime
        .controller
        .as_mut()
        .expect("controller")
        .set_video_publication(TEST_SHARE_GENERATION, false, now);
}

fn set_runtime(core: &mut Core, runtime: Arc<dyn Runtime>) {
    core.router.sfu.as_mut().expect("SFU fallback").media = runtime;
}

#[test]
fn room_sfu_video_failure_retains_room_admission_during_replacement() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    let (host, first, _) = h.establish_sfu_room(&created);
    h.settle();
    h.locked(|core| {
        enable_room_media(core, &room, None);
        let publication = core
            .controller(&room)
            .and_then(|c| c.publication())
            .expect("publication");
        let edge = core
            .snapshot(&room)
            .and_then(|snapshot| snapshot.upstream_by_viewer.get(&first.peer_id).cloned())
            .expect("edge");
        let fence = resource_fence(publication.resource.as_ref().expect("resource"));
        let revision = core.controller_revision(&room);
        do_failed(
            core,
            &host,
            revision,
            "active",
            Some(&publication.connection_id),
        );
        let current = core.controller(&room).and_then(|c| c.publication());
        assert!(
            current
                .is_some_and(|current| current.generation == publication.generation
                    && current.physical_active),
            "video failure retired the room publication before replacement"
        );
        let subscription = subscription_fence(edge.resource.as_ref().expect("edge resource"));
        assert!(
            admission(core).has_publication(&fence)
                && admission(core).has_subscription(&subscription),
            "video failure released room admission used by healthy voice"
        );
    });
}

#[test]
fn room_sfu_video_attachment_uses_existing_admission_and_exact_physical_identity() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    let (host, first, second) = h.establish_sfu_room(&created);
    h.settle();
    h.locked(|core| {
        enable_room_media(core, &room, Some("room_media_12345678"));
        let now = core.now();
        let publication = core
            .controller(&room)
            .and_then(|c| c.publication())
            .expect("publication");
        let original_host_id = publication.connection_id.clone();
        let op = {
            let controller = core.controller_mut(&room).expect("controller");
            controller.set_video_publication("", false, now);
            controller.set_video_publication("video_next_12345678", false, now);
            controller.set_host_video_connection_id("publisher_next_12345678");
            controller.reconcile(now).operation.expect("operation")
        };
        assert_eq!(
            op.reason,
            DemandReason::PublicationAttach,
            "expected publication attachment"
        );
        let (child, other) = if op.child_peer_id == second.peer_id {
            (&second, &first)
        } else {
            (&first, &second)
        };
        let edge = core
            .snapshot(&room)
            .and_then(|snapshot| snapshot.upstream_by_viewer.get(&child.peer_id).cloned())
            .expect("edge");
        let begun = core
            .controller_mut(&room)
            .expect("controller")
            .begin_current_candidate(BeginInput {
                guard: cursor_guard(&op, &op.candidates[op.cursor]),
                now_ms: now,
                connection_id: "subscriber_next_12345678".to_owned(),
                reservation: Default::default(),
                publication_generation: String::new(),
                publication_connection_id: String::new(),
                host_session_id: String::new(),
            });
        assert!(begun.accepted, "attachment did not begin");
        let op = begun.operation.expect("operation");
        let current = op.current.clone().expect("current");
        let mut message = signal(
            SfuSignalKind::Candidate,
            current.revision,
            &publication.generation,
            &current.connection_id,
        );
        let (fence, allowed) = core.sfu_signal_authorized(child, &message);
        assert!(
            allowed
                && fence.connection_id == edge.connection_id
                && fence.resource
                    == resource_fence(publication.resource.as_ref().expect("resource")),
            "physical video attachment did not retain the exact logical reservation"
        );
        assert!(
            !core.sfu_signal_authorized(other, &message).1,
            "another viewer could use the admitted video facet"
        );
        let mut stale = child.clone();
        stale.session_id = "replaced_session_12345678".to_owned();
        assert!(
            !core.sfu_signal_authorized(&stale, &message).1,
            "stale session could signal an attachment"
        );
        let mut voice_only = message.clone();
        voice_only.connection_id = edge.connection_id.clone();
        assert!(
            !core.sfu_signal_authorized(child, &voice_only).1,
            "room connection identity was accepted as the replacement video identity"
        );
        let mut host_message = message.clone();
        host_message.connection_id = "publisher_next_12345678".to_owned();
        assert!(
            core.sfu_signal_authorized(&host, &host_message).1,
            "replacement publisher was rejected"
        );
        let snapshot = core.snapshot(&room).expect("snapshot");
        let (prepare, revision) = sfu_connection_phase(
            &snapshot,
            host.role,
            &host.peer_id,
            &publication.generation,
            &host_message.connection_id,
        );
        assert!(
            prepare && revision == current.revision,
            "publisher attachment lost its current operation phase"
        );
        core.prepare_sfu_video_attachment(&room, Some(&op));
        core.send_fresh_sfu_config(child);
        let last = sent(core, &child.session_id).last().cloned();
        assert!(
            matches!(&last, Some(ServerMessage::SfuConfig { connection_id, revision, .. })
                if *connection_id == message.connection_id && *revision == message.revision),
            "reauthentication did not describe the pending physical attachment"
        );
        let guard = CandidateGuard {
            child_peer_id: child.peer_id.clone(),
            child_session_id: child.session_id.clone(),
            revision: current.revision,
            connection_id: current.connection_id.clone(),
        };
        let settled = core
            .controller_mut(&room)
            .expect("controller")
            .candidate_ready(
                &guard,
                now,
                None,
                CandidateProof {
                    relative_quality_approved: false,
                    video_generation: "video_next_12345678".to_owned(),
                },
            );
        assert!(settled.committed, "video proof did not commit");
        message.revision = core.controller_revision(&room);
        assert!(
            core.sfu_signal_authorized(child, &message).1,
            "committed video facet was rejected"
        );
        core.handle_sfu_media_event(media_event(
            resource_fence(publication.resource.as_ref().expect("resource")),
            &original_host_id,
            PeerConnectionState::Failed,
        ));
        let current_publication = core
            .controller(&room)
            .and_then(|c| c.publication())
            .expect("publication");
        assert!(
            current_publication.usable
                && current_publication.connection_id == host_message.connection_id,
            "retired publisher callback invalidated its replacement"
        );
        core.controller_mut(&room)
            .expect("controller")
            .set_video_publication("", false, now);
        assert!(
            !core.sfu_signal_authorized(child, &message).1,
            "stopped video retained signaling authority"
        );
        assert!(
            admission(core).has_subscription(&fence)
                && admission(core).has_publication(&fence.resource),
            "video stop released room admission"
        );
    });
}

fn video_metadata() -> PublicationMedia {
    PublicationMedia {
        codec: "vp8".to_owned(),
        rids: vec![String::new()],
        formats: vec![piik_sfu::runtime::LayerFormat {
            width: 640,
            height: 360,
            bitrate: 500_000,
        }],
        audio: false,
        audio_bitrate: 0,
    }
}

#[test]
fn room_sfu_video_retirement_detaches_before_close_and_keeps_admission() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    h.establish_sfu_room(&created);
    h.settle();
    let media = FakeMedia::new();
    media.set_retire(true);
    h.locked(|core| {
        enable_room_media(core, &room, Some("room_media_12345678"));
        let (fence, connection_id, _) = publication_fence(core, &room);
        media
            .prepare_publication(&fence, &connection_id, &video_metadata())
            .expect("prepare");
        set_runtime(core, media.clone());
        core.retire_room_video(&room);
        assert!(
            !media.has_publication(&fence, &connection_id),
            "retired video remained visible while physical close was pending"
        );
        assert!(
            admission(core).has_publication(&fence),
            "video retirement released room ingress"
        );
        media
            .prepare_publication(&fence, "replacement_publisher_12345678", &video_metadata())
            .expect("prepare replacement");
        assert!(
            media.retire_publication(&fence, &connection_id).is_none(),
            "old identity detached the replacement"
        );
    });
    h.settle();
    h.locked(|core| {
        let (fence, _, _) = publication_fence(core, &room);
        assert!(
            media.has_publication(&fence, "replacement_publisher_12345678"),
            "old physical close removed the replacement"
        );
        assert_eq!(
            core.router.sfu_drain_tasks.len(),
            0,
            "retired physical close did not settle"
        );
    });
}

// Closing pending physical video is the embedded SFU media service's job,
// which lives outside this crate's Runtime boundary; the router-side contract
// is that a logical drain issues `close_subscription` for the logical edge
// identity.
#[test]
fn sfu_logical_subscription_drain_closes_pending_physical_video() {
    let mut h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let (_, first, _) = h.establish_sfu_room(&created);
    let logical = h.must_active_edge(&created.room_id, &first.peer_id);
    h.locked(|core| {
        do_disconnect(core, &first);
        core.remove_viewer(&created.room_id, &first.peer_id);
    });
    h.wait_for("the logical subscription drain", || {
        h.media()
            .drained_subscriptions()
            .iter()
            .any(|fence: &SubscriptionFence| {
                fence.viewer_peer_id == first.peer_id
                    && fence.connection_id == logical.connection_id
            })
    });
    h.close().expect("close");
}

#[test]
fn sfu_publication_creation_cannot_occupy_replacement_room_share() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    let (host, first, second) = h.establish_sfu_room(&created);
    h.settle();
    let inner = FakeMedia::new();
    inner.set_retire(true);
    let (entered, release, accepting, finish) = (
        Barrier::new(),
        Barrier::new(),
        Barrier::new(),
        Barrier::new(),
    );
    let mut hooked = HookedMedia::new(inner.clone());
    let (message, fence) = h.locked(|core| {
        enable_room_media(core, &room, Some("room_media_12345678"));
        let now = core.now();
        core.controller_mut(&room)
            .expect("controller")
            .set_voice_listeners(&[&host.peer_id, &first.peer_id, &second.peer_id], now);
        let (fence, connection_id, generation) = publication_fence(core, &room);
        hooked.hold_create = Some((connection_id.clone(), entered.clone(), release.clone()));
        hooked.hold_accept = Some((connection_id.clone(), accepting.clone(), finish.clone()));
        let revision = core.controller_revision(&room);
        (
            offer(
                signal(
                    SfuSignalKind::Description,
                    revision,
                    &generation,
                    &connection_id,
                ),
                "offer",
            ),
            fence,
        )
    });
    let hooked = Arc::new(hooked);
    h.locked(|core| {
        set_runtime(core, hooked.clone());
        if let Some(hooks) = core.detached.as_mut() {
            hooks.sent.clear();
        }
    });
    let core = h.core.clone();
    let signal_host = host.clone();
    let signal_message = message.clone();
    let done = std::thread::spawn(move || {
        crate::server::locked_with_io(&core, |core| {
            core.handle_sfu_signal(&signal_host, signal_message)
        });
    });
    assert!(
        entered.wait_timeout(WAIT),
        "old publication creation did not start"
    );
    let core = h.core.clone();
    let stop_room = room.clone();
    let stopped = std::thread::spawn(move || {
        crate::server::locked_with_io(&core, |core| {
            core.set_video_publication(&stop_room, "", false);
            core.set_video_publication(&stop_room, "video_latest_12345678", false);
        });
    });
    // Give the replacement the opportunity to overtake the held constructor.
    std::thread::sleep(Duration::from_millis(50));
    release.open();
    assert!(accepting.wait_timeout(WAIT), "old SDP did not start");
    stopped.join().expect("replacement");
    h.settle();
    let latest = h.locked(|core| {
        let (current, _, _) = publication_fence(core, &room);
        assert_eq!(
            current, fence,
            "voice room reservation changed across picture replacement"
        );
        let latest = sent(core, &host.session_id)
            .into_iter()
            .filter_map(|message| match message {
                ServerMessage::SfuConfig {
                    revision,
                    connection_id,
                    ..
                } => Some((revision, connection_id)),
                _ => None,
            })
            .next_back()
            .expect("latest share was not configured");
        assert_ne!(
            latest.1, message.connection_id,
            "latest share was not configured"
        );
        let mut next = message.clone();
        next.connection_id = latest.1.clone();
        next.revision = latest.0;
        core.handle_sfu_signal(&host, next);
        latest
    });
    h.settle();
    h.locked(|core| {
        assert!(
            inner.has_publication(&fence, &latest.1),
            "late old creation occupied the current share's room reservation"
        );
        let answered = sent(core, &host.session_id).iter().any(|message| {
            matches!(message, ServerMessage::SfuSignal(reply) if reply.connection_id == latest.1 && reply.description.is_some())
        });
        assert!(answered, "latest share offer failed behind the old occupant");
    });
    // The old SDP completion must not retire the newly accepted physical owner.
    finish.open();
    done.join().expect("old SDP completion did not settle");
    h.settle();
    assert!(
        inner.has_publication(&fence, &latest.1),
        "old completion retired the current share"
    );
}

#[test]
fn room_sfu_video_replacement_waits_for_physical_retirement() {
    for intent in ["newest-share", "stopped", "abandoned"] {
        let stop = intent != "newest-share";
        let h = RouterHarness::new(sfu(1));
        let created = h.create_room();
        let room = created.room_id.clone();
        let (host, first, second) = h.establish_sfu_room(&created);
        h.settle();
        let inner = FakeMedia::new();
        inner.set_retire(true);
        let (started, release) = (Barrier::new(), Barrier::new());
        let mut hooked = HookedMedia::new(inner.clone());
        hooked.hold_retire = Some((started.clone(), release.clone()));
        let hooked = Arc::new(hooked);
        let fence = h.locked(|core| {
            enable_room_media(core, &room, Some("room_media_12345678"));
            let now = core.now();
            core.controller_mut(&room)
                .expect("controller")
                .set_voice_listeners(&[&host.peer_id, &first.peer_id, &second.peer_id], now);
            let (fence, connection_id, _) = publication_fence(core, &room);
            inner
                .prepare_publication(&fence, &connection_id, &video_metadata())
                .expect("prepare");
            set_runtime(core, hooked.clone());
            if let Some(hooks) = core.detached.as_mut() {
                hooks.sent.clear();
            }
            core.set_video_publication(&room, "", false);
            core.set_video_publication(&room, "video_first_12345678", false);
            fence
        });
        assert!(
            started.wait_timeout(WAIT),
            "{intent}: physical retirement did not start"
        );
        let (latest_id, full_drain) = h.locked(|core| {
            let mut latest_id = String::new();
            if stop {
                core.set_video_publication(&room, "", false);
            } else {
                core.set_video_publication(&room, "video_latest_12345678", false);
                latest_id = core
                    .controller(&room)
                    .and_then(|c| c.publication())
                    .expect("publication")
                    .connection_id;
                core.send_fresh_sfu_config(&host);
            }
            let configured = core.detached.as_ref().is_some_and(|hooks| {
                hooks
                    .sent
                    .values()
                    .flatten()
                    .any(|message| matches!(message, ServerMessage::SfuConfig { .. }))
            });
            assert!(
                !configured,
                "{intent}: new picture allocated before old physical join"
            );
            assert!(
                admission(core).has_publication(&fence)
                    && core
                        .controller(&room)
                        .and_then(|c| c.publication())
                        .is_some(),
                "{intent}: picture retirement discarded voice admission"
            );
            let mut full_drain = None;
            if intent == "abandoned" {
                core.delete_room(&room);
                let key = SubscriptionFence {
                    resource: fence.clone(),
                    viewer_peer_id: String::new(),
                    connection_id: String::new(),
                };
                full_drain = core
                    .router
                    .sfu_drain_tasks
                    .get(&key)
                    .and_then(|task| task.operation.clone());
            }
            (latest_id, full_drain)
        });
        if let Some(operation) = &full_drain {
            assert!(
                operation
                    .wait_until(Instant::now() + Duration::from_millis(50))
                    .is_none(),
                "{intent}: room drain released admission before retired picture joined"
            );
        }
        release.open();
        h.settle();
        h.locked(|core| {
            if intent == "abandoned" {
                assert!(
                    admission(core).usage().ingress == 0 && core.router.sfu_drain_tasks.is_empty(),
                    "room admission did not release after every physical join"
                );
                return;
            }
            let configs: Vec<String> = sent(core, &host.session_id)
                .into_iter()
                .filter_map(|message| match message {
                    ServerMessage::SfuConfig { connection_id, .. } => Some(connection_id),
                    _ => None,
                })
                .collect();
            for connection_id in &configs {
                assert!(
                    !stop && *connection_id == latest_id,
                    "{intent}: retirement resumed superseded picture intent"
                );
            }
            assert_eq!(
                configs.is_empty(),
                stop,
                "{intent}: picture resumption after physical join"
            );
            assert!(
                core.controller(&room)
                    .and_then(|c| c.publication())
                    .is_some()
                    && admission(core).has_publication(&fence),
                "{intent}: voice admission did not survive picture handoff"
            );
            if !stop {
                inner
                    .prepare_publication(&fence, &latest_id, &video_metadata())
                    .expect("prepare latest");
                let (retired_fence, retired_id) = hooked
                    .retired
                    .lock()
                    .expect("retired")
                    .clone()
                    .expect("retired identity");
                inner.close_exact(&retired_fence, &retired_id);
                assert!(
                    inner.has_publication(&fence, &latest_id),
                    "late old close removed current picture"
                );
            }
        });
    }
}

#[test]
fn embedded_sfu_signals_require_exact_admitted_connection_and_current_session() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    let (host, first, second) = h.establish_sfu_room(&created);
    h.settle();
    h.locked(|core| {
        let publication = core
            .controller(&room)
            .and_then(|c| c.publication())
            .expect("publication");
        let edge = do_must_edge(core, &room, &first.peer_id);
        let mut message = signal(
            SfuSignalKind::Candidate,
            edge.revision,
            &publication.generation,
            &edge.connection_id,
        );
        assert!(
            core.sfu_signal_authorized(&first, &message).1,
            "current subscriber was rejected"
        );
        assert!(
            !core.sfu_signal_authorized(&second, &message).1,
            "another viewer could signal the subscription"
        );
        let mut stale: RouteParticipant = first.clone();
        stale.session_id = "stale_session_12345678".to_owned();
        assert!(
            !core.sfu_signal_authorized(&stale, &message).1,
            "replaced control session retained authority"
        );
        let mut forged = message.clone();
        forged.revision += 100;
        assert!(
            !core.sfu_signal_authorized(&first, &forged).1,
            "future route revision was accepted"
        );
        let mut forged = message.clone();
        forged.publication_generation = "unowned_publication_12345678".to_owned();
        assert!(
            !core.sfu_signal_authorized(&first, &forged).1,
            "unowned publication was accepted"
        );
        message.connection_id = publication.connection_id.clone();
        assert!(
            !core.sfu_signal_authorized(&first, &message).1,
            "viewer could signal Host ingress"
        );
        let message = offer(message, "test-offer");
        core.handle_sfu_signal(&host, message.clone());
        assert_eq!(
            h.media().created().len(),
            1,
            "authorized publisher did not create media"
        );
        let mut stale_message = message;
        stale_message.connection_id = "stale_connection_12345678".to_owned();
        core.handle_sfu_signal(&host, stale_message);
        assert_eq!(
            h.media().created().len(),
            1,
            "stale publisher created another physical handle"
        );
    });
}

#[test]
fn embedded_sfu_rejects_bad_publisher_offer_at_pending_revision() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 1,
        with_sfu: true,
        debug: true,
        ..Default::default()
    });
    h.media().set_rejections(Some("invalid"), None);
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "candidate_viewer");
    h.complete(&host);
    h.complete(&viewer);
    let direct = h.wait_prepared(&viewer.session_id);
    h.route_failed(
        &viewer,
        direct.revision,
        "prepare",
        &direct.candidate.connection_id,
    );
    let prepared = h.wait_prepared_transport(&viewer.session_id, "sfu");
    let (revision, generation, connection_id) = h
        .messages(&host.session_id)
        .into_iter()
        .filter_map(|message| match message {
            ServerMessage::SfuConfig {
                revision,
                publication_generation,
                connection_id,
            } => Some((revision, publication_generation, connection_id)),
            _ => None,
        })
        .next_back()
        .expect("host sfu-config");
    h.locked(|core| {
        core.handle_sfu_signal(
            &host,
            offer(
                signal(
                    SfuSignalKind::Description,
                    revision,
                    &generation,
                    &connection_id,
                ),
                "invalid",
            ),
        );
    });
    h.wait_usage(Usage::default());
    assert!(
        h.debug.called_with(
            &created.room_id,
            "sfu-signal-failed",
            &[("role", json!("host")), ("kind", json!("description"))]
        ),
        "failed publisher preparation did not retain diagnostic context"
    );
    assert!(
        h.active_after(&viewer.session_id, prepared.revision)
            .is_some(),
        "failed pending media did not broadcast a newer rollback revision"
    );
}

#[test]
fn embedded_sfu_rejects_malformed_candidates_on_both_physical_facets() {
    // A malformed candidate is reported without failing either physical facet.
    let h = RouterHarness::new(RouterOptions {
        capacity: 1,
        with_sfu: true,
        debug: true,
        ..Default::default()
    });
    h.media().set_rejections(None, Some("candidate:malformed"));
    let created = h.create_room();
    let room = created.room_id.clone();
    let (host, first, _) = h.establish_sfu_room(&created);
    h.settle();
    h.locked(|core| {
        let publication = core
            .controller(&room)
            .and_then(|c| c.publication())
            .expect("publication");
        let edge = do_must_edge(core, &room, &first.peer_id);
        let candidate = |connection: &str, revision: i64| {
            let mut message = signal(
                SfuSignalKind::Candidate,
                revision,
                &publication.generation,
                connection,
            );
            message.candidate = Some(IceCandidate {
                candidate: "candidate:malformed".to_owned(),
                sdp_mid: Nullable::Absent,
                sdp_m_line_index: Nullable::Absent,
                username_fragment: Nullable::Absent,
            });
            message
        };
        core.handle_sfu_signal(&host, candidate(&publication.connection_id, edge.revision));
        core.handle_sfu_signal(&first, candidate(&edge.connection_id, edge.revision));
    });
    h.settle();
    for role in ["host", "viewer"] {
        assert!(
            h.debug.called_with(
                &room,
                "sfu-signal-failed",
                &[("role", json!(role)), ("kind", json!("candidate"))]
            ),
            "candidate rejection was not processed on the {role} facet"
        );
    }
    assert_eq!(
        h.media().deleted().len(),
        0,
        "a rejected candidate failed the publication"
    );
    assert_eq!(
        h.usage(),
        Usage {
            ingress: 1,
            egress: 2
        },
        "a rejected candidate failed a facet"
    );
}

#[test]
fn embedded_sfu_control_loss_keeps_media_until_exact_transport_failure() {
    let h = RouterHarness::new(sfu(1));
    let created = h.create_room();
    let (host, _, _) = h.establish_sfu_room(&created);
    h.settle();
    let event = h.locked(|core| {
        let (fence, connection_id, _) = publication_fence(core, &created.room_id);
        do_disconnect(core, &host);
        media_event(fence, &connection_id, PeerConnectionState::Failed)
    });
    h.settle();
    assert!(
        h.media().deleted().is_empty() && h.usage().ingress == 1,
        "control loss retired healthy media"
    );
    h.locked(|core| {
        let mut stale = event.clone();
        stale.connection_id = "stale_connection_12345678".to_owned();
        core.handle_sfu_media_event(stale);
    });
    assert!(
        h.media().deleted().is_empty(),
        "stale media callback retired the current publication"
    );
    h.locked(|core| core.handle_sfu_media_event(event));
    h.wait_usage(Usage::default());
    assert_eq!(
        h.media().deleted().len(),
        1,
        "exact physical failure did not retire ingress"
    );
}
