//! Password, grant rotation and code-entry admission.

use super::*;

#[test]
fn signal_keeps_password_optional_for_private_room_entry() {
    // Unoptimized scrypt can outlast the 500 ms default in debug test builds.
    let h = Harness::start(HarnessOptions {
        authentication_timeout_ms: PASSWORD_AUTHENTICATION_TIMEOUT_MS,
        ..Default::default()
    });
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "password-settings-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let updated = h.update_room_access(RoomAccessUpdateRequest::SetCodeEntryPolicy {
        policy: CodeEntryPolicy::Private,
    });
    assert_eq!(
        updated,
        RoomAccessUpdateResponse::CodeEntryPolicyUpdated {
            code_entry_policy: CodeEntryPolicy::Private,
            viewer_password_enabled: false,
        }
    );
    let wrong = h.open_client();
    send_password_authenticate(&wrong, &h.room, "wrong-password-viewer", "wrong-password");
    expect_match(
        &wrong.next_within("error", PASSWORD_WAIT).raw,
        r#"{"code":"ROOM_ACCESS_DENIED"}"#,
    );
    let enabled = h.update_room_access(RoomAccessUpdateRequest::SetViewerPassword {
        password: Some("easy-password".into()),
    });
    assert_eq!(
        enabled,
        RoomAccessUpdateResponse::ViewerPasswordUpdated { enabled: true }
    );
    let password_viewer = h.open_client();
    send_password_authenticate(
        &password_viewer,
        &h.room,
        "password-viewer",
        "easy-password",
    );
    let admitted = password_viewer.next_within("authenticated", PASSWORD_WAIT);
    password_viewer.send_json(json!({"type": "relay-capacity", "downstreamEdges": 1}));
    assert_eq!(admitted.json()["role"], "viewer");
    commit_prepared_route(&password_viewer);
    let disabled =
        h.update_room_access(RoomAccessUpdateRequest::SetViewerPassword { password: None });
    assert_eq!(
        disabled,
        RoomAccessUpdateResponse::ViewerPasswordUpdated { enabled: false }
    );
    let grant_viewer = h.open_client();
    let granted = authenticate(
        &grant_viewer,
        &h.room,
        "viewer",
        "grant-after-password-removal",
        1,
        "",
        presence(),
    );
    assert_eq!(granted.role(), "viewer");
    let removed = h.open_client();
    send_password_authenticate(
        &removed,
        &h.room,
        "removed-password-viewer",
        "easy-password",
    );
    expect_match(
        &removed.next_within("error", PASSWORD_WAIT).raw,
        r#"{"code":"ROOM_ACCESS_DENIED"}"#,
    );
}

#[test]
fn signal_reports_password_configuration_only_to_authenticated_host() {
    let h = Harness::start(HarnessOptions::default());
    h.update_room_access(RoomAccessUpdateRequest::SetCodeEntryPolicy {
        policy: CodeEntryPolicy::Private,
    });
    let host = h.open_client();
    let without = authenticate(
        &host,
        &h.room,
        "host",
        "password-state-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    expect_match(
        &without.raw,
        r#"{"role":"host","codeEntryPolicy":"private","viewerPasswordEnabled":false}"#,
    );
    let viewer = h.open_client();
    let viewer_auth = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "password-state-viewer",
        1,
        "",
        presence(),
    );
    assert!(
        !has_key(&viewer_auth.raw, "viewerPasswordEnabled"),
        "viewerPasswordEnabled must be absent for viewers"
    );
    h.update_room_access(RoomAccessUpdateRequest::SetViewerPassword {
        password: Some("room-password".into()),
    });
    let replacement = h.open_client();
    let with = authenticate(
        &replacement,
        &h.room,
        "host",
        "password-state-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    expect_match(
        &with.raw,
        r#"{"role":"host","codeEntryPolicy":"private","viewerPasswordEnabled":true}"#,
    );
}

// ---------------------------------------------------------------------------
// presence roster
// ---------------------------------------------------------------------------

#[test]
fn signal_strongly_rotates_and_revokes_every_viewer_generation_and_media_edge() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "ordinary-access-presence-host",
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
    let first = h.open_client();
    let first_auth = authenticate(
        &first,
        &h.room,
        "viewer",
        "ordinary-access-presence-viewer-1",
        1,
        "",
        with_name("第一位"),
    );
    commit_prepared_route(&first);
    let has_peer_edge = |message: &PresenceMessage| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.upstream_kind() == "peer")
    };
    expect_match(
        &next_viewer_presence_matching(&host, has_peer_edge).viewers_json(),
        &json!([{"role": "viewer", "peerId": first_auth.peer_id(), "displayName": "第一位", "upstream": {"kind": "peer", "peerId": host_auth.peer_id()}}]).to_string(),
    );
    let rotated = h.update_room_access(RoomAccessUpdateRequest::RotateViewerGrant);
    expect_match(
        &first.next("viewer-grant-revoked").raw,
        &json!({"viewerAuthorizationGeneration": first_auth.viewer_authorization_generation()})
            .to_string(),
    );
    expect_close_code(&first, 4004);
    next_viewer_presence_matching(&host, |message| message.viewers().is_empty());
    let grant = viewer_grant_of(&rotated_invite(&rotated).expect("rotated invite"));
    assert!(!grant.is_empty(), "rotated grant must be present");
    let mut rotated_room = h.room.clone();
    rotated_room.viewer_grant = grant;
    let second = h.open_client();
    let second_auth = authenticate(
        &second,
        &rotated_room,
        "viewer",
        "ordinary-access-presence-viewer-2",
        1,
        "",
        with_name("第二位"),
    );
    commit_prepared_route(&second);
    let seen = next_viewer_presence_matching(&host, |message| {
        message
            .viewers()
            .iter()
            .any(|entry| entry.peer_id() == second_auth.peer_id())
    });
    assert_eq!(seen.viewers().len(), 1, "second roster: {}", seen.raw);
    let revoked = h.update_room_access(RoomAccessUpdateRequest::RevokeViewerGrant);
    second.next("viewer-grant-revoked");
    expect_close_code(&second, 4004);
    next_viewer_presence_matching(&host, |message| message.viewers().is_empty());
    assert!(
        matches!(
            revoked,
            RoomAccessUpdateResponse::ViewerGrantUpdated {
                invite_url: None,
                ..
            }
        ),
        "revoke response: {revoked:?}"
    );
    expect_no_viewer_presence_matching(
        &host,
        |message| !message.viewers().is_empty(),
        Duration::from_millis(30),
    );
}

#[test]
fn signal_rotates_grant_and_promotes_waiting_code_admitted_viewer() {
    let h = Harness::start(HarnessOptions {
        endpoint_media_copy_capacity: 1,
        max_viewers_per_room: 2,
        ..Default::default()
    });
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "mixed-access-host",
        2,
        "test_share_generation_12345678",
        with_name(""),
    );
    assert!(
        parse_presence(&host.next("viewer-presence").raw)
            .viewers()
            .is_empty(),
        "initial roster"
    );
    let grant_viewer = h.open_client();
    let grant_auth = authenticate(
        &grant_viewer,
        &h.room,
        "viewer",
        "mixed-grant-viewer",
        0,
        "",
        with_name("邀请观众"),
    );
    commit_prepared_route(&grant_viewer);
    next_viewer_presence_matching(&host, |message| {
        message.viewers().iter().any(|entry| {
            entry.peer_id() == grant_auth.peer_id()
                && entry.upstream_kind() == "peer"
                && entry.media_ready()
        })
    });
    let code_viewer = h.open_client();
    let code_auth = authenticate(
        &code_viewer,
        &h.room,
        "viewer",
        "mixed-code-viewer",
        1,
        "",
        Presence {
            code_only: true,
            display_name: "房间号观众".into(),
            ..Default::default()
        },
    );
    next_viewer_presence_matching(&host, |message| message.viewers().len() == 2);
    h.update_room_access(RoomAccessUpdateRequest::RotateViewerGrant);
    grant_viewer.next("viewer-grant-revoked");
    expect_close_code(&grant_viewer, 4004);
    commit_prepared_route(&code_viewer);
    let remaining = next_viewer_presence_matching(&host, |message| {
        let entries = message.viewers();
        entries.len() == 1
            && entries[0].peer_id() == code_auth.peer_id()
            && entries[0].upstream_kind() == "peer"
    });
    expect_equal(
        &remaining.viewers_json(),
        &json!([{"role": "viewer", "peerId": code_auth.peer_id(), "displayName": "房间号观众", "upstream": {"kind": "peer", "peerId": host_auth.peer_id()}, "mediaReady": true}]).to_string(),
    );
    assert!(!code_viewer.is_closed(), "code viewer must stay open");
    assert!(
        h.locked(|core| core
            .store
            .get_connected_viewer(&h.room.room_id, &code_auth.peer_id())
            .is_some()),
        "code viewer must stay connected"
    );
    h.close_client(&code_viewer);
    h.close_client(&host);
}

// ---------------------------------------------------------------------------
// site access at upgrade
// ---------------------------------------------------------------------------

#[test]
fn signal_requires_site_access_for_code_only_viewers_while_accepting_room_grants() {
    let h = Harness::start(HarnessOptions {
        site_access_password: true,
        authentication_timeout_ms: PASSWORD_AUTHENTICATION_TIMEOUT_MS,
        ..Default::default()
    });
    let viewer = h.open_client();
    send_code_only_authenticate(
        &viewer,
        &h.room.room_id,
        "code-only-viewer-without-site-access",
        Value::Null,
    );
    expect_match(&viewer.next("error").raw, r#"{"code":"INVALID_TOKEN"}"#);
    let granted_viewer = h.open_client();
    let granted = authenticate(
        &granted_viewer,
        &h.room,
        "viewer",
        "grant-viewer-without-site-access",
        1,
        "",
        presence(),
    );
    assert_eq!(granted.role(), "viewer");
    let unauthorized_host = h.open_client();
    unauthorized_host.send_json(json!({
        "type": "authenticate",
        "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": h.room.room_id,
        "role": "host",
        "token": h.room.host_token,
        "clientId": "host-client-without-admission",
    }));
    expect_match(
        &unauthorized_host.next("error").raw,
        r#"{"code":"AUTH_REQUIRED"}"#,
    );
    let with_cookie = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    let admitted = authenticate(
        &with_cookie,
        &h.room,
        "viewer",
        "viewer-with-host-cookie",
        1,
        "",
        code_only(),
    );
    assert_eq!(admitted.role(), "viewer");
    let public_room = h.create_room(CodeEntryPolicy::Open, "");
    let public_without = h.open_client();
    send_code_only_authenticate(
        &public_without,
        &public_room.room_id,
        "public-viewer-without-site-access",
        Value::Null,
    );
    expect_match(
        &public_without.next("error").raw,
        r#"{"code":"INVALID_TOKEN"}"#,
    );
    let forged = h.open_client();
    send_code_only_authenticate(
        &forged,
        &public_room.room_id,
        "public-viewer-with-forged-grant",
        json!({"viewerGrant": format!("{}g", "x".repeat(21))}),
    );
    expect_match(&forged.next("error").raw, r#"{"code":"INVALID_TOKEN"}"#);
    let public_with = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    let public_admitted = authenticate(
        &public_with,
        &public_room,
        "viewer",
        "public-viewer-with-site-access",
        1,
        "",
        code_only(),
    );
    assert_eq!(public_admitted.role(), "viewer");
    let password_room = h.create_room(CodeEntryPolicy::Private, "room-password");
    let password_without = h.open_client();
    send_code_only_authenticate(
        &password_without,
        &password_room.room_id,
        "password-viewer-without-site-access",
        json!({"viewerPassword": "room-password"}),
    );
    expect_match(
        &password_without.next_within("error", PASSWORD_WAIT).raw,
        r#"{"code":"INVALID_TOKEN"}"#,
    );
    let password_with = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    send_password_authenticate(
        &password_with,
        &password_room,
        "password-viewer-with-site-access",
        "room-password",
    );
    assert_eq!(
        password_with
            .next_within("authenticated", PASSWORD_WAIT)
            .json()["role"],
        "viewer"
    );
    let host = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "host-client-protected",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    assert_eq!(host_auth.role(), "host");
}

#[test]
fn signal_distinguishes_missing_rooms_from_existing_room_code_only_denials() {
    let clock = TestClock::new(utc_millis(2026, 8, 24, 12));
    let h = Harness::start(HarnessOptions {
        site_access_password: true,
        max_viewers_per_room: 1,
        now: Some(clock.clock()),
        authentication_timeout_ms: PASSWORD_AUTHENTICATION_TIMEOUT_MS,
        ..Default::default()
    });
    let protected_room = h.create_room(CodeEntryPolicy::Private, "correct-password");
    let abandoned = h.create_room(CodeEntryPolicy::Open, "");
    let unused_room_id = (1_000..10_000)
        .map(|code: i32| code.to_string())
        .find(|candidate| {
            *candidate != h.room.room_id
                && *candidate != protected_room.room_id
                && *candidate != abandoned.room_id
        })
        .expect("unused room id");
    let admitted = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    authenticate(
        &admitted,
        &h.room,
        "viewer",
        "admitted-viewer",
        1,
        "",
        code_only(),
    );
    let expect_denial = |room_id: &str, client_id: &str, code: &str, password: &str| {
        let client = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
        let extra = if password.is_empty() {
            Value::Null
        } else {
            json!({"viewerPassword": password})
        };
        send_code_only_authenticate(&client, room_id, client_id, extra);
        let message = if code == "ROOM_NOT_FOUND" {
            "Room not found"
        } else {
            "Room access denied"
        };
        expect_equal(
            &client.next_within("error", PASSWORD_WAIT).raw,
            &json!({"type": "error", "code": code, "message": message}).to_string(),
        );
        expect_close(&client, 4003, "Authentication failed");
    };
    expect_denial(&unused_room_id, "unknown-room-viewer", "ROOM_NOT_FOUND", "");
    expect_denial(
        &protected_room.room_id,
        "missing-password-viewer",
        "ROOM_ACCESS_DENIED",
        "",
    );
    expect_denial(
        &protected_room.room_id,
        "wrong-password-viewer",
        "ROOM_ACCESS_DENIED",
        "wrong-password",
    );
    expect_denial(
        &h.room.room_id,
        "full-room-viewer",
        "ROOM_ACCESS_DENIED",
        "",
    );
    h.locked(|core| {
        core.store
            .abandon_room(&abandoned.room_id)
            .expect("abandon room")
    });
    expect_denial(
        &abandoned.room_id,
        "abandoned-room-viewer",
        "ROOM_NOT_FOUND",
        "",
    );
    let abandoned_grant = h.open_client_with_cookie(SITE_ACCESS_COOKIE);
    send_code_only_authenticate(
        &abandoned_grant,
        &abandoned.room_id,
        "abandoned-grant-viewer",
        json!({"viewerGrant": abandoned.viewer_grant}),
    );
    expect_match(
        &abandoned_grant.next("error").raw,
        r#"{"code":"INVALID_TOKEN"}"#,
    );
}

// ---------------------------------------------------------------------------
// room lifetime
// ---------------------------------------------------------------------------
