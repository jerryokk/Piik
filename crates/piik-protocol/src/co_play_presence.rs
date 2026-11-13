//! Co-play presence. Grants identify the authenticated input endpoint, not a
//! name or user-chosen participant ID; the server resolves the room identity
//! for presentation.

use std::collections::HashSet;

use serde_json::Value;

use crate::codec::{FromWire, Obj, Result, ToWire, ensure, object};
use crate::constants::MAX_VIEWERS_PER_ROOM_LIMIT;
use crate::punch::canonical_hex;
use crate::scalars::valid_opaque_id;

/// One granted input endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoPlayGrant {
    pub endpoint_id: String,
}

impl FromWire for CoPlayGrant {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["endpointId"])?;
        let endpoint_id = f.string("endpointId")?;
        f.require(&["endpointId"])?;
        Ok(Self { endpoint_id })
    }
}

impl ToWire for CoPlayGrant {
    fn to_wire(&self) -> Value {
        Obj::new().put("endpointId", &self.endpoint_id).build()
    }
}

/// One participant currently co-playing.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoPlayParticipant {
    pub peer_id: String,
}

impl FromWire for CoPlayParticipant {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["peerId"])?;
        let peer_id = f.string("peerId")?;
        f.require(&["peerId"])?;
        Ok(Self { peer_id })
    }
}

impl ToWire for CoPlayParticipant {
    fn to_wire(&self) -> Value {
        Obj::new().put("peerId", &self.peer_id).build()
    }
}

/// Client `set-co-play-presence`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetCoPlayPresenceMessage {
    pub share_generation: String,
    pub grants: Vec<CoPlayGrant>,
}

impl SetCoPlayPresenceMessage {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(value, &["type", "shareGeneration", "grants"])?;
        let message = Self {
            share_generation: f.string("shareGeneration")?,
            grants: f.or_default("grants")?,
        };
        f.require(&["type", "shareGeneration", "grants"])?;
        ensure(
            valid_opaque_id(&message.share_generation)
                && message.grants.len() <= MAX_VIEWERS_PER_ROOM_LIMIT,
            "invalid co-play presence",
        )?;
        let mut seen = HashSet::new();
        for grant in &message.grants {
            ensure(
                canonical_hex(&grant.endpoint_id, 16) && seen.insert(&grant.endpoint_id),
                "invalid co-play endpoint",
            )?;
        }
        Ok(message)
    }

    pub(crate) fn encode(&self) -> Value {
        Obj::message("set-co-play-presence")
            .put("shareGeneration", &self.share_generation)
            .put("grants", &self.grants)
            .build()
    }
}

/// Server `co-play-presence` participants, validated as bounded and unique.
pub(crate) fn decode_co_play_participants(value: &Value) -> Result<Vec<CoPlayParticipant>> {
    let f = object(value, &["type", "participants"])?;
    let participants: Vec<CoPlayParticipant> = f.or_default("participants")?;
    f.require(&["type", "participants"])?;
    ensure(
        participants.len() <= MAX_VIEWERS_PER_ROOM_LIMIT,
        "invalid co-play participants",
    )?;
    let mut seen = HashSet::new();
    for participant in &participants {
        ensure(
            valid_opaque_id(&participant.peer_id) && seen.insert(&participant.peer_id),
            "invalid co-play participant",
        )?;
    }
    Ok(participants)
}
