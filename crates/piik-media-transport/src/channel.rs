//! Thread-safe handles for data channels owned by a shard's `Rtc`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use piik_input_route::InputChannel;
use str0m::channel::ChannelId;
use tokio::sync::Notify;

use crate::connection::ConnId;

/// Bounds operations queued between two shard steps; overflow is a lost send.
const MAX_QUEUED_OPS: usize = 4096;

const OPEN: u8 = 0;
const CLOSING: u8 = 1;
const CLOSED: u8 = 2;

pub(crate) enum ChannelOp {
    Send(String),
    Close,
}

/// Operations from channel handles, applied by the owning shard on its next
/// step. `wake` lets the tokio driver run that step promptly.
#[derive(Default)]
pub(crate) struct OpQueue {
    ops: Mutex<VecDeque<(Arc<DataChannel>, ChannelOp)>>,
    pub(crate) wake: Notify,
}

impl OpQueue {
    fn push(&self, channel: Arc<DataChannel>, op: ChannelOp) -> bool {
        let mut ops = self.ops.lock().unwrap_or_else(|error| error.into_inner());
        if ops.len() >= MAX_QUEUED_OPS {
            return false;
        }
        ops.push_back((channel, op));
        drop(ops);
        self.wake.notify_one();
        true
    }

    pub(crate) fn take(&self) -> VecDeque<(Arc<DataChannel>, ChannelOp)> {
        std::mem::take(&mut *self.ops.lock().unwrap_or_else(|error| error.into_inner()))
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.ops
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    }
}

/// One open data channel. Identity is the `Arc` allocation, as
/// [`InputChannel`] requires. Handles are created when str0m reports the
/// channel open and become closed when it closes or its connection ends.
pub struct DataChannel {
    conn: ConnId,
    id: ChannelId,
    label: String,
    state: AtomicU8,
    /// Text accepted by [`InputChannel::send_text`] but not yet written.
    pending: AtomicUsize,
    /// SCTP's buffered amount after the shard's last step.
    buffered: AtomicUsize,
    ops: Arc<OpQueue>,
    this: std::sync::Weak<DataChannel>,
}

impl DataChannel {
    pub(crate) fn new(conn: ConnId, id: ChannelId, label: String, ops: Arc<OpQueue>) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            conn,
            id,
            label,
            state: AtomicU8::new(OPEN),
            pending: AtomicUsize::new(0),
            buffered: AtomicUsize::new(0),
            ops,
            this: this.clone(),
        })
    }

    pub fn conn(&self) -> ConnId {
        self.conn
    }

    pub fn id(&self) -> ChannelId {
        self.id
    }

    pub(crate) fn set_closed(&self) {
        self.state.store(CLOSED, Ordering::Release);
    }

    pub(crate) fn set_buffered(&self, amount: usize) {
        self.buffered.store(amount, Ordering::Relaxed);
    }

    pub(crate) fn sent(&self, length: usize) {
        self.pending.fetch_sub(length, Ordering::Relaxed);
    }

    fn enqueue(&self, op: ChannelOp) -> bool {
        self.this
            .upgrade()
            .is_some_and(|this| self.ops.push(this, op))
    }
}

impl std::fmt::Debug for DataChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DataChannel")
            .field("conn", &self.conn)
            .field("id", &self.id)
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl InputChannel for DataChannel {
    fn label(&self) -> &str {
        &self.label
    }

    fn is_open(&self) -> bool {
        self.state.load(Ordering::Acquire) == OPEN
    }

    fn is_closed(&self) -> bool {
        self.state.load(Ordering::Acquire) == CLOSED
    }

    fn buffered_amount(&self) -> usize {
        self.pending.load(Ordering::Relaxed) + self.buffered.load(Ordering::Relaxed)
    }

    fn send_text(&self, text: &str) {
        if !self.is_open() {
            return;
        }
        self.pending.fetch_add(text.len(), Ordering::Relaxed);
        if !self.enqueue(ChannelOp::Send(text.to_owned())) {
            self.sent(text.len());
        }
    }

    /// Like `RTCDataChannel.close()`: the channel stops being open at once and
    /// becomes closed once SCTP reports it.
    fn close(&self) {
        if self
            .state
            .compare_exchange(OPEN, CLOSING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.enqueue(ChannelOp::Close);
        }
    }
}
