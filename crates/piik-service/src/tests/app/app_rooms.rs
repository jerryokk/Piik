//! Room creation, replacement and access mutations.

use super::*;

#[tokio::test]
async fn room_creation_requires_a_logged_in_browser() {
    let h = start(options(test_config())).await;
    h.create_room(RoomRequest::default())
        .await
        .expect(401, r#"{"error":"Unauthorized"}"#)
        .expect_header("www-authenticate", "");
    // The site password as a bearer token is not a browser session.
    h.send(
        Req::post("/api/rooms")
            .origin(ALLOWED_ORIGIN)
            .bearer(TEST_ACCESS_PASSWORD)
            .json(r#"{"codeEntryPolicy":"open"}"#),
    )
    .await
    .expect(401, r#"{"error":"Unauthorized"}"#);
    h.close().await;
}

#[tokio::test]
async fn room_creation_issues_an_independent_fragment_only_viewer_grant() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    let reply = h
        .create_room(RoomRequest {
            cookie: &cookie,
            ..Default::default()
        })
        .await;
    reply
        .expect_status(201)
        .expect_header("cache-control", "no-store")
        .expect_header("x-content-type-options", "nosniff");
    let created = reply.room();
    assert!(
        piik_protocol::valid_room_code(&created.room_id),
        "{}",
        created.room_id
    );
    let invite = Url::parse(&created.invite_url).expect("invite URL");
    assert_eq!(piik_config::origin(&invite), "https://share.example.test");
    assert_eq!(invite.path(), format!("/r/{}", created.room_id));
    let grant = invite
        .fragment()
        .and_then(|fragment| fragment.strip_prefix("v="))
        .unwrap_or_default();
    assert!(
        grant.len() == 22
            && grant
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
            && grant.ends_with(['A', 'Q', 'g', 'w']),
        "invite fragment {:?}",
        invite.fragment()
    );
    assert_eq!(invite.query(), None);
    assert_eq!(created.code_entry_policy, "open");
    // The response carries only room authority and no ICE configuration.
    let raw = reply.json();
    for absent in ["iceConfig", "viewerGrant"] {
        assert!(raw.get(absent).is_none(), "response exposes {absent}");
    }
    h.close().await;
}

#[test]
fn room_creation_passes_the_shared_viewer_ceiling_to_room_admission() {
    let mut config = test_config();
    config.max_viewers_per_room = piik_protocol::MAX_VIEWERS_PER_ROOM_LIMIT;
    let store = build_store(&config, None).expect("store");
    assert_eq!(
        store.max_viewers_per_room(),
        piik_protocol::MAX_VIEWERS_PER_ROOM_LIMIT
    );
}

#[tokio::test]
async fn room_creation_creates_private_rooms_with_optional_passwords() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    for password in [Some("room-password"), None] {
        let created = h
            .create_room(RoomRequest {
                cookie: &cookie,
                code_entry_policy: "private",
                password,
                ..Default::default()
            })
            .await;
        assert_eq!(
            created.expect_status(201).room().code_entry_policy,
            "private"
        );
    }
    h.close().await;
}

#[tokio::test]
async fn production_allows_room_creation_without_site_access_but_requires_ownership() {
    let mut config = test_config();
    config.env = Environment::Production;
    config.site_access_password = String::new();
    let h = start(options(config)).await;
    let created = h
        .create_room(RoomRequest::default())
        .await
        .expect_status(201)
        .room();
    assert_eq!(created.code_entry_policy, "open");
    assert!(created.invite_url.contains("#v="), "{}", created.invite_url);
    let private = r#"{"action":"set-code-entry-policy","policy":"private"}"#;
    h.update_access(&created.room_id, "wrong-token", private, "")
        .await
        .expect_status(404);
    h.update_access(&created.room_id, &created.host_token, private, "")
        .await
        .expect_status(200);
    h.close().await;
}

#[tokio::test]
async fn room_creation_allocates_unique_codes_concurrently() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    let request = || {
        h.create_room(RoomRequest {
            cookie: &cookie,
            ..Default::default()
        })
    };
    let (first, second) = tokio::join!(request(), request());
    let (first, second) = (
        first.expect_status(201).room(),
        second.expect_status(201).room(),
    );
    assert_ne!(first.room_id, second.room_id);
    h.close().await;
}

#[tokio::test]
async fn room_creation_reuses_a_free_preferred_code_and_never_replaces_an_occupied_room() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    let preferred = || {
        h.create_room(RoomRequest {
            cookie: &cookie,
            preferred_room_id: "4321",
            ..Default::default()
        })
    };
    let first = preferred().await.expect_status(201).room();
    let fallback = preferred().await.expect_status(201).room();
    assert_eq!(first.room_id, "4321");
    assert_ne!(
        fallback.room_id, "4321",
        "the occupied preferred code was handed out twice"
    );
    assert_ne!(fallback.host_token, first.host_token);
    h.close().await;
}

async fn participant(
    h: &Harness,
    cookie: &str,
    authenticate: serde_json::Value,
) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let mut request = format!("ws://127.0.0.1:{}/signal", h.port)
        .into_client_request()
        .expect("upgrade request");
    request
        .headers_mut()
        .insert("origin", ALLOWED_ORIGIN.parse().expect("origin"));
    request
        .headers_mut()
        .insert("cookie", cookie.parse().expect("cookie"));
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("upgrade /signal");
    socket
        .send(Message::text(authenticate.to_string()))
        .await
        .expect("authenticate");
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("authenticated in time")
            .expect("open socket")
            .expect("frame");
        if let Message::Text(text) = frame
            && serde_json::from_str::<serde_json::Value>(&text).expect("server message")["type"]
                == "authenticated"
        {
            return socket;
        }
    }
}

async fn closed(
    socket: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) {
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(frame) = socket.next().await {
            if matches!(frame, Ok(Message::Close(_)) | Err(_)) {
                return;
            }
        }
    });
    ended.await.expect("the old membership was not closed");
}

#[tokio::test]
async fn room_replacement_replaces_authority_and_closes_old_membership() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    let original = h
        .create_room(RoomRequest {
            cookie: &cookie,
            code_entry_policy: "private",
            password: Some("old-password"),
            preferred_room_id: "4321",
        })
        .await
        .expect_status(201)
        .room();
    let protocol = piik_protocol::SIGNALING_PROTOCOL;
    let mut host = participant(
        &h,
        &cookie,
        serde_json::json!({"type": "authenticate", "protocol": protocol, "roomId": original.room_id,
            "role": "host", "clientId": "host_client_1234", "token": original.host_token}),
    )
    .await;
    let invite = Url::parse(&original.invite_url).expect("invite");
    let grant = invite
        .fragment()
        .unwrap_or_default()
        .trim_start_matches("v=")
        .to_owned();
    let mut viewer = participant(
        &h,
        &cookie,
        serde_json::json!({"type": "authenticate", "protocol": protocol, "roomId": original.room_id,
            "role": "viewer", "clientId": "viewer_client_1234", "viewerGrant": grant}),
    )
    .await;

    h.replace_room(
        &original.room_id,
        &original.host_token,
        r#"{"codeEntryPolicy":"open"}"#,
        "",
    )
    .await
    .expect(401, r#"{"error":"Unauthorized"}"#);
    let replacement = h
        .replace_room(
            &original.room_id,
            &original.host_token,
            r#"{"codeEntryPolicy":"private","roomPassword":"new-password"}"#,
            &cookie,
        )
        .await
        .expect_status(201)
        .room();
    assert_ne!(
        replacement.room_id, original.room_id,
        "the replacement kept the old room code"
    );
    assert_eq!(replacement.code_entry_policy, "private");
    closed(&mut host).await;
    closed(&mut viewer).await;
    // The old Host authority is gone with its room.
    h.update_access(
        &original.room_id,
        &original.host_token,
        r#"{"action":"rotate-viewer-grant"}"#,
        &cookie,
    )
    .await
    .expect(404, r#"{"error":"Room not found"}"#);
    h.close().await;
}

#[test]
fn room_mutation_failures_map_to_the_http_table() {
    use crate::routes::{access_failure, creation_failure, replacement_failure};
    use piik_room::{Error, ErrorCode as C};
    let other = || Error::Invalid("storage failed".to_owned());
    let room = |code| Error::Room(code);
    type Mapper = fn(&Error) -> Option<(axum::http::StatusCode, &'static str)>;
    let replacement: Mapper = |error| replacement_failure(error.code());
    let access: Mapper = access_failure;
    let creation: Mapper = |error| creation_failure(error.code());
    for (name, mapper, error, want) in [
        (
            "replacement limit",
            replacement,
            room(C::RoomLimit),
            Some((503, "Room capacity reached")),
        ),
        (
            "replacement denied",
            replacement,
            room(C::RoomAccessDenied),
            Some((503, "Room replacement unavailable")),
        ),
        (
            "replacement busy",
            replacement,
            room(C::RoomBusy),
            Some((503, "Room replacement unavailable")),
        ),
        (
            "replacement token",
            replacement,
            room(C::InvalidToken),
            Some((404, "Room not found")),
        ),
        (
            "replacement missing",
            replacement,
            room(C::RoomNotFound),
            Some((404, "Room not found")),
        ),
        ("replacement full", replacement, room(C::RoomFull), None),
        ("replacement other", replacement, other(), None),
        (
            "access busy",
            access,
            room(C::RoomBusy),
            Some((503, "Room access update unavailable")),
        ),
        (
            "access denied",
            access,
            room(C::RoomAccessDenied),
            Some((409, "Room access update rejected")),
        ),
        (
            "access token",
            access,
            room(C::InvalidToken),
            Some((404, "Room not found")),
        ),
        (
            "access limit",
            access,
            room(C::RoomLimit),
            Some((404, "Room not found")),
        ),
        ("access other", access, other(), None),
        (
            "creation limit",
            creation,
            room(C::RoomLimit),
            Some((503, "Room capacity reached")),
        ),
        (
            "creation busy",
            creation,
            room(C::RoomBusy),
            Some((503, "Room creation unavailable")),
        ),
        (
            "creation token",
            creation,
            room(C::InvalidToken),
            Some((400, "Invalid room request")),
        ),
        ("creation denied", creation, room(C::RoomAccessDenied), None),
        ("creation other", creation, other(), None),
    ] {
        let got = mapper(&error).map(|(status, message)| (status.as_u16(), message));
        assert_eq!(got, want, "{name}");
    }
}

#[tokio::test]
async fn room_access_manages_a_dormant_room_without_starting_sharing() {
    let now = Arc::new(AtomicI64::new(0));
    let mut options = options(test_config());
    options.now = Some(clock(&now));
    let h = start(options).await;
    let cookie = h.cookie().await;
    let created = h
        .create_room(RoomRequest {
            cookie: &cookie,
            ..Default::default()
        })
        .await
        .expect_status(201)
        .room();
    now.store(900, Ordering::SeqCst);
    let update = |body: &'static str, cookie: String| {
        let (h, created) = (&h, &created);
        async move {
            h.update_access(&created.room_id, &created.host_token, body, &cookie)
                .await
        }
    };
    update(r#"{"action":"set-code-entry-policy","policy":"private"}"#, cookie.clone())
        .await
        .expect(
            200,
            r#"{"type":"code-entry-policy-updated","codeEntryPolicy":"private","viewerPasswordEnabled":false}"#,
        )
        .expect_header("cache-control", "no-store");
    update(
        r#"{"action":"set-viewer-password","password":"room-password"}"#,
        cookie.clone(),
    )
    .await
    .expect(200, r#"{"type":"viewer-password-updated","enabled":true}"#);
    update(
        r#"{"action":"set-viewer-password","password":null}"#,
        cookie.clone(),
    )
    .await
    .expect(200, r#"{"type":"viewer-password-updated","enabled":false}"#);

    let rotated = update(r#"{"action":"rotate-viewer-grant"}"#, cookie.clone()).await;
    let rotated = rotated.expect_status(200).json();
    assert_eq!(rotated["type"], "viewer-grant-updated");
    let invite = rotated["inviteUrl"].as_str().expect("inviteUrl");
    let grant = invite
        .rsplit_once("#v=")
        .map(|(_, grant)| grant)
        .unwrap_or_default();
    assert!(
        grant.len() == 22
            && grant
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'),
        "rotated inviteUrl {invite}"
    );

    let revoked = update(r#"{"action":"revoke-viewer-grant"}"#, cookie.clone()).await;
    revoked.expect_status(200);
    assert!(
        revoked.body.contains(r#""inviteUrl":null"#)
            && revoked.body.contains(r#""type":"viewer-grant-updated""#)
    );

    now.store(30 * 24 * 60 * 60 * 1_000, Ordering::SeqCst);
    let fresh = h.cookie().await;
    update(
        r#"{"action":"set-code-entry-policy","policy":"open"}"#,
        fresh,
    )
    .await
    .expect_status(200);
    h.close().await;
}

#[tokio::test]
async fn room_access_requires_same_origin_site_access_and_the_exact_host_token() {
    let h = start(options(test_config())).await;
    let cookie = h.cookie().await;
    let room = || {
        h.create_room(RoomRequest {
            cookie: &cookie,
            ..Default::default()
        })
    };
    let first = room().await.expect_status(201).room();
    let second = room().await.expect_status(201).room();
    let rotate = r#"{"action":"rotate-viewer-grant"}"#;
    let path = format!("/api/rooms/{}/access", first.room_id);

    h.update_access(&first.room_id, &first.host_token, rotate, "")
        .await
        .expect(401, r#"{"error":"Unauthorized"}"#);
    h.send(
        Req::post(&path)
            .origin("https://foreign.test")
            .cookie(&cookie)
            .bearer(&first.host_token)
            .json(rotate),
    )
    .await
    .expect(403, r#"{"error":"Origin not allowed"}"#);
    for token in ["", "wrong-token", second.host_token.as_str()] {
        h.update_access(&first.room_id, token, rotate, &cookie)
            .await
            .expect(404, r#"{"error":"Room not found"}"#);
    }
    h.update_access(
        &first.room_id,
        &first.host_token,
        r#"{"action":"unknown"}"#,
        &cookie,
    )
    .await
    .expect(400, r#"{"error":"Invalid room access request"}"#);
    h.send(
        Req::get(&path)
            .origin(ALLOWED_ORIGIN)
            .cookie(&cookie)
            .bearer(&first.host_token),
    )
    .await
    .expect(405, r#"{"error":"Method not allowed"}"#)
    .expect_header("allow", "POST");
    h.close().await;
}

#[tokio::test]
async fn room_creation_rejects_malformed_requests_and_foreign_origins() {
    let mut config = test_config();
    config.site_access_password = String::new();
    let h = start(options(config)).await;
    let invalid = r#"{"error":"Invalid room request"}"#;
    h.send(Req::post("/api/rooms").origin(ALLOWED_ORIGIN).json("{}"))
        .await
        .expect(400, invalid);
    h.send(
        Req::post("/api/rooms")
            .origin("https://foreign.test")
            .json(r#"{"codeEntryPolicy":"open"}"#),
    )
    .await
    .expect(403, r#"{"error":"Origin not allowed"}"#);
    // An oversized or non-JSON body is the same 400.
    let oversized = format!(
        r#"{{"codeEntryPolicy":"open","preferredRoomId":"{}"}}"#,
        "1".repeat(1_024)
    );
    h.send(
        Req::post("/api/rooms")
            .origin(ALLOWED_ORIGIN)
            .json(&oversized),
    )
    .await
    .expect(400, invalid);
    h.send(
        Req::post("/api/rooms")
            .origin(ALLOWED_ORIGIN)
            .body("text/plain", r#"{"codeEntryPolicy":"open"}"#),
    )
    .await
    .expect(400, invalid);
    h.send(Req::get("/api/rooms").origin(ALLOWED_ORIGIN))
        .await
        .expect(405, r#"{"error":"Method not allowed"}"#)
        .expect_header("allow", "POST");
    h.close().await;
}

#[tokio::test]
async fn room_creation_behind_a_proxy_uses_the_configured_browser_origin() {
    for (name, public_url, origins, browser, status) in [
        (
            "public default",
            "https://share.example.test",
            "",
            "https://share.example.test",
            201,
        ),
        (
            "normalized default port",
            "https://share.example.test:443",
            "",
            "https://share.example.test",
            201,
        ),
        (
            "stale localhost override",
            "https://share.example.test",
            "http://localhost:8787",
            "https://share.example.test",
            403,
        ),
        (
            "explicit alternate origin",
            "https://share.example.test",
            "https://alternate.test",
            "https://alternate.test",
            201,
        ),
        (
            "different public port",
            "https://share.example.test:8443",
            "",
            "https://share.example.test",
            403,
        ),
        ("missing origin", "https://share.example.test", "", "", 403),
        (
            "foreign origin",
            "https://share.example.test",
            "",
            "https://foreign.test",
            403,
        ),
    ] {
        let env = HashMap::from([
            ("PUBLIC_BASE_URL".to_owned(), public_url.to_owned()),
            ("ALLOWED_ORIGINS".to_owned(), origins.to_owned()),
            ("ROOM_DATABASE_PATH".to_owned(), ":memory:".to_owned()),
            ("LISTEN_HOST".to_owned(), "127.0.0.1".to_owned()),
        ]);
        let mut config = piik_config::load(&env).unwrap_or_else(|error| panic!("{name}: {error}"));
        config.port = 0;
        let h = start(options(config)).await;
        let proxy = |request: Req| {
            request
                .host("127.0.0.1:8787")
                .header("X-Forwarded-Host", "share.example.test")
                .header("X-Forwarded-Proto", "https")
        };
        h.send(proxy(Req::get("/api/site-access")))
            .await
            .expect(200, r#"{"required":false,"authenticated":true}"#);
        let reply = h
            .send(proxy(
                Req::post("/api/rooms")
                    .origin(browser)
                    .json(r#"{"codeEntryPolicy":"open"}"#),
            ))
            .await;
        assert_eq!(reply.status, status, "{name}: {}", reply.body);
        if status == 403 {
            reply.expect(403, r#"{"error":"Origin not allowed"}"#);
        }
        h.close().await;
    }
}

#[tokio::test]
async fn room_creation_returns_service_unavailable_at_the_global_room_bound() {
    let mut config = test_config();
    config.site_access_password = String::new();
    let mut options = options(config);
    options.room_store = Some(
        piik_room::Store::new(piik_room::Options {
            max_rooms: 1,
            max_viewers_per_room: 8,
            ..Default::default()
        })
        .expect("store"),
    );
    let h = start(options).await;
    h.create_room(RoomRequest::default())
        .await
        .expect_status(201);
    h.create_room(RoomRequest::default())
        .await
        .expect(503, r#"{"error":"Room capacity reached"}"#);
    h.close().await;
}
