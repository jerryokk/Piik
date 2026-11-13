//! The runtime against real sockets: a temporary configuration, the launcher
//! API, loopback discovery and health, and one Local launch.

use std::sync::{Arc, Mutex};

use clap::Parser;
use serde_json::Value;

use super::*;

const INDEX: &[u8] = b"<!doctype html><title>Piik</title>";

fn assets() -> WebAssets {
    WebAssets::from_fn(|path| (path == "index.html").then(|| INDEX.to_vec()))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn serves_launcher_control_and_a_local_room() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("client.json");
    let port = free_port();
    let options = Options::try_parse_from([
        "piik-app",
        "--config",
        config.to_str().unwrap(),
        "--port",
        &port.to_string(),
        "--capture-process",
        directory.path().join("missing.exe").to_str().unwrap(),
    ])
    .unwrap();
    let navigations = Arc::new(Mutex::new(Vec::<Url>::new()));
    let recorded = navigations.clone();
    let origins = AllowedOrigins::default();
    // Some Windows hosts reserve 39721..=39730 invisibly; any free port
    // exercises the same server.
    let (runtime, entry) = PiikRuntime::start_on(
        RuntimeContext {
            options,
            assets: assets(),
            origins: origins.clone(),
            shell: ShellHandle::new(
                move |url| recorded.lock().unwrap().push(url),
                |failed| panic!("runtime exited (failed: {failed})"),
            ),
            diagnostics: Diagnostics::default(),
        },
        free_port(),
    )
    .await
    .unwrap();
    assert!(config.is_file(), "client.json was created");
    assert_eq!(entry.path(), "/client");
    assert!(origins.allows(&entry));
    assert!(origins.allows(&Url::parse(&format!("http://127.0.0.1:{port}/")).unwrap()));

    let http = reqwest::Client::new();
    let launcher = entry.origin().ascii_serialization();
    let state: Value = http
        .get(format!("{launcher}/api/client-launcher"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let control_port = runtime.control.endpoint().port;
    assert_eq!(state["controlPort"], control_port);
    assert_eq!(state["sites"], serde_json::json!([]));
    assert_eq!(state["starting"], false);
    let page = http.get(entry.as_str()).send().await.unwrap();
    assert_eq!(page.bytes().await.unwrap().as_ref(), INDEX);

    let control = format!("http://127.0.0.1:{control_port}");
    let discovery: Value = http
        .get(format!("{control}/connect-discovery"))
        .header("Origin", "https://site.example")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(discovery["connectURL"], entry.as_str());
    assert_eq!(discovery["protocol"], 10);
    let health: Value = http
        .get(format!("{control}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["service"], "piik-client");
    // Without a capture helper only native Viewer media is advertised.
    assert_eq!(health["nativeMedia"]["video"], false);
    assert_eq!(health["nativeMedia"]["receiverReuse"], true);
    assert_eq!(health["nativeMedia"]["maxRelayChildren"], 20);

    // Approval reaches the window allow-list and native control.
    let approved = http
        .post(format!("{launcher}/api/client-launcher/site"))
        .header("Origin", &launcher)
        .json(&serde_json::json!({ "site": "https://site.example", "allowed": true }))
        .send()
        .await
        .unwrap();
    assert!(approved.status().is_success());
    assert!(origins.allows(&Url::parse("https://site.example/").unwrap()));
    let site_health = http
        .get(format!("{control}/health"))
        .header("Origin", "https://site.example")
        .send()
        .await
        .unwrap();
    assert!(site_health.status().is_success());
    assert!(
        piik_app_config::load_or_create(&config)
            .unwrap()
            .sites
            .contains(&"https://site.example".to_owned())
    );

    let launch = http
        .post(format!("{launcher}/api/client-launcher/launch"))
        .header("Origin", &launcher)
        .json(&serde_json::json!({ "mode": "local", "language": "en", "localAccessPassword": "" }))
        .send()
        .await
        .unwrap();
    let ok = launch.status().is_success();
    let result: Value = launch.json().await.unwrap();
    if ok {
        let target = result["target"].as_str().unwrap();
        assert!(
            target.starts_with(&format!("http://127.0.0.1:{port}/")),
            "{target}"
        );
        assert!(origins.allows(&Url::parse(target).unwrap()));
        let host = http
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert_eq!(host.bytes().await.unwrap().as_ref(), INDEX);
    } else {
        // Hosts without exactly one private LAN address cannot start Local.
        assert_eq!(result["reason"], "local-network", "{result}");
    }
    // The launcher page navigates itself; the shell is not asked to.
    assert!(navigations.lock().unwrap().is_empty());

    tokio::time::timeout(std::time::Duration::from_secs(10), runtime.shutdown())
        .await
        .expect("bounded shutdown");
    assert!(
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok(),
        "the room released its port"
    );
}
