//! The embedded SFU: admitted publication and subscription resources, their
//! str0m media, and room voice junctions.
//!
//! - [`admission`]: the pure capacity ledger room signaling consults.
//! - [`runtime`]: the [`runtime::Runtime`] / [`runtime::VoiceRuntime`]
//!   boundary signaling drives (re-exported by `piik-signal`).
//! - [`Media`]: the str0m implementation, one shard per room on one
//!   ICE-lite [`piik_media_transport::SharedUdp`] port.

#![forbid(unsafe_code)]

pub mod admission;
mod media;
mod room;
pub mod runtime;
mod voice;

pub use media::{Media, MediaEventSink};
pub use voice::MAX_VOICE_PUBLICATIONS;
