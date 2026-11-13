//! Route assignment, prepared candidates and participant presence.

use std::collections::HashSet;

use serde_json::Value;

use crate::codec::{FromWire, Int, Obj, Result, ToWire, in_range, invalid, object, wire_enum};
use crate::constants::{MAX_SAFE_INTEGER, MAX_VIEWERS_PER_ROOM_LIMIT};
use crate::scalars::{DisplayName, valid_opaque_id};

use super::{MediaRouteUpstream, Role};

/// `preparedRouteCandidateSchema.connectionAttempt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionAttempt {
    pub current: Int,
    pub total: Int,
}

impl FromWire for ConnectionAttempt {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(value, &["current", "total"])?;
        let attempt = Self {
            current: f.int("current")?,
            total: f.int("total")?,
        };
        f.require(&["current", "total"])?;
        if !in_range(attempt.current, 1, attempt.total) {
            return Err(invalid("connectionAttempt.current is out of range"));
        }
        if !in_range(attempt.total, 1, MAX_SAFE_INTEGER) {
            return Err(invalid("connectionAttempt.total is out of range"));
        }
        Ok(attempt)
    }
}

impl ToWire for ConnectionAttempt {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("current", &self.current)
            .put("total", &self.total)
            .build()
    }
}

wire_enum! {
    /// `preparedRouteCandidateSchema.transport`.
    pub enum RouteTransport {
        Direct = "direct",
        Sfu = "sfu",
    }
}

/// `preparedRouteCandidateSchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedRouteCandidate {
    pub child_peer_id: String,
    pub connection_id: String,
    pub transport: RouteTransport,
    pub quality_probe: bool,
    pub connection_attempt: Option<ConnectionAttempt>,
}

impl FromWire for PreparedRouteCandidate {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &[
                "childPeerId",
                "connectionId",
                "transport",
                "qualityProbe",
                "connectionAttempt",
            ],
        )?;
        let child_peer_id = f.string("childPeerId")?;
        let connection_id = f.string("connectionId")?;
        let quality_probe = f.boolean("qualityProbe")?;
        let connection_attempt = f.opt("connectionAttempt")?;
        f.require(&["childPeerId", "connectionId", "transport", "qualityProbe"])?;
        f.optional(&["connectionAttempt"])?;
        if !valid_opaque_id(&child_peer_id) || !valid_opaque_id(&connection_id) {
            return Err(invalid("prepared route candidate ids are not opaque ids"));
        }
        Ok(Self {
            child_peer_id,
            connection_id,
            transport: f.value("transport")?,
            quality_probe,
            connection_attempt,
        })
    }
}

impl ToWire for PreparedRouteCandidate {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("childPeerId", &self.child_peer_id)
            .put("connectionId", &self.connection_id)
            .put("transport", &self.transport)
            .put("qualityProbe", &self.quality_probe)
            .put_some("connectionAttempt", &self.connection_attempt)
            .build()
    }
}

/// `participantRouteAssignmentSchema`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ParticipantRouteAssignment {
    pub upstream: MediaRouteUpstream,
    pub child_peer_ids: Vec<String>,
    pub sfu_publication_generation: Option<String>,
}

impl FromWire for ParticipantRouteAssignment {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &["upstream", "childPeerIds", "sfuPublicationGeneration"],
        )?;
        f.require(&["upstream", "childPeerIds"])?;
        f.require_nullable(&["sfuPublicationGeneration"])?;
        let assignment = Self {
            upstream: f.value("upstream")?,
            child_peer_ids: f.or_default("childPeerIds")?,
            sfu_publication_generation: f.opt("sfuPublicationGeneration")?,
        };
        if assignment.child_peer_ids.len() > MAX_VIEWERS_PER_ROOM_LIMIT {
            return Err(invalid("childPeerIds holds too many entries"));
        }
        let mut seen = HashSet::with_capacity(assignment.child_peer_ids.len());
        for peer_id in &assignment.child_peer_ids {
            if !valid_opaque_id(peer_id) {
                return Err(invalid("childPeerIds holds a non-opaque id"));
            }
            if !seen.insert(peer_id) {
                return Err(invalid("childPeerIds must be unique"));
            }
        }
        if assignment
            .sfu_publication_generation
            .as_deref()
            .is_some_and(|generation| !valid_opaque_id(generation))
        {
            return Err(invalid("sfuPublicationGeneration is not an opaque id"));
        }
        // superRefine: an SFU upstream requires its publication generation
        // and a peer upstream cannot own one.
        match (&assignment.upstream, &assignment.sfu_publication_generation) {
            (MediaRouteUpstream::Sfu, None) => Err(invalid(
                "An SFU upstream requires its publication generation",
            )),
            (MediaRouteUpstream::Peer { .. }, Some(_)) => Err(invalid(
                "A peer upstream cannot own an SFU publication generation",
            )),
            _ => Ok(assignment),
        }
    }
}

impl ToWire for ParticipantRouteAssignment {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("upstream", &self.upstream)
            .put("childPeerIds", &self.child_peer_ids)
            .put("sfuPublicationGeneration", &self.sfu_publication_generation)
            .build()
    }
}

/// `participantPresenceEntrySchema`; the viewer member is
/// `viewerPresenceEntrySchema`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParticipantPresenceEntry {
    pub role: Role,
    pub peer_id: String,
    pub display_name: DisplayName,
    pub upstream: MediaRouteUpstream,
    /// Emitted only when true.
    pub media_ready: bool,
}

impl ParticipantPresenceEntry {
    /// The host entry, whose upstream is always `{ kind: "none" }` and which
    /// never carries `mediaReady`.
    pub fn host(peer_id: impl Into<String>, display_name: DisplayName) -> Self {
        Self {
            role: Role::Host,
            peer_id: peer_id.into(),
            display_name,
            upstream: MediaRouteUpstream::None,
            media_ready: false,
        }
    }

    /// A viewer entry; `mediaReady` is emitted only when true, matching
    /// `...(mediaReady ? { mediaReady: true } : {})`.
    pub fn viewer(
        peer_id: impl Into<String>,
        display_name: DisplayName,
        upstream: MediaRouteUpstream,
        media_ready: bool,
    ) -> Self {
        Self {
            role: Role::Viewer,
            peer_id: peer_id.into(),
            display_name,
            upstream,
            media_ready,
        }
    }
}

impl FromWire for ParticipantPresenceEntry {
    fn from_wire(value: &Value) -> Result<Self> {
        let f = object(
            value,
            &["role", "peerId", "displayName", "upstream", "mediaReady"],
        )?;
        let peer_id = f.string("peerId")?;
        let media_ready = f.boolean("mediaReady")?;
        f.require(&["role", "peerId", "displayName", "upstream"])?;
        let role: Role = f.value("role")?;
        let display_name = f.value("displayName")?;
        let upstream: MediaRouteUpstream = f.value("upstream")?;
        if !valid_opaque_id(&peer_id) {
            return Err(invalid("presence peerId is not an opaque id"));
        }
        match role {
            Role::Host => {
                if f.has("mediaReady") {
                    return Err(invalid("host presence must not carry mediaReady"));
                }
                if upstream != MediaRouteUpstream::None {
                    return Err(invalid("host presence upstream must be none"));
                }
            }
            Role::Viewer => {
                f.optional(&["mediaReady"])?;
                if f.has("mediaReady") && !media_ready {
                    return Err(invalid("mediaReady must be true when present"));
                }
            }
        }
        Ok(Self {
            role,
            peer_id,
            display_name,
            upstream,
            media_ready,
        })
    }
}

impl ToWire for ParticipantPresenceEntry {
    fn to_wire(&self) -> Value {
        Obj::new()
            .put("role", &self.role)
            .put("peerId", &self.peer_id)
            .put("displayName", &self.display_name)
            .put("upstream", &self.upstream)
            .put_if(self.media_ready, "mediaReady", &true)
            .build()
    }
}
