//! The physical media boundary that room signaling drives.
//!
//! These are the media and voice runtime traits and the value types signaling
//! exchanges with them. The embedded SFU ([`crate::Media`]) implements the
//! traits; signaling tests inject deterministic fakes. Every method is synchronous: signaling calls them
//! either under its authority lock (cheap registration) or after releasing it
//! (SDP/ICE and physical close). `piik-signal` re-exports this module.

use std::fmt;
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

pub use crate::admission::{Admission, AdmissionOptions, ResourceFence, SubscriptionFence, Usage};

/// One real publisher encoding's format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LayerFormat {
    pub width: u32,
    pub height: u32,
    pub bitrate: u32,
}

/// Metadata from the authorized publisher, not a quality ladder.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicationMedia {
    pub codec: String,
    pub rids: Vec<String>,
    pub formats: Vec<LayerFormat>,
    pub audio: bool,
    pub audio_bitrate: u32,
}

/// `RTCPeerConnectionState`, serialized with Pion's spellings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PeerConnectionState {
    #[default]
    Unknown,
    New,
    Connecting,
    Connected,
    Disconnected,
    Failed,
    Closed,
}

impl PeerConnectionState {
    /// The Pion spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::New => "new",
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
            Self::Closed => "closed",
        }
    }
}

impl fmt::Display for PeerConnectionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `RTCSdpType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SdpType {
    Offer,
    Pranswer,
    Answer,
    Rollback,
}

/// Pion's `SessionDescription` JSON shape: `{"type": "...", "sdp": "..."}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDescription {
    #[serde(rename = "type")]
    pub sdp_type: SdpType,
    pub sdp: String,
}

/// Pion's `ICECandidateInit` JSON shape; absent members encode as null.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct IceCandidateInit {
    pub candidate: String,
    #[serde(rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex")]
    pub sdp_m_line_index: Option<u16>,
    #[serde(rename = "usernameFragment")]
    pub username_fragment: Option<String>,
}

/// One physical-handle callback handed back to room authority.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaEvent {
    pub fence: ResourceFence,
    /// Empty for the publisher.
    pub viewer_peer_id: String,
    pub connection_id: String,
    /// `None` with [`PeerConnectionState::Unknown`] ends ICE gathering.
    pub candidate: Option<IceCandidateInit>,
    pub state: PeerConnectionState,
    pub active_count: Option<i64>,
}

/// A media failure. [`MediaError::CandidateRejected`] is the disposable
/// per-candidate rejection (Pion's `rtcerr.OperationError`); every other
/// failure retires the physical connection through its route owner.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MediaError {
    #[error("{0}")]
    CandidateRejected(String),
    #[error("{0}")]
    Failed(String),
}

impl MediaError {
    pub fn failed(message: impl Into<String>) -> Self {
        Self::Failed(message.into())
    }
}

/// Work a runtime returns for signaling to run outside its authority lock.
pub type DeferredWork = Box<dyn FnOnce() -> Result<(), MediaError> + Send>;

/// The physical publication/subscription boundary.
pub trait Runtime: Send + Sync {
    fn has_publication(&self, fence: &ResourceFence, connection_id: &str) -> bool;
    fn prepare_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError>;
    fn update_publication(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        media: &PublicationMedia,
    ) -> Result<(), MediaError>;
    fn publication_demand(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
    ) -> Result<i64, MediaError>;
    fn accept_publisher_offer(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        offer: SessionDescription,
    ) -> Result<SessionDescription, MediaError>;
    fn add_publisher_ice(
        &self,
        fence: &ResourceFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError>;
    /// Prepares the subscriber offer. `deadline` is the route operation's
    /// deadline; a runtime must give up once it passes.
    fn prepare_subscriber(
        &self,
        deadline: Instant,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError>;
    fn apply_subscriber_answer(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        answer: SessionDescription,
    ) -> Result<(), MediaError>;
    fn add_subscriber_ice(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
        candidate: IceCandidateInit,
    ) -> Result<(), MediaError>;
    fn restart_subscriber(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<SessionDescription, MediaError>;
    fn close_subscription(
        &self,
        fence: &SubscriptionFence,
        connection_id: &str,
    ) -> Result<(), MediaError>;
    fn close_publication(&self, fence: &ResourceFence) -> Result<(), MediaError>;

    /// Reports whether this runtime can detach one physical video facet from a
    /// reusable room-generation publication.
    fn supports_retire_publication(&self) -> bool {
        false
    }

    /// Detaches the exact video facet synchronously and returns its physical
    /// close, or `None` when nothing remains to close.
    fn retire_publication(
        &self,
        _fence: &ResourceFence,
        _connection_id: &str,
    ) -> Option<DeferredWork> {
        None
    }

    /// The room-voice runtime, when this media owner provides one.
    fn voice(&self) -> Option<&dyn VoiceRuntime> {
        None
    }
}

/// An already-authorized source, including one relayed by a peer.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VoicePublication {
    pub id: String,
    #[serde(rename = "peerId")]
    pub peer_id: String,
}

/// Physical work for one junction of the room graph. The caller owns
/// admission, publication authority, routing and eventual retirement.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct VoiceConnectionPlan {
    pub room_id: String,
    pub peer_id: String,
    pub session_id: String,
    pub connection_id: String,
    pub publish: Vec<VoicePublication>,
    pub subscribe: Vec<VoicePublication>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VoiceTrack {
    pub mid: String,
    #[serde(rename = "publicationId")]
    pub publication_id: String,
}

/// Offers name both directions; answers name only tracks the peer actually
/// sends. `revision` fences an answer from earlier negotiation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceDescription {
    pub revision: u64,
    pub sdp: SessionDescription,
    pub tracks: Vec<VoiceTrack>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct VoiceSignal {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<VoiceDescription>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<IceCandidateInit>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub renegotiate: bool,
}

/// Rechecks transport evidence after the receiver acquires its authority lock.
pub type CurrentStateCheck = Arc<dyn Fn() -> bool + Send + Sync>;

#[derive(Clone, Default)]
pub struct VoiceMediaEvent {
    pub room_id: String,
    pub peer_id: String,
    pub session_id: String,
    pub connection_id: String,
    pub description: Option<VoiceDescription>,
    pub candidate: Option<IceCandidateInit>,
    pub state: PeerConnectionState,
    pub is_current_state: Option<CurrentStateCheck>,
}

impl fmt::Debug for VoiceMediaEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VoiceMediaEvent")
            .field("room_id", &self.room_id)
            .field("peer_id", &self.peer_id)
            .field("session_id", &self.session_id)
            .field("connection_id", &self.connection_id)
            .field("description", &self.description)
            .field("candidate", &self.candidate)
            .field("state", &self.state)
            .field("is_current_state", &self.is_current_state.is_some())
            .finish()
    }
}

/// Receives voice media callbacks; it takes the authority lock itself.
pub type VoiceEventSink = Arc<dyn Fn(VoiceMediaEvent) + Send + Sync>;

/// Commits plans and retirement synchronously. The returned work performs SDP
/// or physical close outside the caller's authority lock.
pub trait VoiceRuntime: Send + Sync {
    fn configure_voice(
        &self,
        plan: VoiceConnectionPlan,
        emit: VoiceEventSink,
    ) -> Result<Option<DeferredWork>, MediaError>;
    fn signal_voice(
        &self,
        room: &str,
        peer: &str,
        session: &str,
        connection: &str,
        signal: VoiceSignal,
    ) -> Result<(), MediaError>;
    fn close_voice(
        &self,
        room: &str,
        peer: &str,
        session: &str,
        connection: &str,
    ) -> Option<DeferredWork>;
}
