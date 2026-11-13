//! Remote input. Signaling establishes endpoint identity and the key; input
//! itself follows the media route and never enters room chat, storage or
//! diagnostics.

use serde_json::Value;

use crate::codec::{FromWire, Obj, Result, ToWire, ensure, object};
use crate::constants::MAX_VIEWERS_PER_ROOM_LIMIT;
use crate::punch::canonical_hex;
use crate::scalars::valid_opaque_id;

/// Reports whether `endpoint` is 16 and `secret` 32 bytes of lowercase hex.
pub fn valid_input_key(endpoint: &str, secret: &str) -> bool {
    canonical_hex(endpoint, 16) && canonical_hex(secret, 32)
}

/// An input endpoint identity with its key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInputKey {
    pub endpoint_id: String,
    pub secret: String,
}

/// Client `subscribe-remote-input`; `key` is absent for a receive-only
/// subscription.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubscribeRemoteInputMessage {
    pub key: Option<RemoteInputKey>,
}

impl SubscribeRemoteInputMessage {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(value, &["type", "endpointId", "secret"])?;
        let endpoint_id = f.string("endpointId")?;
        let secret = f.string("secret")?;
        f.require(&["type"])?;
        if !f.has("endpointId") && !f.has("secret") {
            return Ok(Self { key: None });
        }
        ensure(
            valid_input_key(&endpoint_id, &secret),
            "invalid input endpoint",
        )?;
        Ok(Self {
            key: Some(RemoteInputKey {
                endpoint_id,
                secret,
            }),
        })
    }

    pub(crate) fn encode(&self) -> Value {
        let out = Obj::message("subscribe-remote-input");
        match &self.key {
            Some(key) => out
                .put_if(!key.endpoint_id.is_empty(), "endpointId", &key.endpoint_id)
                .put_if(!key.secret.is_empty(), "secret", &key.secret),
            None => out,
        }
        .build()
    }
}

/// One peer's input endpoint in server `remote-input-peers`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteInputPeer {
    pub peer_id: String,
    pub endpoint_id: String,
    pub secret: String,
}

impl FromWire for RemoteInputPeer {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["peerId", "endpointId", "secret"])?;
        Ok(Self {
            peer_id: f.string("peerId")?,
            endpoint_id: f.string("endpointId")?,
            secret: f.string("secret")?,
        })
    }
}

impl ToWire for RemoteInputPeer {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("peerId", &self.peer_id)
            .put("endpointId", &self.endpoint_id)
            .put("secret", &self.secret)
            .build()
    }
}

pub(crate) fn decode_remote_input_peers(value: &Value) -> Result<Vec<RemoteInputPeer>> {
    let f = object(value, &["type", "peers"])?;
    let peers: Vec<RemoteInputPeer> = f.or_default("peers")?;
    f.require(&["type", "peers"])?;
    ensure(
        peers.len() <= MAX_VIEWERS_PER_ROOM_LIMIT,
        "too many input endpoints",
    )?;
    for peer in &peers {
        ensure(
            valid_opaque_id(&peer.peer_id) && valid_input_key(&peer.endpoint_id, &peer.secret),
            "invalid input endpoint",
        )?;
    }
    Ok(peers)
}
