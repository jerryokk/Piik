//! Room-session persistence and interaction ordering over real clients.

use super::*;

#[test]
fn create_room_releases_lock_when_derivation_or_commit_panics() {
    for password in [None, Some("room-password".to_owned())] {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let server = runtime
            .block_on(async { Server::new(base_options(panicking_store(), 2)).expect("server") });
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            runtime.block_on(server.create_room(CodeEntryPolicy::Open, password.clone(), None))
        }));
        assert!(outcome.is_err(), "the random-source panic must propagate");
        assert!(
            !matches!(
                server.core.try_lock(),
                Err(std::sync::TryLockError::WouldBlock)
            ),
            "create_room left the server locked after a recovered panic"
        );
        runtime
            .block_on(async { server.close(Duration::from_secs(1)).await })
            .expect("close");
    }
}

#[test]
fn password_preparation_does_not_block_another_room() {
    for update in [false, true] {
        let started = Barrier::new();
        let release = Barrier::new();
        let hold = Arc::new(AtomicBool::new(false));
        let random = {
            let (started, release, hold) = (started.clone(), release.clone(), hold.clone());
            Arc::new(move |size: usize| {
                // Hold the existing salt source in the unlocked password phase.
                if hold.swap(false, Ordering::SeqCst) {
                    started.open();
                    release.wait();
                }
                vec![u8::try_from(size).unwrap_or(u8::MAX); size]
            })
        };
        let store = piik_room::Store::new(piik_room::Options {
            max_rooms: 3,
            max_viewers_per_room: 1,
            random: Some(random),
            ..Default::default()
        })
        .expect("store");
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let server =
            runtime.block_on(async { Server::new(base_options(store, 2)).expect("server") });
        let original = runtime
            .block_on(server.create_room(CodeEntryPolicy::Open, None, Some("1234".to_owned())))
            .expect("original room");
        hold.store(true, Ordering::SeqCst);
        let worker = {
            let server = server.clone();
            runtime.spawn(async move {
                let password = Some("room-password".to_owned());
                if update {
                    server
                        .update_room_access(
                            &original.room_id,
                            &original.host_token,
                            RoomAccessUpdateRequest::SetViewerPassword { password },
                        )
                        .await
                        .map(|_| ())
                } else {
                    server
                        .create_room(CodeEntryPolicy::Private, password, Some("2345".to_owned()))
                        .await
                        .map(|_| ())
                }
            })
        };
        assert!(
            started.wait_timeout(DEFAULT_WAIT),
            "password preparation did not start"
        );
        assert!(
            server.core.try_lock().is_ok(),
            "password work blocks signaling for other rooms"
        );
        runtime
            .block_on(server.create_room(CodeEntryPolicy::Open, None, Some("3456".to_owned())))
            .expect("independent room creation");
        release.open();
        let result = runtime
            .block_on(async { tokio::time::timeout(DEFAULT_WAIT, worker).await })
            .expect("password operation did not finish")
            .expect("join");
        result.expect("password operation");
        runtime
            .block_on(async { server.close(Duration::from_secs(1)).await })
            .expect("close");
    }
}

#[test]
fn stop_sharing_does_not_write_room_authority() {
    let directory = tempdir();
    let database = piik_room::Database::new(directory.join("rooms.sqlite")).expect("database");
    let store = new_store(1, Some(database));
    let h = Harness::start(HarnessOptions {
        store: Some(store),
        ..Default::default()
    });
    let host = h.open_client();
    let authenticated = authenticate(
        &host,
        &h.room,
        "host",
        "storage-free-host",
        1,
        "storage_free_generation_12345678",
        presence(),
    );
    host.ignore("route-update");
    h.locked(|core| core.store.close().expect("close database"));
    host.send_json(
        json!({"type": "stop-sharing", "shareGeneration": authenticated.share_generation()}),
    );
    // Stopping media must not close the persistent Host session or touch storage.
    host.send_json(json!({"type": "signaling-challenge", "sequence": 1}));
    host.next("signaling-challenge-response");
    assert_eq!(h.store_size(), 1, "stopping sharing removed room authority");
    let resumed = h.open_client();
    authenticate(
        &resumed,
        &h.room,
        "host",
        "storage-free-host",
        1,
        "next_storage_free_generation",
        presence(),
    );
}

#[test]
fn host_keeps_room_session_and_interactions_after_sharing_stops() {
    let h = Harness::start(HarnessOptions {
        max_viewers_per_room: 2,
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "persistent-host",
        1,
        "first_share_generation_12345678",
        Presence {
            display_name: "Host".into(),
            ..Default::default()
        },
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "persistent-viewer",
        1,
        "",
        Presence {
            display_name: "Viewer".into(),
            ..Default::default()
        },
    );
    host.ignore("route-update");
    host.ignore("viewer-presence");
    host.send_json(
        json!({"type": "stop-sharing", "shareGeneration": host_auth.share_generation()}),
    );
    viewer.next("sharing-stopped");
    viewer.next("host-status");
    assert!(!host.is_closed(), "stopping media closed the Host session");
    h.locked(|core| {
        let connected = core.store.get_connected_host(&h.room.room_id);
        assert!(
            connected.is_some_and(|host| !host.session_id.is_empty()),
            "stopping media must retain the Host room session"
        );
    });
    chat(&host, "persistent_chat_request", "still here");
    host.next("room-interaction");
    viewer.next("room-interaction");
    host.send_json(json!({
        "type": "start-sharing",
        "shareGeneration": "second_share_generation_12345678",
        "routePolicy": {"peerOnly": true, "topologyOptimization": true, "natPrediction": false},
    }));
    let started = host.next("sharing-started");
    assert!(
        started
            .raw
            .contains(r#""shareGeneration":"second_share_generation_12345678""#),
        "unexpected sharing-started: {}",
        started.raw
    );
    viewer.next("host-status");
    h.close_client(&viewer);
    h.close_client(&host);
}

#[test]
fn idle_host_chat_and_publication_acknowledgment_order() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "room-only-host",
        1,
        "",
        Presence {
            ..Default::default()
        },
    );
    host.ignore("viewer-presence");
    host.ignore("room-voice-state");
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "room-only-viewer",
        1,
        "",
        presence(),
    );
    chat(&host, "chat_before_capture", "before capture");
    host.next("room-interaction");
    viewer.next("room-interaction");
    host.send_json(json!({
        "type": "start-sharing",
        "shareGeneration": "new_publication_generation",
        "routePolicy": policy_json(DEFAULT_ROUTE_POLICY),
    }));
    // No ignored route messages: acknowledgment must precede media effects.
    host.send_json(json!({"type": "signaling-challenge", "sequence": 1}));
    host.next("signaling-challenge-response");
    assert_eq!(
        host.first_type().as_deref(),
        Some("sharing-started"),
        "first publication message"
    );
    host.next("sharing-started");
    host.next("route-update");
}

#[test]
fn rejected_publication_retains_host_room_session() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "rejected-start-host",
        1,
        "",
        Presence {
            ..Default::default()
        },
    );
    host.ignore("viewer-presence");
    // Inject the existing route-construction failure, after room admission.
    h.locked(|core| core.router.capacity = 0);
    host.send_json(json!({
        "type": "start-sharing",
        "shareGeneration": "rejected_publication_generation",
        "routePolicy": policy_json(DEFAULT_ROUTE_POLICY),
    }));
    expect_match(
        &host.next("sharing-start-failed").raw,
        r#"{"shareGeneration":"rejected_publication_generation","code":"SERVER_ERROR"}"#,
    );
    h.locked(|core| {
        core.router.capacity = 2;
        assert!(
            core.active_host_share_generation(&h.room.room_id)
                .is_empty(),
            "failed publication remained active"
        );
    });
    chat(&host, "chat_after_rejected_start", "still in room");
    host.next("room-interaction");
    host.send_json(json!({
        "type": "start-sharing",
        "shareGeneration": "next_publication_generation",
        "routePolicy": policy_json(DEFAULT_ROUTE_POLICY),
    }));
    host.next("sharing-started");
    host.next("route-update");
    host.ignore("route-update");
    host.send_json(
        json!({"type": "stop-sharing", "shareGeneration": "next_publication_generation"}),
    );
    host.send_json(json!({
        "type": "start-sharing",
        "shareGeneration": "next_publication_generation",
        "routePolicy": policy_json(DEFAULT_ROUTE_POLICY),
    }));
    expect_match(
        &host.next("sharing-start-failed").raw,
        r#"{"shareGeneration":"next_publication_generation","code":"FORBIDDEN"}"#,
    );
}

#[test]
fn persistent_room_session_reconnect_stays_media_idle() {
    let h = Harness::start(HarnessOptions {
        max_viewers_per_room: 2,
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "idle-reconnect-host",
        1,
        "idle_reconnect_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "idle-reconnect-viewer",
        1,
        "",
        presence(),
    );
    host.ignore("route-update");
    viewer.ignore("route-update");
    host.send_json(
        json!({"type": "stop-sharing", "shareGeneration": host_auth.share_generation()}),
    );
    viewer.next("sharing-stopped");
    viewer.next("host-status");
    h.close_client(&viewer);
    h.close_client(&host);
    let resumed = h.open_client();
    let resumed_auth = authenticate(
        &resumed,
        &h.room,
        "host",
        "idle-reconnect-host",
        1,
        "",
        Presence {
            ..Default::default()
        },
    );
    assert!(
        resumed_auth.share_generation().is_none(),
        "room-only Host must not receive a media generation"
    );
    h.locked(|core| {
        assert!(
            !core.has_controller(&h.room.room_id),
            "room-only Host reconnect created a media controller"
        );
    });
    let late_viewer = h.open_client();
    let late = authenticate(
        &late_viewer,
        &h.room,
        "viewer",
        "idle-reconnect-late-viewer",
        1,
        "",
        presence(),
    );
    assert!(
        !late.host_online() && late.host_paused() == Some(false),
        "idle room advertised active Host media: {}",
        late.raw
    );
    assert!(
        late.share_generation().is_none() && late.upstream_kind() == "none",
        "idle room advertised a media route: {}",
        late.raw
    );
    h.close_client(&late_viewer);
    h.close_client(&resumed);
}

#[test]
fn stopped_publication_reauthentication_preserves_room_session_fence() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let (client_id, stopped_generation) =
        ("stopped-reconnect-host", "stopped_reconnect_generation");
    authenticate(
        &host,
        &h.room,
        "host",
        client_id,
        1,
        stopped_generation,
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "stopped-reconnect-viewer",
        1,
        "",
        presence(),
    );
    host.ignore("route-update");
    viewer.ignore("route-update");
    host.send_json(json!({"type": "stop-sharing", "shareGeneration": stopped_generation}));
    viewer.next("sharing-stopped");
    viewer.next("host-status");
    h.locked(|core| {
        assert!(
            core.active_host_share_generation(&h.room.room_id)
                .is_empty(),
            "stopped publication remained active"
        );
    });
    // A delayed authenticate still carries the generation that was valid when sent.
    let resumed = h.open_client();
    let auth = authenticate(
        &resumed,
        &h.room,
        "host",
        client_id,
        1,
        stopped_generation,
        presence(),
    );
    assert!(
        auth.share_generation().is_none() && auth.upstream_kind() == "none",
        "delayed authentication restored a stopped publication"
    );
    h.locked(|core| {
        assert!(
            core.active_host_share_generation(&h.room.room_id)
                .is_empty(),
            "delayed authentication reacquired media authority"
        );
        assert!(
            !core.has_controller(&h.room.room_id),
            "delayed authentication rebuilt source-less routes"
        );
    });
    let fresh = "fresh_reconnect_generation";
    resumed.send_json(json!({"type": "start-sharing", "shareGeneration": fresh, "routePolicy": policy_json(DEFAULT_ROUTE_POLICY)}));
    resumed.next("sharing-started");
    h.locked(|core| {
        assert_eq!(
            core.active_host_share_generation(&h.room.room_id),
            fresh,
            "fresh publication could not start"
        );
    });
    let active = h.open_client();
    let active_auth = authenticate(&active, &h.room, "host", client_id, 1, fresh, presence());
    assert_eq!(
        active_auth.share_generation().as_deref(),
        Some(fresh),
        "active publication did not survive reauthentication"
    );
}

// ---------------------------------------------------------------------------
// route policy and ICE
// ---------------------------------------------------------------------------
