//! Launcher server, site-access and preference tests.

use std::io::Read as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use piik_lan::Address;
use piik_launcher::{
    BoxError, ChangeSite, MemoryAssets, Mode, Options, Selection, Server, load_mode,
    load_startup_site, save_mode, save_startup_site,
};
use reqwest::StatusCode;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const SECOND: Duration = Duration::from_secs(1);
const TARGET: &str = "http://localhost:8787/#piik-client=1";

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(SECOND * 3)
        .build()
        .unwrap()
}

fn app_assets() -> MemoryAssets {
    MemoryAssets::new().with(
        "index.html",
        b"<!doctype html><html><body>client</body></html>".as_slice(),
    )
}

fn no_lan() -> Option<piik_launcher::LanAddresses> {
    Some(Arc::new(|| Ok(Vec::new())))
}

fn start(options: Options) -> Server {
    Server::start(&CancellationToken::new(), app_assets(), options).unwrap()
}

fn sites(site: &str) -> Vec<String> {
    if site.is_empty() {
        Vec::new()
    } else {
        vec![site.to_owned()]
    }
}

fn fixture(site: &str, password: &str) -> Server {
    start(Options {
        sites: sites(site),
        version: "development".to_owned(),
        local_access_password: password.to_owned(),
        enable_debug: Some(Arc::new(|| Ok(()))),
        lan_addresses: no_lan(),
        ..Options::default()
    })
}

fn origin(server: &Server) -> String {
    server.url().trim_end_matches("/client").to_owned()
}

async fn state(server: &Server) -> Value {
    let response = client()
        .get(format!("{}/api/client-launcher", origin(server)))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    response.json().await.unwrap()
}

async fn local_post(server: &Server, path: &str, body: &str) -> reqwest::Response {
    let origin = origin(server);
    client()
        .post(format!("{origin}/api/client-launcher/{path}"))
        .header("Origin", &origin)
        .header("Content-Type", "application/json")
        .body(body.to_owned())
        .send()
        .await
        .unwrap()
}

async fn selection(server: &Server) -> Selection {
    tokio::time::timeout(SECOND, server.next_selection())
        .await
        .expect("launcher did not emit a selection")
        .unwrap()
}

#[tokio::test]
async fn launcher_compresses_entry_and_assets_without_changing_api() {
    let body = "Piik launcher asset ".repeat(400).into_bytes();
    let server = Server::start(
        &CancellationToken::new(),
        MemoryAssets::new()
            .with("index.html", body.clone())
            .with("assets/app.js", body.clone()),
        Options {
            version: "development".to_owned(),
            lan_addresses: no_lan(),
            ..Options::default()
        },
    )
    .unwrap();
    let origin = origin(&server);
    for path in ["/client", "/client/", "/assets/app.js"] {
        let response = client()
            .get(format!("{origin}{path}"))
            .header("Accept-Encoding", "gzip")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()["content-encoding"], "gzip", "{path}");
        let no_store = response
            .headers()
            .get("cache-control")
            .map(|value| value.to_str().unwrap().to_owned());
        let mut decoded = Vec::new();
        flate2::read::GzDecoder::new(&response.bytes().await.unwrap()[..])
            .read_to_end(&mut decoded)
            .unwrap();
        assert_eq!(decoded, body, "compressed asset changed: {path}");
        if path != "/assets/app.js" {
            assert_eq!(
                no_store.as_deref(),
                Some("no-store"),
                "launcher entry lost its cache policy"
            );
        }
    }
    let response = client()
        .get(format!("{origin}/api/client-launcher"))
        .header("Accept-Encoding", "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get("content-encoding").is_none());
    response.json::<Value>().await.unwrap();
    let missing = client()
        .get(format!("{origin}/assets/missing.js"))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn launcher_serves_state_and_completes_one_selection() {
    let server = fixture("https://share.example", "");
    let state = state(&server).await;
    assert_eq!(state["sites"], serde_json::json!(["https://share.example"]));
    assert_eq!(state["defaultMode"], "link");
    assert_eq!(state["version"], "development");
    assert_eq!(state["revision"], "");
    assert_eq!(state["localAccessPassword"], "");
    assert_eq!(state["debug"], false);
    assert_eq!(state["modes"], serde_json::json!(["local", "link"]));
    assert_eq!(state["starting"], false);

    let launching = server.clone();
    let request = tokio::spawn(async move {
        local_post(
            &launching,
            "launch",
            r#"{"mode":"link","language":"vis","debug":true}"#,
        )
        .await
    });
    let selected = selection(&server).await;
    assert_eq!(
        selected,
        Selection {
            mode: Mode::Link,
            language: "vis".to_owned(),
            local_access_password: String::new(),
            debug: true,
            lan_address: None,
        }
    );
    server.set_result(Ok(TARGET.to_owned()));
    let response = tokio::time::timeout(SECOND, request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let launch: Value = response.json().await.unwrap();
    assert_eq!(launch, serde_json::json!({ "target": TARGET }));
}

#[tokio::test]
async fn launcher_reentry_returns_the_existing_room() {
    let server = fixture("https://share.example", "");
    let body = r#"{"mode":"local","language":"en"}"#;
    let first_server = server.clone();
    let first = tokio::spawn(async move { local_post(&first_server, "launch", body).await });
    selection(&server).await;
    // Leaving the page cancels HTTP, but the accepted startup remains alive.
    first.abort();
    assert!(first.await.unwrap_err().is_cancelled());
    // A second page or rapid repeat entry must join the accepted startup.
    let mut entries = Vec::new();
    for _ in 0..2 {
        let entry = server.clone();
        entries.push(tokio::spawn(async move {
            local_post(&entry, "launch", body).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    server.set_result(Ok(TARGET.to_owned()));
    for entry in entries {
        let response = tokio::time::timeout(SECOND, entry).await.unwrap().unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let result: Value = response.json().await.unwrap();
        assert_eq!(
            result["target"], TARGET,
            "entry did not open the existing room"
        );
    }
    // Returning to the launcher after sharing uses the same healthy room.
    let response = local_post(&server, "launch", body).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        server.try_selection().is_none(),
        "reentry started a second room"
    );
}

#[tokio::test]
async fn room_failure_invalidates_ready_target_but_keeps_site_approval_available() {
    let server = start(Options {
        sites: sites("https://share.example"),
        change_site: Some(Arc::new(|_, _| Box::pin(async { (Vec::new(), Ok(())) }))),
        lan_addresses: no_lan(),
        ..Options::default()
    });
    server.begin();
    server.set_result(Ok(TARGET.to_owned()));
    server.set_result(Err("public invitation link stopped".into()));
    let state = state(&server).await;
    assert_eq!(state["target"], "");
    assert_eq!(state["starting"], false);
    assert_eq!(state["detail"], "public invitation link stopped");
    assert_eq!(state["sites"], serde_json::json!(["https://share.example"]));
    let response = local_post(
        &server,
        "site",
        r#"{"site":"https://share.example","allowed":false}"#,
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "failed room blocked approval revocation"
    );
}

#[tokio::test]
async fn launcher_includes_the_injected_build_version_and_revision() {
    let revision = "0123456789abcdef0123456789abcdef01234567";
    let server = start(Options {
        version: "v1.2.3".to_owned(),
        revision: format!(" {revision} "),
        lan_addresses: no_lan(),
        ..Options::default()
    });
    let state = state(&server).await;
    assert_eq!(state["version"], "v1.2.3");
    assert_eq!(state["revision"], revision);
    assert_eq!(
        state["packageTarget"],
        piik_loopback::buildinfo::package_target()
    );
    assert_eq!(state["defaultMode"], "link");
    assert!(
        state.get("debug").is_none(),
        "debug advertised without an owner"
    );
}

#[tokio::test]
async fn launcher_lan_choices_use_current_addresses_and_the_explicit_preference() {
    let address = |address: &str, name: &str| Address {
        address: address.to_owned(),
        name: name.to_owned(),
    };
    for (name, preferred, selected, addresses) in [
        (
            "one",
            "",
            "192.168.1.4",
            Some(vec![address("192.168.1.4", "Wi-Fi")]),
        ),
        (
            "one-private",
            "",
            "192.168.1.4",
            Some(vec![
                address("192.168.1.4", "Wi-Fi"),
                address("198.18.0.1", "Tunnel"),
            ]),
        ),
        (
            "ambiguous",
            "",
            "",
            Some(vec![
                address("10.0.0.2", "Ethernet"),
                address("192.168.1.4", "Wi-Fi"),
            ]),
        ),
        (
            "cli-preferred",
            "192.168.1.4",
            "192.168.1.4",
            Some(vec![
                address("10.0.0.2", "Ethernet"),
                address("192.168.1.4", "Wi-Fi"),
            ]),
        ),
        (
            "stale-preference",
            "192.168.1.4",
            "",
            Some(vec![address("10.0.0.2", "Ethernet")]),
        ),
        ("unavailable", "", "", None),
    ] {
        let listed = addresses.clone();
        let server = start(Options {
            sites: sites("https://share.example"),
            lan_address: preferred.to_owned(),
            lan_addresses: Some(Arc::new(move || {
                listed
                    .clone()
                    .ok_or_else(|| BoxError::from("interfaces are unavailable"))
            })),
            ..Options::default()
        });
        let state = state(&server).await;
        assert_eq!(state["defaultMode"], "link", "{name}");
        assert_eq!(state["lan"]["selected"], selected, "{name}");
        let expected = serde_json::to_value(addresses.unwrap_or_default()).unwrap();
        assert_eq!(
            state["lan"]["addresses"], expected,
            "{name}: LAN interface label lost"
        );
    }
}

#[tokio::test]
async fn launcher_passes_the_chosen_lan_address_to_the_app() {
    let server = fixture("", "");
    let owner = server.clone();
    let selected = tokio::spawn(async move {
        let selection = owner.next_selection().await.unwrap();
        owner.set_result(Ok(TARGET.to_owned()));
        selection
    });
    let response = local_post(
        &server,
        "launch",
        r#"{"mode":"local","language":"en","lanAddress":" 192.168.1.4 "}"#,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let selection = selected.await.unwrap();
    assert_eq!(selection.lan_address.as_deref(), Some("192.168.1.4"));
}

#[tokio::test]
async fn launcher_preserves_an_optional_local_password() {
    let long = "x".repeat(256);
    for password in ["", "x", "中文", " ", "  中文 +&  ", long.as_str()] {
        let server = fixture("", password);
        assert_eq!(state(&server).await["localAccessPassword"], password);
        let payload = serde_json::json!({
            "mode": "local", "language": "zh", "localAccessPassword": password,
        })
        .to_string();
        let launching = server.clone();
        let request = tokio::spawn(async move { local_post(&launching, "launch", &payload).await });
        let selected = selection(&server).await;
        assert_eq!(selected.mode, Mode::Local);
        assert_eq!(selected.language, "zh");
        assert_eq!(selected.local_access_password, password);
        server.set_result(Ok(TARGET.to_owned()));
        assert_eq!(request.await.unwrap().status(), StatusCode::OK);
    }
}

#[tokio::test]
async fn launcher_rejects_an_invalid_saved_site() {
    let started = Server::start(
        &CancellationToken::new(),
        app_assets(),
        Options {
            sites: sites("https://example.test/path"),
            ..Options::default()
        },
    );
    assert!(started.is_err(), "launcher accepted a Site path");
}

#[tokio::test]
async fn launcher_requires_the_browser_build() {
    let started = Server::start(
        &CancellationToken::new(),
        MemoryAssets::new(),
        Options::default(),
    );
    assert!(
        started.is_err(),
        "launcher started without an index document"
    );
}

#[tokio::test]
async fn launcher_rejects_invalid_selections() {
    let server = fixture("", "");
    for (name, payload) in [
        ("unknown", r#"{"mode":"other","language":"en"}"#),
        (
            "local-site",
            r#"{"mode":"local","language":"en","site":"https://share.example"}"#,
        ),
        ("missing-site", r#"{"mode":"site","language":"en"}"#),
        (
            "site-path",
            r#"{"mode":"site","language":"en","site":"https://share.example/path"}"#,
        ),
        (
            "bare-site-path",
            r#"{"mode":"site","language":"en","site":"share.example/path"}"#,
        ),
        (
            "site-credentials",
            r#"{"mode":"site","language":"en","site":"user:pass@share.example"}"#,
        ),
        (
            "site-password",
            r#"{"mode":"site","language":"en","site":"https://share.example","localAccessPassword":"valid-pass"}"#,
        ),
        (
            "unknown-field",
            r#"{"mode":"local","language":"en","extra":true}"#,
        ),
        (
            "password-type",
            r#"{"mode":"local","language":"en","localAccessPassword":123}"#,
        ),
        (
            "debug-type",
            r#"{"mode":"local","language":"en","debug":"true"}"#,
        ),
        (
            "lan-type",
            r#"{"mode":"local","language":"en","lanAddress":123}"#,
        ),
        (
            "lan-invalid",
            r#"{"mode":"local","language":"en","lanAddress":"bad-address"}"#,
        ),
        (
            "lan-ipv6",
            r#"{"mode":"local","language":"en","lanAddress":"::1"}"#,
        ),
        (
            "link-lan",
            r#"{"mode":"link","language":"en","lanAddress":"192.168.1.4"}"#,
        ),
        (
            "link-lan-empty",
            r#"{"mode":"link","language":"en","lanAddress":""}"#,
        ),
        (
            "site-lan",
            r#"{"mode":"site","language":"en","site":"https://share.example","lanAddress":"192.168.1.4"}"#,
        ),
        ("missing-language", r#"{"mode":"local"}"#),
        ("unknown-language", r#"{"mode":"local","language":"other"}"#),
        ("trailing", r#"{"mode":"local","language":"en"}{}"#),
    ] {
        let response = local_post(&server, "launch", payload).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{name}");
        let failure: Value = response.json().await.unwrap();
        assert_eq!(failure["error"], "invalid launch request", "{name}");
        assert!(
            !failure["detail"].as_str().unwrap().is_empty(),
            "{name}: lost its reason"
        );
    }
    assert!(server.try_selection().is_none());
    let too_large = format!(
        r#"{{"mode":"local","language":"en","localAccessPassword":"{}"}}"#,
        "x".repeat(2048)
    );
    assert_eq!(
        local_post(&server, "launch", &too_large).await.status(),
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn launcher_restricts_modes_to_the_offered_ones() {
    let server = start(Options {
        modes: vec![Mode::Local],
        lan_addresses: no_lan(),
        ..Options::default()
    });
    assert_eq!(state(&server).await["modes"], serde_json::json!(["local"]));
    let response = local_post(&server, "launch", r#"{"mode":"link","language":"en"}"#).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(response.text().await.unwrap(), "unsupported launch mode\n");
}

#[tokio::test]
async fn launcher_reports_safe_bounded_failure_details() {
    let server = start(Options {
        version: "development".to_owned(),
        debug: true,
        lan_addresses: no_lan(),
        ..Options::default()
    });
    assert_eq!(
        state(&server).await["debug"],
        true,
        "CLI debug was not advertised"
    );
    #[derive(Debug, thiserror::Error)]
    #[error("{0}")]
    struct Cause(String);
    let cause = Cause(format!(
        "public tunnel failed: token=private-secret {}",
        "network unavailable ".repeat(200)
    ));
    let owner = server.clone();
    tokio::spawn(async move {
        owner.next_selection().await;
        owner.set_result(Err(Box::new(piik_failure::Failure::new(
            piik_failure::Reason::TunnelExited,
            cause,
        ))));
    });
    let response = local_post(&server, "launch", r#"{"mode":"link","language":"en"}"#).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let failure: Value = response.json().await.unwrap();
    let detail = failure["detail"].as_str().unwrap();
    assert_eq!(failure["reason"], "tunnel-exited");
    assert_eq!(failure["error"], "Piik App could not start");
    assert!(detail.contains("public tunnel failed"));
    assert!(!detail.contains("private-secret"), "unsafe failure detail");
    assert!(detail.chars().count() <= 2051, "unbounded failure detail");
    let restored = state(&server).await;
    assert_eq!(restored["reason"], failure["reason"]);
    assert_eq!(restored["detail"], failure["detail"]);
}

#[tokio::test]
async fn launcher_accepts_localhost_alias() {
    let server = fixture("", "");
    let port = origin(&server).rsplit(':').next().unwrap().to_owned();
    let response = client()
        .get(format!("{}/api/client-launcher", origin(&server)))
        .header("Host", format!("localhost:{port}"))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn launcher_rejects_cross_origin_control_and_foreign_hosts() {
    let server = fixture("", "");
    let url = format!("{}/api/client-launcher", origin(&server));
    let response = client()
        .get(&url)
        .header("Origin", "https://unrelated.example")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let response = client()
        .get(&url)
        .header("Host", "evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "DNS rebinding host was served"
    );
}

fn edit_sites(sites: &mut Vec<String>, site: &str, allowed: bool) {
    if allowed {
        if !sites.iter().any(|value| value == site) {
            sites.push(site.to_owned());
        }
    } else {
        sites.retain(|value| value != site);
    }
}

fn editing(current: Arc<Mutex<Vec<String>>>) -> ChangeSite {
    Arc::new(move |site, allowed| {
        let mut sites = current.lock().unwrap();
        edit_sites(&mut sites, &site, allowed);
        let result = sites.clone();
        Box::pin(async move { (result, Ok(())) })
    })
}

#[tokio::test]
async fn site_approval_is_local_explicit_and_does_not_start_a_room() {
    let current: Arc<Mutex<Vec<String>>> = Arc::default();
    let debug_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let calls = debug_calls.clone();
    let server = start(Options {
        change_site: Some(editing(current.clone())),
        enable_debug: Some(Arc::new(move || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        })),
        lan_addresses: no_lan(),
        ..Options::default()
    });
    let origin = origin(&server);
    for path in ["site", "startup-site", "launch", "debug"] {
        for (request_origin, content_type) in [
            ("", "application/json"),
            ("null", "application/json"),
            ("https://other.example", "application/json"),
            (origin.as_str(), "text/plain"),
            (origin.as_str(), "application/x-www-form-urlencoded"),
        ] {
            let response = client()
                .post(format!("{origin}/api/client-launcher/{path}"))
                .header("Origin", request_origin)
                .header("Content-Type", content_type)
                .body(r#"{"site":"https://other.example","allowed":true}"#)
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::NOT_FOUND,
                "mutation admitted nonlocal or form request: {path} {request_origin} {content_type}"
            );
        }
    }
    assert!(current.lock().unwrap().is_empty());
    assert_eq!(
        debug_calls.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "side effects"
    );
    for _ in 0..2 {
        let response = local_post(&server, "debug", "{}").await;
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "standby diagnostics could not start"
        );
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            serde_json::json!({ "debug": true })
        );
    }
    assert_eq!(debug_calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    for (site, allowed, want) in [
        (
            "https://Share.Example:443/",
            true,
            vec!["https://share.example"],
        ),
        (
            "https://other.example",
            true,
            vec!["https://share.example", "https://other.example"],
        ),
        (
            "https://other.example",
            true,
            vec!["https://share.example", "https://other.example"],
        ),
        (
            "https://share.example",
            false,
            vec!["https://other.example"],
        ),
    ] {
        let body = serde_json::json!({ "site": site, "allowed": allowed }).to_string();
        let response = local_post(&server, "site", &body).await;
        assert_eq!(response.status(), StatusCode::OK);
        let result: Value = response.json().await.unwrap();
        assert_eq!(result["sites"], serde_json::json!(want));
        assert_eq!(*current.lock().unwrap(), want);
    }
    assert!(
        server.try_selection().is_none(),
        "site approval created room authority"
    );
    for (body, status) in [
        (r#"{"site":"https://share.example"}"#, 400),
        (r#"{"site":"","allowed":true}"#, 400),
        (
            r#"{"site":"https://share.example/path","allowed":true}"#,
            400,
        ),
        (
            r#"{"site":"https://share.example","allowed":true,"extra":1}"#,
            400,
        ),
    ] {
        assert_eq!(
            local_post(&server, "site", body).await.status().as_u16(),
            status,
            "{body}"
        );
    }
    assert_eq!(
        local_post(
            &server,
            "startup-site",
            r#"{"site":"https://other.example","allowed":true}"#
        )
        .await
        .status(),
        StatusCode::BAD_REQUEST
    );
    let response = client().get(server.url()).send().await.unwrap();
    assert_eq!(response.headers()["x-frame-options"], "DENY");
    assert_eq!(
        response.headers()["content-security-policy"],
        "frame-ancestors 'none'"
    );
    assert_eq!(response.headers()["referrer-policy"], "no-referrer");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn debug_cannot_start_after_a_room() {
    let server = fixture("", "");
    server.begin();
    let response = local_post(&server, "debug", "{}").await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn site_approval_failure_reflects_the_actual_permission() {
    for applied in [false, true] {
        let server = start(Options {
            sites: sites("https://old.example"),
            change_site: Some(Arc::new(move |_, _| {
                Box::pin(async move {
                    if applied {
                        (Vec::new(), Err(BoxError::from("retirement still pending")))
                    } else {
                        (
                            vec!["https://old.example".to_owned()],
                            Err(BoxError::from("save failed")),
                        )
                    }
                })
            })),
            lan_addresses: no_lan(),
            ..Options::default()
        });
        let response = local_post(
            &server,
            "site",
            r#"{"site":"https://old.example","allowed":false}"#,
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let failure: Value = response.json().await.unwrap();
        let want = if applied {
            serde_json::json!([])
        } else {
            serde_json::json!(["https://old.example"])
        };
        assert_eq!(failure["sites"], want, "error hid actual permission");
        assert!(!failure["detail"].as_str().unwrap().is_empty());
        assert_eq!(
            state(&server).await["sites"],
            want,
            "launcher disagrees with permission"
        );
    }
}

#[tokio::test]
async fn saved_mode_survives_relaunch_without_changing_config() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("client.json");
    let mut config = piik_app_config::load_or_create(&path).unwrap();
    config.sites = vec!["https://share.example".to_owned()];
    piik_app_config::save(&path, &config).unwrap();
    let original = std::fs::read(&path).unwrap();
    for chosen in [Mode::Local, Mode::Link] {
        save_mode(&path, chosen).unwrap();
        let mode = load_mode(&path).unwrap();
        assert_eq!(mode, Some(chosen));
        let server = start(Options {
            sites: config.sites.clone(),
            default_mode: mode,
            lan_addresses: no_lan(),
            ..Options::default()
        });
        let state = state(&server).await;
        server.close().await.unwrap();
        assert_eq!(state["defaultMode"], chosen.as_str());
        assert_eq!(state["sites"], serde_json::json!(config.sites));
    }
    assert_eq!(
        std::fs::read(&path).unwrap(),
        original,
        "preference changed configuration"
    );
}

#[tokio::test]
async fn starred_site_survives_other_approvals_and_clears_on_revocation() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("client.json");
    let config = piik_app_config::load_or_create(&path).unwrap();
    let current = Arc::new(Mutex::new(vec!["https://share.example".to_owned()]));
    let saved_path = path.clone();
    let server = start(Options {
        sites: current.lock().unwrap().clone(),
        control_port: 39730,
        save_startup_site: Some(Arc::new(move |site: &str| {
            save_startup_site(&saved_path, site).map_err(BoxError::from)
        })),
        change_site: Some(editing(current.clone())),
        lan_addresses: no_lan(),
        ..Options::default()
    });
    for (setting, body, want, status) in [
        (
            "startup-site",
            r#"{"site":"https://share.example"}"#,
            "https://share.example",
            200,
        ),
        (
            "startup-site",
            r#"{"site":"https://unapproved.example"}"#,
            "https://share.example",
            409,
        ),
        (
            "site",
            r#"{"site":"https://other.example","allowed":true}"#,
            "https://share.example",
            200,
        ),
        (
            "startup-site",
            r#"{"site":"https://other.example"}"#,
            "https://other.example",
            200,
        ),
        (
            "site",
            r#"{"site":"https://share.example","allowed":false}"#,
            "https://other.example",
            200,
        ),
        (
            "site",
            r#"{"site":"https://other.example","allowed":false}"#,
            "",
            200,
        ),
    ] {
        let response = local_post(&server, setting, body).await;
        assert_eq!(response.status().as_u16(), status, "{setting} {body}");
        let state: Value = response.json().await.unwrap();
        assert_eq!(state["startupSite"], want, "{setting} {body}");
        let approved: Vec<String> = serde_json::from_value(state["sites"].clone()).unwrap();
        assert_eq!(
            load_startup_site(&path, &approved).unwrap(),
            want,
            "saved startup"
        );
        let entry = if want.is_empty() {
            server.url()
        } else {
            format!("{want}/?piik-client-port=39730#piik-client=1")
        };
        assert_eq!(
            server.entry_url(),
            entry,
            "startup entry lost its App endpoint"
        );
    }
    assert!(
        server.try_selection().is_none() && config.sites.is_empty(),
        "preference created a room"
    );
}

#[tokio::test]
async fn close_releases_a_waiting_launch() {
    let server = fixture("", "");
    let launching = server.clone();
    let request = tokio::spawn(async move {
        local_post(&launching, "launch", r#"{"mode":"link","language":"en"}"#).await
    });
    selection(&server).await;
    server.close().await.unwrap();
    let response = tokio::time::timeout(SECOND * 3, request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert!(server.next_selection().await.is_none());
}
