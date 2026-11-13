//! Binds Piik's sans-IO forwarding core (`piik-media-forwarding`) to
//! `str0m::Rtc` in RTP mode.
//!
//! Every type here is sans-IO. Its owner (a transport shard) owns each `Rtc`,
//! its sockets and its clock; it passes `&mut Rtc` and `now` into each call and
//! drains `poll_output` afterwards, as str0m requires after a mutation. Sources
//! belong to the same shard and are passed by reference, so forwarding stays
//! one synchronous call chain without locks.
//!
//! - [`VideoSend`]: one outbound video stream on a subscriber connection.
//! - [`SimulcastSend`]: native Host to SFU simulcast, one RID per encoding.
//! - [`EncodedSource`]: local encoded access units into a [`Source`].
//! - [`AudioSlot`]: a reusable Opus send slot across input sources.
//! - [`Ingress`]: received RTP and sender reports into a [`Source`].
//!
//! [`Source`]: piik_media_forwarding::Source

#![forbid(unsafe_code)]

mod audio;
mod encoded;
mod ingress;
mod probe;
mod publication;
mod transport;
mod write;

use std::sync::Arc;

use piik_media_forwarding::{AudioLimitError, InboundPacket, PublicationError, SourceError};

pub use audio::{AudioAttachment, AudioSlot, AudioSlotOptions};
pub use encoded::{ClockAnchor, EncodedLayerStats, EncodedSource, EncodedWrite, InputClock};
pub use ingress::{Ingress, IngressPacket};
pub use probe::REPROBE_INTERVAL;
pub use publication::{PublicationCounters, PublicationOptions, SimulcastSend};
pub use transport::{PAUSED_MIN_WAIT, TransportOptions, VideoSend};
pub use write::DRAIN_BOUND;

/// One accepted source packet with the payload every output forwards.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourcePacket {
    pub info: InboundPacket,
    pub payload: Arc<[u8]>,
}

/// A keyframe the source owner must request upstream (a publisher PLI or a
/// local encoder recovery). The owner applies its `PliThrottle`; a forced
/// request (new subscriber, source replacement) bypasses the interval.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpstreamKeyframe {
    pub layer: usize,
    pub force: bool,
}

/// Local RTP handed to str0m. It is not proof of remote receipt or decode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EgressStats {
    pub packets: u64,
    /// RTP header (without extensions) and payload bytes.
    pub rtp_bytes: u64,
    /// Whole frames refused by bounded admission.
    pub dropped_frames: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("media session options are invalid")]
    Invalid,
    #[error("media session is closed")]
    Closed,
    #[error("replacement source changed its transport contract")]
    Incompatible,
    #[error("encoded output has no matching source input")]
    NoInput,
    #[error("encoded output has no source clock reference")]
    NoClock,
    #[error(transparent)]
    Source(#[from] SourceError),
    #[error(transparent)]
    Publication(#[from] PublicationError),
    #[error(transparent)]
    Encoded(#[from] piik_encoded::Error),
    #[error(transparent)]
    AudioLimit(#[from] AudioLimitError),
}
