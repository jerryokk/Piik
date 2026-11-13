//! Dormant authority and share lifecycle across reconnects.

use super::*;

#[test]
fn signal_retains_dormant_authority_across_long_host_absence() {
    let clock = TestClock::new(utc_millis(2026, 8, 20, 12));
    let h = Harness::start(HarnessOptions {
        now: Some(clock.clock()),
        ..Default::default()
    });
    clock.advance(30 * DAY_MS);
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "returning-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "waiting-viewer",
        1,
        "",
        presence(),
    );
    h.close_client(&host);
    viewer.next("host-status");
    clock.advance(30 * DAY_MS);
    let resumed = h.open_client();
    authenticate(
        &resumed,
        &h.room,
        "host",
        "returning-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let late = h.open_client();
    authenticate(&late, &h.room, "viewer", "late-viewer", 1, "", presence());
    assert!(
        h.store_size() == 1 && !viewer.is_closed(),
        "host absence retired the room or its Viewer"
    );
}

#[test]
fn signal_keeps_pre_host_viewer_session_current_after_host_connects() {
    let clock = TestClock::new(utc_millis(2026, 8, 20, 12));
    let h = Harness::start(HarnessOptions {
        now: Some(clock.clock()),
        ..Default::default()
    });
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "pre-host-viewer",
        1,
        "",
        with_name("Before"),
    );
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "pre-host-host",
        1,
        "test_share_generation_12345678",
        with_name(""),
    );
    let named = |name: &'static str| {
        move |message: &PresenceMessage| {
            message
                .viewers()
                .iter()
                .any(|entry| entry.display_name() == name)
        }
    };
    next_viewer_presence_matching(&host, named("Before"));
    clock.advance(86_400_001);
    viewer.send_json(json!({"type": "set-display-name", "displayName": "After"}));
    let updated = next_viewer_presence_matching(&host, named("After"));
    assert_eq!(
        updated.viewers().len(),
        1,
        "updated roster: {}",
        updated.raw
    );
}

// ---------------------------------------------------------------------------
// grant update failure
// ---------------------------------------------------------------------------

#[test]
fn signal_leaves_established_authorization_and_media_untouched_when_grant_update_fails() {
    // The stable-authority database is closed underneath the store, so the
    // write fails before memory changes.
    let database = piik_room::Database::new(tempdir().join("rooms.sqlite")).expect("database");
    let h = Harness::start(HarnessOptions {
        store: Some(new_store(8, Some(database))),
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "persistence-failure-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "persistence-failure-viewer",
        1,
        "",
        presence(),
    );
    commit_prepared_route(&viewer);
    host.send_json(signal_description(
        &viewer_auth.peer_id(),
        "persistence-failure-edge",
        "offer",
    ));
    viewer.next("signal");
    h.locked(|core| core.store.close().expect("close database"));
    let error = h
        .try_update_room_access(&h.room, RoomAccessUpdateRequest::RotateViewerGrant)
        .expect_err("rotation must fail");
    assert!(
        error.code().is_none(),
        "expected a non-room error, got {error:?}"
    );
    if let Some(message) = viewer.try_next("viewer-grant-revoked", Duration::from_millis(30)) {
        panic!("viewer was revoked: {}", message.raw);
    }
    host.send_json(signal_candidate(
        &viewer_auth.peer_id(),
        "persistence-failure-edge",
    ));
    expect_match(
        &viewer.next("signal").raw,
        &json!({"fromPeerId": host_auth.peer_id(), "payload": {"connectionId": "persistence-failure-edge"}}).to_string(),
    );
    let second = h.open_client();
    let second_auth = authenticate(
        &second,
        &h.room,
        "viewer",
        "persistence-failure-new-viewer",
        1,
        "",
        presence(),
    );
    assert_eq!(
        second_auth.viewer_authorization_generation(),
        viewer_auth.viewer_authorization_generation()
    );
}

// ---------------------------------------------------------------------------
// host arrival, pause and quality
// ---------------------------------------------------------------------------

#[test]
fn signal_lets_viewers_arrive_first_and_replays_their_snapshot_when_host_joins() {
    let h = Harness::start(HarnessOptions::default());
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer-client-early",
        1,
        "",
        presence(),
    );
    assert!(!viewer_auth.host_online(), "host must be offline");
    assert!(
        has_key(&viewer_auth.raw, "mediaMode"),
        "mediaMode must be present"
    );
    assert!(
        has_key(&viewer_auth.raw, "routeRevision"),
        "routeRevision must be present"
    );
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    host.ignore("route-update");
    expect_equal(
        &viewer.next("host-status").raw,
        r#"{"type":"host-status","online":true,"paused":false}"#,
    );
}

#[test]
fn signal_stops_sharing_without_deleting_room_and_restricts_room_abandonment() {
    let max_viewers = 5;
    let h = Harness::start(HarnessOptions {
        max_viewers_per_room: max_viewers,
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    assert_eq!(host_auth.value["maxViewers"], json!(max_viewers));
    let mut viewers = Vec::new();
    for index in 1..=max_viewers {
        let viewer = h.open_client();
        let viewer_auth = authenticate(
            &viewer,
            &h.room,
            "viewer",
            &format!("viewer-client-{index}"),
            1,
            "",
            presence(),
        );
        assert_eq!(viewer_auth.value["maxViewers"], json!(max_viewers));
        commit_prepared_route(&viewer);
        viewers.push(viewer);
    }
    let overflow = h.open_client();
    overflow.send_json(json!({
        "type": "authenticate",
        "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": h.room.room_id,
        "role": "viewer",
        "viewerGrant": h.room.viewer_grant,
        "clientId": format!("viewer-client-{}", max_viewers + 1),
    }));
    expect_match(&overflow.next("error").raw, r#"{"code":"ROOM_FULL"}"#);
    host.send_json(json!({"type": "stop-sharing"}));
    for viewer in &viewers {
        expect_equal(
            &viewer.next("sharing-stopped").raw,
            r#"{"type":"sharing-stopped"}"#,
        );
        expect_match(&viewer.next("host-status").raw, r#"{"online":false}"#);
    }
    // Stopping media keeps the persistent Host room session.
    assert!(!host.is_closed(), "stopping media closed the Host session");
    assert_eq!(h.store_size(), 1);
    viewers[0].send_json(json!({"type": "abandon-room"}));
    expect_match(&viewers[0].next("error").raw, r#"{"code":"FORBIDDEN"}"#);
    assert_eq!(h.store_size(), 1);
    let abandon_host = h.open_client();
    authenticate(
        &abandon_host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    abandon_host.send_json(json!({"type": "abandon-room"}));
    expect_match(
        &abandon_host.next("room-closed").raw,
        r#"{"reason":"host-ended"}"#,
    );
    for viewer in &viewers {
        expect_match(
            &viewer.next("room-closed").raw,
            r#"{"reason":"host-ended"}"#,
        );
    }
    assert_eq!(h.store_size(), 0);
}

// ---------------------------------------------------------------------------
// transport hardening
// ---------------------------------------------------------------------------
