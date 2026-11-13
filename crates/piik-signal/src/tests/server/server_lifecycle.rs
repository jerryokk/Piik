//! Service shutdown, connection admission and signal bounds.

use super::*;

#[test]
fn signal_identifies_graceful_service_restart_to_connected_clients() {
    let mut h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "restart-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let server = h.server.clone();
    let shutdown = h
        .runtime
        .spawn(async move { server.close(Duration::from_secs(5)).await });
    expect_close(&host, 1012, "Service restart");
    h.runtime.block_on(shutdown).expect("join").expect("close");
    h.close();
}

// Every client that has not finished the close handshake one second after
// close began is terminated, however many there are.
#[test]
fn signal_close_terminates_unresponsive_clients_after_one_second_grace() {
    let mut h = Harness::start(HarnessOptions {
        authentication_timeout_ms: 60_000,
        ..Default::default()
    });
    // Raw sockets that never read: the close frame is never answered.
    let raw: Vec<std::net::TcpStream> = (0..2)
        .map(|_| {
            use std::io::Write;
            let mut stream = std::net::TcpStream::connect(h.address).expect("dial");
            let request = format!(
                "GET /signal HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {ALLOWED_ORIGIN}\r\n\r\n",
                h.address
            );
            stream.write_all(request.as_bytes()).expect("write");
            stream
        })
        .collect();
    assert!(
        wait_for(DEFAULT_WAIT, || h.session_count() == 2),
        "sessions = {}, want 2",
        h.session_count()
    );
    let started = Instant::now();
    let server = h.server.clone();
    h.runtime
        .block_on(async move { server.close(Duration::from_secs(10)).await })
        .expect("close");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= crate::server::SERVICE_RESTART_CLOSE_GRACE && elapsed <= Duration::from_secs(3),
        "close took {elapsed:?}, want the one-second grace"
    );
    drop(raw);
    h.close();
}

#[test]
fn signal_ends_every_room_before_local_authority_stops() {
    let mut h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "ending-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "ending-viewer",
        1,
        "",
        presence(),
    );
    h.server.end_all_rooms().expect("end all rooms");
    let server = h.server.clone();
    let shutdown = h
        .runtime
        .spawn(async move { server.close(Duration::from_secs(5)).await });
    expect_equal(
        &host.next("room-closed").raw,
        r#"{"type":"room-closed","reason":"host-ended"}"#,
    );
    expect_equal(
        &viewer.next("room-closed").raw,
        r#"{"type":"room-closed","reason":"host-ended"}"#,
    );
    expect_close(&host, 1000, "Room abandoned");
    expect_close(&viewer, 1000, "Room abandoned");
    h.runtime.block_on(shutdown).expect("join").expect("close");
    assert_eq!(h.store_size(), 0, "store size");
    h.close();
}

// ---------------------------------------------------------------------------
// challenge and diagnostics
// ---------------------------------------------------------------------------

#[test]
fn signal_requires_allowed_origin_timely_authentication_and_bounded_payloads() {
    let h = Harness::start(HarnessOptions {
        authentication_timeout_ms: 30,
        ..Default::default()
    });
    assert_eq!(
        h.rejected_upgrade_status(&format!("{ALLOWED_ORIGIN}/path")),
        403,
        "origin with path"
    );
    assert_eq!(
        h.rejected_upgrade_status("https://foreign.example.test"),
        403,
        "foreign origin"
    );
    let unauthenticated = h.open_client();
    expect_match(
        &unauthenticated
            .next_within("error", Duration::from_millis(300))
            .raw,
        r#"{"code":"AUTH_REQUIRED"}"#,
    );
    expect_close_code(&unauthenticated, 4003);
    let oversized = h.open_client();
    oversized.send_text(&"x".repeat(piik_protocol::MAX_SIGNAL_BYTES + 1));
    expect_close_code(&oversized, 1009);
}

#[test]
fn closing_signal_session_does_not_reserve_late_viewer_admission() {
    let h = Harness::start(HarnessOptions::default());
    let client = h.open_client();
    assert!(
        wait_for(DEFAULT_WAIT, || h.session_count() == 1),
        "signaling session was not registered"
    );
    let message = json!({
        "type": "authenticate",
        "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": h.room.room_id,
        "role": "viewer",
        "clientId": "late-viewer",
    })
    .to_string();
    h.locked(|core| {
        let key = core.sessions.keys()[0];
        // An authentication deadline can queue close before a frame already
        // read from the socket acquires the signaling lock.
        core.close_session(
            key,
            crate::session::CLOSE_AUTHENTICATION_FAILED,
            "Authentication required",
        );
        core.handle_message(key, message.as_bytes());
        let peers = core.store.get_viewer_peer_ids(&h.room.room_id);
        assert!(
            peers.is_empty(),
            "closing session left room capacity reserved: {peers:?}"
        );
    });
    expect_close_code(
        &client,
        i32::from(crate::session::CLOSE_AUTHENTICATION_FAILED),
    );
}

#[test]
fn signal_terminates_stale_v12_before_authentication() {
    let h = Harness::start(HarnessOptions::default());
    let client = h.open_client();
    client.send_json(json!({"type": "authenticate", "protocol": "piik-v12", "roomId": h.room.room_id, "role": "viewer", "clientId": "invalid-client"}));
    expect_match(&client.next("error").raw, r#"{"code":"INVALID_MESSAGE"}"#);
    expect_close(&client, 1008, "Invalid message");
}

#[test]
fn signal_rejects_invalid_upgrade_request_target_without_stopping_server() {
    let h = Harness::start(HarnessOptions::default());
    for (target, status) in [
        ("//[", "HTTP/1.1 400 Bad Request"),
        ("/%73ignal", "HTTP/1.1 404 Not Found"),
    ] {
        let response = h.raw_upgrade_response(target);
        assert!(
            response.starts_with(status),
            "target {target:?}: raw response {response:?}, want {status}"
        );
    }
    let host = h.open_client();
    let host_auth = authenticate(
        &host,
        &h.room,
        "host",
        "host-after-invalid-url",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    assert_eq!(host_auth.role(), "host");
}

#[test]
fn signal_rejects_unauthenticated_and_total_connection_overflow_before_upgrade() {
    let h = Harness::start(HarnessOptions {
        max_connections: 2,
        max_unauthenticated_connections: 1,
        ..Default::default()
    });
    let host = h.open_client();
    assert!(
        wait_for(DEFAULT_WAIT, || h.session_count() == 1),
        "first session"
    );
    assert_eq!(
        h.rejected_upgrade_status(ALLOWED_ORIGIN),
        503,
        "unauthenticated overflow"
    );
    authenticate(
        &host,
        &h.room,
        "host",
        "capacity-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "capacity-viewer",
        1,
        "",
        presence(),
    );
    assert_eq!(
        h.rejected_upgrade_status(ALLOWED_ORIGIN),
        503,
        "total overflow"
    );
}

// ---------------------------------------------------------------------------
// heartbeat, outbound bound and the close window
// ---------------------------------------------------------------------------

struct QueuedTimer {
    duration: Duration,
    callback: Option<Box<dyn FnOnce() + Send>>,
    stopped: Arc<AtomicBool>,
}

/// An `AfterFunc` whose callbacks fire only on demand.
#[derive(Default)]
pub(crate) struct TimerQueue {
    entries: std::sync::Mutex<Vec<QueuedTimer>>,
}

impl TimerQueue {
    pub(crate) fn after_func(self: &Arc<Self>) -> crate::runtime::AfterFunc {
        let queue = self.clone();
        Arc::new(move |duration, callback| {
            let stopped = Arc::new(AtomicBool::new(false));
            queue.entries.lock().expect("timers").push(QueuedTimer {
                duration,
                callback: Some(callback),
                stopped: stopped.clone(),
            });
            Box::new(move || !stopped.swap(true, Ordering::SeqCst))
        })
    }

    /// Runs every pending callback armed with exactly `duration`.
    pub(crate) fn fire(&self, duration: Duration) {
        self.fire_where(|armed| armed == duration);
    }

    /// Runs every pending callback armed with less than `limit`.
    pub(crate) fn fire_due(&self, limit: Duration) {
        self.fire_where(|armed| armed < limit);
    }

    fn fire_where(&self, due: impl Fn(Duration) -> bool) {
        let fired: Vec<Box<dyn FnOnce() + Send>> = {
            let mut entries = self.entries.lock().expect("timers");
            entries.retain(|entry| !entry.stopped.load(Ordering::SeqCst));
            let mut fired = Vec::new();
            entries.retain_mut(|entry| {
                if due(entry.duration) {
                    entry.stopped.store(true, Ordering::SeqCst);
                    fired.extend(entry.callback.take());
                    return false;
                }
                true
            });
            fired
        };
        for callback in fired {
            callback();
        }
    }
}

#[test]
fn signal_heartbeat_terminates_silent_connection_after_missed_pong() {
    let timers = Arc::new(TimerQueue::default());
    let h = Harness::start(HarnessOptions {
        after_func: Some(timers.after_func()),
        ..Default::default()
    });
    let heartbeat = Duration::from_millis(60_000);
    let responsive = h.open_client();
    // A raw upgraded socket that never reads never answers a ping.
    let silent = {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(h.address).expect("dial");
        let request = format!(
            "GET /signal HTTP/1.1\r\nHost: {}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {ALLOWED_ORIGIN}\r\n\r\n",
            h.address
        );
        stream.write_all(request.as_bytes()).expect("write");
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).expect("upgrade response");
        assert!(head.starts_with(b"HTTP/1.1 101"), "silent upgrade");
        stream
    };
    assert!(
        wait_for(DEFAULT_WAIT, || h.session_count() == 2),
        "sessions: {}",
        h.session_count()
    );
    let alive = || {
        h.locked(|core| {
            core.sessions
                .iter()
                .filter(|(_, session)| session.alive)
                .count()
        })
    };
    timers.fire(heartbeat);
    assert!(
        wait_for(DEFAULT_WAIT, || alive() == 1),
        "expected exactly the responsive session to answer, alive={}",
        alive()
    );
    timers.fire(heartbeat);
    assert!(
        wait_for(DEFAULT_WAIT, || h.session_count() == 1),
        "silent session was not terminated"
    );
    drop(silent);
    assert!(!responsive.is_closed(), "responsive client must stay open");
    let host_auth = authenticate(
        &responsive,
        &h.room,
        "host",
        "heartbeat-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    assert_eq!(host_auth.role(), "host");
}

#[test]
fn signal_terminates_connection_above_outbound_buffer_bound() {
    let mut over = crate::session::Session::new(true);
    let mut closed = crate::session::Session::new(true);
    over.queued_bytes = crate::server::MAX_BUFFERED_SIGNAL_BYTES + 1;
    over.send_encoded(Arc::from(&b"{}"[..]));
    closed.close(1000, "bye");
    closed.send_encoded(Arc::from(&b"{}"[..]));
    assert!(
        over.terminated && over.queue.is_empty(),
        "a session above the bound is terminated, not queued"
    );
    assert!(
        !closed.terminated && closed.queue.len() == 1,
        "a closing session drops sends without terminating"
    );
}

// Close sets `closing` and runs the router's synchronous closing prefix in
// one locked section, so a deadline callback that runs once closing is
// observable finds the router closing and starts no media operation. The
// callback fires as soon as the closing flag is visible.
#[test]
fn signal_close_marks_router_closing_before_releasing_the_lock() {
    let timers = Arc::new(TimerQueue::default());
    let clock = TestClock::new(utc_millis(2026, 8, 24, 12));
    let control = FakeMedia::new();
    let mut h = Harness::start(HarnessOptions {
        after_func: Some(timers.after_func()),
        now: Some(clock.clock()),
        sfu: Some(SfuFallback {
            media: control.clone(),
            admission: Admission::new(AdmissionOptions {
                ingress_capacity: 2,
                egress_capacity: 20,
            }),
            prepare_timeout_ms: 300,
        }),
        ..Default::default()
    });
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "close-window-host",
        1,
        "test_share_generation_12345678",
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "close-window-viewer",
        1,
        "",
        presence(),
    );
    next_prepared_route(&viewer);
    let router_idle = || h.locked(|core| core.router.inflight == 0);
    assert!(
        wait_for(DEFAULT_WAIT, router_idle),
        "pump driver did not settle"
    );
    clock.advance(150);
    let guard = h.server.core.lock().expect("core");
    let server = h.server.clone();
    let closed = h
        .runtime
        .spawn(async move { server.close(Duration::from_secs(5)).await });
    std::thread::sleep(Duration::from_millis(50));
    drop(guard);
    assert!(
        wait_for(DEFAULT_WAIT, || h.locked(|core| core.closing)),
        "close did not start"
    );
    assert!(
        h.locked(|core| core.router.closing),
        "the router was not closing when the lock was released"
    );
    timers.fire_due(Duration::from_secs(60));
    h.runtime.block_on(closed).expect("join").expect("close");
    assert!(
        wait_for(DEFAULT_WAIT, router_idle),
        "router work did not settle after close"
    );
    assert!(
        control.created().is_empty(),
        "a deadline firing during close created {} publication(s)",
        control.created().len()
    );
    h.close();
}
