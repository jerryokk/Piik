//! Sanitized route debug events.

use std::sync::Arc;

use piik_protocol::Role;

use crate::{CandidateTuple, Controller, UpstreamKind};

/// Receives `("piik-route", fields)`. The first fields are `event`, `roomId`,
/// `revision` and `factVersion`, followed by the event details. Values are
/// pre-formatted and never contain raw peer, session or connection IDs.
pub type DebugSink = Arc<dyn Fn(&str, &[(&'static str, String)]) + Send + Sync>;

/// Emits one event when the sink and room are configured; detail values are
/// formatted only then.
macro_rules! route_debug {
    ($controller:expr, $event:expr $(, $key:literal => $value:expr)* $(,)?) => {
        if $controller.debug_enabled() {
            let details: Vec<(&'static str, String)> = vec![$(($key, ($value).to_string())),*];
            $controller.emit_debug(&$event, details);
        }
    };
}
pub(crate) use route_debug;

impl Controller {
    pub(crate) fn debug_enabled(&self) -> bool {
        self.debug_log.is_some() && !self.debug_room_id.is_empty()
    }

    /// Emits one sanitized event with the diagnostic room ID and current facts.
    pub(crate) fn emit_debug(&self, event: &str, details: Vec<(&'static str, String)>) {
        let Some(sink) = self
            .debug_log
            .as_ref()
            .filter(|_| !self.debug_room_id.is_empty())
        else {
            return;
        };
        let mut fields = Vec::with_capacity(4 + details.len());
        fields.push(("event", event.to_owned()));
        fields.push(("roomId", self.debug_room_id.clone()));
        fields.push(("revision", self.revision.to_string()));
        fields.push(("factVersion", self.fact_version.to_string()));
        fields.extend(details);
        sink("piik-route", &fields);
    }

    /// Returns `host`, `viewer-<joinOrder>` or `viewer-unknown`.
    pub(crate) fn debug_peer(&self, peer_id: &str) -> String {
        let current = self.participant(peer_id);
        if peer_id == self.host_peer_id || current.is_some_and(|current| current.role == Role::Host)
        {
            return "host".to_owned();
        }
        match current {
            None => "viewer-unknown".to_owned(),
            Some(current) => format!("viewer-{}", current.join_order),
        }
    }

    /// Returns `p2p:<label>[:regenerate]` or `sfu:<publication>`.
    pub(crate) fn debug_tuple(&self, tuple: &CandidateTuple) -> String {
        if tuple.kind == UpstreamKind::Peer {
            let mut label = format!("p2p:{}", self.debug_peer(&tuple.parent_peer_id));
            if tuple.regenerate {
                label.push_str(":regenerate");
            }
            return label;
        }
        format!("sfu:{}", tuple.publication.map_or("", |plan| plan.as_str()))
    }
}
