//! Server app tests. Route tests drive [`Inner::dispatch`] in memory;
//! listener and lifecycle tests use real sockets.

mod access;
mod app;
mod lifecycle;
mod sfu;
mod static_files;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, Uri};
use piik_config::{Config, Environment};
use piik_sfu::runtime::{
    IceCandidateInit, MediaError, PublicationMedia, ResourceFence, Runtime, SessionDescription,
    SubscriptionFence,
};
use serde_json::Value;
use url::Url;

use crate::server::Inner;
use crate::{MediaEvents, Options, Server, SfuMedia, SfuPublicAddress};

pub(crate) const ALLOWED_ORIGIN: &str = "http://allowed.test";
pub(crate) const TEST_ACCESS_PASSWORD: &str = "instance-access-password";
pub(crate) const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn test_config() -> Config {
    Config {
        env: Environment::Development,
        port: 0,
        listen_host: "127.0.0.1".to_owned(),
        public_base_url: Url::parse("https://share.example.test").expect("test URL"),
        allowed_origins: HashSet::from([ALLOWED_ORIGIN.to_owned()]),
        site_access_password: TEST_ACCESS_PASSWORD.to_owned(),
        max_viewers_per_room: 8,
        endpoint_media_copy_capacity: 2,
        nat_prediction_enabled: false,
        ..Config::default()
    }
}

/// Options over [`test_config`] with the heartbeat out of the way.
pub(crate) fn options(config: Config) -> Options {
    Options {
        config,
        heartbeat_interval_ms: 60_000,
        ..Options::default()
    }
}

pub(crate) fn new_server(mut options: Options) -> Server {
    if options.heartbeat_interval_ms == 0 {
        options.heartbeat_interval_ms = 60_000;
    }
    Server::new(options).expect("Server::new")
}

/// A started server and the port its first listener took.
pub(crate) struct Harness {
    pub server: Server,
    pub port: u16,
}

pub(crate) async fn start(options: Options) -> Harness {
    let server = new_server(options);
    let port = server.listen().await.expect("listen");
    Harness { server, port }
}

pub(crate) fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("probe port")
        .port()
}

pub(crate) fn assert_port_free(port: u16) {
    std::net::TcpListener::bind(("127.0.0.1", port))
        .unwrap_or_else(|error| panic!("port {port} is still owned: {error}"));
}

/// One request through the application handler.
#[derive(Clone)]
pub(crate) struct Req {
    method: Method,
    path: String,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
    tls: bool,
}

impl Req {
    pub(crate) fn new(method: &str, path: &str) -> Self {
        Self {
            method: Method::from_bytes(method.as_bytes()).expect("test method"),
            path: path.to_owned(),
            headers: Vec::new(),
            body: None,
            tls: false,
        }
    }

    pub(crate) fn get(path: &str) -> Self {
        Self::new("GET", path)
    }

    pub(crate) fn post(path: &str) -> Self {
        Self::new("POST", path)
    }

    pub(crate) fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    pub(crate) fn origin(self, origin: &str) -> Self {
        if origin.is_empty() {
            self
        } else {
            self.header("Origin", origin)
        }
    }

    pub(crate) fn cookie(self, cookie: &str) -> Self {
        if cookie.is_empty() {
            self
        } else {
            self.header("Cookie", cookie)
        }
    }

    pub(crate) fn bearer(self, token: &str) -> Self {
        if token.is_empty() {
            self
        } else {
            self.header("Authorization", &format!("Bearer {token}"))
        }
    }

    pub(crate) fn host(self, host: &str) -> Self {
        self.header("Host", host)
    }

    pub(crate) fn body(mut self, content_type: &str, payload: &str) -> Self {
        if !content_type.is_empty() {
            self = self.header("Content-Type", content_type);
        }
        self.body = Some(payload.as_bytes().to_vec());
        self
    }

    pub(crate) fn json(self, payload: &str) -> Self {
        self.body("application/json", payload)
    }

    pub(crate) fn build(&self) -> Request<Body> {
        let uri = Uri::builder()
            .path_and_query(self.path.as_str())
            .build()
            .expect("test path");
        let mut request = Request::builder().method(self.method.clone()).uri(uri);
        for (name, value) in &self.headers {
            request = request.header(name, value);
        }
        if let Some(body) = &self.body {
            request = request.header("Content-Length", body.len());
        }
        request
            .body(
                self.body
                    .clone()
                    .map(Body::from)
                    .unwrap_or_else(Body::empty),
            )
            .expect("test request")
    }
}

/// A buffered response.
#[derive(Debug)]
pub(crate) struct Reply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: String,
}

impl Reply {
    pub(crate) async fn read(response: axum::http::Response<Body>) -> Self {
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body");
        Self {
            status,
            headers,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    #[track_caller]
    pub(crate) fn expect(&self, status: u16, body: &str) -> &Self {
        assert_eq!(self.status, status, "status (body {})", self.body);
        assert_eq!(self.body, body, "body");
        self
    }

    #[track_caller]
    pub(crate) fn expect_status(&self, status: u16) -> &Self {
        assert_eq!(self.status, status, "status (body {})", self.body);
        self
    }

    #[track_caller]
    pub(crate) fn expect_header(&self, name: &str, want: &str) -> &Self {
        assert_eq!(self.header(name), want, "header {name}");
        self
    }

    pub(crate) fn header(&self, name: &str) -> String {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_owned()
    }

    pub(crate) fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|error| panic!("decode {}: {error}", self.body))
    }

    pub(crate) fn room(&self) -> Room {
        let value = self.json();
        let text = |key: &str| {
            value[key]
                .as_str()
                .unwrap_or_else(|| panic!("{key} in {}", self.body))
                .to_owned()
        };
        Room {
            room_id: text("roomId"),
            host_token: text("hostToken"),
            invite_url: text("inviteUrl"),
            code_entry_policy: text("codeEntryPolicy"),
        }
    }
}

/// A decoded `CreateRoomResponse`.
#[derive(Debug, Clone)]
pub(crate) struct Room {
    pub room_id: String,
    pub host_token: String,
    pub invite_url: String,
    pub code_entry_policy: String,
}

/// `name=value` of a Set-Cookie header, which is what a browser sends back.
#[track_caller]
pub(crate) fn cookie_pair(set_cookie: &str) -> String {
    assert!(!set_cookie.is_empty(), "expected a Set-Cookie value");
    set_cookie.split(';').next().unwrap_or_default().to_owned()
}

pub(crate) async fn call(inner: &Arc<Inner>, request: &Req) -> Reply {
    Reply::read(inner.dispatch(request.build(), request.tls).await).await
}

#[derive(Default)]
pub(crate) struct RoomRequest<'a> {
    pub cookie: &'a str,
    pub code_entry_policy: &'a str,
    pub password: Option<&'a str>,
    pub preferred_room_id: &'a str,
}

impl Harness {
    pub(crate) async fn send(&self, request: Req) -> Reply {
        call(self.server.inner(), &request).await
    }

    pub(crate) async fn login(&self) -> Reply {
        self.send(
            Req::post("/api/site-access")
                .bearer(TEST_ACCESS_PASSWORD)
                .origin(ALLOWED_ORIGIN),
        )
        .await
    }

    pub(crate) async fn cookie(&self) -> String {
        let login = self.login().await;
        login.expect_status(200);
        cookie_pair(&login.header("set-cookie"))
    }

    pub(crate) async fn create_room(&self, request: RoomRequest<'_>) -> Reply {
        let policy = if request.code_entry_policy.is_empty() {
            "open"
        } else {
            request.code_entry_policy
        };
        let mut body = serde_json::Map::new();
        body.insert("codeEntryPolicy".into(), policy.into());
        if let Some(password) = request.password {
            body.insert("roomPassword".into(), password.into());
        }
        if !request.preferred_room_id.is_empty() {
            body.insert("preferredRoomId".into(), request.preferred_room_id.into());
        }
        self.send(
            Req::post("/api/rooms")
                .origin(ALLOWED_ORIGIN)
                .cookie(request.cookie)
                .json(&Value::Object(body).to_string()),
        )
        .await
    }

    pub(crate) async fn update_access(
        &self,
        room_id: &str,
        host_token: &str,
        body: &str,
        cookie: &str,
    ) -> Reply {
        self.send(
            Req::post(&format!("/api/rooms/{room_id}/access"))
                .origin(ALLOWED_ORIGIN)
                .cookie(cookie)
                .bearer(host_token)
                .json(body),
        )
        .await
    }

    pub(crate) async fn replace_room(
        &self,
        room_id: &str,
        host_token: &str,
        body: &str,
        cookie: &str,
    ) -> Reply {
        self.send(
            Req::post(&format!("/api/rooms/{room_id}/replacement"))
                .origin(ALLOWED_ORIGIN)
                .cookie(cookie)
                .bearer(host_token)
                .json(body),
        )
        .await
    }

    pub(crate) async fn close(&self) {
        self.server.close(CLOSE_TIMEOUT).await.expect("close");
    }
}

pub(crate) async fn wait_for(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// A media runtime that admits nothing; signaling only needs its presence.
pub(crate) struct NoMedia;

impl Runtime for NoMedia {
    fn has_publication(&self, _: &ResourceFence, _: &str) -> bool {
        false
    }
    fn prepare_publication(
        &self,
        _: &ResourceFence,
        _: &str,
        _: &PublicationMedia,
    ) -> Result<(), MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn update_publication(
        &self,
        _: &ResourceFence,
        _: &str,
        _: &PublicationMedia,
    ) -> Result<(), MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn publication_demand(&self, _: &ResourceFence, _: &str) -> Result<i64, MediaError> {
        Ok(0)
    }
    fn accept_publisher_offer(
        &self,
        _: &ResourceFence,
        _: &str,
        _: SessionDescription,
    ) -> Result<SessionDescription, MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn add_publisher_ice(
        &self,
        _: &ResourceFence,
        _: &str,
        _: IceCandidateInit,
    ) -> Result<(), MediaError> {
        Ok(())
    }
    fn prepare_subscriber(
        &self,
        _: Instant,
        _: &SubscriptionFence,
        _: &str,
    ) -> Result<SessionDescription, MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn apply_subscriber_answer(
        &self,
        _: &SubscriptionFence,
        _: &str,
        _: SessionDescription,
    ) -> Result<(), MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn add_subscriber_ice(
        &self,
        _: &SubscriptionFence,
        _: &str,
        _: IceCandidateInit,
    ) -> Result<(), MediaError> {
        Ok(())
    }
    fn restart_subscriber(
        &self,
        _: &SubscriptionFence,
        _: &str,
    ) -> Result<SessionDescription, MediaError> {
        Err(MediaError::failed("no media"))
    }
    fn close_subscription(&self, _: &SubscriptionFence, _: &str) -> Result<(), MediaError> {
        Ok(())
    }
    fn close_publication(&self, _: &ResourceFence) -> Result<(), MediaError> {
        Ok(())
    }
}

pub(crate) fn no_media() -> SfuMedia {
    SfuMedia {
        runtime: Arc::new(NoMedia),
        events: MediaEvents::new(),
        public_address: SfuPublicAddress::new(),
        prepare_timeout_ms: 0,
    }
}

pub(crate) fn sfu_config(public_ip: &str, only: bool) -> piik_config::SfuConfig {
    piik_config::SfuConfig {
        listen_host: std::net::Ipv4Addr::LOCALHOST,
        port: 0,
        public_ip: public_ip.to_owned(),
        only,
    }
}
