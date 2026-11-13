//! Room retention and departure: capacity reclamation and revoked media.

use std::sync::Arc;

use piik_protocol::{CodeEntryPolicy, Role, RoomAccessUpdateRequest};
use piik_room::{ConnectParticipantInput, ViewerGrantAction};
use serde_json::json;

use super::fake_media::FakeMedia;
use super::harness::*;
use super::router_harness::{
    RouterHarness, RouterOptions, admission, do_complete, do_disconnect, do_must_edge, do_relay,
};
use super::server::presence;
use super::wire::*;
use crate::router::pump::subscription_fence;
use crate::router::{RouteParticipant, SfuFallback};
use piik_sfu::runtime::{Admission, AdmissionOptions, Usage};

use super::rooms::room_error_code;

// ---------------------------------------------------------------------------
// retention
// ---------------------------------------------------------------------------

fn clocked_store(
    max_rooms: usize,
    max_viewers: usize,
    clock: piik_room::Clock,
) -> piik_room::Store {
    piik_room::Store::new(piik_room::Options {
        max_rooms,
        max_viewers_per_room: max_viewers,
        now: Some(clock),
        ..Default::default()
    })
    .expect("store")
}

fn fixed(ms: i64) -> crate::runtime::Clock {
    Arc::new(move || ms)
}

fn is_room_limit(result: &Result<piik_room::CreatedRoom, piik_room::Error>) -> bool {
    matches!(result, Err(error) if room_error_code(error) == Some(piik_room::ErrorCode::RoomLimit))
}

#[test]
fn room_capacity_reclaims_oldest_empty_room_only_when_enabled() {
    for enabled in [false, true] {
        let store = clocked_store(2, 2, Arc::new(|| 0));
        let h = Harness::start(HarnessOptions {
            store: Some(store),
            now: Some(fixed(51)),
            room_empty_timeout_ms: if enabled { 3_600_000 } else { 0 },
            ..Default::default()
        });
        let second = h.create_room(CodeEntryPolicy::Open, "");
        let created = h.try_create_room(CodeEntryPolicy::Open, "", Some(&h.room.room_id));
        if !enabled {
            assert!(
                is_room_limit(&created),
                "default capacity result = {created:?}"
            );
            continue;
        }
        let created = created.expect("reclaim oldest empty");
        assert!(
            created.room_id == h.room.room_id && created.host_token != h.room.host_token,
            "reclaim oldest empty"
        );
        assert!(
            h.locked(|core| core
                .store
                .host_managed_room(&second.room_id, &second.host_token)
                .is_ok()),
            "newer empty room was retired"
        );
        let host = h.open_client();
        authenticate(&host, &created, "host", "idle-host", 1, "", presence());
        let viewer = h.open_client();
        authenticate(
            &viewer,
            &second,
            "viewer",
            "waiting-viewer",
            1,
            "",
            presence(),
        );
        assert!(
            is_room_limit(&h.try_create_room(CodeEntryPolicy::Open, "", None)),
            "occupied rooms must not be evicted"
        );
    }
}

#[test]
fn empty_expiry_cleans_share_and_reconnect_state() {
    let clock = TestClock::new(0);
    let store = clocked_store(2, 2, clock.clock());
    let h = Harness::start(HarnessOptions {
        store: Some(store),
        now: Some(clock.clock()),
        room_empty_timeout_ms: 3_600_000,
        viewer_disconnect_grace_ms: 20_000,
        ..Default::default()
    });
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "retention-host",
        1,
        "retention_share_generation_12345678",
        presence(),
    );
    host.ignore("route-update");
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "retention-viewer",
        1,
        "",
        presence(),
    );
    viewer.ignore("route-update");
    clock.advance(7_200_000);
    h.locked(|core| {
        core.expire_empty_rooms();
        assert_eq!(core.store.size(), 1, "connected room expired");
    });
    h.close_client(&viewer);
    h.close_client(&host);
    h.locked(|core| {
        assert!(
            core.viewer_grace_timers.len() == 1 && core.shares.len() == 1,
            "test did not retain reconnect/share state"
        );
    });
    clock.advance(3_599_999);
    h.locked(|core| {
        core.expire_empty_rooms();
        assert_eq!(core.store.size(), 1, "room expired before an hour empty");
    });
    clock.advance(1);
    h.locked(|core| {
        core.expire_empty_rooms();
        assert!(
            core.store.size() == 0 && core.shares.is_empty() && core.viewer_grace_timers.is_empty(),
            "expired room left authority or effects behind"
        );
    });
    let created = h
        .try_create_room(CodeEntryPolicy::Open, "", Some(&h.room.room_id))
        .expect("recreate");
    let new_host = h.open_client();
    authenticate(&new_host, &created, "host", "new-host", 1, "", presence());
}

#[test]
fn room_capacity_prefers_expired_room_over_recently_vacated_older_room() {
    let clock = TestClock::new(0);
    let store = clocked_store(2, 2, clock.clock());
    let h = Harness::start(HarnessOptions {
        store: Some(store),
        now: Some(clock.clock()),
        room_empty_timeout_ms: 3_600_000,
        ..Default::default()
    });
    let host = h.open_client();
    authenticate(&host, &h.room, "host", "retention-host", 1, "", presence());
    clock.advance(10);
    let newer = h.create_room(CodeEntryPolicy::Open, "");
    clock.advance(3_600_000);
    h.close_client(&host);
    clock.advance(100);
    let created = h
        .try_create_room(CodeEntryPolicy::Open, "", Some(&newer.room_id))
        .expect("reclaim");
    assert_eq!(
        created.room_id, newer.room_id,
        "expired room not reclaimed first"
    );
    assert!(
        h.locked(|core| core
            .store
            .host_managed_room(&h.room.room_id, &h.room.host_token)
            .is_ok()),
        "recent departure lost its room before an already-expired room"
    );
}

#[test]
fn room_replacement_at_all_9000_codes_reclaims_another_empty_room() {
    let mut store = clocked_store(piik_room::CAPACITY, 2, Arc::new(|| 0));
    let oldest = create_store_room(&mut store, CodeEntryPolicy::Open, "");
    let second = create_store_room(&mut store, CodeEntryPolicy::Open, "");
    while store.size() < piik_room::CAPACITY {
        create_store_room(&mut store, CodeEntryPolicy::Open, "");
    }
    let h = Harness::start(HarnessOptions {
        store: Some(store),
        room: Some(oldest.clone()),
        now: Some(fixed(51)),
        room_empty_timeout_ms: 3_600_000,
        ..Default::default()
    });
    let replace = |token: &str| {
        let server = h.server.clone();
        let (room_id, token) = (oldest.room_id.clone(), token.to_owned());
        h.runtime.block_on(async move {
            server
                .replace_room(&room_id, &token, CodeEntryPolicy::Open, None)
                .await
        })
    };
    assert!(
        replace("wrong-token").is_err(),
        "invalid ownership replaced a room"
    );
    assert_eq!(
        h.store_size(),
        piik_room::CAPACITY,
        "rejected replacement evicted another room"
    );
    let replacement = replace(&oldest.host_token).expect("replacement");
    assert!(
        replacement.room_id == second.room_id && replacement.host_token != second.host_token,
        "replacement did not reclaim the next oldest empty room: {}",
        replacement.room_id
    );
    assert_eq!(
        h.store_size(),
        piik_room::CAPACITY - 1,
        "replacement left incorrect room capacity"
    );
    assert!(
        h.locked(|core| core
            .store
            .host_managed_room(&oldest.room_id, &oldest.host_token)
            .is_err()),
        "old replacement authority survived"
    );
}

#[test]
fn capacity_reclamation_protects_creation_and_reconnect_window() {
    let clock = TestClock::new(0);
    let store = clocked_store(1, 1, clock.clock());
    let h = Harness::start(HarnessOptions {
        store: Some(store),
        now: Some(clock.clock()),
        room_empty_timeout_ms: 3_600_000,
        viewer_disconnect_grace_ms: 20_000,
        ..Default::default()
    });
    let assert_protected = || {
        assert!(
            is_room_limit(&h.try_create_room(CodeEntryPolicy::Open, "", None)),
            "creation/reconnect window lost its room"
        );
    };
    clock.advance(19_999);
    assert_protected();
    let host = h.open_client();
    authenticate(&host, &h.room, "host", "retention-host", 1, "", presence());
    h.close_client(&host);
    clock.advance(19_999);
    assert_protected();
    clock.advance(1);
    h.try_create_room(CodeEntryPolicy::Open, "", None)
        .expect("reclamation after reconnect window");
    assert_protected();
}

// ---------------------------------------------------------------------------
// departure
// ---------------------------------------------------------------------------

#[test]
fn paused_grant_revocation_retires_committed_media() {
    for transport in ["peer", "sfu"] {
        let media = FakeMedia::new();
        let h = Harness::start(HarnessOptions {
            sfu: Some(SfuFallback {
                media: media.clone(),
                admission: Admission::new(AdmissionOptions {
                    ingress_capacity: 1,
                    egress_capacity: 2,
                }),
                prepare_timeout_ms: 0,
            }),
            ..Default::default()
        });
        let (host, viewer) = (h.open_client(), h.open_client());
        let host_auth = authenticate(
            &host,
            &h.room,
            "host",
            "departure_host_12345678",
            1,
            "departure_share_generation_12345678",
            presence(),
        );
        authenticate(
            &viewer,
            &h.room,
            "viewer",
            "departure_viewer_12345678",
            1,
            "",
            presence(),
        );
        if transport == "sfu" {
            let direct = next_prepared_route(&viewer);
            viewer.send_json(json!({"type": "route-failed", "revision": direct.revision(), "phase": "prepare", "connectionId": direct.connection_id()}));
        }
        let active = commit_prepared_route(&viewer);
        super::server::set_sharing_paused(
            &host,
            &host_auth.share_generation().expect("generation"),
            true,
        );
        let room = h.room.room_id.clone();
        assert!(
            wait_for(DEFAULT_WAIT, || h.locked(|core| core
                .snapshot(&room)
                .is_some_and(|snapshot| snapshot.paused))),
            "{transport}: sharing did not pause"
        );
        h.update_room_access(RoomAccessUpdateRequest::RevokeViewerGrant);
        viewer.next("viewer-grant-revoked");
        expect_close_code(&viewer, 4004);
        assert!(
            wait_for(DEFAULT_WAIT, || h.locked(|core| {
                core.snapshot(&room).is_some_and(|snapshot| {
                    snapshot.upstream_by_viewer.is_empty() && snapshot.revision > active.revision()
                }) && admission(core).usage() == Usage::default()
            })),
            "{transport}: revoked viewer retained media authority while sharing was paused"
        );
        if transport == "sfu" {
            assert!(
                wait_for(DEFAULT_WAIT, || media.deleted().len() == 1),
                "orphaned publication and its subscriptions were not physically closed"
            );
        }
    }
}

#[test]
fn revoked_relay_retires_media_before_child_recovery() {
    for mode in ["active", "paused", "host-offline"] {
        let h = RouterHarness::new(RouterOptions {
            capacity: 1,
            with_sfu: true,
            ..Default::default()
        });
        let created = h.create_room();
        let (host, first, _) = h.establish_sfu_room(&created);
        h.settle();
        let (child, fence) = h.locked(|core| {
            let admitted = core
                .store
                .connect_participant(&ConnectParticipantInput {
                    room_id: created.room_id.clone(),
                    role: Role::Viewer,
                    token: String::new(),
                    viewer_grant: String::new(),
                    client_id: "code_child_12345678".into(),
                    session_id: "code_session_12345678".into(),
                })
                .expect("code admission");
            let child = RouteParticipant::new(
                &created.room_id,
                Role::Viewer,
                &admitted.peer_id,
                "code_session_12345678",
            );
            do_relay(core, &first, 1);
            do_complete(core, &child);
            let edge = core
                .snapshot(&created.room_id)
                .and_then(|snapshot| snapshot.upstream_by_viewer.get(&first.peer_id).cloned())
                .expect("first edge");
            (
                child,
                subscription_fence(edge.resource.as_ref().expect("resource")),
            )
        });
        let prepared = h.wait_prepared_transport(&child.session_id, "direct");
        h.route_ready(&child, prepared.revision);
        h.settle();
        h.locked(|core| {
            assert_eq!(
                do_must_edge(core, &created.room_id, &child.peer_id).upstream,
                piik_protocol::MediaRouteUpstream::peer(&first.peer_id),
                "{mode}: child must initially attach to the grant-admitted relay"
            );
            if mode == "paused" {
                core.router_set_paused(&created.room_id, true);
            }
            if mode == "host-offline" {
                do_disconnect(core, &host);
            }
            let update = core
                .store
                .set_viewer_grant(
                    &created.room_id,
                    ViewerGrantAction::Revoke,
                    &created.host_token,
                )
                .expect("revoke");
            for viewer in update.revoked_viewers {
                core.remove_viewer(&created.room_id, &viewer.peer_id);
            }
        });
        h.settle();
        h.locked(|core| {
            assert!(
                !admission(core).has_subscription(&fence),
                "{mode}: revoked relay still holds an admitted subscription"
            );
            assert!(
                core.store
                    .get_connected_viewer(&created.room_id, &child.peer_id)
                    .is_some(),
                "{mode}: grant revocation must preserve the code-admitted child"
            );
        });
        let media = h.media();
        assert!(
            media.drained_subscriptions().contains(&fence)
                || media.deleted().contains(&fence.resource),
            "{mode}: neither the exact subscription nor its publication was physically closed"
        );
    }
}
