//! The shard: one owner for many connections and their app state.
//!
//! [`Shard`] is sans-IO. Feed it datagrams, commands and time; [`Shard::poll`]
//! returns the datagrams to send, and [`Shard::next_timeout`] says when to
//! call [`Shard::handle_timeout`] again. Tests drive it on virtual time;
//! [`spawn`] runs it as one tokio task.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use str0m::Input;
use str0m::net::{Protocol, Receive, Transmit};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::channel::OpQueue;
use crate::connection::{ConnEvent, ConnId, Connection, Polled};
use crate::socket::Egress;

/// Bound of a shard's command channel.
pub const COMMAND_CAPACITY: usize = 256;
/// Bound of a shard's datagram channel; overflow is packet loss.
pub const DATAGRAM_CAPACITY: usize = 4096;
/// Datagrams handled per wakeup before transmits are flushed.
const DATAGRAM_BATCH: usize = 64;
/// Wakeup when nothing is scheduled, so a quiet shard still notices time.
const IDLE: Duration = Duration::from_secs(1);

/// One received UDP datagram. `destination` must be the local candidate
/// address it arrived on; str0m answers only checks to its own candidates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Datagram {
    /// Socket receive time, before waiting in the shard inbox. TWCC must not
    /// count local queue/processing delay as network delay.
    pub received_at: Instant,
    pub source: SocketAddr,
    pub destination: SocketAddr,
    pub contents: Vec<u8>,
}

/// Where sockets deliver datagrams for a shard's connections.
pub type Inbox = mpsc::Sender<(ConnId, Datagram)>;

/// A transmit with the connection that produced it and that connection's
/// egress at the time.
#[derive(Debug)]
pub struct Outgoing {
    pub conn: ConnId,
    pub egress: Option<Egress>,
    pub transmit: Transmit,
}

/// The shard's application: forwarding, admission or edge state that must
/// run in the same task as its connections.
pub trait ShardApp: Send + 'static {
    type Command: Send + 'static;

    fn on_command(&mut self, now: Instant, command: Self::Command, conns: &mut Connections);

    /// Every connection event, in order. The final event of a closed
    /// connection is `State(Closed)`; the shard drops it right after.
    fn on_event(&mut self, now: Instant, conn: ConnId, event: ConnEvent, conns: &mut Connections);

    /// The app's own next deadline.
    fn poll_timeout(&self) -> Option<Instant> {
        None
    }

    fn on_timeout(&mut self, _now: Instant, _conns: &mut Connections) {}

    /// The shard is stopping; every connection has been closed.
    fn on_shutdown(&mut self, _now: Instant, _conns: &mut Connections) {}
}

/// The connections of one shard.
pub struct Connections {
    conns: BTreeMap<ConnId, Connection>,
    ops: Arc<OpQueue>,
}

impl Connections {
    pub fn insert(&mut self, mut conn: Connection) -> ConnId {
        let id = conn.id();
        conn.attach(self.ops.clone());
        self.conns.insert(id, conn);
        id
    }

    pub fn get(&self, id: ConnId) -> Option<&Connection> {
        self.conns.get(&id)
    }

    pub fn get_mut(&mut self, id: ConnId) -> Option<&mut Connection> {
        self.conns.get_mut(&id)
    }

    pub fn ids(&self) -> impl Iterator<Item = ConnId> + '_ {
        self.conns.keys().copied()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Connection> {
        self.conns.values_mut()
    }

    pub fn len(&self) -> usize {
        self.conns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.conns.is_empty()
    }

    /// Closes a connection; its final events still reach the app.
    pub fn close(&mut self, id: ConnId) {
        if let Some(conn) = self.conns.get_mut(&id) {
            conn.close();
        }
    }
}

/// The sans-IO shard core.
pub struct Shard<A: ShardApp> {
    app: A,
    conns: Connections,
    /// Transmits drained while handling datagrams, sent by the next poll.
    outgoing: Vec<Outgoing>,
}

impl<A: ShardApp> Shard<A> {
    pub fn new(app: A) -> Self {
        Self {
            app,
            outgoing: Vec::new(),
            conns: Connections {
                conns: BTreeMap::new(),
                ops: Arc::new(OpQueue::default()),
            },
        }
    }

    pub fn app(&self) -> &A {
        &self.app
    }

    pub fn conns(&mut self) -> &mut Connections {
        &mut self.conns
    }

    pub fn into_app(self) -> A {
        self.app
    }

    pub fn handle_command(&mut self, now: Instant, command: A::Command) {
        self.app.on_command(now, command, &mut self.conns);
    }

    /// Delivers one datagram and drains its connection, as str0m requires
    /// after every input: in RTP mode a received packet waits in a single slot
    /// for the next `poll_output`, so a second input before that drain drops
    /// it. Transmits are kept for the next [`Shard::poll`]. Unknown
    /// connections and unparseable datagrams are dropped.
    pub fn handle_datagram(&mut self, now: Instant, id: ConnId, datagram: &Datagram) {
        let Some(conn) = self.conns.get_mut(id) else {
            return;
        };
        let Ok(receive) = Receive::new(
            Protocol::Udp,
            datagram.source,
            datagram.destination,
            &datagram.contents,
        ) else {
            return;
        };
        conn.handle_input(Input::Receive(datagram.received_at, receive));
        let mut out = std::mem::take(&mut self.outgoing);
        self.drain(now, id, &mut out);
        self.outgoing = out;
    }

    pub fn handle_timeout(&mut self, now: Instant) {
        for conn in self.conns.iter_mut() {
            if conn.next_timeout() <= now {
                conn.handle_timeout(now);
            }
        }
        if self.app.poll_timeout().is_some_and(|at| at <= now) {
            self.app.on_timeout(now, &mut self.conns);
        }
    }

    /// Runs connections and app until nothing more happens at `now`, then
    /// returns the transmits and drops closed connections.
    pub fn poll(&mut self, now: Instant) -> Vec<Outgoing> {
        let mut out = std::mem::take(&mut self.outgoing);
        loop {
            let mut progressed = self.apply_channel_ops();
            let ids: Vec<ConnId> = self.conns.ids().collect();
            for id in ids {
                progressed |= self.drain(now, id, &mut out);
            }
            if !progressed {
                break;
            }
        }
        for conn in self.conns.iter_mut() {
            conn.refresh_buffered();
        }
        self.conns
            .conns
            .retain(|_, conn| conn.state() != crate::PcState::Closed);
        out
    }

    /// Polls one connection until it only waits on time, delivering its
    /// events to the app. Returns whether anything was polled.
    fn drain(&mut self, now: Instant, id: ConnId, out: &mut Vec<Outgoing>) -> bool {
        let mut progressed = false;
        while let Some(polled) = self.conns.get_mut(id).and_then(|c| c.poll(now)) {
            progressed = true;
            match polled {
                Polled::Transmit(transmit) => out.push(Outgoing {
                    conn: id,
                    egress: self.conns.get(id).and_then(|c| c.egress().cloned()),
                    transmit: *transmit,
                }),
                Polled::Event(event) => {
                    self.app.on_event(now, id, event, &mut self.conns);
                    // App writes and channel sends take effect now.
                    self.apply_channel_ops();
                }
            }
        }
        progressed
    }

    /// The earliest connection or app deadline.
    pub fn next_timeout(&self) -> Option<Instant> {
        self.conns
            .conns
            .values()
            .map(Connection::next_timeout)
            .chain(self.app.poll_timeout())
            .min()
    }

    /// Closes every connection and lets the app observe the final events.
    pub fn shutdown(&mut self, now: Instant) -> Vec<Outgoing> {
        let ids: Vec<ConnId> = self.conns.ids().collect();
        for id in ids {
            self.conns.close(id);
        }
        let out = self.poll(now);
        self.app.on_shutdown(now, &mut self.conns);
        out
    }

    fn apply_channel_ops(&mut self) -> bool {
        let ops = self.conns.ops.take();
        let any = !ops.is_empty();
        for (channel, op) in ops {
            if let Some(conn) = self.conns.get_mut(channel.conn()) {
                conn.apply(&channel, op);
            }
        }
        any
    }

    fn has_channel_ops(&self) -> bool {
        !self.conns.ops.is_empty()
    }
}

/// Senders for a spawned shard.
pub struct ShardHandle<C> {
    pub commands: mpsc::Sender<C>,
    pub inbox: Inbox,
}

impl<C> Clone for ShardHandle<C> {
    fn clone(&self) -> Self {
        Self {
            commands: self.commands.clone(),
            inbox: self.inbox.clone(),
        }
    }
}

/// Runs `shard` as a tokio task until every command sender is dropped, then
/// closes its connections and returns the app.
pub fn spawn<A: ShardApp>(mut shard: Shard<A>) -> (ShardHandle<A::Command>, JoinHandle<A>) {
    let (commands, mut command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let (inbox, mut datagram_rx) = mpsc::channel::<(ConnId, Datagram)>(DATAGRAM_CAPACITY);
    let ops = shard.conns.ops.clone();
    let task = tokio::spawn(async move {
        let mut timer_resolution = None;
        loop {
            // A quiet service needs no timer request; active peers do, even
            // when their sparse RTP cannot wake the shard between deadlines.
            if shard.conns.is_empty() {
                timer_resolution = None;
            } else {
                timer_resolution.get_or_insert_with(crate::timer_resolution::TimerResolution::new);
            }
            let now = Instant::now();
            shard.handle_timeout(now);
            send(shard.poll(now));
            if shard.has_channel_ops() {
                continue;
            }
            let deadline = shard.next_timeout().unwrap_or(now + IDLE).min(now + IDLE);
            tokio::select! {
                biased;
                command = command_rx.recv() => match command {
                    Some(command) => shard.handle_command(Instant::now(), command),
                    None => break,
                },
                Some((conn, datagram)) = datagram_rx.recv() => {
                    let now = Instant::now();
                    shard.handle_datagram(now, conn, &datagram);
                    for _ in 1..DATAGRAM_BATCH {
                        let Ok((conn, datagram)) = datagram_rx.try_recv() else {
                            break;
                        };
                        shard.handle_datagram(now, conn, &datagram);
                    }
                }
                () = ops.wake.notified() => {}
                () = tokio::time::sleep_until(deadline.into()) => {}
            }
        }
        let out = shard.shutdown(Instant::now());
        send(out);
        shard.into_app()
    });
    (ShardHandle { commands, inbox }, task)
}

fn send(out: Vec<Outgoing>) {
    for outgoing in out {
        if let Some(egress) = &outgoing.egress {
            egress.send(outgoing.transmit.destination, &outgoing.transmit.contents);
        }
    }
}
