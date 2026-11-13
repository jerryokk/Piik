//! One signaling connection: admission state, the bounded outbound queue and
//! its reader and writer tasks.
//!
//! Handlers enqueue outgoing frames under the authority lock; only the writer
//! drains that queue, and it never holds the lock while writing.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use piik_protocol::{ErrorCode, Role, ServerMessage, encode_server_message};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::runtime::{Locker, TimerStop, opaque_id};
use crate::server::{Core, MAX_BUFFERED_SIGNAL_BYTES};

pub(crate) const CLOSE_NORMAL: u16 = 1000;
pub(crate) const CLOSE_POLICY_VIOLATION: u16 = 1008;
pub(crate) const CLOSE_MESSAGE_TOO_BIG: u16 = 1009;
pub(crate) const CLOSE_INTERNAL_ERROR: u16 = 1011;
pub(crate) const CLOSE_SERVICE_RESTART: u16 = 1012;
pub(crate) const CLOSE_SESSION_REPLACED: u16 = 4001;
pub(crate) const CLOSE_AUTHENTICATION_FAILED: u16 = 4003;
pub(crate) const CLOSE_VIEWER_ACCESS_REVOKED: u16 = 4004;

/// How long a queued close waits for the peer's reply before termination.
const CLOSE_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// The admitted identity of a session. `id` identifies this admission:
/// handlers reject work from an authentication replaced between
/// dispatch and use. Empty `share_generation` means no share; `None`
/// `display_name` means the participant has no display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AuthState {
    pub id: u64,
    pub room_id: String,
    pub role: Role,
    pub peer_id: String,
    pub share_generation: String,
    pub display_name: Option<piik_protocol::DisplayName>,
}

/// One queue item.
#[derive(Debug)]
pub(crate) enum Outbound {
    Text(Arc<[u8]>),
    Ping,
    Close(u16, String),
}

/// Every field is guarded by the authority lock; the tokens and the wake
/// handle are shared with the session's tasks.
pub(crate) struct Session {
    pub session_id: String,
    pub alive: bool,
    pub site_access_authenticated: bool,
    pub revoked: bool,
    pub authenticating: bool,
    /// Distinguishes no challenge yet from a timestamp of zero.
    pub challenged: bool,
    pub last_signaling_challenge_at_ms: i64,
    pub authenticated: Option<AuthState>,
    pub remote_input: bool,
    pub input_endpoint: String,
    pub input_secret: String,
    pub next_interaction_at_ms: i64,

    /// Stops the authentication deadline; `auth_generation` is bumped when
    /// the timer is cleared so a callback that lost the stop race is stale.
    pub auth_stop: Option<TimerStop>,
    pub auth_generation: u64,

    pub queue: VecDeque<Outbound>,
    /// Pending outbound text bytes.
    pub queued_bytes: usize,
    pub wake: Arc<Notify>,
    /// Stops further sends while the writer completes the close handshake.
    pub close_queued: bool,
    pub terminated: bool,
    /// Cancelled by `terminate`; both tasks then drop the socket.
    pub cancel: CancellationToken,
    /// Cancelled once the reader ran disconnect handling.
    pub reader_done: CancellationToken,
}

impl Session {
    pub(crate) fn new(site_access_authenticated: bool) -> Self {
        Self {
            session_id: opaque_id(),
            alive: true,
            site_access_authenticated,
            revoked: false,
            authenticating: false,
            challenged: false,
            last_signaling_challenge_at_ms: 0,
            authenticated: None,
            remote_input: false,
            input_endpoint: String::new(),
            input_secret: String::new(),
            next_interaction_at_ms: 0,
            auth_stop: None,
            auth_generation: 0,
            queue: VecDeque::new(),
            queued_bytes: 0,
            wake: Arc::new(Notify::new()),
            close_queued: false,
            terminated: false,
            cancel: CancellationToken::new(),
            reader_done: CancellationToken::new(),
        }
    }

    /// Whether this session still accepts outgoing messages.
    pub(crate) fn open(&self) -> bool {
        !self.close_queued && !self.terminated
    }

    fn enqueue(&mut self, encoded: Arc<[u8]>) {
        self.queued_bytes += encoded.len();
        self.queue.push_back(Outbound::Text(encoded));
        self.wake.notify_one();
    }

    /// Queues a close handshake only while the socket is open.
    pub(crate) fn close(&mut self, code: u16, reason: &str) {
        if !self.open() {
            return;
        }
        self.close_queued = true;
        self.queue
            .push_back(Outbound::Close(code, reason.to_owned()));
        self.wake.notify_one();
    }

    /// Closes abruptly without a close frame.
    pub(crate) fn terminate(&mut self) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        self.cancel.cancel();
    }

    /// Invalidates pending and already-fired callbacks.
    pub(crate) fn clear_authentication_timer(&mut self) {
        if let Some(stop) = self.auth_stop.take() {
            stop();
        }
        self.auth_generation += 1;
    }

    /// Drops unless open, terminates above the buffer bound and otherwise
    /// queues the encoded message.
    pub(crate) fn send_encoded(&mut self, encoded: Arc<[u8]>) {
        if !self.open() {
            return;
        }
        if self.queued_bytes > MAX_BUFFERED_SIGNAL_BYTES {
            self.terminate();
            return;
        }
        self.enqueue(encoded);
    }
}

pub(crate) fn encode(message: &ServerMessage) -> Arc<[u8]> {
    encode_server_message(message).into()
}

impl Core {
    pub(crate) fn session(&self, key: u64) -> Option<&Session> {
        self.sessions.get(&key)
    }

    pub(crate) fn session_mut(&mut self, key: u64) -> Option<&mut Session> {
        self.sessions.get_mut(&key)
    }

    pub(crate) fn session_key(&self, session_id: &str) -> Option<u64> {
        self.sessions_by_id.get(session_id).copied()
    }

    pub(crate) fn session_by_id(&self, session_id: &str) -> Option<&Session> {
        self.session(self.session_key(session_id)?)
    }

    pub(crate) fn auth_of(&self, key: u64) -> Option<&AuthState> {
        self.session(key)?.authenticated.as_ref()
    }

    /// `socketStates.get(socketsBySessionId.get(id))?.authenticated`.
    pub(crate) fn authenticated_of(&self, session_id: &str) -> Option<&AuthState> {
        self.session_by_id(session_id)?.authenticated.as_ref()
    }

    pub(crate) fn send(&mut self, key: u64, message: &ServerMessage) {
        if let Some(session) = self.session_mut(key) {
            session.send_encoded(encode(message));
        }
    }

    pub(crate) fn send_encoded(&mut self, key: u64, encoded: Arc<[u8]>) {
        if let Some(session) = self.session_mut(key) {
            session.send_encoded(encoded);
        }
    }

    pub(crate) fn send_to_session(&mut self, session_id: &str, message: &ServerMessage) {
        if let Some(key) = self.session_key(session_id) {
            self.send(key, message);
        }
    }

    /// Reuses the same encoded bytes for every recipient.
    pub(crate) fn send_encoded_to_session(&mut self, session_id: &str, encoded: Arc<[u8]>) {
        if let Some(key) = self.session_key(session_id) {
            self.send_encoded(key, encoded);
        }
    }

    pub(crate) fn send_error(&mut self, key: u64, code: ErrorCode, message: &str) {
        self.send(
            key,
            &ServerMessage::Error {
                code,
                message: message.to_owned(),
            },
        );
    }

    pub(crate) fn close_session(&mut self, key: u64, code: u16, reason: &str) {
        if let Some(session) = self.session_mut(key) {
            session.close(code, reason);
        }
    }

    pub(crate) fn reject_invalid_message(&mut self, key: u64) {
        self.send_error(key, ErrorCode::InvalidMessage, "Message is invalid");
        self.close_session(key, CLOSE_POLICY_VIOLATION, "Invalid message");
    }

    /// Counts the slot and registers the session; the caller starts its tasks.
    pub(crate) fn accept(&mut self, site_access_authenticated: bool) -> Option<SessionHandles> {
        if !self.has_connection_capacity() {
            return None;
        }
        self.next_session_key += 1;
        let key = self.next_session_key;
        let session = Session::new(site_access_authenticated);
        let handles = SessionHandles {
            key,
            wake: session.wake.clone(),
            cancel: session.cancel.clone(),
            reader_done: session.reader_done.clone(),
        };
        self.sessions_by_id.insert(session.session_id.clone(), key);
        self.sessions.set(key, session);
        self.arm_authentication_timer(key);
        self.unauthenticated_connections += 1;
        Some(handles)
    }

    /// Bounds admission time; the generation rejects a stale callback.
    fn arm_authentication_timer(&mut self, key: u64) {
        let Some(generation) = self.session(key).map(|s| s.auth_generation) else {
            return;
        };
        let locker = self.locker.clone();
        let stop = (self.after_func)(
            Duration::from_millis(self.authentication_timeout_ms),
            Box::new(move || {
                locker.with(|core| {
                    let Some(session) = core.session_mut(key) else {
                        return;
                    };
                    if session.auth_generation != generation {
                        return;
                    }
                    session.auth_stop = None;
                    core.send_error(key, ErrorCode::AuthRequired, "Authentication timed out");
                    core.close_session(key, CLOSE_AUTHENTICATION_FAILED, "Authentication required");
                });
            }),
        );
        if let Some(session) = self.session_mut(key) {
            session.auth_stop = Some(stop);
        }
    }

    fn handle_incoming(&mut self, key: u64, data: &[u8], binary: bool) {
        if binary {
            self.reject_invalid_message(key);
        } else {
            self.handle_message(key, data);
        }
    }

    /// Resets `alive` and returns the sessions to ping, in insertion order;
    /// a session that missed the previous ping is terminated.
    pub(crate) fn heartbeat(&mut self) {
        for key in self.sessions.keys() {
            let session = self.sessions.get_mut(&key).expect("session");
            if !session.alive {
                session.terminate();
                continue;
            }
            session.alive = false;
            if session.open() {
                session.queue.push_front(Outbound::Ping);
                session.wake.notify_one();
            }
        }
    }
}

/// The shared handles of a newly accepted session.
pub(crate) struct SessionHandles {
    pub key: u64,
    pub wake: Arc<Notify>,
    pub cancel: CancellationToken,
    pub reader_done: CancellationToken,
}

/// Runs both tasks of an accepted socket.
pub(crate) fn spawn_session(locker: Locker, handles: SessionHandles, socket: WebSocket) {
    let (sink, stream) = socket.split();
    let writer_done = CancellationToken::new();
    tokio::spawn(write_loop(
        locker.clone(),
        handles.key,
        handles.wake,
        handles.cancel.clone(),
        handles.reader_done.clone(),
        writer_done.clone(),
        sink,
    ));
    tokio::spawn(read_loop(
        locker,
        handles.key,
        handles.cancel,
        handles.reader_done,
        writer_done,
        stream,
    ));
}

fn is_capacity_error(error: &axum::Error) -> bool {
    let mut source: Option<&(dyn std::error::Error + 'static)> = std::error::Error::source(error);
    while let Some(current) = source {
        if let Some(tungstenite) = current.downcast_ref::<tokio_tungstenite::tungstenite::Error>() {
            return matches!(
                tungstenite,
                tokio_tungstenite::tungstenite::Error::Capacity(_)
            );
        }
        source = current.source();
    }
    false
}

/// Reads until the connection fails, then runs disconnect handling once.
async fn read_loop(
    locker: Locker,
    key: u64,
    cancel: CancellationToken,
    reader_done: CancellationToken,
    writer_done: CancellationToken,
    mut stream: futures_util::stream::SplitStream<WebSocket>,
) {
    loop {
        let next = tokio::select! {
            () = cancel.cancelled() => break,
            next = stream.next() => next,
        };
        match next {
            None => break,
            Some(Err(error)) => {
                if is_capacity_error(&error) {
                    // The read limit closes with 1009 (message too big).
                    locker.with(|core| {
                        if let Some(session) = core.session_mut(key)
                            && !session.terminated
                        {
                            session.close_queued = true;
                            session.queue.clear();
                            session.queue.push_back(Outbound::Close(
                                CLOSE_MESSAGE_TOO_BIG,
                                "message too big".to_owned(),
                            ));
                            session.wake.notify_one();
                        }
                    });
                    tokio::select! {
                        () = writer_done.cancelled() => {}
                        () = cancel.cancelled() => {}
                        () = tokio::time::sleep(CLOSE_HANDSHAKE_TIMEOUT) => {}
                    }
                }
                break;
            }
            Some(Ok(Message::Text(text))) => {
                locker.with(|core| core.handle_incoming(key, text.as_bytes(), false));
            }
            Some(Ok(Message::Binary(_))) => {
                locker.with(|core| core.handle_incoming(key, &[], true));
            }
            Some(Ok(Message::Pong(_))) => {
                locker.with(|core| {
                    if let Some(session) = core.session_mut(key) {
                        session.alive = true;
                    }
                });
            }
            Some(Ok(Message::Ping(_) | Message::Close(_))) => {}
        }
    }
    cancel.cancel();
    locker.with(|core| core.handle_disconnect(key));
    reader_done.cancel();
}

/// Drains the outbound queue; the socket is only written here.
async fn write_loop(
    locker: Locker,
    key: u64,
    wake: Arc<Notify>,
    cancel: CancellationToken,
    reader_done: CancellationToken,
    writer_done: CancellationToken,
    mut sink: futures_util::stream::SplitSink<WebSocket, Message>,
) {
    loop {
        let next = locker
            .with(|core| {
                core.session_mut(key)
                    .and_then(|session| session.queue.pop_front())
            })
            .flatten();
        let Some(next) = next else {
            // A missing session has disconnected; an empty queue waits.
            if locker
                .with(|core| core.session(key).is_some())
                .unwrap_or(false)
            {
                tokio::select! {
                    () = wake.notified() => continue,
                    () = cancel.cancelled() => break,
                }
            }
            break;
        };
        if cancel.is_cancelled() {
            break;
        }
        match next {
            Outbound::Text(data) => {
                let length = data.len();
                let text = String::from_utf8_lossy(&data).into_owned();
                let sent = tokio::select! {
                    sent = sink.send(Message::Text(text.into())) => sent.is_ok(),
                    () = cancel.cancelled() => false,
                };
                locker.with(|core| {
                    if let Some(session) = core.session_mut(key) {
                        session.queued_bytes = session.queued_bytes.saturating_sub(length);
                    }
                });
                if !sent {
                    break;
                }
            }
            Outbound::Ping => {
                let sent = tokio::select! {
                    sent = sink.send(Message::Ping(Default::default())) => sent.is_ok(),
                    () = cancel.cancelled() => false,
                };
                if !sent {
                    break;
                }
            }
            Outbound::Close(code, reason) => {
                let frame = CloseFrame {
                    code,
                    reason: reason.into(),
                };
                let sent = tokio::select! {
                    sent = sink.send(Message::Close(Some(frame))) => sent.is_ok(),
                    () = cancel.cancelled() => false,
                };
                if sent {
                    tokio::select! {
                        () = reader_done.cancelled() => {}
                        () = cancel.cancelled() => {}
                        () = tokio::time::sleep(CLOSE_HANDSHAKE_TIMEOUT) => {}
                    }
                }
                break;
            }
        }
    }
    writer_done.cancel();
    cancel.cancel();
    let _ = sink.close().await;
}
