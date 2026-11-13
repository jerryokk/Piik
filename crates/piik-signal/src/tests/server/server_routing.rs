//! Route policy, NAT prediction and route recovery over signaling.

use super::*;

#[test]
fn signal_echoes_exact_per_share_route_policy_authority() {
    for sfu_enabled in [false, true] {
        for peer_only in [false, true] {
            let h = Harness::start(HarnessOptions {
                nat_prediction_enabled: true,
                sfu: sfu_enabled.then(fake_sfu),
                ..Default::default()
            });
            let viewer = h.open_client();
            let waiting = authenticate(
                &viewer,
                &h.room,
                "viewer",
                "route-policy-viewer",
                1,
                "",
                presence(),
            );
            assert!(
                waiting.share_generation().is_none()
                    && waiting.route_policy().peer_only != sfu_enabled,
                "waiting policy: {}",
                waiting.raw
            );
            let host = h.open_client();
            let requested = RoutePolicy {
                peer_only,
                topology_optimization: true,
                nat_prediction: true,
            };
            let expected = RoutePolicy {
                peer_only: !sfu_enabled || peer_only,
                ..requested
            };
            let authenticated = authenticate(
                &host,
                &h.room,
                "host",
                "route-policy-host",
                1,
                "route_policy_share_generation_12345678",
                Presence {
                    route_policy: Some(requested),
                    ..Default::default()
                },
            );
            assert_eq!(authenticated.route_policy(), expected, "routePolicy");
            assert!(
                !has_key(&authenticated.raw, "sfuStandbyUrl"),
                "sfuStandbyUrl must be absent"
            );
            expect_equal(
                &viewer.next("route-policy").raw,
                &format!(
                    r#"{{"type":"route-policy","shareGeneration":"route_policy_share_generation_12345678","routePolicy":{{"peerOnly":{},"topologyOptimization":true,"natPrediction":true}}}}"#,
                    expected.peer_only
                ),
            );
        }
    }
}

#[test]
fn signal_broadcasts_configured_nat_prediction_in_lightweight_rooms() {
    let h = Harness::start(HarnessOptions {
        stun_urls: vec!["stun:share.example.test:3478".into()],
        nat_prediction_enabled: true,
        ..Default::default()
    });
    let waiting_viewer = h.open_client();
    let waiting = authenticate(
        &waiting_viewer,
        &h.room,
        "viewer",
        "ordinary-nat-waiting-viewer",
        -1,
        "",
        presence(),
    );
    assert!(
        !waiting.route_policy().nat_prediction,
        "waiting natPrediction must be false"
    );
    let host = h.open_client();
    let mut policy = DEFAULT_ROUTE_POLICY;
    policy.nat_prediction = true;
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "ordinary-nat-host",
        -1,
        "ordinary_nat_share_generation_12345678",
        Presence {
            route_policy: Some(policy),
            ..Default::default()
        },
    );
    policy.peer_only = true;
    assert_eq!(host_auth.route_policy(), policy);
    expect_match(
        &waiting_viewer.next("route-policy").raw,
        r#"{"routePolicy":{"peerOnly":true,"topologyOptimization":true,"natPrediction":true}}"#,
    );
    let joining = h.open_client();
    let joined = authenticate(
        &joining,
        &h.room,
        "viewer",
        "ordinary-nat-joining-viewer",
        -1,
        "",
        presence(),
    );
    assert_eq!(joined.route_policy(), policy, "joined routePolicy");
}

#[test]
fn signal_clamps_nat_prediction_off_when_server_capability_is_disabled() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let mut policy = DEFAULT_ROUTE_POLICY;
    policy.nat_prediction = true;
    let authenticated = authenticate(
        &host,
        &h.room,
        "host",
        "disabled-nat-host",
        -1,
        "disabled_nat_share_generation_12345678",
        Presence {
            route_policy: Some(policy),
            ..Default::default()
        },
    );
    assert!(
        !authenticated.route_policy().nat_prediction,
        "natPrediction must be clamped off"
    );
}

#[test]
fn signal_delivers_only_self_hosted_auxiliary_nat_stun_endpoints() {
    let h = Harness::start(HarnessOptions {
        stun_urls: vec!["stun:ordinary.example.test:3478".into()],
        nat_prediction_enabled: true,
        ..Default::default()
    });
    let viewer = h.open_client();
    let authenticated = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "nat-auxiliary-viewer",
        -1,
        "",
        presence(),
    );
    expect_equal(
        &authenticated.ice_config(),
        r#"{"iceServers":[{"urls":["stun:ordinary.example.test:3478"]}],"natPredictionStunUrls":["stun:ordinary.example.test:3479","stun:ordinary.example.test:3480"]}"#,
    );
}

// ---------------------------------------------------------------------------
// lifecycle and restart
// ---------------------------------------------------------------------------

#[test]
fn connection_attempt_progress_matches_both_recipients() {
    let h = Harness::start(HarnessOptions {
        nat_prediction_enabled: true,
        stun_urls: vec!["stun:share.example.test:3478".into()],
        ..Default::default()
    });
    let (host, viewer) = (h.open_client(), h.open_client());
    let policy = RoutePolicy {
        peer_only: true,
        topology_optimization: false,
        nat_prediction: true,
    };
    authenticate(
        &host,
        &h.room,
        "host",
        "progress-host",
        1,
        "progress_share_generation_12345678",
        Presence {
            route_policy: Some(policy),
            ..Default::default()
        },
    );
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "progress-viewer",
        0,
        "",
        presence(),
    );
    for current in 1..=4 {
        let (host_prepare, viewer_prepare) =
            (next_prepared_route(&host), next_prepared_route(&viewer));
        for update in [&host_prepare, &viewer_prepare] {
            decode_server_message(update.raw.as_bytes()).expect("decode");
            assert_eq!(
                update.value.pointer("/candidate/connectionAttempt"),
                Some(&json!({"current": current, "total": 4})),
                "progress"
            );
        }
        assert_eq!(
            host_prepare.connection_id(),
            viewer_prepare.connection_id(),
            "recipients disagree on the candidate"
        );
        viewer.send_json(json!({
            "type": "route-failed",
            "phase": "prepare",
            "revision": viewer_prepare.revision(),
            "connectionId": viewer_prepare.connection_id(),
        }));
    }
}

#[test]
fn signal_rebuilds_route_when_viewer_reconnects_before_host_after_restart() {
    let path = tempdir().join("rooms.sqlite");
    let mut first_store = new_store(8, Some(piik_room::Database::new(&path).expect("database")));
    let room = create_store_room(&mut first_store, CodeEntryPolicy::Open, "");
    let mut first = Harness::start(HarnessOptions {
        store: Some(first_store),
        room: Some(room.clone()),
        ..Default::default()
    });
    let first_host = first.open_client();
    authenticate(
        &first_host,
        &room,
        "host",
        "restart-route-host",
        1,
        "restart_share_generation_12345678",
        presence(),
    );
    let first_viewer = first.open_client();
    authenticate(
        &first_viewer,
        &room,
        "viewer",
        "restart-route-viewer",
        1,
        "",
        presence(),
    );
    next_prepared_route(&first_viewer);
    first.close();

    let second_store = new_store(8, Some(piik_room::Database::new(&path).expect("database")));
    let second = Harness::start(HarnessOptions {
        store: Some(second_store),
        room: Some(room.clone()),
        ..Default::default()
    });
    let second_viewer = second.open_client();
    let viewer_auth = authenticate(
        &second_viewer,
        &room,
        "viewer",
        "restart-route-viewer",
        1,
        "",
        presence(),
    );
    assert_eq!(viewer_auth.upstream_kind(), "none", "viewer upstream");
    let second_host = second.open_client();
    authenticate(
        &second_host,
        &room,
        "host",
        "restart-route-host",
        1,
        "restart_share_generation_12345678",
        presence(),
    );
    let viewer_prepare = next_prepared_route(&second_viewer);
    let host_prepare = next_prepared_route(&second_host);
    assert!(
        viewer_prepare.revision() == host_prepare.revision()
            && viewer_prepare.candidate() == host_prepare.candidate(),
        "viewer prepare {}\nhost prepare {}",
        viewer_prepare.raw,
        host_prepare.raw
    );
}

#[test]
fn signal_allows_retained_child_restart_while_relay_repairs_ingress() {
    let h = Harness::start(HarnessOptions {
        endpoint_media_copy_capacity: 1,
        ..Default::default()
    });
    let room = h.room.room_id.clone();
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "restart-host-client",
        0,
        "test_share_generation_12345678",
        presence(),
    );
    let relay = h.open_client();
    let relay_auth = authenticate(
        &relay,
        &h.room,
        "viewer",
        "restart-relay-client",
        1,
        "",
        presence(),
    );
    commit_prepared_route(&relay);
    let child = h.open_client();
    let child_auth = authenticate(
        &child,
        &h.room,
        "viewer",
        "restart-child-client",
        0,
        "",
        presence(),
    );
    let active = commit_prepared_route(&child);
    assert_eq!(
        active.upstream_peer(),
        relay_auth.peer_id(),
        "child upstream must be the relay"
    );
    let (relay_edge, child_edge) = h.locked(|core| {
        (
            core.resolve_active_viewer_media_edge(&room, &relay_auth.peer_id())
                .expect("relay ingress must be active"),
            core.resolve_active_viewer_media_edge(&room, &child_auth.peer_id())
                .expect("child ingress must be active"),
        )
    });
    relay.send_json(json!({"type": "route-failed", "phase": "active", "revision": relay_edge.revision, "connectionId": relay_edge.connection_id}));
    let repair = next_prepared_route(&relay);
    // The report is processed on the relay's reader; wait for its effect.
    assert!(
        wait_for(DEFAULT_WAIT, || h.locked(|core| {
            core.snapshot(&room)
                .and_then(|snapshot| {
                    snapshot
                        .upstream_by_viewer
                        .get(&relay_auth.peer_id())
                        .map(|edge| !edge.usable)
                })
                .unwrap_or(false)
        })),
        "relay ingress failure was not applied"
    );
    h.locked(|core| {
        let snapshot = core.snapshot(&room).expect("snapshot");
        let edge = snapshot
            .upstream_by_viewer
            .get(&child_auth.peer_id())
            .expect("child edge");
        assert!(
            edge.physical_active
                && edge.usable
                && crate::router::video_connection_id(&snapshot, edge) == child_edge.connection_id,
            "relay repair must retain its child's physical connection"
        );
        assert!(
            core.resolve_active_viewer_media_edge(&room, &child_auth.peer_id())
                .is_none(),
            "a retained subtree must not claim the repairing source path is ready"
        );
    });
    child.send_json(json!({"type": "restart-request", "targetPeerId": relay_auth.peer_id(), "connectionId": child_edge.connection_id, "rebuild": false}));
    let forwarded = relay
        .try_next("restart-request", DEFAULT_WAIT)
        .unwrap_or_else(|| {
            let denial = child
                .try_next("error", Duration::ZERO)
                .map(|message| message.raw)
                .unwrap_or_default();
            panic!("retained child restart was not forwarded: {denial}");
        });
    expect_match(
        &forwarded.raw,
        &json!({"fromPeerId": child_auth.peer_id(), "connectionId": child_edge.connection_id, "rebuild": false}).to_string(),
    );
    for request in [
        json!({"type": "restart-request", "targetPeerId": host_auth.peer_id(), "connectionId": child_edge.connection_id, "rebuild": true}),
        json!({"type": "restart-request", "targetPeerId": relay_auth.peer_id(), "connectionId": "retired_connection_12345678", "rebuild": true}),
    ] {
        child.send_json(request);
        expect_match(&child.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
    }
    relay.send_json(json!({"type": "restart-request", "targetPeerId": child_auth.peer_id(), "connectionId": child_edge.connection_id, "rebuild": true}));
    expect_match(&relay.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
    relay.send_json(json!({"type": "restart-request", "targetPeerId": repair.upstream_peer(), "connectionId": repair.connection_id(), "rebuild": true}));
    expect_match(&relay.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
}

// ---------------------------------------------------------------------------
// capacity and abandonment
// ---------------------------------------------------------------------------
