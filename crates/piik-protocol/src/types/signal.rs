//! `signalPayloadSchema` and its WebRTC members.

use serde_json::Value;

use crate::codec::{
    FromWire, Int, Nullable, Obj, Result, ToWire, discriminator, in_range, invalid, object,
    wire_enum,
};
use crate::punch::PunchSignal;
use crate::scalars::{utf16_length, valid_opaque_id};

wire_enum! {
    /// `sessionDescriptionSchema.type`.
    pub enum SdpType {
        Offer = "offer",
        Answer = "answer",
    }
}

/// `sessionDescriptionSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    pub kind: SdpType,
    pub sdp: String,
}

impl FromWire for SessionDescription {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["type", "sdp"])?;
        let sdp = f.string("sdp")?;
        f.require(&["type", "sdp"])?;
        let kind = f.value("type")?;
        if !(1..=48 * 1024).contains(&utf16_length(&sdp)) {
            return Err(invalid("sdp length is out of range"));
        }
        Ok(Self { kind, sdp })
    }
}

impl ToWire for SessionDescription {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("type", &self.kind)
            .put("sdp", &self.sdp)
            .build()
    }
}

/// `iceCandidateSchema`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: Nullable<String>,
    pub sdp_m_line_index: Nullable<Int>,
    pub username_fragment: Nullable<String>,
}

impl FromWire for IceCandidate {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &["candidate", "sdpMid", "sdpMLineIndex", "usernameFragment"],
        )?;
        let candidate = Self {
            candidate: f.string("candidate")?,
            sdp_mid: f.nullable("sdpMid")?,
            sdp_m_line_index: f.nullable("sdpMLineIndex")?,
            username_fragment: f.nullable("usernameFragment")?,
        };
        f.require(&["candidate"])?;
        if utf16_length(&candidate.candidate) > 4096 {
            return Err(invalid("candidate is too long"));
        }
        if candidate
            .sdp_mid
            .value()
            .is_some_and(|mid| utf16_length(mid) > 128)
        {
            return Err(invalid("sdpMid is too long"));
        }
        if candidate
            .sdp_m_line_index
            .value()
            .is_some_and(|index| !in_range(*index, 0, 255))
        {
            return Err(invalid("sdpMLineIndex is out of range"));
        }
        if candidate
            .username_fragment
            .value()
            .is_some_and(|fragment| utf16_length(fragment) > 256)
        {
            return Err(invalid("usernameFragment is too long"));
        }
        Ok(candidate)
    }
}

impl ToWire for IceCandidate {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("candidate", &self.candidate)
            .put_nullable("sdpMid", &self.sdp_mid)
            .put_nullable("sdpMLineIndex", &self.sdp_m_line_index)
            .put_nullable("usernameFragment", &self.username_fragment)
            .build()
    }
}

/// `signalPayloadSchema`.
#[derive(Debug, Clone, PartialEq)]
pub enum SignalPayload {
    /// `{ kind: "punch", connectionId, punch }`.
    Punch {
        connection_id: String,
        punch: PunchSignal,
    },
    /// `{ kind: "description", connectionId, description, punched? }`;
    /// `punched` is emitted only when true.
    Description {
        connection_id: String,
        description: SessionDescription,
        punched: bool,
    },
    /// `{ kind: "candidate", connectionId, candidate }`; `None` is the JSON
    /// null that ends candidate gathering.
    Candidate {
        connection_id: String,
        candidate: Option<IceCandidate>,
    },
}

impl SignalPayload {
    /// The `connectionId` every member carries.
    pub fn connection_id(&self) -> &str {
        match self {
            Self::Punch { connection_id, .. }
            | Self::Description { connection_id, .. }
            | Self::Candidate { connection_id, .. } => connection_id,
        }
    }
}

impl FromWire for SignalPayload {
    fn from_wire(value: &Value) -> Result<Self> {
        let kind = discriminator(value, "kind")?;
        match kind {
            "punch" => {
                let f = object(value, &["kind", "connectionId", "punch"])?;
                let connection_id = f.string("connectionId")?;
                let punch = f.opt("punch")?;
                f.require(&["kind", "connectionId", "punch"])?;
                match punch {
                    Some(punch) if valid_opaque_id(&connection_id) => Ok(Self::Punch {
                        connection_id,
                        punch,
                    }),
                    _ => Err(invalid("invalid connection preparation signal")),
                }
            }
            "description" => {
                let f = object(value, &["kind", "connectionId", "description", "punched"])?;
                let connection_id = f.string("connectionId")?;
                let description = f.opt("description")?;
                let punched = f.boolean("punched")?;
                f.require(&["kind", "connectionId", "description"])?;
                let description = description.ok_or_else(|| invalid("description is required"))?;
                if !valid_opaque_id(&connection_id) {
                    return Err(invalid("connectionId is not an opaque id"));
                }
                if f.has("punched") && !punched {
                    return Err(invalid("punched must be true when present"));
                }
                Ok(Self::Description {
                    connection_id,
                    description,
                    punched,
                })
            }
            "candidate" => {
                let f = object(value, &["kind", "connectionId", "candidate"])?;
                let connection_id = f.string("connectionId")?;
                let candidate = f.opt("candidate")?;
                f.require(&["kind", "connectionId"])?;
                f.require_nullable(&["candidate"])?;
                if !valid_opaque_id(&connection_id) {
                    return Err(invalid("connectionId is not an opaque id"));
                }
                Ok(Self::Candidate {
                    connection_id,
                    candidate,
                })
            }
            other => Err(invalid(format!("unknown signal payload kind {other:?}"))),
        }
    }
}

impl ToWire for SignalPayload {
    fn to_wire(&self) -> Value {
        match self {
            Self::Punch {
                connection_id,
                punch,
            } => Obj::new()
                .put("kind", "punch")
                .put("connectionId", connection_id)
                .put("punch", punch),
            Self::Description {
                connection_id,
                description,
                punched,
            } => Obj::new()
                .put("kind", "description")
                .put("connectionId", connection_id)
                .put("description", description)
                .put_if(*punched, "punched", &true),
            Self::Candidate {
                connection_id,
                candidate,
            } => Obj::new()
                .put("kind", "candidate")
                .put("connectionId", connection_id)
                .put("candidate", candidate),
        }
        .build()
    }
}
