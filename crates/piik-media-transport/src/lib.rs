//! The str0m wrapper of Piik's Rust media layer.
//!
//! - [`Connection`] wraps one RTP-mode [`str0m::Rtc`] and projects its ICE and
//!   DTLS state onto the Pion [`PcState`] wire strings.
//! - [`Shard`] is the sans-IO driver that owns many connections plus one
//!   [`ShardApp`]; [`spawn`] runs it as a tokio task.
//! - [`SharedUdp`] is the SFU's single UDP port; [`ConnSocket`] is the Native
//!   edge's one socket per connection, shared with its STUN survey.
//! - [`sdp`] holds the codec tables and SDP text policy str0m does not cover;
//!   [`candidate`] holds ICE candidate JSON and host candidate gathering.
//!
//! See `docs/architecture/media.md` for the adaptation boundary this crate
//! sits inside.

#![deny(unsafe_code)]

pub mod candidate;
mod channel;
mod conn_socket;
mod connection;
pub mod sdp;
mod shard;
mod shared_udp;
mod socket;
mod state;
mod timer_resolution;

pub use str0m;

pub use candidate::{CandidateError, HostAddrs, IceCandidateInit};
pub use channel::DataChannel;
pub use conn_socket::{ConnSocket, PunchedSocket, STUN_SURVEY_TIMEOUT, SURVEY_CANDIDATE_PRIORITY};
pub use connection::{CONNECT_TIMEOUT, ConnEvent, ConnId, Connection, FAILED_AFTER_DISCONNECTED};
pub use shard::{
    COMMAND_CAPACITY, Connections, DATAGRAM_CAPACITY, Datagram, Inbox, Outgoing, Shard, ShardApp,
    ShardHandle, spawn,
};
pub use shared_udp::{SharedRoute, SharedUdp};
pub use socket::{Egress, RECEIVE_BUFFER_BYTES, bind_udp4, configure_udp};
pub use state::PcState;
