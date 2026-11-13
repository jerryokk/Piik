//! Share quality binding and viewer receive-evidence forwarding.

use super::*;

#[test]
fn sender_quality_budget_admits_room_producer_cadence() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 2,
        ..Default::default()
    });
    // Producers report each physical sender every two seconds. A room may
    // retain all Viewer senders plus one serial preparation.
    let senders = MAX_VIEWERS_PER_ROOM_LIMIT + 1;
    h.locked(|core| {
        for window in 0..4 {
            let now = window * VIEWER_QUALITY_EVIDENCE_INTERVAL_MS;
            for _ in 0..senders {
                assert!(
                    core.consume_sender_quality_budget("producer-session", now),
                    "legal producer burst was rate limited"
                );
            }
            // The double allowance absorbs boundary timing; further traffic waits.
            for _ in 0..senders {
                assert!(
                    core.consume_sender_quality_budget("producer-session", now),
                    "producer boundary allowance was lost"
                );
            }
            assert!(
                !core.consume_sender_quality_budget("producer-session", now),
                "sender evidence above the room budget was accepted"
            );
        }
    });
}

#[test]
fn signal_binds_new_share_quality_and_preserves_active_share_quality_across_reconnect() {
    let h = Harness::start(HarnessOptions::default());
    let initial = json!({"resolution": {"width": 2560, "height": 1440}, "maxFramerate": 60, "maxBitrate": 8_000_000, "degradationPreference": "maintain-resolution", "maxAudioBitrate": 192_000});
    let first_generation = "first_share_generation_12345678";
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "quality-viewer-client",
        1,
        "",
        presence(),
    );
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "quality-host-client",
        1,
        first_generation,
        quality(initial.clone()),
    );
    expect_equal(
        &host_auth.quality_settings().to_string(),
        &initial.to_string(),
    );
    expect_equal(
        &viewer.next("quality-settings").raw,
        &json!({"type": "quality-settings", "qualitySettings": initial}).to_string(),
    );
    let reconnect = json!({"resolution": {"width": 854, "height": 480}, "maxFramerate": 15, "maxBitrate": 2_000_000, "degradationPreference": "maintain-framerate", "maxAudioBitrate": 64_000});
    let reconnected = h.open_client();
    let reconnected_auth = authenticate(
        &reconnected,
        &h.room,
        "host",
        "quality-host-client",
        1,
        first_generation,
        quality(reconnect.clone()),
    );
    expect_close_code(&host, 4001);
    expect_equal(
        &reconnected_auth.quality_settings().to_string(),
        &initial.to_string(),
    );
    if let Some(message) = viewer.try_next("quality-settings", Duration::from_millis(30)) {
        panic!("unexpected quality-settings: {}", message.raw);
    }
    let updated = json!({"resolution": {"width": 1280, "height": 720}, "maxFramerate": 30, "maxBitrate": 3_000_000, "degradationPreference": "balanced", "maxAudioBitrate": 128_000});
    reconnected.send_json(json!({"type": "set-quality-settings", "qualitySettings": updated}));
    expect_equal(
        &viewer.next("quality-settings").raw,
        &json!({"type": "quality-settings", "qualitySettings": updated}).to_string(),
    );
    reconnected.send_json(json!({"type": "stop-sharing", "shareGeneration": first_generation}));
    // Stopping media keeps the Host session; its replacement moves to a new share.
    viewer.next("sharing-stopped");
    let next_host = h.open_client();
    let next_auth = authenticate(
        &next_host,
        &h.room,
        "host",
        "quality-host-client",
        1,
        "next_share_generation_12345678",
        quality(reconnect.clone()),
    );
    expect_close_code(&reconnected, 4001);
    expect_equal(
        &next_auth.quality_settings().to_string(),
        &reconnect.to_string(),
    );
    expect_equal(
        &viewer.next("quality-settings").raw,
        &json!({"type": "quality-settings", "qualitySettings": reconnect}).to_string(),
    );
}

// ---------------------------------------------------------------------------
// grace, reconnect and media identity
// ---------------------------------------------------------------------------

#[test]
fn signal_forwards_exact_direct_viewer_receive_evidence_to_host() {
    let clock = TestClock::new(10_000);
    let h = Harness::start(HarnessOptions {
        now: Some(clock.clock()),
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "evidence-direct-host",
        1,
        "test_share_generation_12345678",
        with_name(""),
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "evidence-direct-viewer",
        1,
        "",
        presence(),
    );
    host.ignore("viewer-presence");
    let prepared = next_prepared_route(&viewer);
    let (connection, revision) = (prepared.connection_id(), prepared.revision());
    send_ready(&viewer, &prepared);
    next_active_route_revision(&viewer, revision);
    next_active_route_revision(&host, revision);
    host.send_json(signal_description(
        &viewer_auth.peer_id(),
        &connection,
        "offer",
    ));
    viewer.next("signal");
    viewer.send_json(viewer_quality_evidence(&connection, revision, 0, 0));
    expect_match(
        &host.next("viewer-quality-evidence").raw,
        &json!({
            "viewerPeerId": viewer_auth.peer_id(),
            "upstream": {"kind": "peer", "peerId": host_auth.peer_id()},
            "guard": {"connectionId": connection, "routeRevision": revision},
        })
        .to_string(),
    );
    viewer.send_json(viewer_quality_evidence(&connection, revision, 0, 1));
    host.expect_no("viewer-quality-evidence", Duration::from_millis(40));
    clock.advance(2_000);
    viewer.send_json(viewer_quality_evidence(&connection, revision, 7, 1));
    expect_match(
        &host.next("viewer-quality-evidence").raw,
        &json!({"guard": {"connectionId": connection, "routeRevision": revision, "presentationEpoch": 1}, "sequence": 7}).to_string(),
    );
    clock.advance(2_000);
    viewer.send_json(viewer_quality_evidence(&connection, revision, 1, 0));
    host.expect_no("viewer-quality-evidence", Duration::from_millis(40));
    viewer.send_json(viewer_quality_evidence(&connection, revision, 0, 2));
    host.expect_no("viewer-quality-evidence", Duration::from_millis(40));
    clock.advance(2_000);
    viewer.send_json(viewer_quality_evidence(&connection, revision, 9, 2));
    expect_match(
        &host.next("viewer-quality-evidence").raw,
        &json!({"guard": {"connectionId": connection, "routeRevision": revision, "presentationEpoch": 2}, "sequence": 9}).to_string(),
    );
}

#[test]
fn signal_forwards_relayed_child_receive_report_to_exact_parent_and_host() {
    let clock = TestClock::new(10_000);
    let h = Harness::start(HarnessOptions {
        endpoint_media_copy_capacity: 1,
        now: Some(clock.clock()),
        ..Default::default()
    });
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "evidence-relay-host",
        1,
        "test_share_generation_12345678",
        with_name(""),
    );
    let parent = h.open_client();
    let parent_auth = authenticate(
        &parent,
        &h.room,
        "viewer",
        "evidence-relay-parent",
        1,
        "",
        presence(),
    );
    let parent_prepare = next_prepared_route(&parent);
    parent.send_json(
        json!({"type": "route-ready", "revision": parent_prepare.revision(), "phase": "prepare"}),
    );
    next_active_route_revision(&parent, parent_prepare.revision());
    let child = h.open_client();
    let child_auth = authenticate(
        &child,
        &h.room,
        "viewer",
        "evidence-relay-child",
        1,
        "",
        presence(),
    );
    let child_prepare = next_prepared_route(&child);
    assert_eq!(
        child_prepare.upstream_peer(),
        parent_auth.peer_id(),
        "child upstream"
    );
    child.send_json(
        json!({"type": "route-ready", "revision": child_prepare.revision(), "phase": "prepare"}),
    );
    next_active_route_revision(&child, child_prepare.revision());
    let (connection, revision) = (child_prepare.connection_id(), child_prepare.revision());
    child.send_json(viewer_quality_evidence(&connection, revision, 0, 0));
    let expected = json!({
        "viewerPeerId": child_auth.peer_id(),
        "upstream": {"kind": "peer", "peerId": parent_auth.peer_id()},
        "guard": {"connectionId": connection, "routeRevision": revision},
    })
    .to_string();
    expect_match(&parent.next("viewer-quality-evidence").raw, &expected);
    expect_match(&host.next("viewer-quality-evidence").raw, &expected);
    host.send_json(json!({"type": "request-route-diagnostic"}));
    let quality = diagnostic_quality(&host.next("route-diagnostic-snapshot").raw);
    expect_equal(
        &quality.to_string(),
        r#"{"eligibleWindows":1,"eligibleDurationMs":2000,"freezeWindows":0,"freezeCount":0,"freezeDurationMs":0,"pauseCount":0,"pauseDurationMs":0}"#,
    );
    clock.advance(2_000);
    let mut incomplete = viewer_quality_evidence(&connection, revision, 1, 0);
    incomplete["metrics"]["pauseCountDelta"] = Value::Null;
    child.send_json(incomplete);
    expect_match(
        &parent.next("viewer-quality-evidence").raw,
        r#"{"metrics":{"pauseCountDelta":null}}"#,
    );
    expect_match(
        &host.next("viewer-quality-evidence").raw,
        r#"{"metrics":{"pauseCountDelta":null}}"#,
    );
    host.send_json(json!({"type": "request-route-diagnostic"}));
    let unchanged = diagnostic_quality(&host.next("route-diagnostic-snapshot").raw);
    assert_eq!(unchanged["eligibleWindows"], json!(1), "eligibleWindows");
}

// ---------------------------------------------------------------------------
// presence roster (continued)
// ---------------------------------------------------------------------------
