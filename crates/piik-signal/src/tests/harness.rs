//! The WebSocket server harness: a memory store, short timers (authentication
//! 500 ms, viewer grace 50 ms, heartbeat 60 s), an axum listener on loopback
//! and tokio-tungstenite clients whose inbox validates every server message
//! against the wire schema.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use piik_protocol::{
    CodeEntryPolicy, RoomAccessUpdateRequest, RoomAccessUpdateResponse, decode_server_message,
};
use piik_room::CreatedRoom;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::router_harness::base_options;
use crate::router::SfuFallback;
use crate::runtime::{AfterFunc, Clock, lock};
use crate::server::{Core, Server, locked_with_io};

pub(crate) const ALLOWED_ORIGIN: &str = "http://allowed.test";
pub(crate) const SITE_ACCESS_COOKIE: &str = "piik-site-access=granted";
pub(crate) const DEFAULT_WAIT: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(crate) struct HarnessOptions {
    pub authentication_timeout_ms: u64,
    pub viewer_disconnect_grace_ms: u64,
    pub room_empty_timeout_ms: i64,
    pub max_viewers_per_room: usize,
    pub max_connections: usize,
    pub max_unauthenticated_connections: usize,
    pub site_access_password: bool,
    pub endpoint_media_copy_capacity: i64,
    pub stun_urls: Vec<String>,
    pub nat_prediction_enabled: bool,
    pub now: Option<Clock>,
    pub store: Option<piik_room::Store>,
    pub room: Option<CreatedRoom>,
    pub after_func: Option<AfterFunc>,
    pub sfu: Option<SfuFallback>,
    pub sfu_only: bool,
    pub route_debug: Option<crate::server::RouteDebugSink>,
}

fn or_value<T: PartialEq + Default>(value: T, fallback: T) -> T {
    if value == T::default() {
        fallback
    } else {
        value
    }
}

/// A store at full room capacity, optionally persistent.
pub(crate) fn new_store(
    max_viewers_per_room: usize,
    database: Option<piik_room::Database>,
) -> piik_room::Store {
    let persistent = database.is_some();
    let mut store = piik_room::Store::new(piik_room::Options {
        max_rooms: piik_room::CAPACITY,
        max_viewers_per_room,
        database,
        ..Default::default()
    })
    .expect("store");
    if persistent {
        store.initialize().expect("initialize store");
    }
    store
}

/// `roomStore.createRoom()` on a store no server guards yet.
pub(crate) fn create_store_room(
    store: &mut piik_room::Store,
    policy: CodeEntryPolicy,
    password: &str,
) -> CreatedRoom {
    store.begin_create_room().expect("begin create room");
    let derived = if password.is_empty() {
        Ok(None)
    } else {
        store
            .password_deriver()
            .derive_viewer_password_material(password, None)
            .map(Some)
    };
    store
        .create_room(policy, derived, None)
        .expect("create room")
}

pub(crate) struct Harness {
    pub runtime: tokio::runtime::Runtime,
    pub server: Arc<Server>,
    pub room: CreatedRoom,
    pub ws_url: String,
    pub address: std::net::SocketAddr,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    closed: bool,
}

impl Harness {
    pub(crate) fn start(options: HarnessOptions) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("runtime");
        let max_viewers = or_value(options.max_viewers_per_room, 8);
        let mut store = options
            .store
            .unwrap_or_else(|| new_store(max_viewers, None));
        let room = options
            .room
            .unwrap_or_else(|| create_store_room(&mut store, CodeEntryPolicy::Open, ""));
        let config = piik_config::Config {
            stun_urls: options.stun_urls.clone(),
            nat_prediction_enabled: options.nat_prediction_enabled,
            ..Default::default()
        };
        let mut server_options =
            base_options(store, or_value(options.endpoint_media_copy_capacity, 2));
        server_options.sfu_fallback = options.sfu;
        server_options.sfu_only = options.sfu_only;
        server_options.ice = piik_config::ice_config(&config);
        server_options.nat_prediction_enabled = options.nat_prediction_enabled;
        server_options.allowed_origins = HashSet::from([ALLOWED_ORIGIN.to_owned()]);
        let site_password = options.site_access_password;
        server_options.site_access_at_upgrade =
            Some(Arc::new(move |headers: &axum::http::HeaderMap| {
                !site_password
                    || headers
                        .get(axum::http::header::COOKIE)
                        .and_then(|value| value.to_str().ok())
                        .is_some_and(|cookie| cookie.contains(SITE_ACCESS_COOKIE))
            }));
        server_options.public_base_url =
            url::Url::parse("https://share.example.test").expect("url");
        server_options.now = options.now;
        server_options.authentication_timeout_ms = or_value(options.authentication_timeout_ms, 500);
        server_options.viewer_disconnect_grace_ms =
            or_value(options.viewer_disconnect_grace_ms, 50);
        server_options.heartbeat_interval_ms = 60_000;
        server_options.room_empty_timeout_ms = options.room_empty_timeout_ms;
        server_options.max_connections = options.max_connections;
        server_options.max_unauthenticated_connections = options.max_unauthenticated_connections;
        server_options.after_func = options.after_func;
        server_options.route_debug = options.route_debug;
        let (server, listener) = runtime.block_on(async {
            let server = Server::new(server_options).expect("signal server");
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .expect("bind");
            (server, listener)
        });
        let address = listener.local_addr().expect("address");
        let (shutdown, stop) = tokio::sync::oneshot::channel::<()>();
        let app = server.router();
        runtime.spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = stop.await;
                })
                .await;
        });
        Self {
            runtime,
            server,
            room,
            ws_url: format!("ws://{address}/signal"),
            address,
            shutdown: Some(shutdown),
            closed: false,
        }
    }

    /// `runningServer.close()`: the signaling server first, then the listener
    /// and the store. Idempotent.
    pub(crate) fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        let server = self.server.clone();
        let result = self
            .runtime
            .block_on(async move { server.close(Duration::from_secs(5)).await });
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let _ = self.server.close_storage();
        result.expect("server close");
    }

    pub(crate) fn locked<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        locked_with_io(&self.server.core, f)
    }

    pub(crate) fn session_count(&self) -> usize {
        self.locked(|core| core.sessions.len())
    }

    pub(crate) fn store_size(&self) -> usize {
        self.locked(|core| core.store.size())
    }

    pub(crate) fn update_room_access(
        &self,
        request: RoomAccessUpdateRequest,
    ) -> RoomAccessUpdateResponse {
        let server = self.server.clone();
        let room = self.room.clone();
        self.runtime
            .block_on(async move {
                server
                    .update_room_access(&room.room_id, &room.host_token, request)
                    .await
            })
            .expect("update room access")
    }

    pub(crate) fn try_update_room_access(
        &self,
        room: &CreatedRoom,
        request: RoomAccessUpdateRequest,
    ) -> Result<RoomAccessUpdateResponse, piik_room::Error> {
        let server = self.server.clone();
        let room = room.clone();
        self.runtime.block_on(async move {
            server
                .update_room_access(&room.room_id, &room.host_token, request)
                .await
        })
    }

    pub(crate) fn create_room(&self, policy: CodeEntryPolicy, password: &str) -> CreatedRoom {
        self.try_create_room(policy, password, None)
            .expect("create room")
    }

    pub(crate) fn try_create_room(
        &self,
        policy: CodeEntryPolicy,
        password: &str,
        preferred: Option<&str>,
    ) -> Result<CreatedRoom, piik_room::Error> {
        let server = self.server.clone();
        let password = (!password.is_empty()).then(|| password.to_owned());
        let preferred = preferred.map(str::to_owned);
        self.runtime
            .block_on(async move { server.create_room(policy, password, preferred).await })
    }

    pub(crate) fn open_client(&self) -> TestClient {
        self.open_client_with_cookie("")
    }

    pub(crate) fn open_client_with_cookie(&self, cookie: &str) -> TestClient {
        self.dial(ALLOWED_ORIGIN, cookie).expect("websocket dial")
    }

    /// Connects, returning the HTTP status of a rejected upgrade.
    pub(crate) fn dial(&self, origin: &str, cookie: &str) -> Result<TestClient, u16> {
        let mut request = self.ws_url.as_str().into_client_request().expect("request");
        if !origin.is_empty() {
            request
                .headers_mut()
                .insert("Origin", HeaderValue::from_str(origin).expect("origin"));
        }
        if !cookie.is_empty() {
            request
                .headers_mut()
                .insert("Cookie", HeaderValue::from_str(cookie).expect("cookie"));
        }
        let connected = self.runtime.block_on(async {
            tokio::time::timeout(
                Duration::from_secs(5),
                tokio_tungstenite::connect_async(request),
            )
            .await
        });
        match connected {
            Ok(Ok((socket, _))) => Ok(TestClient::spawn(&self.runtime, socket)),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                Err(response.status().as_u16())
            }
            Ok(Err(error)) => panic!("upgrade failed without an HTTP response: {error}"),
            Err(_) => panic!("upgrade timed out"),
        }
    }

    pub(crate) fn rejected_upgrade_status(&self, origin: &str) -> u16 {
        match self.dial(origin, "") {
            Ok(_) => panic!("WebSocket upgrade unexpectedly succeeded"),
            Err(status) => status,
        }
    }

    /// Sends a raw upgrade request and reads the whole response.
    pub(crate) fn raw_upgrade_response(&self, request_target: &str) -> String {
        use std::io::{Read, Write};
        let mut stream =
            std::net::TcpStream::connect_timeout(&self.address, Duration::from_secs(2))
                .expect("dial");
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let request = [
            format!("GET {request_target} HTTP/1.1"),
            format!("Host: {}", self.address),
            "Upgrade: websocket".to_owned(),
            "Connection: Upgrade".to_owned(),
            "Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==".to_owned(),
            "Sec-WebSocket-Version: 13".to_owned(),
            format!("Origin: {ALLOWED_ORIGIN}"),
            String::new(),
            String::new(),
        ]
        .join("\r\n");
        stream.write_all(request.as_bytes()).expect("write");
        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        String::from_utf8_lossy(&response).into_owned()
    }

    /// A client-initiated close that also waits for the server to release
    /// the session.
    pub(crate) fn close_client(&self, client: &TestClient) {
        if client.is_closed() {
            return;
        }
        let before = self.session_count();
        client.close(1000, "");
        client.close_code();
        assert!(
            wait_for(DEFAULT_WAIT, || self.session_count() < before),
            "server did not release the closed session"
        );
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            self.close();
        } else {
            self.closed = true;
        }
    }
}

pub(crate) fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// One received text message.
#[derive(Debug, Clone)]
pub(crate) struct Inbound {
    pub typ: String,
    pub raw: String,
}

impl Inbound {
    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.raw).expect("server message JSON")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CloseEvent {
    pub code: i32,
    pub reason: String,
}

#[derive(Default)]
struct Inbox {
    messages: Vec<Inbound>,
    ignored: HashSet<String>,
    decode_errors: Vec<String>,
    closed: Option<CloseEvent>,
}

enum Outgoing {
    Text(String),
    Close(u16, String),
}

/// MessageInbox plus the socket.
pub(crate) struct TestClient {
    inbox: Arc<Mutex<Inbox>>,
    outgoing: mpsc::UnboundedSender<Outgoing>,
    changed: Arc<std::sync::Condvar>,
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

impl TestClient {
    fn spawn(runtime: &tokio::runtime::Runtime, socket: Socket) -> Self {
        let inbox = Arc::new(Mutex::new(Inbox::default()));
        let changed = Arc::new(std::sync::Condvar::new());
        let (outgoing, mut queue) = mpsc::unbounded_channel::<Outgoing>();
        let (mut sink, mut stream) = socket.split();
        runtime.spawn(async move {
            while let Some(message) = queue.recv().await {
                let frame = match message {
                    Outgoing::Text(text) => Message::Text(text.into()),
                    Outgoing::Close(code, reason) => {
                        Message::Close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
                            code: code.into(),
                            reason: reason.into(),
                        }))
                    }
                };
                if sink.send(frame).await.is_err() {
                    break;
                }
            }
        });
        let reader_inbox = inbox.clone();
        let reader_changed = changed.clone();
        runtime.spawn(async move {
            let event = loop {
                match stream.next().await {
                    None => {
                        break CloseEvent {
                            code: 1006,
                            reason: String::new(),
                        };
                    }
                    Some(Err(error)) => {
                        break CloseEvent {
                            code: -1,
                            reason: error.to_string(),
                        };
                    }
                    Some(Ok(Message::Close(frame))) => {
                        break frame.map_or(
                            CloseEvent {
                                code: 1005,
                                reason: String::new(),
                            },
                            |frame| CloseEvent {
                                code: i32::from(u16::from(frame.code)),
                                reason: frame.reason.to_string(),
                            },
                        );
                    }
                    Some(Ok(Message::Text(text))) => {
                        let raw = text.to_string();
                        let typ = serde_json::from_str::<Value>(&raw)
                            .ok()
                            .and_then(|value| {
                                value.get("type").and_then(Value::as_str).map(str::to_owned)
                            })
                            .unwrap_or_default();
                        let mut inbox = lock(&reader_inbox);
                        if let Err(error) = decode_server_message(raw.as_bytes()) {
                            inbox.decode_errors.push(format!(
                                "server message {typ:?} failed the wire schema: {error}: {raw}"
                            ));
                        }
                        if !inbox.ignored.contains(&typ) {
                            inbox.messages.push(Inbound { typ, raw });
                        }
                        drop(inbox);
                        reader_changed.notify_all();
                    }
                    Some(Ok(_)) => {}
                }
            };
            lock(&reader_inbox).closed = Some(event);
            reader_changed.notify_all();
        });
        Self {
            inbox,
            outgoing,
            changed,
        }
    }

    pub(crate) fn send_text(&self, text: &str) {
        let _ = self.outgoing.send(Outgoing::Text(text.to_owned()));
    }

    pub(crate) fn send_json(&self, message: Value) {
        self.send_text(&message.to_string());
    }

    pub(crate) fn close(&self, code: u16, reason: &str) {
        let _ = self.outgoing.send(Outgoing::Close(code, reason.to_owned()));
    }

    fn wait_change(&self, deadline: Instant, check: impl FnMut(&mut Inbox) -> bool) -> bool {
        let mut check = check;
        let mut inbox = lock(&self.inbox);
        loop {
            if check(&mut inbox) {
                return true;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return false;
            };
            inbox = self
                .changed
                .wait_timeout(inbox, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    pub(crate) fn try_next(&self, typ: &str, timeout: Duration) -> Option<Inbound> {
        let mut found = None;
        self.wait_change(Instant::now() + timeout, |inbox| {
            if let Some(index) = inbox.messages.iter().position(|message| message.typ == typ) {
                found = Some(inbox.messages.remove(index));
                return true;
            }
            false
        });
        found
    }

    /// `inbox.next(type)`.
    #[track_caller]
    pub(crate) fn next(&self, typ: &str) -> Inbound {
        self.next_within(typ, DEFAULT_WAIT)
    }

    #[track_caller]
    pub(crate) fn next_within(&self, typ: &str, timeout: Duration) -> Inbound {
        self.try_next(typ, timeout)
            .unwrap_or_else(|| panic!("Timed out waiting for {typ}"))
    }

    /// Asserts that no message of one type arrives within `timeout`.
    #[track_caller]
    pub(crate) fn expect_no(&self, typ: &str, timeout: Duration) {
        if let Some(message) = self.try_next(typ, timeout) {
            panic!("Unexpected {typ}: {}", message.raw);
        }
    }

    /// `inbox.ignore(type)`.
    pub(crate) fn ignore(&self, typ: &str) {
        let mut inbox = lock(&self.inbox);
        inbox.ignored.insert(typ.to_owned());
        inbox.messages.retain(|message| message.typ != typ);
    }

    /// The first buffered message type, if any.
    pub(crate) fn first_type(&self) -> Option<String> {
        lock(&self.inbox)
            .messages
            .first()
            .map(|message| message.typ.clone())
    }

    /// Waits for the socket's close event.
    #[track_caller]
    pub(crate) fn close_code(&self) -> CloseEvent {
        let mut event = None;
        self.wait_change(Instant::now() + DEFAULT_WAIT, |inbox| {
            event = inbox.closed.clone();
            event.is_some()
        });
        event.expect("Timed out waiting for the socket to close")
    }

    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.inbox).closed.is_some()
    }

    pub(crate) fn decode_errors(&self) -> Vec<String> {
        lock(&self.inbox).decode_errors.clone()
    }
}

impl Drop for TestClient {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let errors = self.decode_errors();
            assert!(errors.is_empty(), "{}", errors.join("\n"));
        }
    }
}

#[track_caller]
pub(crate) fn expect_close(client: &TestClient, code: i32, reason: &str) {
    let event = client.close_code();
    assert_eq!(
        (event.code, event.reason.as_str()),
        (code, reason),
        "close event"
    );
}

#[track_caller]
pub(crate) fn expect_close_code(client: &TestClient, code: i32) {
    let event = client.close_code();
    assert_eq!(event.code, code, "close event {event:?}");
}

// ---------------------------------------------------------------------------
// JSON assertions (toEqual / toMatchObject on decoded JSON)
// ---------------------------------------------------------------------------

/// `toMatchObject`: objects subset-match recursively, arrays need equal
/// length, scalars must be equal (numbers by value).
pub(crate) fn matches(actual: &Value, expected: &Value) -> bool {
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) => expected
            .iter()
            .all(|(key, value)| actual.get(key).is_some_and(|actual| matches(actual, value))),
        (Value::Array(actual), Value::Array(expected)) => {
            actual.len() == expected.len()
                && actual.iter().zip(expected).all(|(a, e)| matches(a, e))
        }
        (Value::Number(actual), Value::Number(expected)) => actual.as_f64() == expected.as_f64(),
        _ => actual == expected,
    }
}

#[track_caller]
pub(crate) fn expect_equal(raw: &str, expected: &str) {
    let actual: Value = serde_json::from_str(raw).expect("actual JSON");
    let expected_value: Value = serde_json::from_str(expected).expect("expected JSON");
    assert!(
        matches(&actual, &expected_value) && matches(&expected_value, &actual),
        "expected exactly {expected}\ngot {raw}"
    );
}

#[track_caller]
pub(crate) fn expect_match(raw: &str, expected: &str) {
    let actual: Value = serde_json::from_str(raw).expect("actual JSON");
    let expected_value: Value = serde_json::from_str(expected).expect("expected JSON");
    assert!(
        matches(&actual, &expected_value),
        "expected a match of {expected}\ngot {raw}"
    );
}

pub(crate) fn has_key(raw: &str, key: &str) -> bool {
    serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|value| value.as_object().map(|object| object.contains_key(key)))
        .unwrap_or(false)
}
