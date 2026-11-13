//! Viewer reconnect grace and media identity retention.

use super::*;

#[test]
fn signal_keeps_viewer_peer_stable_when_it_reconnects_inside_grace_period() {
    let h = Harness::start(grace(80));
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "grace_share_generation_12345678",
        presence(),
    );
    let first = h.open_client();
    let first_auth = authenticate(
        &first,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        presence(),
    );
    let active = commit_prepared_route(&first);
    next_active_route_revision(&host, active.revision());
    host.send_json(signal_description(
        &first_auth.peer_id(),
        "stable-connection",
        "offer",
    ));
    first.next("signal");
    h.close_client(&first);
    let reconnected = h.open_client();
    let reconnected_auth = authenticate(
        &reconnected,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        presence(),
    );
    assert_eq!(reconnected_auth.peer_id(), first_auth.peer_id());
    assert_eq!(
        reconnected_auth.connection_id().as_deref(),
        Some("stable-connection")
    );
    next_active_route_revision(&host, reconnected_auth.route_revision());
    host.expect_no("route-update", Duration::from_millis(110));
    h.close_client(&reconnected);
    next_active_route_after(&host, active.revision());
    host.send_json(json!({"type": "request-route-diagnostic"}));
    expect_match(
        &host.next("route-diagnostic-snapshot").raw,
        r#"{"snapshot":{"children":[]}}"#,
    );
}

#[test]
fn signal_clears_previous_media_generation_when_sharing_stops() {
    let h = Harness::start(grace(300));
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stop",
        1,
        "grace_stop_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer-client-stop",
        1,
        "",
        presence(),
    );
    commit_prepared_route(&viewer);
    host.send_json(signal_description(
        &viewer_auth.peer_id(),
        "connection-before-stop",
        "offer",
    ));
    viewer.next("signal");
    host.send_json(json!({"type": "stop-sharing"}));
    viewer.next("sharing-stopped");
    expect_match(&viewer.next("host-status").raw, r#"{"online":false}"#);
    // Stopping media keeps the Host's persistent room session.
    host.send_json(json!({"type": "signaling-challenge", "sequence": 1}));
    host.next("signaling-challenge-response");
    h.close_client(&viewer);
    let reconnected = h.open_client();
    let reconnected_auth = authenticate(
        &reconnected,
        &h.room,
        "viewer",
        "viewer-client-stop",
        1,
        "",
        presence(),
    );
    assert!(
        reconnected_auth.connection_id().is_none(),
        "connectionId must be null"
    );
    assert_eq!(reconnected_auth.peer_id(), viewer_auth.peer_id());
    assert!(!reconnected_auth.host_online(), "host must be offline");
}

#[test]
fn signal_replaces_same_client_presence_without_transient_leave_churn() {
    let h = Harness::start(grace(40));
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "test_share_generation_12345678",
        with_name(""),
    );
    assert!(
        parse_presence(&host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "initial roster"
    );
    let original = h.open_client();
    let original_auth = authenticate(
        &original,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        with_name("旧会话"),
    );
    commit_prepared_route(&original);
    let peer_edge = next_viewer_presence_matching(&host, |message| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.upstream_kind() == "peer")
    });
    expect_match(
        &peer_edge.viewers_json(),
        &json!([{"role": "viewer", "peerId": original_auth.peer_id(), "displayName": "旧会话", "upstream": {"kind": "peer", "peerId": host_auth.peer_id()}}]).to_string(),
    );
    let replacement = h.open_client();
    let replacement_auth = authenticate(
        &replacement,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        with_name("新会话"),
    );
    assert_eq!(replacement_auth.peer_id(), original_auth.peer_id());
    expect_close_code(&original, 4001);
    expect_match(
        &parse_presence(&host.next("viewer-presence").raw).viewers_json(),
        &json!([{"role": "viewer", "peerId": original_auth.peer_id(), "displayName": "新会话", "upstream": {"kind": "peer", "peerId": host_auth.peer_id()}}]).to_string(),
    );
    let peer = original_auth.peer_id();
    expect_no_viewer_presence_matching(
        &host,
        |message| {
            !message
                .viewers()
                .iter()
                .any(|entry| entry.peer_id() == peer)
        },
        Duration::from_millis(70),
    );
}

#[test]
fn signal_notifies_host_when_pre_offer_viewer_reconnects_inside_grace() {
    let h = Harness::start(grace(300));
    let original = h.open_client();
    let original_auth = authenticate(
        &original,
        &h.room,
        "viewer",
        "viewer-client-before-offer",
        1,
        "",
        presence(),
    );
    h.close_client(&original);
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "grace_reconnect_generation_12345678",
        presence(),
    );
    next_active_route_revision(&host, host_auth.route_revision());
    let reconnected = h.open_client();
    let reconnected_auth = authenticate(
        &reconnected,
        &h.room,
        "viewer",
        "viewer-client-before-offer",
        1,
        "",
        presence(),
    );
    assert_eq!(reconnected_auth.peer_id(), original_auth.peer_id());
    let prepared = next_prepared_route(&host);
    expect_match(
        &prepared.raw,
        &json!({"candidate": {"childPeerId": original_auth.peer_id(), "transport": "direct", "qualityProbe": false}}).to_string(),
    );
}

#[test]
fn signal_replays_viewers_when_host_reconnects() {
    let h = Harness::start(grace(500));
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        presence(),
    );
    let first_host = h.open_client();
    let first_host_auth = authenticate(
        &first_host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "grace_replay_generation_12345678",
        presence(),
    );
    commit_prepared_route(&viewer);
    viewer.next("host-status");
    first_host.send_json(signal_description(
        &viewer_auth.peer_id(),
        "connection-before-control-outage",
        "offer",
    ));
    viewer.next("signal");
    h.close_client(&first_host);
    expect_match(&viewer.next("host-status").raw, r#"{"online":false}"#);
    h.close_client(&viewer);
    let refreshed = h.open_client();
    let refreshed_auth = authenticate(
        &refreshed,
        &h.room,
        "viewer",
        "viewer-client-stable",
        1,
        "",
        presence(),
    );
    assert_eq!(refreshed_auth.peer_id(), viewer_auth.peer_id());
    assert!(!refreshed_auth.host_online(), "host must be offline");
    let second_host = h.open_client();
    let second_host_auth = authenticate(
        &second_host,
        &h.room,
        "host",
        "host-client-stable",
        1,
        "grace_replay_generation_12345678",
        presence(),
    );
    assert_eq!(second_host_auth.peer_id(), first_host_auth.peer_id());
    let peers = h.locked(|core| core.store.get_viewer_peer_ids(&h.room.room_id));
    assert_eq!(peers, vec![viewer_auth.peer_id()]);
    next_active_route_revision(&second_host, second_host_auth.route_revision());
    expect_match(&refreshed.next("host-status").raw, r#"{"online":true}"#);
}

#[test]
fn signal_reconciles_empty_roster_after_viewer_grace_period_ends_offline() {
    let h = Harness::start(grace(30));
    let first_host = h.open_client();
    let first_host_auth = authenticate(
        &first_host,
        &h.room,
        "host",
        "host-client-roster",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer-client-roster",
        1,
        "",
        presence(),
    );
    commit_prepared_route(&viewer);
    h.close_client(&first_host);
    h.close_client(&viewer);
    std::thread::sleep(Duration::from_millis(70));
    let second_host = h.open_client();
    let second_host_auth = authenticate(
        &second_host,
        &h.room,
        "host",
        "host-client-roster",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    assert_eq!(second_host_auth.peer_id(), first_host_auth.peer_id());
    assert!(
        h.locked(|core| core.store.get_viewer_peer_ids(&h.room.room_id))
            .is_empty(),
        "viewer peers"
    );
    next_active_route_revision(&second_host, second_host_auth.route_revision());
    assert!(
        parse_presence(&second_host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "empty roster was not reconciled"
    );
}

#[test]
fn signal_keeps_media_identity_through_grace_while_presence_returns_once() {
    let h = Harness::start(grace(500));
    let first_host = h.open_client();
    let first_host_auth = authenticate(
        &first_host,
        &h.room,
        "host",
        "host-client-grace-roster",
        1,
        "grace_roster_generation_12345678",
        with_name(""),
    );
    assert!(
        parse_presence(&first_host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "initial roster"
    );
    let first_viewer = h.open_client();
    let first_viewer_auth = authenticate(
        &first_viewer,
        &h.room,
        "viewer",
        "viewer-client-grace-roster",
        1,
        "",
        with_name("短线重连"),
    );
    commit_prepared_route(&first_viewer);
    let peer_edge = next_viewer_presence_matching(&first_host, |message| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.upstream_kind() == "peer")
    });
    expect_match(
        &peer_edge.viewers_json(),
        &json!([{"role": "viewer", "peerId": first_viewer_auth.peer_id(), "displayName": "短线重连", "upstream": {"kind": "peer", "peerId": first_host_auth.peer_id()}}]).to_string(),
    );
    first_host.send_json(signal_description(
        &first_viewer_auth.peer_id(),
        "connection-during-grace",
        "offer",
    ));
    first_viewer.next("signal");
    h.close_client(&first_viewer);
    assert!(
        parse_presence(&first_host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "roster after viewer left"
    );
    h.close_client(&first_host);
    let second_host = h.open_client();
    let second_host_auth = authenticate(
        &second_host,
        &h.room,
        "host",
        "host-client-grace-roster",
        1,
        "grace_roster_generation_12345678",
        with_name(""),
    );
    let peers = h.locked(|core| core.store.get_viewer_peer_ids(&h.room.room_id));
    assert_eq!(peers, vec![first_viewer_auth.peer_id()]);
    assert!(
        parse_presence(&second_host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "second host roster"
    );
    let second_viewer = h.open_client();
    let second_viewer_auth = authenticate(
        &second_viewer,
        &h.room,
        "viewer",
        "viewer-client-grace-roster",
        1,
        "",
        with_name("短线重连"),
    );
    assert_eq!(second_viewer_auth.peer_id(), first_viewer_auth.peer_id());
    assert_eq!(
        second_viewer_auth.connection_id().as_deref(),
        Some("connection-during-grace")
    );
    let peer = first_viewer_auth.peer_id();
    let returned = next_viewer_presence_matching(&second_host, |message| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.peer_id() == peer)
    });
    expect_equal(
        &returned.viewers_json(),
        &json!([{"role": "viewer", "peerId": peer, "displayName": "短线重连", "upstream": {"kind": "peer", "peerId": second_host_auth.peer_id()}, "mediaReady": true}]).to_string(),
    );
    expect_no_viewer_presence_matching(
        &second_host,
        |message| {
            !message
                .viewers()
                .iter()
                .any(|entry| entry.peer_id() == peer)
        },
        Duration::from_millis(30),
    );
    second_viewer.send_json(json!({
        "type": "restart-request",
        "targetPeerId": second_host_auth.peer_id(),
        "connectionId": "connection-during-grace",
        "rebuild": false,
    }));
    expect_match(
        &second_host.next("restart-request").raw,
        &json!({"fromPeerId": peer, "connectionId": "connection-during-grace", "rebuild": false})
            .to_string(),
    );
}
