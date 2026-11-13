//! Site access, origin checks and login cookies.

use super::*;

fn set_cookie_matches(set_cookie: &str, secure: bool) -> bool {
    let (name, suffix) = if secure {
        (
            "__Host-piik-site-access=v1.",
            "; Path=/; Max-Age=86400; HttpOnly; SameSite=Strict; Secure",
        )
    } else {
        (
            "piik-site-access=v1.",
            "; Path=/; Max-Age=86400; HttpOnly; SameSite=Strict",
        )
    };
    let Some(value) = set_cookie
        .strip_prefix(name)
        .and_then(|rest| rest.strip_suffix(suffix))
    else {
        return false;
    };
    let Some((expires, signature)) = value.split_once('.') else {
        return false;
    };
    !expires.is_empty()
        && expires.bytes().all(|byte| byte.is_ascii_digit())
        && signature.len() == 43
        && signature
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

#[tokio::test]
async fn site_access_reports_status_and_issues_a_stateless_day_long_cookie() {
    let h = start(options(test_config())).await;
    h.send(Req::get("/api/site-access"))
        .await
        .expect(200, r#"{"required":true,"authenticated":false}"#)
        .expect_header("cache-control", "no-store")
        .expect_header("set-cookie", "");
    h.send(
        Req::post("/api/site-access")
            .bearer("wrong-password")
            .origin(ALLOWED_ORIGIN),
    )
    .await
    .expect(401, r#"{"error":"Unauthorized"}"#)
    .expect_header("set-cookie", "")
    .expect_header("www-authenticate", "Bearer");

    let login = h.login().await;
    login.expect(200, r#"{"required":true,"authenticated":true}"#);
    let set_cookie = login.header("set-cookie");
    assert!(set_cookie_matches(&set_cookie, false), "{set_cookie}");
    assert!(!set_cookie.contains(TEST_ACCESS_PASSWORD));
    let cookie = cookie_pair(&set_cookie);

    h.send(
        Req::post("/api/site-access")
            .bearer("wrong-password")
            .cookie(&cookie)
            .origin(ALLOWED_ORIGIN),
    )
    .await
    .expect(401, r#"{"error":"Unauthorized"}"#)
    .expect_header("set-cookie", "");

    let status = h.send(Req::get("/api/site-access").cookie(&cookie)).await;
    status.expect(200, r#"{"required":true,"authenticated":true}"#);
    assert!(set_cookie_matches(&status.header("set-cookie"), false));
    h.close().await;
}

#[tokio::test]
async fn site_access_renews_an_authenticated_cookie_across_idle_deadlines() {
    let now = Arc::new(AtomicI64::new(1_000));
    let mut options = options(test_config());
    options.now = Some(clock(&now));
    options.site_access_ttl_seconds = 12;
    let h = start(options).await;
    let original = cookie_pair(&h.login().await.header("set-cookie"));

    now.store(7_000, Ordering::SeqCst);
    let renew = |cookie: String| {
        let h = &h;
        async move { h.send(Req::get("/api/site-access").cookie(&cookie)).await }
    };
    let first = cookie_pair(&renew(original.clone()).await.header("set-cookie"));
    assert_ne!(first, original);

    now.store(13_000, Ordering::SeqCst);
    renew(original.clone())
        .await
        .expect(200, r#"{"required":true,"authenticated":false}"#)
        .expect_header("set-cookie", "");
    let second = cookie_pair(&renew(first.clone()).await.header("set-cookie"));
    assert_ne!(second, first);

    now.store(20_000, Ordering::SeqCst);
    renew(first)
        .await
        .expect(200, r#"{"required":true,"authenticated":false}"#)
        .expect_header("set-cookie", "");
    let active = renew(second).await;
    active.expect(200, r#"{"required":true,"authenticated":true}"#);
    assert!(
        !active.header("set-cookie").is_empty(),
        "an active cookie was not renewed"
    );
    h.close().await;
}

#[tokio::test]
async fn site_access_uses_a_secure_host_cookie_for_production_https() {
    let mut config = test_config();
    config.env = Environment::Production;
    let h = start(options(config)).await;
    let set_cookie = h.login().await.expect_status(200).header("set-cookie");
    assert!(set_cookie_matches(&set_cookie, true), "{set_cookie}");
    assert!(!set_cookie.contains("Domain="));
    h.close().await;
}

#[tokio::test]
async fn local_password_uses_the_configured_destination_cookie_across_lan_and_public_link() {
    let config = piik_config::local(piik_config::LocalOptions {
        port: free_port(),
        public_address: "192.0.2.10".to_owned(),
        public_origin: "https://public.example.test".to_owned(),
        site_access_password: TEST_ACCESS_PASSWORD.to_owned(),
        ..Default::default()
    })
    .expect("local config");
    let lan_origin = format!("http://192.0.2.10:{}", config.port);
    let h = start(options(config)).await;
    for origin in [lan_origin.as_str(), "https://public.example.test"] {
        let destination = Url::parse(origin).expect("origin");
        let host = &destination[url::Position::BeforeHost..url::Position::AfterPort];
        let secure = destination.scheme() == "https";
        let login = h
            .send(
                Req::post("/api/site-access")
                    .host(host)
                    .origin(&lan_origin)
                    .bearer(TEST_ACCESS_PASSWORD),
            )
            .await;
        login.expect_status(200);
        let set_cookie = login.header("set-cookie");
        // Browser cookie rules: a __Host- cookie needs Secure, Path=/ and no
        // Domain; a Secure cookie is never stored for plain HTTP.
        assert_eq!(
            set_cookie.contains("; Secure"),
            secure,
            "{origin}: {set_cookie}"
        );
        assert_eq!(
            set_cookie.starts_with("__Host-"),
            secure,
            "{origin}: {set_cookie}"
        );
        assert!(set_cookie.contains("Path=/") && !set_cookie.contains("Domain="));
        let cookie = cookie_pair(&set_cookie);
        h.send(Req::get("/api/site-access").host(host).cookie(&cookie))
            .await
            .expect(200, r#"{"required":true,"authenticated":true}"#);
        h.send(
            Req::post("/api/rooms")
                .host(host)
                .origin(origin)
                .cookie(&cookie)
                .json(r#"{"codeEntryPolicy":"open"}"#),
        )
        .await
        .expect_status(201);

        let mut upgrade = axum::http::HeaderMap::new();
        upgrade.insert("host", host.parse().expect("host"));
        upgrade.insert("cookie", cookie.parse().expect("cookie"));
        upgrade.insert("x-forwarded-proto", "http".parse().expect("proto"));
        let access = &h.server.inner().access;
        assert!(
            crate::routes::site_access_at_upgrade(access, &upgrade),
            "{origin}: WebSocket admission does not use the same cookie policy"
        );
        if secure {
            let plain = cookie.trim_start_matches("__Host-");
            upgrade.insert("cookie", plain.parse().expect("cookie"));
            assert!(
                !crate::routes::site_access_at_upgrade(access, &upgrade),
                "public authority accepted the plain LAN cookie name"
            );
        }
    }
    h.close().await;
}

#[tokio::test]
async fn configured_origin_matches_browser_serialization() {
    for (configured, browser) in [
        (
            "https://bücher.example:443",
            "https://xn--bcher-kva.example",
        ),
        ("http://[0:0:0:0:0:0:0:1]:8787", "http://[::1]:8787"),
    ] {
        for explicit in [false, true] {
            let mut env = HashMap::from([
                ("PUBLIC_BASE_URL".to_owned(), configured.to_owned()),
                ("ROOM_DATABASE_PATH".to_owned(), ":memory:".to_owned()),
                ("STUN_URLS".to_owned(), String::new()),
            ]);
            if explicit {
                env.insert("ALLOWED_ORIGINS".to_owned(), configured.to_owned());
            }
            let mut config = piik_config::load(&env).expect("config");
            config.port = 0;
            config.listen_host = "127.0.0.1".to_owned();
            let h = start(options(config)).await;
            for origin in [
                browser.to_owned(),
                "https://foreign.test".to_owned(),
                format!("{browser}/path"),
            ] {
                for path in ["/api/rooms", "/signal"] {
                    let request = if path == "/signal" {
                        Req::get(path)
                            .origin(&origin)
                            .header("Connection", "Upgrade")
                            .header("Upgrade", "websocket")
                    } else {
                        Req::post(path)
                            .origin(&origin)
                            .json(r#"{"codeEntryPolicy":"open"}"#)
                    };
                    let want = match (origin == browser, path) {
                        (false, _) => 403,
                        (true, "/signal") => 400, // Missing WebSocket version, past origin admission.
                        (true, _) => 201,
                    };
                    let reply = h.send(request).await;
                    assert_eq!(
                        reply.status, want,
                        "{configured} {path} origin={origin}: {}",
                        reply.body
                    );
                }
            }
            h.close().await;
        }
    }
}

#[tokio::test]
async fn site_access_requires_an_allowed_origin_and_a_valid_login_body() {
    let h = start(options(test_config())).await;
    let forbidden = r#"{"error":"Origin not allowed"}"#;
    for origin in ["", "https://foreign.test", "http://allowed.test/path"] {
        h.send(
            Req::post("/api/site-access")
                .bearer(TEST_ACCESS_PASSWORD)
                .origin(origin),
        )
        .await
        .expect(403, forbidden);
    }
    let invalid = r#"{"error":"Invalid site access request"}"#;
    h.send(
        Req::post("/api/site-access")
            .bearer(TEST_ACCESS_PASSWORD)
            .origin(ALLOWED_ORIGIN)
            .body("text/plain;charset=UTF-8", "{}"),
    )
    .await
    .expect(400, invalid);
    let oversized = format!(
        r#"{{"password":"{}"}}"#,
        "x".repeat(crate::http::MAX_JSON_REQUEST_BYTES)
    );
    for payload in [
        "{",
        "{}",
        "null",
        "[]",
        r#"{"password":null}"#,
        r#"{"password":123}"#,
        r#"{"password":"wrong","extra":true}"#,
        r#"{"password":"wrong"} {}"#,
        oversized.as_str(),
    ] {
        h.send(
            Req::post("/api/site-access")
                .origin(ALLOWED_ORIGIN)
                .bearer(TEST_ACCESS_PASSWORD)
                .json(payload),
        )
        .await
        .expect(400, invalid);
    }
    h.send(
        Req::post("/api/site-access")
            .origin(ALLOWED_ORIGIN)
            .bearer(TEST_ACCESS_PASSWORD)
            .json(r#"{"password":"wrong"}"#),
    )
    .await
    .expect_status(401);
    h.send(Req::new("DELETE", "/api/site-access").origin(ALLOWED_ORIGIN))
        .await
        .expect(405, r#"{"error":"Method not allowed"}"#)
        .expect_header("allow", "GET, POST");
    h.close().await;
}

#[tokio::test]
async fn local_site_accepts_the_exact_password_through_json() {
    let long = "x".repeat(256);
    for password in ["x", "中文", " ", "  中文 +&  ", long.as_str()] {
        let mut config = piik_config::local(piik_config::LocalOptions {
            port: free_port(),
            public_address: "192.0.2.10".to_owned(),
            site_access_password: password.to_owned(),
            ..Default::default()
        })
        .expect("local config");
        // The public test address is not an installed interface.
        config.stun_listen_addresses = vec!["127.0.0.1:0".parse().expect("address")];
        let origin = piik_config::origin(&config.public_base_url);
        let h = start(options(config)).await;
        for wrong in [String::new(), format!("{password}!")] {
            let payload = serde_json::json!({ "password": wrong }).to_string();
            h.send(Req::post("/api/site-access").origin(&origin).json(&payload))
                .await
                .expect_status(401)
                .expect_header("set-cookie", "");
        }
        let payload = serde_json::json!({ "password": password }).to_string();
        let login = h
            .send(Req::post("/api/site-access").origin(&origin).json(&payload))
            .await;
        login.expect(200, r#"{"required":true,"authenticated":true}"#);
        let cookie = cookie_pair(&login.header("set-cookie"));
        h.send(Req::get("/api/site-access").cookie(&cookie))
            .await
            .expect(200, r#"{"required":true,"authenticated":true}"#);
        h.send(
            Req::post("/api/rooms")
                .origin(&origin)
                .cookie(&cookie)
                .json(r#"{"codeEntryPolicy":"open"}"#),
        )
        .await
        .expect_status(201);
        h.close().await;
    }
}

#[tokio::test]
async fn site_access_rejects_an_expired_or_modified_cookie() {
    let now = Arc::new(AtomicI64::new(1_000));
    let mut options = options(test_config());
    options.now = Some(clock(&now));
    options.site_access_ttl_seconds = 1;
    let h = start(options).await;
    let cookie = cookie_pair(&h.login().await.header("set-cookie"));
    h.send(Req::get("/api/site-access").cookie(&format!("{cookie}x")))
        .await
        .expect(200, r#"{"required":true,"authenticated":false}"#);
    now.store(2_000, Ordering::SeqCst);
    h.send(Req::get("/api/site-access").cookie(&cookie))
        .await
        .expect(200, r#"{"required":true,"authenticated":false}"#);
    h.close().await;
}

#[tokio::test]
async fn site_access_is_immediate_without_a_password() {
    for env in [Environment::Development, Environment::Production] {
        let mut config = test_config();
        config.env = env;
        config.site_access_password = String::new();
        let h = start(options(config)).await;
        let open = r#"{"required":false,"authenticated":true}"#;
        h.send(Req::get("/api/site-access"))
            .await
            .expect(200, open)
            .expect_header("set-cookie", "");
        h.send(Req::post("/api/site-access").origin(ALLOWED_ORIGIN))
            .await
            .expect(200, open)
            .expect_header("set-cookie", "");
        h.send(
            Req::post("/api/site-access")
                .origin(ALLOWED_ORIGIN)
                .json(r#"{"password":""}"#),
        )
        .await
        .expect(200, open)
        .expect_header("set-cookie", "");
        h.close().await;
    }
}
