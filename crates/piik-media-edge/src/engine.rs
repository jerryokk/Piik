//! The Engine: one shard task owning every native connection, source and
//! forwarding state of one App role.
//!
//! Public handles ([`crate::Source`], [`crate::Edge`], ...) are cheap clones
//! that send closures to the shard and await their result. Forwarding stays
//! one synchronous call chain on the shard, as the design requires; sockets,
//! STUN, gateway mapping, punching and codec processes run outside it.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use piik_media_session::ClockAnchor;
use piik_media_transport::{
    ConnEvent, ConnId, Connections, Inbox, Shard, ShardApp, ShardHandle, spawn,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::audio::AudioState;
use crate::codec::DEFAULT_INITIAL_BITRATE;
use crate::edge::EdgeState;
use crate::error::{Error, Result};
use crate::publication::PublicationState;
use crate::punch::PunchSlot;
use crate::receiver::ReceiverState;
use crate::source::SourceState;

/// A capable native peer's voluntary steady fanout bound.
pub const MAX_RELAY_CHILDREN: usize = 20;
/// The shard's own housekeeping interval: tracker windows, probe timers and
/// the paused-estimator reset.
const TICK: Duration = Duration::from_millis(50);

/// Reserves the single route operation's overlap without changing the
/// ordinary endpoint's admission bound.
pub fn physical_source_capacity(children: usize) -> usize {
    if children > 3 { children + 1 } else { children }
}

#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    /// IPv4 `address:port`; empty binds every interface on an ephemeral port.
    pub bind_address: String,
    pub include_loopback: bool,
    pub port_mapping: bool,
    /// Zero selects the 3 Mbps default.
    pub initial_bitrate: i64,
}

#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub bind: SocketAddrV4,
    pub include_loopback: bool,
    pub port_mapping: bool,
    pub initial_bitrate: i64,
    /// Test seam: loopback punch fixtures have no public mapping.
    pub allow_private_punch: bool,
}

pub(crate) type Command = Box<dyn FnOnce(&mut App, &mut Connections, Instant) + Send>;

/// Which owner a connection belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Owner {
    Edge(u64),
    Receiver(u64),
    Publication(u64),
}

pub(crate) fn next_key() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// All shard-owned state.
pub(crate) struct App {
    pub anchor: ClockAnchor,
    pub settings: Settings,
    pub sources: HashMap<u64, SourceState>,
    pub audios: HashMap<u64, AudioState>,
    pub edges: HashMap<u64, EdgeState>,
    pub receivers: HashMap<u64, ReceiverState>,
    pub publications: HashMap<u64, PublicationState>,
    pub owners: HashMap<ConnId, Owner>,
    pub closed: bool,
    /// The owning engine, for off-shard work started on the shard. Weak:
    /// the shard ends when every handle is gone.
    engine: std::sync::Weak<EngineInner>,
    next_tick: Instant,
}

impl App {
    /// Closes every owner; the engine stays closed.
    fn shutdown(&mut self, conns: &mut Connections, now: Instant) {
        self.closed = true;
        let edges: Vec<u64> = self.edges.keys().copied().collect();
        for key in edges {
            self.close_edge(conns, key, now);
        }
        let publications: Vec<u64> = self.publications.keys().copied().collect();
        for key in publications {
            self.close_publication(conns, key, now);
        }
        let receivers: Vec<u64> = self.receivers.keys().copied().collect();
        for key in receivers {
            self.close_receiver(conns, key, now);
        }
        let sources: Vec<u64> = self.sources.keys().copied().collect();
        for key in sources {
            self.close_source(conns, key, now);
        }
        let audios: Vec<u64> = self.audios.keys().copied().collect();
        for key in audios {
            self.close_audio(conns, key, now);
        }
    }

    /// Runs deferred work every shard step: recovery notifications.
    fn flush(&mut self, conns: &mut Connections, now: Instant) {
        let keys: Vec<u64> = self.sources.keys().copied().collect();
        for key in keys {
            self.flush_recovery(conns, key, now);
        }
    }
}

impl ShardApp for App {
    type Command = Command;

    fn on_command(&mut self, now: Instant, command: Command, conns: &mut Connections) {
        command(self, conns, now);
        self.flush(conns, now);
    }

    fn on_event(&mut self, now: Instant, conn: ConnId, event: ConnEvent, conns: &mut Connections) {
        match self.owners.get(&conn).copied() {
            Some(Owner::Edge(key)) => self.edge_event(conns, key, event, now),
            Some(Owner::Receiver(key)) => self.receiver_event(conns, key, event, now),
            Some(Owner::Publication(key)) => self.publication_event(conns, key, event, now),
            None => {}
        }
        self.flush(conns, now);
    }

    fn poll_timeout(&self) -> Option<Instant> {
        Some(self.next_tick)
    }

    fn on_timeout(&mut self, now: Instant, conns: &mut Connections) {
        self.next_tick = now + TICK;
        let sources: Vec<u64> = self.sources.keys().copied().collect();
        for key in sources {
            self.poll_source(conns, key, now);
        }
        let edges: Vec<u64> = self.edges.keys().copied().collect();
        for key in edges {
            self.poll_edge(conns, key, now);
        }
        let publications: Vec<u64> = self.publications.keys().copied().collect();
        for key in publications {
            self.poll_publication(conns, key, now);
        }
        self.flush(conns, now);
    }

    fn on_shutdown(&mut self, now: Instant, conns: &mut Connections) {
        self.shutdown(conns, now);
    }
}

pub(crate) struct EngineInner {
    pub handle: ShardHandle<Command>,
    pub settings: Settings,
    pub closed: AtomicBool,
    /// Cancels every off-shard worker (gathering, punch, relay processes).
    pub cancel: CancellationToken,
    pub punch: PunchSlot,
}

/// One native media engine. Cheap to clone; every clone refers to the same
/// shard. Must be created inside a Tokio runtime.
#[derive(Clone)]
pub struct Engine {
    pub(crate) inner: Arc<EngineInner>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Engine")
            .field("bind", &self.inner.settings.bind)
            .finish_non_exhaustive()
    }
}

impl Engine {
    pub fn new(options: EngineOptions) -> Result<Self> {
        Self::build(options, false)
    }

    /// Test seam: loopback punch fixtures have no public mapping.
    #[cfg(test)]
    pub(crate) fn with_private_punch(options: EngineOptions) -> Result<Self> {
        Self::build(options, true)
    }

    fn build(options: EngineOptions, allow_private_punch: bool) -> Result<Self> {
        let bind = if options.bind_address.is_empty() || options.bind_address == ":0" {
            SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)
        } else {
            options
                .bind_address
                .parse::<SocketAddrV4>()
                .map_err(|_| Error::Invalid("native media UDP bind address is invalid"))?
        };
        let settings = Settings {
            bind,
            include_loopback: options.include_loopback,
            port_mapping: options.port_mapping,
            initial_bitrate: if options.initial_bitrate <= 0 {
                DEFAULT_INITIAL_BITRATE
            } else {
                options.initial_bitrate
            },
            allow_private_punch,
        };
        let now = Instant::now();
        let app = App {
            anchor: ClockAnchor::now(),
            settings: settings.clone(),
            sources: HashMap::new(),
            audios: HashMap::new(),
            edges: HashMap::new(),
            receivers: HashMap::new(),
            publications: HashMap::new(),
            owners: HashMap::new(),
            closed: false,
            engine: std::sync::Weak::new(),
            next_tick: now + TICK,
        };
        let (handle, _task) = spawn(Shard::new(app));
        let inner = Arc::new(EngineInner {
            handle,
            settings,
            closed: AtomicBool::new(false),
            cancel: CancellationToken::new(),
            punch: PunchSlot::default(),
        });
        let weak = Arc::downgrade(&inner);
        let install: Command = Box::new(move |app, _, _| app.engine = weak);
        // The command queue is empty: this is the shard's first command.
        let _ = inner.handle.commands.try_send(install);
        Ok(Self { inner })
    }

    pub(crate) fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub(crate) fn settings(&self) -> &Settings {
        &self.inner.settings
    }

    pub(crate) fn inbox(&self) -> Inbox {
        self.inner.handle.inbox.clone()
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::Acquire)
    }

    /// Runs `work` on the shard and returns its result.
    pub(crate) async fn call<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut App, &mut Connections, Instant) -> R + Send + 'static,
    ) -> Result<R> {
        let (reply, result) = oneshot::channel();
        let command: Command = Box::new(move |app, conns, now| {
            let _ = reply.send(work(app, conns, now));
        });
        self.inner
            .handle
            .commands
            .send(command)
            .await
            .map_err(|_| Error::EngineClosed)?;
        result.await.map_err(|_| Error::EngineClosed)
    }

    /// Queues `work` without waiting; used by off-shard workers.
    pub(crate) fn post(
        &self,
        work: impl FnOnce(&mut App, &mut Connections, Instant) + Send + 'static,
    ) {
        let _ = self.inner.handle.commands.try_send(Box::new(work));
    }

    /// Queues `work`, waiting for command space; a closed engine drops it.
    pub(crate) async fn send(
        &self,
        work: impl FnOnce(&mut App, &mut Connections, Instant) + Send + 'static,
    ) {
        let _ = self.inner.handle.commands.send(Box::new(work)).await;
    }

    /// Retires every edge, receiver, publication and source, then the
    /// connection preparation. Later creation fails.
    pub async fn close(&self) -> Result<()> {
        if self.inner.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.inner.cancel.cancel();
        self.close_punch("");
        let _ = self.call(|app, conns, now| app.shutdown(conns, now)).await;
        Ok(())
    }
}

impl App {
    /// The owning engine while any handle is alive.
    pub(crate) fn engine(&self) -> Option<Engine> {
        self.engine.upgrade().map(|inner| Engine { inner })
    }
}
