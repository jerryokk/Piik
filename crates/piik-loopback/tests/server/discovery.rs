//! Port binding, discovery and site-approval revocation.

use super::*;

#[tokio::test]
async fn start_always_binds_to_ipv4_loopback() {
    let server = start_with_origin(TEST_ORIGIN);
    let host = &server.endpoint().host;
    assert!(host.starts_with("127.0.0.1:"), "helper host = {host}");
}

#[tokio::test]
async fn start_uses_the_first_free_port_in_range() {
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let next = std::net::TcpListener::bind(("127.0.0.1", port + 1));
    if next.is_err() {
        return; // The neighbouring port belongs to another process.
    }
    drop(next);
    let server = Server::start(
        &CancellationToken::new(),
        Options {
            port_start: port,
            port_end: port + 1,
            ..Options::default()
        },
    )
    .unwrap();
    assert_eq!(server.endpoint().port, port + 1);
    let error = Server::start(
        &CancellationToken::new(),
        Options {
            port_start: port,
            port_end: port,
            ..Options::default()
        },
    )
    .err()
    .expect("busy range was bound");
    assert!(error.to_string().contains(&format!("{port}-{port}")));
    assert!(
        Server::start(
            &CancellationToken::new(),
            Options {
                max_control_sessions: 3,
                ..Options::default()
            }
        )
        .is_err()
    );
}

#[tokio::test]
async fn connect_discovery_does_not_grant_native_access() {
    let server = start_with_origin("");
    server.set_connect_url("http://127.0.0.1:18990/client");
    let endpoint = server.endpoint().clone();
    let with_path = format!("{TEST_ORIGIN}/path");
    for (method, path, origin, status) in [
        (Method::GET, "/connect-discovery", TEST_ORIGIN, 200),
        (Method::OPTIONS, "/connect-discovery", TEST_ORIGIN, 204),
        (Method::POST, "/connect-discovery", TEST_ORIGIN, 405),
        (Method::GET, "/connect-discovery", "null", 403),
        (Method::GET, "/connect-discovery", "", 403),
        (Method::GET, "/connect-discovery", with_path.as_str(), 403),
        (Method::GET, "/health", TEST_ORIGIN, 403),
        (Method::GET, "/control", TEST_ORIGIN, 403),
        (Method::GET, "/quality-preference", TEST_ORIGIN, 403),
    ] {
        let response = client()
            .request(method.clone(), format!("{}{path}", endpoint.url))
            .header("Origin", origin)
            .header("Access-Control-Request-Private-Network", "true")
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status().as_u16(),
            status,
            "{method} {path} {origin}"
        );
        if status == 200 {
            let body: serde_json::Map<String, Value> = response.json().await.unwrap();
            assert_eq!(
                body.len(),
                5,
                "discovery exposed more than identity: {body:?}"
            );
            assert_eq!(body["connectURL"], "http://127.0.0.1:18990/client");
            assert_eq!(body["instanceToken"], endpoint.instance_token.as_str());
            assert_eq!(body["service"], SERVICE_NAME);
            assert_eq!(body["protocol"], PROTOCOL_VERSION);
            assert_eq!(body["port"], endpoint.port);
        }
    }
    server.set_connect_url("");
    let response = client()
        .get(format!("{}/connect-discovery", endpoint.url))
        .header("Origin", TEST_ORIGIN)
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "hidden discovery answered"
    );
}

#[tokio::test]
async fn site_revocation_preserves_unrelated_sessions() {
    for (other_origin, remove) in [
        ("", false),
        ("https://keep.example", false),
        ("https://keep.example", true),
    ] {
        let finish = CancellationToken::new();
        let _finish_on_exit = finish.clone().drop_guard();
        let (factory, mut sessions_created) = recording_factory(Some(finish.clone()));
        let server = start(Options {
            allowed_origins: vec![TEST_ORIGIN.to_owned(), "https://keep.example".to_owned()],
            new_control: Some(factory),
            ..Options::default()
        });
        let mut old = dial(&server, TEST_ORIGIN).await;
        hello(&mut old).await;
        let (old_session, _) = created(&mut sessions_created).await;
        let other_origin = if other_origin.is_empty() {
            server.endpoint().url.clone()
        } else {
            other_origin.to_owned()
        };
        let mut local = dial(&server, &other_origin).await;
        hello(&mut local).await;
        let (local_session, _) = created(&mut sessions_created).await;

        let changing = server.clone();
        let mut done = tokio::spawn(async move {
            if remove {
                changing.remove_allowed_origin(TEST_ORIGIN).await
            } else {
                changing
                    .set_allowed_origins(&[
                        "https://next.example".to_owned(),
                        "https://keep.example".to_owned(),
                    ])
                    .await
            }
        });
        assert!(
            signalled(&old_session.closed).await,
            "revoked session did not retire"
        );
        assert!(
            !done.is_finished(),
            "permission reported ready before cleanup"
        );
        assert!(
            !local_session.closed.is_cancelled(),
            "unrelated session was closed"
        );
        write(&mut local, "local_ping", "ping").await;
        assert_eq!(
            read(&mut local).await.kind,
            "pong",
            "retained session was interrupted"
        );
        // A stuck media owner must not hang consent forever or restore old permission.
        let result = tokio::time::timeout(SECOND * 3, &mut done)
            .await
            .expect("permission retirement was unbounded")
            .unwrap();
        assert!(result.is_err(), "stuck cleanup was reported as successful");
        let response = health(&server, &server.endpoint().host, TEST_ORIGIN).await;
        assert_eq!(
            response.status(),
            StatusCode::FORBIDDEN,
            "timeout restored the origin"
        );
        drop(old);
    }
}
