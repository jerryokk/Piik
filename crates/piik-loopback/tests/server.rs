//! Loopback server, site-access and quality-preference tests. Shared fixture
//! and HTTP helpers live here; each scenario family has its own file.

#[path = "server/control.rs"]
mod control;
#[path = "server/discovery.rs"]
mod discovery;
#[path = "server/http.rs"]
mod http;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use piik_app_config::QualityPreference;
use piik_loopback::{
    BoxFuture, CONTROL_SUBPROTOCOL, CancellationToken, ControlEvent, ControlFactory,
    ControlMessage, ControlSession, EventSink, Health, NativeMediaCapabilities, Options,
    PROTOCOL_VERSION, ProtocolViolation, SERVICE_NAME, Server, buildinfo, decode_envelope,
};
use piik_protocol::{DEFAULT_QUALITY_SETTINGS, QualitySettings};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const TEST_ORIGIN: &str = "https://share.example.com";
const SECOND: Duration = Duration::from_secs(1);

type Socket = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(SECOND * 3)
        .build()
        .unwrap()
}

// The server walks the range and binds on its own, so tests never probe a port
// and then race another test for it. The range is above the fixed listeners.
const TEST_PORT_START: u16 = 39_000;
const TEST_PORT_END: u16 = 39_999;

fn start(mut options: Options) -> Server {
    options.port_start = TEST_PORT_START;
    options.port_end = TEST_PORT_END;
    Server::start(&CancellationToken::new(), options).unwrap()
}

fn start_with_origin(origin: &str) -> Server {
    start(Options {
        allowed_origins: vec![origin.to_owned()],
        ..Options::default()
    })
}

async fn health(server: &Server, host: &str, origin: &str) -> reqwest::Response {
    let mut request = client()
        .get(format!("{}/health", server.endpoint().url))
        .header("Host", host);
    if !origin.is_empty() {
        request = request.header("Origin", origin);
    }
    request.send().await.unwrap()
}

async fn try_dial(server: &Server, origin: &str) -> Result<Socket, tungstenite::Error> {
    let endpoint = server.endpoint();
    let url = endpoint.url.replacen("http://", "ws://", 1) + "/control";
    let mut request = url.into_client_request().unwrap();
    let headers = request.headers_mut();
    headers.insert("Origin", HeaderValue::from_str(origin).unwrap());
    headers.insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_str(&format!(
            "{CONTROL_SUBPROTOCOL}.{}",
            endpoint.instance_token
        ))
        .unwrap(),
    );
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(socket, _)| socket)
}

async fn dial(server: &Server, origin: &str) -> Socket {
    try_dial(server, origin).await.unwrap()
}

fn request(id: &str, kind: &str) -> String {
    json!({ "version": PROTOCOL_VERSION, "id": id, "type": kind }).to_string()
}

async fn write(socket: &mut Socket, id: &str, kind: &str) {
    socket.send(Message::text(request(id, kind))).await.unwrap();
}

async fn read(socket: &mut Socket) -> ControlMessage {
    loop {
        let message = tokio::time::timeout(SECOND, socket.next())
            .await
            .expect("control read timed out")
            .expect("control closed")
            .expect("control failed");
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

async fn hello(socket: &mut Socket) -> ControlMessage {
    write(socket, "request_hello", "hello").await;
    read(socket).await
}

/// Resolves to whether the socket ends (close or error) within one second.
async fn ends(socket: &mut Socket) -> bool {
    let ended = async {
        loop {
            match socket.next().await {
                None | Some(Err(_) | Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            }
        }
    };
    tokio::time::timeout(SECOND, ended).await.is_ok()
}

async fn signalled(token: &CancellationToken) -> bool {
    tokio::time::timeout(SECOND, token.cancelled())
        .await
        .is_ok()
}

/// A control session that answers `extension` and defers `deferred`.
#[derive(Clone, Default)]
struct TestSession {
    closed: CancellationToken,
    finish: Option<CancellationToken>,
}

impl ControlSession for TestSession {
    fn handle<'a>(
        &'a self,
        _: &'a CancellationToken,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<Option<ControlEvent>, ProtocolViolation>> {
        Box::pin(async move {
            let message = decode_envelope(payload)
                .map_err(|_| ProtocolViolation::new("unexpected extension request"))?;
            match message.kind.as_str() {
                "deferred" => Ok(None),
                "extension" => Ok(Some(event(&message.id, "extension-response"))),
                _ => Err(ProtocolViolation::new("unexpected extension request")),
            }
        })
    }

    fn close(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.closed.cancel();
            if let Some(finish) = &self.finish {
                finish.cancelled().await;
            }
        })
    }
}

fn event(id: &str, kind: &str) -> ControlEvent {
    ControlEvent::from_serialize(&ControlMessage::new(id, kind)).unwrap()
}

/// Records each created session and its event sink.
fn recording_factory(
    finish: Option<CancellationToken>,
) -> (
    ControlFactory,
    mpsc::UnboundedReceiver<(TestSession, EventSink)>,
) {
    let (created, receiver) = mpsc::unbounded_channel();
    let factory: ControlFactory = Arc::new(move |sink| {
        let session = TestSession {
            closed: CancellationToken::new(),
            finish: finish.clone(),
        };
        created.send((session.clone(), sink)).unwrap();
        Some(Box::new(session) as Box<dyn ControlSession>)
    });
    (factory, receiver)
}

async fn created(
    receiver: &mut mpsc::UnboundedReceiver<(TestSession, EventSink)>,
) -> (TestSession, EventSink) {
    tokio::time::timeout(SECOND, receiver.recv())
        .await
        .expect("accepted control did not acquire its own session")
        .unwrap()
}
