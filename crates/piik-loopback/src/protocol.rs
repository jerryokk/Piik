//! The loopback v10 control envelope shared by the Web client and the App.

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: i64 = 10;
pub const SERVICE_NAME: &str = "piik-client";
pub const CONTROL_SUBPROTOCOL: &str = "piik-client-v10";
pub const MAX_CONTROL_MESSAGE_BYTES: usize = 256 << 10;
pub const DEFAULT_PORT_START: u16 = 39721;
pub const DEFAULT_PORT_END: u16 = 39730;

/// The base control message. Extensions own every other field.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlMessage {
    #[serde(default)]
    pub version: i64,
    #[serde(default)]
    pub id: String,
    #[serde(default, rename = "type")]
    pub kind: String,
}

impl ControlMessage {
    pub fn new(id: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            id: id.into(),
            kind: kind.into(),
        }
    }
}

/// The strict base shape: hello and ping carry no other field.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictMessage {
    #[serde(default)]
    version: i64,
    #[serde(default)]
    id: String,
    #[serde(default, rename = "type")]
    kind: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("loopback control message is invalid")]
pub struct InvalidMessage;

/// Request identifiers share the Web client's `[A-Za-z0-9_-]{8,256}` bound.
pub fn valid_request_id(id: &str) -> bool {
    (8..=256).contains(&id.len())
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Decodes the version/id/type envelope of any message, leaving its other
/// fields to the extension that owns that type.
pub fn decode_envelope(payload: &[u8]) -> Result<ControlMessage, InvalidMessage> {
    let message: ControlMessage = serde_json::from_slice(payload).map_err(|_| InvalidMessage)?;
    if message.version != PROTOCOL_VERSION
        || !valid_request_id(&message.id)
        || message.kind.is_empty()
    {
        return Err(InvalidMessage);
    }
    Ok(message)
}

pub(crate) fn decode_request(payload: &[u8]) -> Result<ControlMessage, InvalidMessage> {
    let message: StrictMessage = serde_json::from_slice(payload).map_err(|_| InvalidMessage)?;
    if message.version != PROTOCOL_VERSION || !valid_request_id(&message.id) {
        return Err(InvalidMessage);
    }
    Ok(ControlMessage {
        version: message.version,
        id: message.id,
        kind: message.kind,
    })
}

pub(crate) fn validate_hello(message: &ControlMessage) -> Result<(), InvalidMessage> {
    if message.kind == "hello" {
        Ok(())
    } else {
        Err(InvalidMessage)
    }
}

pub(crate) fn validate_ping(message: &ControlMessage) -> Result<(), InvalidMessage> {
    if message.kind == "ping" {
        Ok(())
    } else {
        Err(InvalidMessage)
    }
}

/// Encodes one outgoing message inside the shared message bound.
pub fn encode_message<T: Serialize + ?Sized>(value: &T) -> Result<String, InvalidMessage> {
    let payload = serde_json::to_string(value).map_err(|_| InvalidMessage)?;
    if payload.is_empty() || payload.len() > MAX_CONTROL_MESSAGE_BYTES {
        return Err(InvalidMessage);
    }
    Ok(payload)
}

/// Zero keeps the bounded default range; a single bound selects one port.
pub(crate) fn normalize_port_range(start: u16, end: u16) -> (u16, u16) {
    if start == 0 && end == 0 {
        return (DEFAULT_PORT_START, DEFAULT_PORT_END);
    }
    let start = if start == 0 { end } else { start };
    (start, end.max(start))
}

#[cfg(test)]
mod tests;
