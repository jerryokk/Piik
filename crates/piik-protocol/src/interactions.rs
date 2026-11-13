//! Room interactions: chat and reactions.

use serde_json::Value;
use unicode_normalization::is_nfc;

use crate::codec::{
    FromWire, Int, Obj, Result, ToWire, ensure, in_range, invalid, object, wire_enum,
};
use crate::constants::MAX_SAFE_INTEGER;
use crate::scalars::{
    DisplayName, FORBIDDEN_DISPLAY_NAME_CHARACTERS, is_js_whitespace, valid_opaque_id,
};
use crate::types::Role;

pub const MAX_CHAT_CODE_POINTS: usize = 280;
pub const INTERACTION_INTERVAL_MS: Int = 800;

/// The registered reaction set, shared by commands and events.
pub const ROOM_REACTION_IDS: [&str; 16] = [
    "thumbsup",
    "thumbsdown",
    "laugh",
    "clown",
    "eyeroll",
    "eyes",
    "wow",
    "wave",
    "heart",
    "clap",
    "party",
    "fire",
    "star",
    "sleep",
    "tomato",
    "poop",
];

/// Reactions that are thrown at a participant and therefore need a target.
const TARGETED_REACTIONS: [&str; 2] = ["tomato", "poop"];

/// Chat text: 1..280 code points, NFC, trimmed, without forbidden characters.
/// An unpaired surrogate escape is already rejected by the JSON parser.
pub fn valid_chat_text(value: &str) -> bool {
    let count = value.chars().count();
    (1..=MAX_CHAT_CODE_POINTS).contains(&count)
        && !FORBIDDEN_DISPLAY_NAME_CHARACTERS.is_match(value)
        && is_nfc(value)
        && value.trim_matches(is_js_whitespace) == value
}

/// One chat or reaction payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InteractionPayload {
    Chat {
        text: String,
    },
    /// `target_peer_id` is omitted on encode when `None`.
    Reaction {
        reaction: String,
        target_peer_id: Option<String>,
    },
}

impl FromWire for InteractionPayload {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["kind", "text", "reaction", "targetPeerId"])?;
        let kind = f.string("kind")?;
        let text = f.string("text")?;
        let reaction = f.string("reaction")?;
        let target = f.string("targetPeerId")?;
        f.require(&["kind"])?;
        match kind.as_str() {
            "chat" => {
                f.require(&["text"])?;
                ensure(
                    !f.has("reaction") && !f.has("targetPeerId") && valid_chat_text(&text),
                    "invalid chat payload",
                )?;
                Ok(Self::Chat { text })
            }
            "reaction" => {
                f.require(&["reaction"])?;
                f.optional(&["targetPeerId"])?;
                ensure(
                    !f.has("text")
                        && ROOM_REACTION_IDS.contains(&reaction.as_str())
                        && (!f.has("targetPeerId") || valid_opaque_id(&target)),
                    "invalid reaction payload",
                )?;
                ensure(
                    !TARGETED_REACTIONS.contains(&reaction.as_str()) || valid_opaque_id(&target),
                    "throwing a prop requires a participant",
                )?;
                Ok(Self::Reaction {
                    reaction,
                    target_peer_id: (!target.is_empty()).then_some(target),
                })
            }
            _ => Err(invalid("unknown interaction kind")),
        }
    }
}

impl ToWire for InteractionPayload {
    fn to_wire(&self) -> Value {
        match self {
            Self::Chat { text } => {
                Obj::new()
                    .put("kind", "chat")
                    .put_if(!text.is_empty(), "text", text)
            }
            Self::Reaction {
                reaction,
                target_peer_id,
            } => Obj::new()
                .put("kind", "reaction")
                .put_if(!reaction.is_empty(), "reaction", reaction)
                .put_some("targetPeerId", target_peer_id),
        }
        .build()
    }
}

/// Client `send-room-interaction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRoomInteractionMessage {
    pub request_id: String,
    pub payload: InteractionPayload,
}

impl SendRoomInteractionMessage {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(value, &["type", "requestId", "payload"])?;
        f.require(&["type", "requestId", "payload"])?;
        let message = Self {
            request_id: f.string("requestId")?,
            payload: f.value("payload")?,
        };
        ensure(
            valid_opaque_id(&message.request_id),
            "invalid interaction requestId",
        )?;
        Ok(message)
    }

    pub(crate) fn encode(&self) -> Value {
        Obj::message("send-room-interaction")
            .put("requestId", &self.request_id)
            .put("payload", &self.payload)
            .build()
    }
}

/// The resolved sender of a room interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InteractionSender {
    pub peer_id: String,
    pub role: Role,
    pub display_name: DisplayName,
}

impl FromWire for InteractionSender {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["peerId", "role", "displayName"])?;
        let peer_id = f.string("peerId")?;
        let role = f.string("role")?;
        f.require(&["peerId", "role", "displayName"])?;
        let display_name = f.value("displayName")?;
        ensure(valid_opaque_id(&peer_id), "invalid sender peerId")?;
        let role = Role::parse(&role).ok_or_else(|| invalid("invalid sender role"))?;
        Ok(Self {
            peer_id,
            role,
            display_name,
        })
    }
}

impl ToWire for InteractionSender {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("peerId", &self.peer_id)
            .put("role", &self.role)
            .put("displayName", &self.display_name)
            .build()
    }
}

/// Server `room-interaction`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoomInteractionMessage {
    pub id: String,
    pub request_id: String,
    pub occurred_at: Int,
    pub sender: InteractionSender,
    pub payload: InteractionPayload,
}

impl RoomInteractionMessage {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let keys = ["type", "id", "requestId", "occurredAt", "sender", "payload"];
        let f = object(value, &keys)?;
        f.require(&keys)?;
        let message = Self {
            id: f.string("id")?,
            request_id: f.string("requestId")?,
            occurred_at: f.int("occurredAt")?,
            sender: f.value("sender")?,
            payload: f.value("payload")?,
        };
        ensure(
            valid_opaque_id(&message.id) && valid_opaque_id(&message.request_id),
            "invalid interaction identity",
        )?;
        ensure(
            in_range(message.occurred_at, 0, MAX_SAFE_INTEGER),
            "invalid interaction time",
        )?;
        Ok(message)
    }

    pub(crate) fn encode(&self) -> Value {
        Obj::message("room-interaction")
            .put("id", &self.id)
            .put("requestId", &self.request_id)
            .put("occurredAt", &self.occurred_at)
            .put("sender", &self.sender)
            .put("payload", &self.payload)
            .build()
    }
}

wire_enum! {
    /// `room-interaction-rejected` reason.
    pub enum InteractionRejection {
        RateLimited = "rate-limited",
        TargetUnavailable = "target-unavailable",
        Busy = "busy",
    }
}

pub(crate) fn decode_interactions_ready(value: &Value) -> Result<Int> {
    let f = object(value, &["type", "serverTime"])?;
    f.require(&["type", "serverTime"])?;
    let server_time = f.int("serverTime")?;
    ensure(
        in_range(server_time, 0, MAX_SAFE_INTEGER),
        "invalid interaction server time",
    )?;
    Ok(server_time)
}

pub(crate) fn decode_interaction_rejected(value: &Value) -> Result<(String, InteractionRejection)> {
    let f = object(value, &["type", "requestId", "reason"])?;
    f.require(&["type", "requestId", "reason"])?;
    let request_id = f.string("requestId")?;
    ensure(
        valid_opaque_id(&request_id),
        "invalid interaction requestId",
    )?;
    let reason = InteractionRejection::parse(&f.string("reason")?)
        .ok_or_else(|| invalid("invalid rejection reason"))?;
    Ok((request_id, reason))
}
