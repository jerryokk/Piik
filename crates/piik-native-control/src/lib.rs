//! The App's native control session behind loopback v10 `/control`.
//!
//! One [`Session`] serves one accepted control connection. It owns the strict
//! request wire, Host-versus-Viewer role exclusivity, the capture operation
//! and connection-preparation admission slots, and event delivery. Media
//! itself sits behind the [`media`] traits.

#![forbid(unsafe_code)]

mod capture;
mod events;
mod handle;
pub mod media;
mod punch;
mod session;
pub mod wire;

#[cfg(test)]
mod tests;

pub use capture::ProcessSources;
pub use session::{Config, Session};
