//! Events a native edge, receiver or publication reports to its owner.
//!
//! Sinks run on the Engine's shard task and must not block: an owner forwards
//! each event into its own bounded queue. Every state-carrying event holds a
//! predicate that reports whether it still describes the live connection, so
//! a consumer behind a queue can drop stale events.

use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use piik_media_transport::{DataChannel, IceCandidateInit, PcState};

use crate::quality::SelectedPair;

/// Whether an event's owner is still current.
pub type Current = Arc<dyn Fn() -> bool + Send + Sync>;

/// The live projected state of one connection.
#[derive(Clone, Default)]
pub struct StateWatch(Arc<AtomicU8>);

impl StateWatch {
    pub fn get(&self) -> PcState {
        match self.0.load(Ordering::Acquire) {
            0 => PcState::New,
            1 => PcState::Connecting,
            2 => PcState::Connected,
            3 => PcState::Disconnected,
            4 => PcState::Failed,
            _ => PcState::Closed,
        }
    }

    /// Whether the connection is in `state` now.
    pub fn is(&self, state: PcState) -> bool {
        self.get() == state
    }

    pub(crate) fn set(&self, state: PcState) {
        let value = match state {
            PcState::New => 0,
            PcState::Connecting => 1,
            PcState::Connected => 2,
            PcState::Disconnected => 3,
            PcState::Failed => 4,
            PcState::Closed => 5,
        };
        self.0.store(value, Ordering::Release);
    }
}

impl fmt::Debug for StateWatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StateWatch")
            .field(&self.get())
            .finish()
    }
}

/// One data channel lifecycle event.
#[derive(Debug, Clone)]
pub enum ChannelEvent {
    Open(Arc<DataChannel>),
    Data {
        channel: Arc<DataChannel>,
        data: Vec<u8>,
        binary: bool,
    },
    Close(Arc<DataChannel>),
}

#[derive(Clone)]
pub enum EdgeEvent {
    /// A trickled local candidate; `None` ends gathering. `current` is false
    /// once a later gathering generation or retirement replaced its owner.
    LocalCandidate {
        candidate: Option<IceCandidateInit>,
        current: Current,
    },
    /// The projected peer connection state (Pion wire strings).
    State {
        state: PcState,
        current: StateWatch,
    },
    /// The selected candidate pair, reported once per connected period from
    /// the first transport statistics after `connected`.
    Path {
        pair: SelectedPair,
        current: StateWatch,
    },
    Channel(ChannelEvent),
    /// The input route's ordered transport observer.
    InputTransport(bool),
}

impl fmt::Debug for EdgeEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LocalCandidate { candidate, current } => formatter
                .debug_struct("LocalCandidate")
                .field("candidate", candidate)
                .field("current", &current())
                .finish(),
            Self::State { state, .. } => formatter.debug_tuple("State").field(state).finish(),
            Self::Path { pair, .. } => formatter.debug_tuple("Path").field(pair).finish(),
            Self::Channel(event) => formatter.debug_tuple("Channel").field(event).finish(),
            Self::InputTransport(available) => formatter
                .debug_tuple("InputTransport")
                .field(available)
                .finish(),
        }
    }
}

pub type EventSink = Arc<dyn Fn(EdgeEvent) + Send + Sync>;

pub(crate) fn discard() -> EventSink {
    Arc::new(|_| {})
}
