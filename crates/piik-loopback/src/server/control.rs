//! The strict `/control` WebSocket: claim, handshake, writer and read loop.

use std::sync::Arc;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts, Request};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use futures_util::stream::{SplitSink, SplitStream};
use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::{
    ClaimGuard, HELLO_TIMEOUT, Inner, MAX_CONTROL_MESSAGE_BYTES, SHUTDOWN_TIMEOUT, encode_message,
    header_text, method_not_allowed, text_error,
};
use crate::protocol::{
    CONTROL_SUBPROTOCOL, ControlMessage, decode_envelope, decode_request, validate_hello,
    validate_ping,
};
use crate::session::{ControlEvent, ControlSession, EventSink};

const CONTROL_QUEUE_SIZE: usize = 64;

pub(super) async fn handle_control(inner: Arc<Inner>, request: Request) -> Response {
    if request.method() != Method::GET {
        return method_not_allowed("GET");
    }
    let origin = header_text(request.headers(), header::ORIGIN).to_owned();
    let Some(claim) = inner.claim_connection(&origin) else {
        return text_error(StatusCode::CONFLICT, "native control capacity is occupied");
    };
    let guard = ClaimGuard { inner, claim };
    let (mut parts, _) = request.into_parts();
    let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let expected = format!(
        "{CONTROL_SUBPROTOCOL}.{}",
        guard.inner.endpoint.instance_token
    );
    upgrade
        .protocols([expected.clone()])
        .max_message_size(MAX_CONTROL_MESSAGE_BYTES)
        .max_frame_size(MAX_CONTROL_MESSAGE_BYTES)
        .on_upgrade(move |socket| run_control(guard, socket, expected))
}

/// One socket writer shared by responses and session events.
struct Writer {
    sink: tokio::sync::Mutex<SplitSink<WebSocket, Message>>,
    cancel: CancellationToken,
}

impl Writer {
    async fn write(&self, event: ControlEvent) -> Result<(), ()> {
        let mut sink = self.sink.lock().await;
        if event.current.as_ref().is_some_and(|current| !current()) {
            return Ok(());
        }
        if let Some(before_write) = event.before_write {
            before_write();
        }
        let payload = encode_message(&event.value).map_err(drop)?;
        tokio::select! {
            result = sink.send(Message::Text(payload.into())) => result.map_err(drop),
            () = self.cancel.cancelled() => Err(()),
        }
    }

    async fn message(&self, id: &str, kind: &str) -> Result<(), ()> {
        let value = serde_json::to_value(ControlMessage::new(id, kind)).map_err(drop)?;
        self.write(ControlEvent::new(value)).await
    }

    async fn close(&self, reason: &'static str) {
        if self.cancel.is_cancelled() {
            return;
        }
        let mut sink = self.sink.lock().await;
        let frame = Message::Close(Some(CloseFrame {
            code: axum::extract::ws::close_code::POLICY,
            reason: reason.into(),
        }));
        let _ = tokio::time::timeout(SHUTDOWN_TIMEOUT, sink.send(frame)).await;
    }
}

/// The next data frame; ping/pong are answered by the WebSocket layer.
async fn next_data(stream: &mut SplitStream<WebSocket>) -> Option<Message> {
    loop {
        match stream.next().await {
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
            Some(Ok(Message::Close(_)) | Err(_)) | None => return None,
            Some(Ok(message)) => return Some(message),
        }
    }
}

async fn run_control(guard: ClaimGuard, socket: WebSocket, expected: String) {
    let cancel = guard.claim.cancel.clone();
    let selected = socket.protocol().map(HeaderValue::as_bytes) == Some(expected.as_bytes());
    let (sink, mut stream) = socket.split();
    let writer = Arc::new(Writer {
        sink: tokio::sync::Mutex::new(sink),
        cancel: cancel.clone(),
    });
    if !selected {
        writer.close("unsupported control protocol").await;
        return;
    }
    if !guard.inner.set_connection(&guard.claim) {
        return;
    }
    let hello = tokio::select! {
        () = cancel.cancelled() => return,
        hello = tokio::time::timeout(HELLO_TIMEOUT, next_data(&mut stream)) => hello,
    };
    let Ok(Some(hello)) = hello else { return };
    let Message::Text(hello) = hello else {
        writer.close("hello frame required").await;
        return;
    };
    let Ok(hello) = decode_request(hello.as_bytes()) else {
        writer.close("invalid control message").await;
        return;
    };
    if validate_hello(&hello).is_err() {
        writer.close("hello required").await;
        return;
    }
    if writer.message(&hello.id, "ready").await.is_err() {
        return;
    }
    let (events_sink, mut events) = EventSink::channel();
    let session = guard
        .inner
        .new_control
        .as_ref()
        .and_then(|factory| factory(events_sink));
    let event_writer = writer.clone();
    let event_cancel = cancel.clone();
    let event_task = tokio::spawn(async move {
        loop {
            let event = tokio::select! {
                () = event_cancel.cancelled() => return,
                event = events.recv() => event,
            };
            let Some(event) = event else { return };
            if event_writer.write(event).await.is_err() {
                event_cancel.cancel();
                return;
            }
        }
    });
    let (reads_sender, mut reads) =
        mpsc::channel::<axum::extract::ws::Utf8Bytes>(CONTROL_QUEUE_SIZE);
    let read_cancel = cancel.clone();
    let reader = tokio::spawn(async move {
        loop {
            let message = tokio::select! {
                () = read_cancel.cancelled() => return,
                message = next_data(&mut stream) => message,
            };
            let Some(Message::Text(payload)) = message else {
                read_cancel.cancel();
                return;
            };
            tokio::select! {
                () = read_cancel.cancelled() => return,
                sent = reads_sender.send(payload) => if sent.is_err() { return },
            }
        }
    });
    control_loop(&writer, session.as_deref(), &cancel, &mut reads).await;
    cancel.cancel();
    let _ = reader.await;
    let _ = event_task.await;
    drop(writer);
    if let Some(session) = session {
        session.close().await;
    }
    drop(guard);
}

async fn control_loop(
    writer: &Writer,
    session: Option<&dyn ControlSession>,
    cancel: &CancellationToken,
    reads: &mut mpsc::Receiver<axum::extract::ws::Utf8Bytes>,
) {
    loop {
        let payload = tokio::select! {
            () = cancel.cancelled() => return,
            payload = reads.recv() => payload,
        };
        let Some(payload) = payload else { return };
        if cancel.is_cancelled() {
            return;
        }
        let payload = payload.as_bytes();
        let Ok(message) = decode_envelope(payload) else {
            writer.close("invalid control message").await;
            return;
        };
        if message.kind == "ping" {
            if !decode_request(payload).is_ok_and(|strict| validate_ping(&strict).is_ok()) {
                writer.close("invalid control message").await;
                return;
            }
            if writer.message(&message.id, "pong").await.is_err() {
                return;
            }
            continue;
        }
        let Some(session) = session else {
            writer.close("unsupported control message").await;
            return;
        };
        match session.handle(cancel, payload).await {
            Err(_) => {
                writer.close("invalid control message").await;
                return;
            }
            Ok(None) => {}
            Ok(Some(response)) => {
                if writer.write(response).await.is_err() {
                    return;
                }
            }
        }
    }
}
