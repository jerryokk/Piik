//! The App's loopback v10 native-control server.
//!
//! Pages reach the App on `127.0.0.1`, at the first free port in
//! 39721..=39730. `/connect-discovery` is the only endpoint open before local
//! approval; `/health`, `/presentation`, `/quality-preference` and the strict
//! `/control` WebSocket admit only approved or local origins. Every control
//! message beyond hello and ping belongs to a [`ControlSession`].

#![forbid(unsafe_code)]

pub mod buildinfo;
mod capabilities;
pub mod http;
mod protocol;
mod server;
mod session;

pub use capabilities::{Health, NativeMediaCapabilities};
pub use protocol::{
    CONTROL_SUBPROTOCOL, ControlMessage, DEFAULT_PORT_END, DEFAULT_PORT_START, InvalidMessage,
    MAX_CONTROL_MESSAGE_BYTES, PROTOCOL_VERSION, SERVICE_NAME, decode_envelope, encode_message,
    valid_request_id,
};
pub use server::{Endpoint, Error, Options, PresentationCallback, Server};
pub use session::{
    ControlEvent, ControlFactory, ControlSession, EVENT_QUEUE_SIZE, EventSink, ProtocolViolation,
};
// Sessions name these in their signatures.
pub use futures_util::future::BoxFuture;
pub use tokio_util::sync::CancellationToken;
