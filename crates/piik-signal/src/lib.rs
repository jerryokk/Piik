//! Authenticated room signaling and its room/media effects.
//!
//! [`Server`] serializes session and presence changes, HTTP-facing room
//! mutations and execution of the synchronous route controller's decisions
//! behind one authority lock. Mount [`Server::router`] (or call
//! [`Server::serve_upgrade`]) for `GET /signal`; the embedded SFU plugs in
//! through the [`media_runtime::Runtime`] and [`media_runtime::VoiceRuntime`]
//! traits and reports its callbacks with [`Server::handle_sfu_media_event`].

#![forbid(unsafe_code)]

mod auth;
mod handlers;
mod presence;
mod router;
mod runtime;
mod server;
mod session;
mod transport;
mod voice;

#[cfg(test)]
mod tests;

pub use router::SfuFallback;
pub use runtime::{AfterFunc, Clock, TimerStop};
pub use server::{Error, Options, RouteDebugSink, Server, SiteAccessCheck, invite_url};
