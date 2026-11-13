//! Signaling server, router and voice tests.

mod fake_media;
mod harness;
mod rooms;
mod rooms_departure;
mod router;
mod router_failure;
mod router_harness;
mod router_sfu;
mod routes;
mod server;
mod voice;
mod voice_owner;
mod wire;

use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

/// A subscriber that enables every level, including debug.
pub(crate) struct DebugSubscriber;

impl Subscriber for DebugSubscriber {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _: &Id, _: &Record<'_>) {}
    fn record_follows_from(&self, _: &Id, _: &Id) {}
    fn event(&self, _: &Event<'_>) {}
    fn enter(&self, _: &Id) {}
    fn exit(&self, _: &Id) {}
}
