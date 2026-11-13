//! Room voice: a room capability that does not change the video messages.

use serde_json::Value;

use crate::codec::{FromWire, Int, Obj, Result, ToWire, ensure, invalid, object, wire_enum};
use crate::constants::{MAX_AUDIO_BITRATE, MAX_PARTICIPANTS_PER_ROOM_LIMIT, MIN_AUDIO_BITRATE};
use crate::scalars::{utf16_length, valid_opaque_id};
use crate::types::{IceCandidate, MediaRouteUpstream, SessionDescription, valid_revision};

fn valid_voice_bitrate(bitrate: Int) -> bool {
    (MIN_AUDIO_BITRATE..=MAX_AUDIO_BITRATE).contains(&bitrate)
}

/// `roomVoiceCommandSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoomVoiceCommand {
    Join,
    Leave,
    Publish,
    Unpublish { publication_id: String },
    Deafen { deafened: bool },
    Bitrate { bitrate: Int },
}

impl FromWire for RoomVoiceCommand {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["action", "publicationId", "deafened", "bitrate"])?;
        let action = f.string("action")?;
        let publication_id = f.string("publicationId")?;
        let deafened = f.opt::<bool>("deafened")?;
        let bitrate = f.int("bitrate")?;
        f.require(&["action"])?;
        match action.as_str() {
            "bitrate" => {
                ensure(
                    f.len() == 2 && f.has("bitrate") && valid_voice_bitrate(bitrate),
                    "invalid voice bitrate",
                )?;
                Ok(Self::Bitrate { bitrate })
            }
            "deafen" => {
                f.require(&["deafened"])?;
                match deafened {
                    Some(deafened) if f.len() == 2 => Ok(Self::Deafen { deafened }),
                    _ => Err(invalid("invalid voice listening state")),
                }
            }
            "join" | "leave" | "publish" => {
                ensure(f.len() == 1, "unexpected voice command fields")?;
                Ok(match action.as_str() {
                    "join" => Self::Join,
                    "leave" => Self::Leave,
                    _ => Self::Publish,
                })
            }
            "unpublish" => {
                f.require(&["publicationId"])?;
                ensure(
                    f.len() == 2 && valid_opaque_id(&publication_id),
                    "invalid voice publication",
                )?;
                Ok(Self::Unpublish { publication_id })
            }
            _ => Err(invalid("unknown voice command")),
        }
    }
}

impl ToWire for RoomVoiceCommand {
    fn to_wire(&self) -> Value {
        match self {
            Self::Join => Obj::new().put("action", "join"),
            Self::Leave => Obj::new().put("action", "leave"),
            Self::Publish => Obj::new().put("action", "publish"),
            Self::Unpublish { publication_id } => Obj::new()
                .put("action", "unpublish")
                .put("publicationId", publication_id),
            Self::Deafen { deafened } => {
                Obj::new().put("action", "deafen").put("deafened", deafened)
            }
            Self::Bitrate { bitrate } => {
                Obj::new().put("action", "bitrate").put("bitrate", bitrate)
            }
        }
        .build()
    }
}

/// One negotiated voice track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceTrack {
    pub mid: String,
    pub publication_id: String,
}

impl FromWire for VoiceTrack {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["mid", "publicationId"])?;
        let track = Self {
            mid: f.string("mid")?,
            publication_id: f.string("publicationId")?,
        };
        f.require(&["mid", "publicationId"])?;
        ensure(
            (1..=128).contains(&utf16_length(&track.mid)) && valid_opaque_id(&track.publication_id),
            "invalid voice track",
        )?;
        Ok(track)
    }
}

impl ToWire for VoiceTrack {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("mid", &self.mid)
            .put("publicationId", &self.publication_id)
            .build()
    }
}

/// `voiceDescriptionSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceDescription {
    pub revision: Int,
    pub sdp: SessionDescription,
    pub tracks: Vec<VoiceTrack>,
}

impl FromWire for VoiceDescription {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["revision", "sdp", "tracks"])?;
        f.require(&["revision", "sdp", "tracks"])?;
        let description = Self {
            revision: f.int("revision")?,
            sdp: f.value("sdp")?,
            tracks: f.or_default("tracks")?,
        };
        ensure(
            description.revision >= 1
                && description.tracks.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT * 2,
            "invalid voice description",
        )?;
        Ok(description)
    }
}

impl ToWire for VoiceDescription {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("revision", &self.revision)
            .put("sdp", &self.sdp)
            .put("tracks", &self.tracks)
            .build()
    }
}

/// The exactly-one body of a `room-voice-signal`.
#[derive(Debug, Clone, PartialEq)]
pub enum RoomVoiceSignalBody {
    Description(VoiceDescription),
    Candidate(IceCandidate),
    Renegotiate,
}

/// `room-voice-signal`, sent by both sides.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomVoiceSignal {
    pub connection_id: String,
    pub body: RoomVoiceSignalBody,
}

impl RoomVoiceSignal {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "type",
                "connectionId",
                "description",
                "candidate",
                "renegotiate",
            ],
        )?;
        let connection_id = f.string("connectionId")?;
        let description = f.opt::<VoiceDescription>("description")?;
        let candidate = f.opt::<IceCandidate>("candidate")?;
        let renegotiate = f.boolean("renegotiate")?;
        f.require(&["type", "connectionId"])?;
        f.optional(&["description", "candidate", "renegotiate"])?;
        ensure(
            valid_opaque_id(&connection_id) && (!f.has("renegotiate") || renegotiate),
            "invalid voice signal",
        )?;
        let body = match (description, candidate, renegotiate) {
            (Some(description), None, false) => RoomVoiceSignalBody::Description(description),
            (None, Some(candidate), false) => RoomVoiceSignalBody::Candidate(candidate),
            (None, None, true) => RoomVoiceSignalBody::Renegotiate,
            _ => return Err(invalid("invalid voice signal")),
        };
        Ok(Self {
            connection_id,
            body,
        })
    }

    pub(crate) fn encode(&self) -> Value {
        let out = Obj::message("room-voice-signal").put("connectionId", &self.connection_id);
        match &self.body {
            RoomVoiceSignalBody::Description(description) => out.put("description", description),
            RoomVoiceSignalBody::Candidate(candidate) => out.put("candidate", candidate),
            RoomVoiceSignalBody::Renegotiate => out.put("renegotiate", &true),
        }
        .build()
    }
}

wire_enum! {
    /// `room-voice-transport` state.
    pub enum VoiceTransportState {
        Connected = "connected",
        Failed = "failed",
    }
}

wire_enum! {
    /// `room-voice-error` code.
    pub enum RoomVoiceErrorCode {
        NotAllowed = "not-allowed",
        NotEnabled = "not-enabled",
        ConnectionUnavailable = "connection-unavailable",
    }
}

wire_enum! {
    /// A voice connection's transport.
    pub enum VoiceConnectionTransport {
        Peer = "peer",
        Sfu = "sfu",
    }
}

/// One room voice member. Decoding validates only strictness here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceMember {
    pub peer_id: String,
    pub connected: bool,
    pub joined: bool,
    pub upstream: MediaRouteUpstream,
    pub publication_id: Option<String>,
    pub deafened: Option<bool>,
}

impl FromWire for VoiceMember {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "peerId",
                "connected",
                "joined",
                "upstream",
                "publicationId",
                "deafened",
            ],
        )?;
        Ok(Self {
            peer_id: f.string("peerId")?,
            connected: f.boolean("connected")?,
            joined: f.boolean("joined")?,
            upstream: f.or_default("upstream")?,
            publication_id: f
                .opt::<String>("publicationId")?
                .filter(|id| !id.is_empty()),
            deafened: f.opt("deafened")?,
        })
    }
}

impl ToWire for VoiceMember {
    fn to_wire(&self) -> Value {
        let publication = self.publication_id.as_ref().filter(|id| !id.is_empty());
        Obj::new()
            .put("peerId", &self.peer_id)
            .put("connected", &self.connected)
            .put("joined", &self.joined)
            .put("upstream", &self.upstream)
            .put_some("publicationId", &publication)
            .put_some("deafened", &self.deafened)
            .build()
    }
}

/// One published voice track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoicePublication {
    pub id: String,
    pub peer_id: String,
}

impl FromWire for VoicePublication {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["id", "peerId"])?;
        Ok(Self {
            id: f.string("id")?,
            peer_id: f.string("peerId")?,
        })
    }
}

impl ToWire for VoicePublication {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("id", &self.id)
            .put("peerId", &self.peer_id)
            .build()
    }
}

/// One voice media connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VoiceConnection {
    pub connection_id: String,
    pub parent_peer_id: Option<String>,
    pub child_peer_id: String,
    /// Decoding leaves this unvalidated; [`VoiceConnectionTransport`] names
    /// the values the Server produces.
    pub transport: String,
    pub pending: bool,
    pub revision: Int,
    pub send: Vec<String>,
    pub receive: Vec<String>,
}

impl FromWire for VoiceConnection {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "connectionId",
                "parentPeerId",
                "childPeerId",
                "transport",
                "pending",
                "revision",
                "send",
                "receive",
            ],
        )?;
        Ok(Self {
            connection_id: f.string("connectionId")?,
            parent_peer_id: f.opt::<String>("parentPeerId")?.filter(|id| !id.is_empty()),
            child_peer_id: f.string("childPeerId")?,
            transport: f.string("transport")?,
            pending: f.boolean("pending")?,
            revision: f.int("revision")?,
            send: f.or_default("send")?,
            receive: f.or_default("receive")?,
        })
    }
}

impl ToWire for VoiceConnection {
    fn to_wire(&self) -> Value {
        let parent = self.parent_peer_id.as_ref().filter(|id| !id.is_empty());
        Obj::new()
            .put("connectionId", &self.connection_id)
            .put_some("parentPeerId", &parent)
            .put("childPeerId", &self.child_peer_id)
            .put("transport", &self.transport)
            .put("pending", &self.pending)
            .put("revision", &self.revision)
            .put("send", &self.send)
            .put("receive", &self.receive)
            .build()
    }
}

/// Server `room-voice-state`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RoomVoiceState {
    /// Omitted when absent or zero.
    pub bitrate: Option<Int>,
    pub revision: Int,
    pub enabled: bool,
    pub host_peer_id: Option<String>,
    pub members: Vec<VoiceMember>,
    pub publications: Vec<VoicePublication>,
    pub connections: Vec<VoiceConnection>,
    pub unavailable: Vec<String>,
}

impl RoomVoiceState {
    pub(crate) fn decode(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "type",
                "bitrate",
                "revision",
                "enabled",
                "hostPeerId",
                "members",
                "publications",
                "connections",
                "unavailable",
            ],
        )?;
        f.require(&[
            "type",
            "revision",
            "enabled",
            "members",
            "publications",
            "connections",
            "unavailable",
        ])?;
        f.require_nullable(&["hostPeerId"])?;
        let state = Self {
            bitrate: if f.has("bitrate") {
                Some(f.int("bitrate")?)
            } else {
                None
            },
            revision: f.int("revision")?,
            enabled: f.boolean("enabled")?,
            host_peer_id: f.opt("hostPeerId")?,
            members: f.or_default("members")?,
            publications: f.or_default("publications")?,
            connections: f.or_default("connections")?,
            unavailable: f.or_default("unavailable")?,
        };
        ensure(
            state.bitrate.is_none_or(valid_voice_bitrate),
            "invalid voice bitrate",
        )?;
        ensure(
            valid_revision(state.revision)
                && state.host_peer_id.as_deref().is_none_or(valid_opaque_id)
                && state.members.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT
                && state.publications.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT
                && state.connections.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT + 1
                && state.unavailable.len() <= MAX_PARTICIPANTS_PER_ROOM_LIMIT,
            "invalid voice state",
        )?;
        Ok(state)
    }

    pub(crate) fn encode(&self) -> Value {
        let bitrate = self.bitrate.filter(|bitrate| *bitrate != 0);
        Obj::message("room-voice-state")
            .put_some("bitrate", &bitrate)
            .put("revision", &self.revision)
            .put("enabled", &self.enabled)
            .put("hostPeerId", &self.host_peer_id)
            .put("members", &self.members)
            .put("publications", &self.publications)
            .put("connections", &self.connections)
            .put("unavailable", &self.unavailable)
            .build()
    }
}
