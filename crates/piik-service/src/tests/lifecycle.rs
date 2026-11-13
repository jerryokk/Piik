//! Listeners, persistence, startup failures, SFU addressing and shutdown.

use std::net::{SocketAddr, TcpListener, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;

use super::{
    ALLOWED_ORIGIN, CLOSE_TIMEOUT, Req, RoomRequest, assert_port_free, call, free_port, new_server,
    no_media, options, sfu_config, start, test_config, wait_for,
};
use crate::server::{HEADERS_TIMEOUT, REQUEST_TIMEOUT, refresh_sfu_public_ip};
use crate::{Error, Listener, SfuPublicAddress, resolve_sfu_public_ip};

fn assert_udp_free(addresses: &[SocketAddr]) {
    for address in addresses {
        UdpSocket::bind(address)
            .unwrap_or_else(|error| panic!("UDP {address} remained owned: {error}"));
    }
}

fn prebound() -> TcpListener {
    TcpListener::bind("127.0.0.1:0").expect("prebound listener")
}

async fn health(port: u16) -> u16 {
    let response = reqwest::Client::new()
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .expect("health request");
    response.status().as_u16()
}

/// Records formatted log output of the current thread.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log buffer")).into_owned()
    }
}

impl std::io::Write for Captured {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let captured = Captured::default();
    let writer = captured.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::INFO)
        .with_writer(move || writer.clone())
        .finish();
    (captured, tracing::subscriber::set_default(subscriber))
}

// --- STUN ---------------------------------------------------------------------

#[tokio::test]
async fn stun_listeners_follow_application_close_and_end() {
    for ending in [false, true] {
        let mut config = test_config();
        config.stun_urls = vec!["stun:share.example.test:3478".to_owned()];
        config.nat_prediction_enabled = true;
        config.stun_listen_addresses = vec!["127.0.0.1:0".parse().expect("address"); 3];
        let h = start(options(config)).await;
        let addresses = h.server.inner().stun_addresses();
        assert_eq!(addresses.len(), 3);
        if ending {
            h.server.end(CLOSE_TIMEOUT).await.expect("end");
        } else {
            h.close().await;
        }
        assert_udp_free(&addresses);
    }
}

#[tokio::test]
async fn occupied_stun_auxiliary_rolls_back_before_opening_the_database() {
    let first = UdpSocket::bind("127.0.0.1:0")
        .expect("probe")
        .local_addr()
        .expect("address");
    let occupied = UdpSocket::bind("127.0.0.1:0").expect("occupied");
    let directory = tempfile::tempdir().expect("tempdir");
    let mut config = test_config();
    config.port = free_port();
    config.room_database_path = Some(directory.path().join("rooms.sqlite"));
    config.stun_listen_addresses = vec![
        first,
        occupied.local_addr().expect("address"),
        "127.0.0.1:0".parse().expect("address"),
    ];
    let server = new_server(options(config.clone()));
    assert!(
        matches!(server.listen().await, Err(Error::Stun(_))),
        "occupied STUN listener was accepted"
    );
    assert!(
        !directory.path().join("rooms.sqlite").exists(),
        "STUN bind failure touched persistence"
    );
    assert_port_free(config.port);
    assert_udp_free(&[first]);
    occupied
        .set_read_timeout(Some(Duration::from_millis(1)))
        .expect("startup rollback closed another listener");
}

// --- SFU ----------------------------------------------------------------------

#[tokio::test]
async fn sfu_public_address_resolves_only_at_runtime() {
    for input in ["", "198.51.100.5"] {
        assert_eq!(resolve_sfu_public_ip(input).await.expect("literal"), input);
    }
    let resolved = resolve_sfu_public_ip("localhost").await.expect("localhost");
    let address: std::net::Ipv4Addr = resolved.parse().expect("IPv4 literal");
    assert!(address.is_loopback(), "{resolved}");
    let error = resolve_sfu_public_ip("unresolved.invalid")
        .await
        .expect_err("unresolvable");
    assert!(error.contains("SFU_PUBLIC_IP could not resolve"), "{error}");
}

#[tokio::test]
async fn sfu_public_hostname_is_resolved_for_media_and_refreshed() {
    let mut config = test_config();
    config.sfu = Some(sfu_config("localhost", false));
    let media = no_media();
    let address = media.public_address.clone();
    let mut options = options(config);
    options.sfu = Some(media);
    let h = start(options).await;
    let resolved = address.get();
    assert!(
        resolved
            .parse::<std::net::Ipv4Addr>()
            .is_ok_and(|ip| ip.is_loopback()),
        "{resolved}"
    );
    assert!(h.server.inner().has_dns_refresh(), "a DNS name refreshes");
    // A DNS failure keeps the last usable address.
    refresh_sfu_public_ip(&address, None, "unresolved.invalid").await;
    assert_eq!(address.get(), resolved);
    h.close().await;
    assert!(
        !h.server.inner().has_dns_refresh(),
        "shutdown did not retire the DNS refresh worker"
    );
}

#[tokio::test]
async fn sfu_public_literal_needs_no_refresh() {
    let address = SfuPublicAddress::new();
    let mut config = test_config();
    config.sfu = Some(sfu_config("198.51.100.5", false));
    let mut media = no_media();
    media.public_address = address.clone();
    let mut options = options(config);
    options.sfu = Some(media);
    let h = start(options).await;
    assert_eq!(address.get(), "198.51.100.5");
    assert!(!h.server.inner().has_dns_refresh());
    h.close().await;
}

#[tokio::test]
async fn sfu_public_dns_failure_closes_owned_listeners() {
    let listener = prebound();
    let port = listener.local_addr().expect("address").port();
    let mut config = test_config();
    config.sfu = Some(sfu_config("unresolved.invalid", false));
    let mut options = options(config);
    options.sfu = Some(no_media());
    options.listeners = vec![Listener::plain(listener)];
    let server = new_server(options);
    match server.listen().await {
        Err(Error::Sfu(message)) => assert!(
            message.contains("SFU_PUBLIC_IP could not resolve"),
            "{message}"
        ),
        other => panic!("startup must report the DNS failure: {:?}", other.err()),
    }
    assert_port_free(port);
}

// --- listeners and persistence ------------------------------------------------

#[tokio::test]
async fn restores_stable_room_authority_across_an_application_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut config = test_config();
    config.site_access_password = String::new();
    config.room_database_path = Some(directory.path().join("rooms.sqlite"));
    let first = start(options(config.clone())).await;
    let created = first
        .create_room(RoomRequest {
            code_entry_policy: "private",
            ..Default::default()
        })
        .await
        .expect_status(201)
        .room();
    first.close().await;

    let second = start(options(config)).await;
    second
        .update_access(
            &created.room_id,
            &created.host_token,
            r#"{"action":"set-code-entry-policy","policy":"open"}"#,
            "",
        )
        .await
        .expect(
            200,
            r#"{"type":"code-entry-policy-updated","codeEntryPolicy":"open","viewerPasswordEnabled":false}"#,
        );
    second.close().await;
}

#[tokio::test]
async fn does_not_open_the_database_when_another_process_owns_the_listener() {
    let owner = start(options(test_config())).await;
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("missing-parent").join("rooms.sqlite");
    let mut config = test_config();
    config.port = owner.port;
    config.room_database_path = Some(database.clone());
    let contender = new_server(options(config));
    assert!(
        matches!(contender.listen().await, Err(Error::Bind(_))),
        "the contender bound an owned port"
    );
    assert!(!database.exists(), "the room database was created");
    owner
        .send(Req::get("/healthz"))
        .await
        .expect(200, r#"{"status":"ok"}"#);
    assert_eq!(health(owner.port).await, 200);
    owner.close().await;
}

#[tokio::test]
async fn rejects_a_second_database_owner_before_serving_traffic() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut owner_config = test_config();
    owner_config.room_database_path = Some(directory.path().join("rooms.sqlite"));
    let owner = start(options(owner_config.clone())).await;
    let mut contender_config = owner_config;
    contender_config.port = free_port();
    let contender = new_server(options(contender_config.clone()));
    let error = contender.listen().await.expect_err("second owner");
    assert!(
        error.to_string().to_lowercase().contains("locked"),
        "{error}"
    );
    assert_port_free(contender_config.port);
    owner.close().await;
}

#[tokio::test]
async fn rejects_a_mismatched_database_before_serving_traffic() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("rooms.sqlite");
    let raw = rusqlite::Connection::open(&path).expect("open");
    raw.execute_batch("CREATE TABLE unrelated(value TEXT) STRICT")
        .expect("seed unrelated schema");
    drop(raw);
    let mut config = test_config();
    config.port = free_port();
    config.room_database_path = Some(path);
    let server = new_server(options(config.clone()));
    let error = server.listen().await.expect_err("mismatched database");
    assert!(
        error
            .to_string()
            .contains("Room database application identity does not match"),
        "{error}"
    );
    assert_port_free(config.port);
}

#[tokio::test]
async fn rejects_an_inaccessible_database_path_before_serving_traffic() {
    let directory = tempfile::tempdir().expect("tempdir");
    let database = directory.path().join("missing-parent").join("rooms.sqlite");
    let mut config = test_config();
    config.port = free_port();
    config.room_database_path = Some(database.clone());
    let server = new_server(options(config.clone()));
    assert!(
        server.listen().await.is_err(),
        "an unreachable database path was accepted"
    );
    assert!(!database.exists());
    assert_port_free(config.port);
}

#[tokio::test]
async fn uses_the_configured_listen_host_by_default() {
    let h = start(options(test_config())).await;
    assert_eq!(health(h.port).await, 200);
    h.close().await;
}

#[tokio::test]
async fn prebound_listeners_have_one_startup_and_shutdown_owner() {
    for serve in [false, true] {
        let sockets = [prebound(), prebound()];
        let addresses: Vec<SocketAddr> = sockets
            .iter()
            .map(|socket| socket.local_addr().expect("address"))
            .collect();
        let mut options = options(test_config());
        options.listeners = sockets.into_iter().map(Listener::plain).collect();
        let server = new_server(options);
        if serve {
            let port = server.listen().await.expect("listen");
            assert_eq!(
                port,
                addresses[0].port(),
                "startup replaced the prebound listener"
            );
            for address in &addresses {
                assert_eq!(health(address.port()).await, 200);
            }
        }
        server.close(CLOSE_TIMEOUT).await.expect("close");
        for address in &addresses {
            TcpListener::bind(address)
                .unwrap_or_else(|error| panic!("prebound listener survived shutdown: {error}"));
        }
    }
}

#[tokio::test]
async fn listen_is_single_shot_and_refused_after_close() {
    let h = start(options(test_config())).await;
    let again = h.server.listen().await.expect_err("second listen");
    assert_eq!(
        again.to_string(),
        "Piik server startup was already requested"
    );
    h.close().await;
    assert_port_free(h.port);
    // Every shutdown caller joins the first result.
    h.server.end(CLOSE_TIMEOUT).await.expect("joined shutdown");

    let fresh = new_server(options(test_config()));
    fresh
        .close(CLOSE_TIMEOUT)
        .await
        .expect("close before listen");
    let closing = fresh.listen().await.expect_err("listen after close");
    assert_eq!(closing.to_string(), "Piik server is closing");
}

#[test]
fn http_server_keeps_the_published_timeouts() {
    assert_eq!(REQUEST_TIMEOUT, Duration::from_secs(10));
    assert_eq!(HEADERS_TIMEOUT, Duration::from_secs(15));
}

// --- request failures ---------------------------------------------------------

/// A frontend that fails while serving, as a future startup bug would.
struct PanickingAssets;

impl crate::Assets for PanickingAssets {
    fn read(&self, _: &str) -> Option<std::borrow::Cow<'static, [u8]>> {
        panic!("viewer-grant-that-must-not-be-logged");
    }
}

#[tokio::test]
async fn request_failure_answers_500_and_logs_request_categories_only() {
    let (logged, _guard) = capture();
    let mut options = options(test_config());
    options.assets = Some(Arc::new(PanickingAssets));
    let server = new_server(options);
    server.listen().await.expect("listen");
    let path = "/viewer-grant-that-must-not-be-logged?grant=viewer-grant-that-must-not-be-logged";
    let reply = call(server.inner(), &Req::get(path)).await;
    reply.expect(500, r#"{"error":"Internal server error"}"#);
    let record = logged.text();
    assert!(
        record.contains("HTTP request failed")
            && record.contains("method=\"GET\"")
            && record.contains("route=\"frontend\"")
            && !record.contains("path="),
        "{record}"
    );
    assert!(
        !record.contains("viewer-grant-that-must-not-be-logged"),
        "the failure log leaked the request: {record}"
    );

    call(
        server.inner(),
        &Req::new("private-method-token", "/private-path-token"),
    )
    .await;
    let record = logged.text();
    assert!(
        record.contains("method=\"other\"") && !record.contains("private-"),
        "{record}"
    );
    server.close(CLOSE_TIMEOUT).await.expect("close");
}

// --- signaling dispatch --------------------------------------------------------

#[tokio::test]
async fn signal_upgrade_is_dispatched_to_the_signaling_server() {
    let h = start(options(test_config())).await;
    let target = format!("ws://127.0.0.1:{}/signal", h.port);
    let connect = |origin: &'static str| {
        let mut request = target
            .clone()
            .into_client_request()
            .expect("upgrade request");
        request
            .headers_mut()
            .insert("origin", origin.parse().expect("origin"));
        tokio_tungstenite::connect_async(request)
    };
    let (mut socket, _) = connect(ALLOWED_ORIGIN).await.expect("upgrade /signal");
    socket.close(None).await.expect("close signal connection");
    assert!(
        connect("https://foreign.test").await.is_err(),
        "a foreign origin completed the upgrade"
    );
    h.send(Req::get("/signal"))
        .await
        .expect(404, r#"{"error":"Not found"}"#);
    h.close().await;
}

// --- listener failures ----------------------------------------------------------

#[tokio::test]
async fn reports_a_listener_that_dies_while_serving() {
    let (logged, _guard) = capture();
    let h = start(options(test_config())).await;
    h.server.inner().fail_listener(0);
    let failure = tokio::time::timeout(Duration::from_secs(5), h.server.failed())
        .await
        .expect("the listener failure was not reported");
    assert!(matches!(failure, Error::Listener(_)), "{failure}");
    wait_for("the serve failure to be logged", || {
        logged
            .text()
            .contains("Piik HTTP server stopped unexpectedly")
    })
    .await;
    let record = logged.text();
    assert!(
        record.contains("errorType") && !record.contains("127.0.0.1"),
        "{record}"
    );
    h.close().await;
}

#[tokio::test]
async fn owned_http_listener_failure_reaches_its_owner() {
    let sockets = [prebound(), prebound()];
    let ports: Vec<u16> = sockets
        .iter()
        .map(|socket| socket.local_addr().expect("address").port())
        .collect();
    let mut options = options(test_config());
    options.listeners = sockets.into_iter().map(Listener::plain).collect();
    let server = new_server(options);
    server.listen().await.expect("listen");
    assert_eq!(health(ports[0]).await, 200);
    assert_eq!(health(ports[1]).await, 200);
    server.inner().fail_listener(0);
    let failure = tokio::time::timeout(Duration::from_secs(1), server.failed())
        .await
        .expect("dead HTTP listener was not reported to its owner");
    assert!(matches!(failure, Error::Listener(_)));
    assert!(!server.inner().is_accepting());
    assert_eq!(
        health(ports[1]).await,
        503,
        "failed service remained ready on its surviving listener"
    );
    assert!(
        server.inner().reconcile().is_err() && !server.inner().is_accepting(),
        "reconciliation revived a failed listener"
    );
    server.end(CLOSE_TIMEOUT).await.expect("end");
}

#[tokio::test]
async fn owner_shutdown_does_not_report_listener_failure() {
    for ending in [false, true] {
        let mut options = options(test_config());
        options.listeners = vec![Listener::plain(prebound())];
        let server = new_server(options);
        server.listen().await.expect("listen");
        if ending {
            server.end(CLOSE_TIMEOUT).await.expect("end");
        } else {
            server.close(CLOSE_TIMEOUT).await.expect("close");
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(50), server.failed())
                .await
                .is_err(),
            "normal shutdown reported failure"
        );
    }
}

// --- TLS ------------------------------------------------------------------------

/// Trusts any certificate: the test compares nothing but the transport.
#[derive(Debug)]
struct AnyCertificate(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AnyCertificate {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        certificate: &rustls::pki_types::CertificateDer<'_>,
        signature: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            certificate,
            signature,
            &self.0.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

/// One HTTP/1.1 request over TLS; returns the raw response head and body.
async fn tls_request(port: u16, request: &str) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let config = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .expect("protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(AnyCertificate(provider)))
        .with_no_client_auth();
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .expect("connect");
    let name = rustls::pki_types::ServerName::try_from("127.0.0.1").expect("server name");
    let mut stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(name, stream)
        .await
        .expect("TLS handshake");
    stream.write_all(request.as_bytes()).await.expect("request");
    let mut response = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut response)).await;
    String::from_utf8_lossy(&response).into_owned()
}

#[tokio::test]
async fn local_https_listener_serves_the_same_authority_with_its_cookie() {
    let plain = prebound();
    let secure = prebound();
    let secure_port = secure.local_addr().expect("address").port();
    let mut config = piik_config::local(piik_config::LocalOptions {
        port: plain.local_addr().expect("address").port(),
        public_address: "192.0.2.10".to_owned(),
        site_access_password: super::TEST_ACCESS_PASSWORD.to_owned(),
        ..Default::default()
    })
    .expect("local config");
    config.stun_listen_addresses.clear();
    let mut options = options(config);
    options.listeners = vec![
        Listener::plain(plain),
        Listener::tls(
            secure,
            piik_tls::self_signed("127.0.0.1").expect("certificate"),
        ),
    ];
    let server = new_server(options);
    server.listen().await.expect("listen");
    let response = tls_request(
        secure_port,
        "GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )
    .await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(response.ends_with(r#"{"status":"ok"}"#), "{response}");
    // A configured HTTPS destination keeps the Secure host-prefixed cookie.
    let origin = "https://192.0.2.10";
    let host = format!("192.0.2.10:{}", server.inner().config.port);
    let login = format!(
        "POST /api/site-access HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}:{}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
        server.inner().config.port,
        super::TEST_ACCESS_PASSWORD
    );
    let response = tls_request(secure_port, &login).await;
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    assert!(
        response.contains("set-cookie: __Host-piik-site-access="),
        "{response}"
    );
    server.end(CLOSE_TIMEOUT).await.expect("end");
    assert_port_free(secure_port);
}

// --- shutdown ordering -----------------------------------------------------------

/// Shutdown must not close room storage while an admitted creation still
/// holds the signaling-owned store mutation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shutdown_does_not_close_storage_during_an_admitted_room_write() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("rooms.sqlite");
    let (mutating_tx, mutating_rx) = std::sync::mpsc::channel::<()>();
    let (finish_tx, finish_rx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some((mutating_tx, finish_rx))));
    let held = Arc::new(AtomicBool::new(false));
    let random: piik_room::RandomSource = {
        let (gate, held) = (gate.clone(), held.clone());
        Arc::new(move |size| {
            // Hold the first Host-token draw inside the creation commit.
            if size == 32 && !held.swap(true, Ordering::SeqCst) {
                let taken = gate.lock().expect("gate").take();
                if let Some((mutating, finish)) = taken {
                    let _ = mutating.send(());
                    let _ = finish.recv();
                }
            }
            let mut value = vec![0; size];
            rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut value).expect("entropy");
            value
        })
    };
    let store = piik_room::Store::new(piik_room::Options {
        max_rooms: piik_room::CAPACITY,
        max_viewers_per_room: 20,
        database: Some(piik_room::Database::new(&path).expect("database")),
        random: Some(random),
        now: None,
    })
    .expect("store");
    let mut config = test_config();
    config.site_access_password = String::new();
    let mut options = options(config);
    options.room_store = Some(store);
    let server = Arc::new(new_server(options));
    let port = server.listen().await.expect("listen");

    let request = tokio::spawn(async move {
        reqwest::Client::new()
            .post(format!("http://127.0.0.1:{port}/api/rooms"))
            .header("Origin", ALLOWED_ORIGIN)
            .header("Content-Type", "application/json")
            .body(r#"{"codeEntryPolicy":"open"}"#)
            .send()
            .await
            .expect("room request")
    });
    tokio::task::spawn_blocking(move || mutating_rx.recv().expect("mutation started"))
        .await
        .expect("mutation wait");
    let closing = {
        let server = server.clone();
        tokio::spawn(async move { server.close(CLOSE_TIMEOUT).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !closing.is_finished(),
        "shutdown closed room storage while an admitted mutation owned it"
    );
    finish_tx.send(()).expect("release the mutation");
    let response = request.await.expect("request task");
    assert_eq!(response.status().as_u16(), 201);
    let created: serde_json::Value = response.json().await.expect("created room");
    closing.await.expect("close task").expect("close");

    let mut database = piik_room::Database::new(&path).expect("database");
    let restored = database.initialize().expect("restore");
    database.close().expect("close database");
    assert_eq!(restored.len(), 1, "the accepted write was not restored");
    assert_eq!(
        restored[0].room_id,
        created["roomId"].as_str().expect("roomId")
    );
}
