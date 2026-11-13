//! The SFU's single UDP port.
//!
//! Every ICE-lite SFU connection shares one socket. A datagram from a known
//! remote address goes to the connection that owns that address. An unknown
//! address is accepted only through a STUN Binding request whose USERNAME
//! names a registered local ufrag; that connection then owns the address.
//! Everything else is dropped.

use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use crate::candidate::HostAddrs;
use crate::connection::ConnId;
use crate::shard::{Datagram, Inbox};
use crate::socket::{Egress, MAX_DATAGRAM, bind_udp4, transient};

/// Remote addresses one connection may own: ICE checks from every candidate
/// of one browser plus a NAT rebinding, with headroom.
const MAX_REMOTES_PER_CONN: usize = 16;

/// Where one registered connection's datagrams go.
#[derive(Clone)]
pub struct SharedRoute {
    pub conn: ConnId,
    pub inbox: Inbox,
}

struct Entry {
    route: SharedRoute,
    ufrag: String,
    remotes: Vec<SocketAddr>,
}

#[derive(Default)]
struct Table {
    conns: HashMap<ConnId, Entry>,
    ufrags: HashMap<String, ConnId>,
    remotes: HashMap<SocketAddr, ConnId>,
}

impl Table {
    fn remove(&mut self, conn: ConnId) {
        let Some(entry) = self.conns.remove(&conn) else {
            return;
        };
        if self.ufrags.get(&entry.ufrag) == Some(&conn) {
            self.ufrags.remove(&entry.ufrag);
        }
        for remote in entry.remotes {
            if self.remotes.get(&remote) == Some(&conn) {
                self.remotes.remove(&remote);
            }
        }
    }

    /// The route for one datagram, learning `source` from an admitted STUN
    /// request.
    fn route(&mut self, source: SocketAddr, contents: &[u8]) -> Option<SharedRoute> {
        if let Some(conn) = self.remotes.get(&source)
            && let Some(entry) = self.conns.get(conn)
        {
            return Some(entry.route.clone());
        }
        let ufrag = binding_request_local_ufrag(contents)?;
        let conn = *self.ufrags.get(ufrag)?;
        // A previous owner of this address (a closed tab's port reused by
        // another browser) loses it.
        if let Some(previous) = self.remotes.insert(source, conn)
            && previous != conn
            && let Some(old) = self.conns.get_mut(&previous)
        {
            old.remotes.retain(|remote| *remote != source);
        }
        let entry = self.conns.get_mut(&conn)?;
        if !entry.remotes.contains(&source) {
            if entry.remotes.len() >= MAX_REMOTES_PER_CONN {
                let oldest = entry.remotes.remove(0);
                if self.remotes.get(&oldest) == Some(&conn) {
                    self.remotes.remove(&oldest);
                }
            }
            entry.remotes.push(source);
        }
        Some(entry.route.clone())
    }
}

/// The local ufrag (`USERNAME` before `:`) of a STUN Binding request.
/// Message integrity is str0m's to check.
pub(crate) fn binding_request_local_ufrag(datagram: &[u8]) -> Option<&str> {
    // Binding request type, magic cookie, attribute-aligned length.
    if datagram.len() < 20
        || datagram[0..2] != [0x00, 0x01]
        || datagram[4..8] != [0x21, 0x12, 0xa4, 0x42]
        || usize::from(u16::from_be_bytes([datagram[2], datagram[3]])) + 20 != datagram.len()
    {
        return None;
    }
    let mut rest = &datagram[20..];
    while rest.len() >= 4 {
        let kind = u16::from_be_bytes([rest[0], rest[1]]);
        let length = usize::from(u16::from_be_bytes([rest[2], rest[3]]));
        let value = rest.get(4..4 + length)?;
        if kind == 0x0006 {
            let username = std::str::from_utf8(value).ok()?;
            return username.split_once(':').map(|(local, _)| local);
        }
        rest = rest.get(4 + length.next_multiple_of(4)..)?;
    }
    None
}

/// The SFU's UDP port and its route table.
pub struct SharedUdp {
    socket: Arc<UdpSocket>,
    hosts: Arc<Mutex<HostAddrs>>,
    table: Arc<Mutex<Table>>,
    reader: JoinHandle<()>,
}

impl SharedUdp {
    /// Binds the port on IPv4 only and starts its reader.
    /// `hosts` lists the addresses the port is reached on, after any NAT 1:1
    /// override; it labels each datagram with its local candidate.
    pub fn bind(addr: SocketAddrV4, hosts: HostAddrs) -> io::Result<Self> {
        let socket = Arc::new(bind_udp4(addr)?);
        let hosts = Arc::new(Mutex::new(hosts));
        let table = Arc::new(Mutex::new(Table::default()));
        let reader = tokio::spawn(read(socket.clone(), hosts.clone(), table.clone()));
        Ok(Self {
            socket,
            hosts,
            table,
            reader,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn egress(&self) -> Egress {
        Egress::new(self.socket.clone())
    }

    /// Host candidates for a new connection, from the current host snapshot.
    pub fn candidates(&self) -> io::Result<Vec<str0m::Candidate>> {
        let port = self.local_addr()?.port();
        Ok(lock(&self.hosts).candidates(port))
    }

    /// Replaces the host snapshot, as a public IP refresh does. Existing
    /// connections keep the candidates they were given.
    pub fn set_hosts(&self, hosts: HostAddrs) {
        *lock(&self.hosts) = hosts;
    }

    /// Registers (or re-registers after an ICE restart) a connection under
    /// its current local ufrag. Learned remote addresses are kept.
    pub fn register(&self, ufrag: &str, route: SharedRoute) {
        let mut table = lock(&self.table);
        let conn = route.conn;
        if let Some(entry) = table.conns.get_mut(&conn) {
            let old = std::mem::replace(&mut entry.ufrag, ufrag.to_owned());
            entry.route = route;
            if table.ufrags.get(&old) == Some(&conn) {
                table.ufrags.remove(&old);
            }
        } else {
            table.conns.insert(
                conn,
                Entry {
                    route,
                    ufrag: ufrag.to_owned(),
                    remotes: Vec::new(),
                },
            );
        }
        // str0m's ufrags are random; a collision only replaces the stale one.
        if let Some(previous) = table.ufrags.insert(ufrag.to_owned(), conn)
            && previous != conn
        {
            table.remove(previous);
            table.ufrags.insert(ufrag.to_owned(), conn);
        }
    }

    pub fn unregister(&self, conn: ConnId) {
        lock(&self.table).remove(conn);
    }

    /// Number of registered connections.
    pub fn len(&self) -> usize {
        lock(&self.table).conns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Drop for SharedUdp {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

async fn read(socket: Arc<UdpSocket>, hosts: Arc<Mutex<HostAddrs>>, table: Arc<Mutex<Table>>) {
    let Ok(local) = socket.local_addr() else {
        return;
    };
    let mut buffer = vec![0u8; MAX_DATAGRAM];
    loop {
        let (count, source) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(error) if transient(&error) => continue,
            Err(_) => return,
        };
        let received_at = Instant::now();
        let contents = &buffer[..count];
        let Some(route) = lock(&table).route(source, contents) else {
            continue;
        };
        let Some(destination) = lock(&hosts).destination(source, local.port()) else {
            continue;
        };
        let datagram = Datagram {
            received_at,
            source,
            destination,
            contents: contents.to_vec(),
        };
        // A full shard inbox is packet loss; a closed one awaits unregister.
        let _ = route.inbox.try_send((route.conn, datagram));
    }
}
