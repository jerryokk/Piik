//! The extension point that owns every control message beyond hello and ping.

use std::fmt;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Events a session may queue before the socket writer applies backpressure.
pub const EVENT_QUEUE_SIZE: usize = 256;

/// One outgoing message: a response returned from [`ControlSession::handle`]
/// or an event sent through an [`EventSink`].
///
/// `current` revalidates its owner or transport fact after queues; it must be
/// read-only and runs under the socket write lock. `before_write` releases an
/// operation's admission before its ACK can trigger the next command; it must
/// not block on I/O or send another event.
pub struct ControlEvent {
    pub value: Value,
    pub current: Option<Box<dyn Fn() -> bool + Send + Sync>>,
    pub before_write: Option<Box<dyn FnOnce() + Send>>,
}

impl ControlEvent {
    pub fn new(value: Value) -> Self {
        Self {
            value,
            current: None,
            before_write: None,
        }
    }

    /// Serializes `value`; a value that is not JSON is a programming error
    /// reported to the caller.
    pub fn from_serialize<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Self> {
        serde_json::to_value(value).map(Self::new)
    }

    pub fn with_current(mut self, current: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.current = Some(Box::new(current));
        self
    }

    pub fn with_before_write(mut self, before_write: impl FnOnce() + Send + 'static) -> Self {
        self.before_write = Some(Box::new(before_write));
        self
    }
}

impl From<Value> for ControlEvent {
    fn from(value: Value) -> Self {
        Self::new(value)
    }
}

impl fmt::Debug for ControlEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ControlEvent")
            .field("value", &self.value)
            .field("current", &self.current.is_some())
            .field("before_write", &self.before_write.is_some())
            .finish()
    }
}

/// Queues events onto one control connection's socket. Sends fail once that
/// connection has retired.
#[derive(Clone, Debug)]
pub struct EventSink {
    sender: mpsc::Sender<ControlEvent>,
}

impl EventSink {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<ControlEvent>) {
        let (sender, receiver) = mpsc::channel(EVENT_QUEUE_SIZE);
        (Self { sender }, receiver)
    }

    /// Waits for queue space; returns false when the connection has retired.
    pub async fn send(&self, event: impl Into<ControlEvent>) -> bool {
        self.sender.send(event.into()).await.is_ok()
    }

    /// Resolves when the connection has retired and no event can be delivered.
    pub async fn closed(&self) {
        self.sender.closed().await;
    }

    pub fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }
}

/// A protocol violation: the server closes this connection with 1008.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct ProtocolViolation(pub String);

impl ProtocolViolation {
    pub fn new(reason: impl Into<String>) -> Self {
        Self(reason.into())
    }
}

/// The native control behind one accepted `/control` connection.
pub trait ControlSession: Send + Sync + 'static {
    /// Handles one text message whose version/id/type envelope is valid and
    /// whose type is neither hello nor ping; `payload` is the raw JSON.
    ///
    /// An error is a protocol violation and closes this connection. A valid
    /// operation failure returns a response instead. `Ok(None)` completes
    /// later through the [`EventSink`], using the request ID. `cancel` fires
    /// when the connection retires.
    fn handle<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        payload: &'a [u8],
    ) -> BoxFuture<'a, Result<Option<ControlEvent>, ProtocolViolation>>;

    /// Releases the session's resources. The connection's slot (and any
    /// pending origin retirement) completes only after this returns.
    fn close(&self) -> BoxFuture<'_, ()>;
}

/// Creates one session per accepted connection after its hello. `None` leaves
/// the connection limited to ping.
pub type ControlFactory = Arc<dyn Fn(EventSink) -> Option<Box<dyn ControlSession>> + Send + Sync>;
