//! Native media edge.
//!
//! One [`Engine`] per App role owns a shard task with every native
//! connection, source and forwarding state. [`Source`] and [`AudioSource`]
//! carry encoded media; [`Edge`] (Host to Viewer P2P), [`Publication`]
//! (Native Host to SFU) and [`Receiver`] (Viewer native receive and relay)
//! are connections on their own UDP sockets. [`PunchPreparation`] is the
//! optional NAT punch before an edge or receiver adopts its socket.

#![forbid(unsafe_code)]

pub mod access_unit;
mod audio;
mod codec;
mod edge;
mod engine;
mod error;
mod events;
mod gather;
pub mod groups;
mod publication;
mod punch;
mod quality;
mod receiver;
mod relay;
mod relay_input;
mod signal;
mod source;

pub use audio::AudioSource;
pub use codec::{DEFAULT_INITIAL_BITRATE, H264_PROFILE_LEVEL_ID, NativeCodec, VIDEO_CLOCK_RATE};
pub use edge::{Edge, EdgeOptions};
pub use engine::{Engine, EngineOptions, MAX_RELAY_CHILDREN, physical_source_capacity};
pub use error::{Error, Result};
pub use events::{ChannelEvent, Current, EdgeEvent, EventSink, StateWatch};
pub use gather::{IceServerUrls, MAPPED_CANDIDATE_PRIORITY, MappedAddress};
pub use groups::OutputPlan;
pub use piik_media_transport::{IceCandidateInit, PcState, PunchedSocket};
pub use publication::Publication;
pub use punch::{PunchPrediction, PunchPreparation};
pub use quality::{
    CandidateType, PublicationLayer, PublicationMedia, PublicationQualitySample, QualitySample,
    SelectedPair,
};
pub use receiver::{Receiver, ReceiverOptions};
pub use relay::RelayOptions;
pub use signal::MAX_PENDING_CANDIDATES;
pub use source::{RecoveryCallback, VideoSource};

#[cfg(test)]
pub(crate) mod tests;
