//! Presence, roster and host-name publication.

use super::*;

#[test]
fn signal_answers_only_current_sockets_and_rate_limits_each_socket() {
    let clock = TestClock::new(wall_now());
    let h = Harness::start(HarnessOptions {
        now: Some(clock.clock()),
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "challenge-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    next_active_route_revision(&host, host_auth.route_revision());
    host.send_json(json!({"type": "signaling-challenge", "sequence": 7}));
    expect_equal(
        &host.next("signaling-challenge-response").raw,
        r#"{"type":"signaling-challenge-response","sequence":7}"#,
    );
    host.send_json(json!({"type": "signaling-challenge", "sequence": 8}));
    host.expect_no("signaling-challenge-response", Duration::from_millis(40));
    clock.advance(1_000);
    host.send_json(json!({"type": "signaling-challenge", "sequence": 9}));
    expect_equal(
        &host.next("signaling-challenge-response").raw,
        r#"{"type":"signaling-challenge-response","sequence":9}"#,
    );
}

#[test]
fn signal_serves_one_current_route_snapshot_only_to_authenticated_host() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "route-diagnostic-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "route-diagnostic-viewer",
        1,
        "",
        presence(),
    );
    host.send_json(json!({"type": "request-route-diagnostic"}));
    expect_match(
        &host.next("route-diagnostic-snapshot").raw,
        r#"{"snapshot":{"children":[{"ordinal":1,"finalRoute":"waiting"}]}}"#,
    );
    viewer.send_json(json!({"type": "request-route-diagnostic"}));
    expect_match(&viewer.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
}

// ---------------------------------------------------------------------------
// native wire compatibility
// ---------------------------------------------------------------------------

#[test]
fn signal_keeps_native_host_wire_and_accepts_display_names() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "native-compatible-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "native-compatible-viewer",
        1,
        "",
        Presence {
            display_name: "移动观众".into(),
            ..Default::default()
        },
    );
    let host_prepared = next_prepared_route(&host);
    let viewer_active = commit_prepared_route(&viewer);
    assert_eq!(host_prepared.revision(), viewer_active.revision());
    next_active_route_revision(&host, viewer_active.revision());
    host.expect_no("route-update", Duration::from_millis(40));
    host.send_json(json!({"type": "set-display-name", "displayName": "改名的主持人"}));
    next_viewer_presence_matching(&host, |message| {
        message
            .host()
            .is_some_and(|entry| entry.display_name() == "改名的主持人")
    });
}

// ---------------------------------------------------------------------------
// password and code entry
// ---------------------------------------------------------------------------

#[test]
fn signal_reports_every_online_viewer_without_expanding_host_media_fanout() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "presence-host",
        1,
        "test_share_generation_12345678",
        with_name(""),
    );
    next_viewer_presence_matching(&host, |message| message.viewers().is_empty());
    let mut viewers = Vec::new();
    for (index, name) in ["阿明", "阿青", ""].iter().enumerate() {
        let viewer = h.open_client();
        let authenticated = authenticate(
            &viewer,
            &h.room,
            "viewer",
            &format!("presence-viewer-{index}"),
            1,
            "",
            with_name(name),
        );
        let prepared = next_prepared_route(&viewer);
        send_ready(&viewer, &prepared);
        assert!(
            prepared.revision() > authenticated.route_revision(),
            "prepare revision must exceed the authenticated revision"
        );
        next_active_route_revision(&viewer, prepared.revision());
        viewers.push(viewer);
    }
    let host_peer = host_auth.peer_id();
    let is_relay = |entry: &PresenceEntry| {
        entry.upstream_kind() == "peer" && entry.upstream_peer() != host_peer
    };
    let full = next_viewer_presence_matching(&host, |message| {
        let entries = message.viewers();
        entries.len() == 3 && entries.iter().any(is_relay)
    });
    let entries = full.viewers();
    assert_eq!(display_names(&entries), vec!["阿明", "阿青", "观众"]);
    let direct = entries
        .iter()
        .filter(|entry| entry.upstream_kind() == "peer" && entry.upstream_peer() == host_peer)
        .count();
    assert_eq!(direct, 2, "expected 2 direct children of the host");
    assert!(
        entries.iter().all(PresenceEntry::media_ready),
        "every viewer must be mediaReady"
    );
    let relay = entries.iter().find(|entry| is_relay(entry)).expect("relay");
    assert!(
        entries
            .iter()
            .any(|entry| entry.peer_id() == relay.upstream_peer()),
        "relay parent must be in the roster"
    );
    host.ignore("route-update");
    host.send_json(signal_candidate(
        &relay.peer_id(),
        "stale_connection_12345678",
    ));
    host.expect_no("route-update", Duration::from_millis(40));
    viewers[2].send_json(json!({"type": "set-display-name", "displayName": "后来改名"}));
    let renamed = next_viewer_presence_matching(&host, |message| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.display_name() == "后来改名")
    });
    assert_eq!(
        renamed.viewers().len(),
        3,
        "renamed roster: {}",
        renamed.raw
    );
    h.close_client(&viewers[2]);
    let after = next_viewer_presence_matching(&host, |message| message.viewers().len() == 2);
    assert_eq!(display_names(&after.viewers()), vec!["阿明", "阿青"]);
}

// ---------------------------------------------------------------------------
// quality evidence forwarding
// ---------------------------------------------------------------------------

#[test]
fn signal_shares_host_name_with_host_and_viewer_roster_subscribers() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "named-host-client",
        1,
        "test_share_generation_12345678",
        with_name("原始分享者"),
    );
    let initial = parse_presence(&host.next("viewer-presence").raw);
    let entry = initial.host().expect("host entry");
    assert!(
        entry.peer_id() == host_auth.peer_id()
            && entry.display_name() == "原始分享者"
            && entry.upstream_kind() == "none",
        "initial host presence: {}",
        initial.raw
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "named-viewer-client",
        1,
        "",
        with_name("观看者"),
    );
    let host_named = |name: &'static str| {
        move |message: &PresenceMessage| {
            message
                .host()
                .is_some_and(|entry| entry.display_name() == name)
        }
    };
    let snapshot = next_viewer_presence_matching(&viewer, host_named("原始分享者"));
    assert_eq!(
        snapshot.host().expect("host").peer_id(),
        host_auth.peer_id()
    );
    assert!(
        snapshot
            .viewers()
            .iter()
            .any(|entry| entry.peer_id() == viewer_auth.peer_id()),
        "viewer must see itself"
    );
    let host_snapshot = next_viewer_presence_matching(&host, |message| {
        host_named("原始分享者")(message) && message.viewers().len() == 1
    });
    assert_eq!(
        host_snapshot.host().expect("host").peer_id(),
        host_auth.peer_id()
    );
    host.send_json(json!({"type": "set-display-name", "displayName": "改名后的分享者"}));
    let renamed_viewer = next_viewer_presence_matching(&viewer, host_named("改名后的分享者"));
    assert_eq!(
        renamed_viewer.host().expect("host").peer_id(),
        host_auth.peer_id()
    );
    let renamed_host = next_viewer_presence_matching(&host, host_named("改名后的分享者"));
    assert_eq!(
        renamed_host.host().expect("host").peer_id(),
        host_auth.peer_id()
    );
}

#[test]
fn signal_replaces_viewer_roster_after_another_viewer_leaves() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "viewer-roster-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let subscriber = h.open_client();
    let subscriber_auth = authenticate(
        &subscriber,
        &h.room,
        "viewer",
        "viewer-roster-subscriber",
        1,
        "",
        with_name("订阅者"),
    );
    next_viewer_presence_matching(&subscriber, |message| message.viewers().len() == 1);
    let other = h.open_client();
    let other_auth = authenticate(
        &other,
        &h.room,
        "viewer",
        "viewer-roster-other",
        1,
        "",
        with_name("另一位"),
    );
    let joined = next_viewer_presence_matching(&subscriber, |message| message.viewers().len() == 2);
    assert_eq!(
        peer_ids(&joined.viewers()),
        vec![subscriber_auth.peer_id(), other_auth.peer_id()]
    );
    h.close_client(&other);
    let left = next_viewer_presence_matching(&subscriber, |message| message.viewers().len() == 1);
    assert_eq!(peer_ids(&left.viewers()), vec![subscriber_auth.peer_id()]);
}

#[test]
fn signal_clears_disconnected_host_name_and_publishes_replacement_name() {
    let h = Harness::start(HarnessOptions {
        viewer_disconnect_grace_ms: 40,
        ..Default::default()
    });
    let first_host = h.open_client();
    let first_auth = authenticate(
        &first_host,
        &h.room,
        "host",
        "replaceable-host-client",
        1,
        "test_share_generation_12345678",
        with_name("旧分享者"),
    );
    first_host.next("viewer-presence");
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "replacement-roster-viewer",
        1,
        "",
        with_name(""),
    );
    next_viewer_presence_matching(&viewer, |message| {
        message
            .host()
            .is_some_and(|entry| entry.display_name() == "旧分享者")
    });
    h.close_client(&first_host);
    viewer.next("host-status");
    let offline = next_viewer_presence_matching(&viewer, |message| message.host().is_none());
    expect_match(
        &offline.viewers_json(),
        &json!([{"peerId": viewer_auth.peer_id(), "upstream": {"kind": "none"}}]).to_string(),
    );
    let replacement = h.open_client();
    let replacement_auth = authenticate(
        &replacement,
        &h.room,
        "host",
        "replaceable-host-client",
        1,
        "test_share_generation_12345678",
        with_name("新分享者"),
    );
    assert_eq!(
        replacement_auth.peer_id(),
        first_auth.peer_id(),
        "replacement peer"
    );
    let online = next_viewer_presence_matching(&viewer, |message| {
        message
            .host()
            .is_some_and(|entry| entry.display_name() == "新分享者")
    });
    assert_eq!(online.host().expect("host").peer_id(), first_auth.peer_id());
    h.close_client(&replacement);
    h.close_client(&viewer);
}

// ---------------------------------------------------------------------------
// grant rotation and revocation
// ---------------------------------------------------------------------------
