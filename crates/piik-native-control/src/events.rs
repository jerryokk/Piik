//! Event delivery onto one control connection and the media event wire form.

use futures_util::future::BoxFuture;
use piik_loopback::{ControlEvent, EventSink, PROTOCOL_VERSION};
use serde::Serialize;
use serde_json::Value;

use crate::media::{MediaEvent, MediaEventKind};
use crate::wire::{
    AudioStateEvent, CandidateEvent, EdgePathEvent, EdgeQualityEvent, EventEnvelope,
    PublicationQualityEvent, StateEvent,
};

/// The connection's event queue. [`EventSink`] is the production outbox;
/// tests substitute their own.
pub trait Outbox: Send + Sync + 'static {
    /// Waits for queue space; returns false once the connection has retired.
    fn send(&self, event: ControlEvent) -> BoxFuture<'_, bool>;
}

impl Outbox for EventSink {
    fn send(&self, event: ControlEvent) -> BoxFuture<'_, bool> {
        Box::pin(EventSink::send(self, event))
    }
}

/// Serializes a wire struct; the shapes here are always JSON.
pub(crate) fn to_value<T: Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn envelope(
    kind: &str,
    share_id: &str,
    connection_id: &str,
    publication_generation: &str,
) -> EventEnvelope {
    EventEnvelope {
        version: PROTOCOL_VERSION,
        kind: kind.to_owned(),
        share_id: share_id.to_owned(),
        connection_id: connection_id.to_owned(),
        publication_generation: publication_generation.to_owned(),
    }
}

pub(crate) fn media_event_value(share_id: &str, kind: MediaEventKind) -> Value {
    match kind {
        MediaEventKind::EdgeCandidate {
            connection_id,
            candidate,
        } => to_value(&CandidateEvent {
            envelope: envelope("edge-candidate", share_id, &connection_id, ""),
            candidate,
        }),
        MediaEventKind::EdgeState {
            connection_id,
            state,
        } => to_value(&StateEvent {
            envelope: envelope("edge-state", share_id, &connection_id, ""),
            state,
        }),
        MediaEventKind::EdgePath {
            connection_id,
            local_type,
            remote_type,
            nat_traversal_path,
        } => to_value(&EdgePathEvent {
            envelope: envelope("edge-path", share_id, &connection_id, ""),
            local_type,
            remote_type,
            nat_traversal_path,
        }),
        MediaEventKind::EdgeQuality {
            connection_id,
            quality,
        } => to_value(&EdgeQualityEvent::new(
            envelope("edge-quality", share_id, &connection_id, ""),
            &quality,
        )),
        MediaEventKind::PublicationCandidate {
            publication_generation,
            connection_id,
            candidate,
        } => to_value(&CandidateEvent {
            envelope: envelope(
                "publication-candidate",
                share_id,
                &connection_id,
                &publication_generation,
            ),
            candidate,
        }),
        MediaEventKind::PublicationState {
            publication_generation,
            connection_id,
            state,
        } => to_value(&StateEvent {
            envelope: envelope(
                "publication-state",
                share_id,
                &connection_id,
                &publication_generation,
            ),
            state,
        }),
        MediaEventKind::PublicationQuality {
            publication_generation,
            connection_id,
            sample,
        } => to_value(&PublicationQualityEvent {
            envelope: envelope(
                "publication-quality",
                share_id,
                &connection_id,
                &publication_generation,
            ),
            sample,
        }),
        MediaEventKind::CaptureState { state } => to_value(&StateEvent {
            envelope: envelope("capture-state", share_id, "", ""),
            state,
        }),
        MediaEventKind::AudioState {
            source_audio,
            microphone,
            failed,
        } => to_value(&AudioStateEvent {
            envelope: envelope("audio-state", share_id, "", ""),
            source_audio,
            microphone,
            failed,
        }),
    }
}

pub(crate) fn media_event(event: MediaEvent) -> ControlEvent {
    let mut control = ControlEvent::new(media_event_value(&event.share_id, event.kind));
    control.current = event.current;
    control
}
