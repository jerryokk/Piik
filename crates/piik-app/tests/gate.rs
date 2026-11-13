//! The built App in headless gate mode, driven the way the Node gates drive
//! it: piped stdio, `--local`, a temporary configuration and a free port.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, TcpListener};
use std::process::{Child, ChildStdout, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

/// The shell's 12 s runtime bound plus process start and exit.
const STOP_BOUND: Duration = Duration::from_secs(15);

fn free_port() -> u16 {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .unwrap()
}

/// Kills the App if an assertion fails first.
struct App(Child);

impl Drop for App {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn lines(stdout: ChildStdout) -> mpsc::Receiver<String> {
    let (send, lines) = mpsc::channel();
    thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if send.send(line).is_err() {
                return;
            }
        }
    });
    lines
}

#[test]
fn local_gate_prints_go_lines_and_stops_on_stdin() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("client.json");
    let port = free_port();
    // Windows may reserve the published 39721..=39730 range on this host.
    let control_port = free_port();
    let lan = piik_lan::addresses()
        .ok()
        .and_then(|addresses| addresses.into_iter().find(|address| address.contains('.')))
        .expect("an active IPv4 LAN address");
    let mut child = Command::new(env!("CARGO_BIN_EXE_piik-app"))
        .args(["--local", "--config"])
        .arg(&config)
        .args(["--lan-address", &lan, "--port", &port.to_string()])
        .arg("--capture-process")
        .arg(directory.path().join("missing.exe"))
        .env("PIIK_CLIENT_GATE_NO_BROWSER", "true")
        .env("PIIK_CLIENT_GATE_CONTROL_PORT", control_port.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
    let stdout = lines(child.stdout.take().unwrap());
    let mut stdin = child.stdin.take().unwrap();
    let app = App(child);
    let next = || {
        stdout
            .recv_timeout(Duration::from_secs(15))
            .expect("a stdout line")
    };

    // The endpoint line: exactly four fields, first.
    let endpoint: serde_json::Value = serde_json::from_str(&next()).unwrap();
    let object = endpoint.as_object().unwrap();
    assert_eq!(object.len(), 4, "{endpoint}");
    assert_eq!(endpoint["port"], control_port);
    assert_eq!(endpoint["host"], format!("127.0.0.1:{control_port}"));
    assert_eq!(endpoint["url"], format!("http://127.0.0.1:{control_port}"));
    let token = endpoint["instanceToken"].as_str().unwrap();
    assert_eq!(token.len(), 43);

    // A fresh configuration has open Local access.
    assert_eq!(next(), "Local access: open");
    assert_eq!(
        next(),
        format!("LAN invitation origin: https://{lan}:{port}")
    );
    let certificate = next();
    let fingerprint = certificate
        .strip_prefix("LAN certificate SHA-256: ")
        .unwrap_or_else(|| panic!("{certificate}"));
    assert!(
        fingerprint.len() == 64
            && fingerprint
                .chars()
                .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()),
        "{fingerprint}"
    );
    assert!(config.is_file(), "client.json was created");

    // The readiness lines follow the listener.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let http = reqwest::Client::new();
        let health: serde_json::Value = http
            .get(format!("http://127.0.0.1:{port}/healthz"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(health["status"], "ok");
        let room = http
            .get(format!("http://127.0.0.1:{port}/"))
            .send()
            .await
            .unwrap();
        assert!(room.status().is_success(), "{}", room.status());
        let control: serde_json::Value = http
            .get(format!("http://127.0.0.1:{control_port}/health"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(control["instanceToken"], token);
    });

    // Any stdin byte (then EOF) stops the App within the bound.
    stdin.write_all(b"\n").unwrap();
    drop(stdin);
    let mut app = app;
    let started = Instant::now();
    let status = loop {
        if let Some(status) = app.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            started.elapsed() < STOP_BOUND,
            "the App did not stop in time"
        );
        thread::sleep(Duration::from_millis(50));
    };
    assert!(status.success(), "{status}");
}
