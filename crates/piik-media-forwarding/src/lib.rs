//! Sans-IO media forwarding for Piik's Native parents and embedded SFU.
//!
//! This crate owns no sockets, tasks, timers or `str0m::Rtc`. Its owner (the
//! transport shard) supplies packets, sender reports, time and bandwidth
//! estimates, and applies the returned decisions. It implements Piik's
//! allocation semantics over forwarding, identity-munging and stream-tracking
//! behavior.

mod audio;
mod forwarder;
mod layer;
mod munger;
mod output;
mod packet;
mod pli;
mod publication;
mod source;

pub use audio::{AudioLimitError, check_audio_receive_limit};
pub use forwarder::{Forwarded, Forwarder};
pub use layer::{
    BitrateGoal, Bitrates, Congestion, DEFAULT_MAX_SPATIAL, DEFAULT_MAX_TEMPORAL,
    MAX_SPATIAL_LAYERS, MAX_TEMPORAL_LAYERS, PROBE_MIN_BPS, PROBE_OVERAGE_PCT, PauseReason,
    VideoAllocation, VideoLayer, VideoTransition, bitrate_goal,
};
pub use munger::{RtpMunger, Translated, Vp8Munger};
pub use output::{CommittedBudget, OutputAllocator, OutputState};
pub use packet::{CodecKind, InboundPacket, Unwrapper, Vp8Descriptor, Vp8Rewrite};
pub use pli::{DEFAULT_PLI_THROTTLE, PliThrottle};
pub use publication::{PublicationAllocator, PublicationError, publication_rid};
pub use source::{
    BITRATE_WINDOW, Codec, LayerFormat, STREAM_STOP_TIMEOUT, SenderReport, Source, SourceChanges,
    SourceError, SourceOptions, SourceView, ntp_from_system_time, system_time_from_ntp,
};
