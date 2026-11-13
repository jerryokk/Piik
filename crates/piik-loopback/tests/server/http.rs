//! Health, presentation and quality-preference HTTP surface.

use super::*;

#[tokio::test]
async fn health_is_origin_bound() {
    let server = start_with_origin(TEST_ORIGIN);
    let host = &server.endpoint().host;
    let response = health(&server, host, TEST_ORIGIN).await;
    assert_eq!(response.status(), StatusCode::OK);
    response.json::<Value>().await.unwrap();
    let denied = health(&server, host, &format!("{TEST_ORIGIN}.other")).await;
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn start_serves_health_and_strict_control_handshake() {
    let expected = NativeMediaCapabilities {
        video: true,
        process_audio: false,
        system_audio: true,
        hardware_h264: true,
        max_relay_children: 20,
        ..NativeMediaCapabilities::default()
    };
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        native_media: expected,
        ..Options::default()
    });
    let endpoint = server.endpoint().clone();
    let response = health(&server, &endpoint.host, TEST_ORIGIN).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        TEST_ORIGIN
    );
    let raw: Value = response.json().await.unwrap();
    // Optional zero facts are omitted; feature-level fields are always present.
    assert_eq!(
        raw["nativeMedia"],
        json!({
            "maxRelayChildren": 20, "receiverReuse": false, "remoteCoPlay": false,
            "captureBorderControl": false, "shareVolume": false,
            "sourceCancellation": false, "captureFailureDetails": false,
            "video": true, "processAudio": false,
            "systemAudio": true, "hardwareH264": true, "softwareVP8": false,
        })
    );
    assert!(raw.get("qualityPreference").is_none());
    let health: Health = serde_json::from_value(raw).unwrap();
    assert_eq!(
        health,
        Health {
            version: buildinfo::version().to_owned(),
            revision: buildinfo::REVISION.to_owned(),
            package_target: buildinfo::package_target(),
            protocol: PROTOCOL_VERSION,
            service: SERVICE_NAME.to_owned(),
            port: endpoint.port,
            instance_token: endpoint.instance_token.clone(),
            native_media: expected,
        }
    );

    let mut socket = dial(&server, TEST_ORIGIN).await;
    let ready = hello(&mut socket).await;
    assert_eq!(
        (ready.kind.as_str(), ready.id.as_str()),
        ("ready", "request_hello")
    );
    write(&mut socket, "request_ping", "ping").await;
    let pong = read(&mut socket).await;
    assert_eq!(
        (pong.kind.as_str(), pong.id.as_str()),
        ("pong", "request_ping")
    );
}

#[tokio::test]
async fn health_rejects_unexpected_origin_and_host() {
    let server = start_with_origin(TEST_ORIGIN);
    let endpoint = server.endpoint();
    let response = health(&server, &endpoint.host, "https://other.example").await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = health(
        &server,
        &format!("127.0.0.2:{}", endpoint.port),
        TEST_ORIGIN,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn health_accepts_the_local_host_alias() {
    let server = start_with_origin(TEST_ORIGIN);
    let port = server.endpoint().port;
    let response = health(
        &server,
        &format!("localhost:{port}"),
        &format!("http://localhost:{port}"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn presentation_updates_only_language_without_claiming_media_control() {
    let (languages, mut received) = mpsc::unbounded_channel::<String>();
    let slot: Arc<Mutex<Option<Server>>> = Arc::default();
    let callback_slot = slot.clone();
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        presentation: Some(Arc::new(move |language: &str| {
            // Reentrant permission changes must not deadlock presentation.
            if let Some(server) = callback_slot.lock().unwrap().clone() {
                tokio::spawn(async move {
                    let _ = server.set_allowed_origins(&[TEST_ORIGIN.to_owned()]).await;
                });
            }
            languages.send(language.to_owned()).unwrap();
        })),
        ..Options::default()
    });
    *slot.lock().unwrap() = Some(server.clone());
    let endpoint = server.endpoint().clone();
    let mut socket = dial(&server, TEST_ORIGIN).await;
    hello(&mut socket).await;
    let long = format!("{}{{\"language\":\"en\"}}", " ".repeat(257));
    for (method, path, origin, body, language, status) in [
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            r#"{"language":"zh"}"#,
            "zh",
            204,
        ),
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            r#"{"language":"en"}"#,
            "en",
            204,
        ),
        (
            Method::POST,
            "/presentation",
            endpoint.url.as_str(),
            r#"{"language":"vis"}"#,
            "vis",
            204,
        ),
        (Method::OPTIONS, "/presentation", TEST_ORIGIN, "", "", 204),
        (
            Method::POST,
            "/presentation",
            "https://other.example",
            r#"{"language":"en"}"#,
            "",
            403,
        ),
        (
            Method::OPTIONS,
            "/presentation",
            "https://other.example",
            "",
            "",
            403,
        ),
        (Method::OPTIONS, "/health", TEST_ORIGIN, "", "", 204),
        (
            Method::OPTIONS,
            "/health",
            "https://other.example",
            "",
            "",
            403,
        ),
        (Method::GET, "/presentation", TEST_ORIGIN, "", "", 405),
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            r#"{"language":"other"}"#,
            "",
            400,
        ),
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            r#"{"language":"en","capture":true}"#,
            "",
            400,
        ),
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            r#"{"language":"en"}{}"#,
            "",
            400,
        ),
        (
            Method::POST,
            "/presentation",
            TEST_ORIGIN,
            long.as_str(),
            "",
            400,
        ),
    ] {
        let mut request = client()
            .request(method.clone(), format!("{}{path}", endpoint.url))
            .header("Origin", origin)
            .header("Content-Type", "application/json")
            .body(body.to_owned());
        if method == Method::OPTIONS {
            request = request.header("Access-Control-Request-Private-Network", "true");
        }
        let response = request.send().await.unwrap();
        assert_eq!(
            response.status().as_u16(),
            status,
            "{method} {path} {origin} {body}"
        );
        if method == Method::OPTIONS && status == 204 {
            let headers = response.headers();
            assert_eq!(headers["access-control-allow-origin"], TEST_ORIGIN);
            assert_eq!(headers["access-control-allow-private-network"], "true");
            if path == "/presentation" {
                assert_eq!(headers["access-control-allow-methods"], "POST");
                assert_eq!(headers["access-control-allow-headers"], "Content-Type");
            } else {
                assert_eq!(headers["access-control-allow-methods"], "GET");
            }
        }
        match received.try_recv() {
            Ok(value) => assert_eq!(value, language, "unexpected presentation callback"),
            Err(_) => assert!(language.is_empty(), "presentation callback missing"),
        }
    }
    write(&mut socket, "request_ping", "ping").await;
    assert_eq!(
        read(&mut socket).await.kind,
        "pong",
        "presentation changed control"
    );
}

#[tokio::test]
async fn quality_preference_requires_current_origin_and_instance_without_claiming_control() {
    let directory = tempfile::tempdir().unwrap();
    let (preference, result) = QualityPreference::load(&directory.path().join("client.json"));
    result.unwrap();
    let server = start(Options {
        allowed_origins: vec![TEST_ORIGIN.to_owned()],
        quality_preference: Some(Arc::new(preference)),
        max_control_sessions: 1,
        ..Options::default()
    });
    let endpoint = server.endpoint().clone();
    let payload = String::from_utf8(piik_protocol::encode(&DEFAULT_QUALITY_SETTINGS)).unwrap();
    let token = endpoint.instance_token.as_str();
    let padded = format!("{}{payload}", " ".repeat(1025));
    let trailing = format!("{payload}{{}}");
    for (method, origin, instance, body, status) in [
        (Method::OPTIONS, TEST_ORIGIN, "", "", 204),
        (Method::OPTIONS, "https://other.example", "", "", 403),
        (Method::GET, TEST_ORIGIN, "", "", 403),
        (
            Method::PUT,
            TEST_ORIGIN,
            "old-instance",
            payload.as_str(),
            403,
        ),
        (
            Method::PUT,
            "https://other.example",
            token,
            payload.as_str(),
            403,
        ),
        (Method::PUT, TEST_ORIGIN, token, "{}", 400),
        (Method::PUT, TEST_ORIGIN, token, "null", 400),
        (Method::PUT, TEST_ORIGIN, token, trailing.as_str(), 400),
        (Method::PUT, TEST_ORIGIN, token, padded.as_str(), 400),
        (Method::POST, TEST_ORIGIN, token, payload.as_str(), 405),
        (Method::PUT, TEST_ORIGIN, token, payload.as_str(), 204),
        (Method::GET, TEST_ORIGIN, token, "", 200),
    ] {
        let response = client()
            .request(
                method.clone(),
                format!("{}/quality-preference", endpoint.url),
            )
            .header("Origin", origin)
            .header("X-Piik-Instance", instance)
            .header("Access-Control-Request-Private-Network", "true")
            .body(body.to_owned())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            status,
            "{method} {origin} {instance}"
        );
        if method == Method::OPTIONS && status == 204 {
            assert_eq!(
                response.headers()["access-control-allow-methods"],
                "GET, PUT"
            );
            assert_eq!(
                response.headers()["access-control-allow-private-network"],
                "true"
            );
        }
        if status == 200 {
            let body = response.bytes().await.unwrap();
            let quality: QualitySettings = piik_protocol::decode(&body).unwrap();
            assert_eq!(quality, DEFAULT_QUALITY_SETTINGS);
        }
    }
    // The preference never claimed the single control slot.
    let mut socket = dial(&server, TEST_ORIGIN).await;
    assert_eq!(
        hello(&mut socket).await.kind,
        "ready",
        "preference claimed media control"
    );
}
