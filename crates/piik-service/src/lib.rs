//! The shared Piik HTTP service for the Hosted Server and the App.
//!
//! [`Server`] composes the Browser assets, site access, the room HTTP API,
//! `/signal` and optional STUN listeners over one room authority, and owns
//! their startup and shutdown:
//!
//! 1. [`Server::new`] composes without binding sockets or opening the room
//!    database.
//! 2. [`Server::listen`] binds (or adopts prebound [`Listener`]s), starts STUN,
//!    restores room authority and only then accepts traffic.
//! 3. [`Server::failed`] reports an unexpected listener failure.
//! 4. [`Server::close`] keeps durable rooms (Hosted restart);
//!    [`Server::end`] ends every room first (App Local/Link).
//!
//! Embedded SFU media is injected through [`Options::sfu`]; see [`media`].

#![forbid(unsafe_code)]

mod access;
mod assets;
mod http;
pub mod media;
mod routes;
mod server;
pub mod webassets;

#[cfg(test)]
mod tests;

pub use assets::{Assets, DirAssets, MemoryAssets};
pub use media::{MediaEvents, SfuMedia, SfuPublicAddress, resolve_sfu_public_ip};
pub use server::{Error, Listener, Options, Server};

/// Unix milliseconds.
pub type Clock = piik_signal::Clock;
