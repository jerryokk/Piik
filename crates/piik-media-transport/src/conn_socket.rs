//! The Native edge's one UDP socket per connection.
//!
//! A connection owns its socket for life: one socket per connection keeps
//! concurrent DTLS/SRTP sessions with the same remote IP:port apart, which a
//! shared mux cannot. The STUN survey shares the socket so its mappings are
//! the connection's own; responses to Piik's own Binding transactions are
//! matched by transaction id and never reach str0m. ICE restart keeps the
//! socket; replacement takes a new one. A punched socket moves in whole.

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use bytecodec::{DecodeExt, EncodeExt};
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId};
use tokio::net::UdpSocket;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use crate::candidate::{HostAddrs, IceCandidateInit};
use crate::connection::ConnId;
use crate::shard::{Datagram, Inbox};
use crate::socket::{Egress, MAX_DATAGRAM, adopt, bind_udp4, transient};

/// The survey bound per gathering.
pub const STUN_SURVEY_TIMEOUT: Duration = Duration::from_secs(5);
/// RFC 5389 initial retransmission timeout; doubles per resend.
const STUN_RTO: Duration = Duration::from_millis(500);
/// The priority of a punched or surveyed server-reflexive candidate.
pub const SURVEY_CANDIDATE_PRIORITY: u32 = 1_694_498_815;
/// Outstanding survey transactions per socket.
const MAX_TRANSACTIONS: usize = 32;

type Transactions = HashMap<[u8; 12], oneshot::Sender<SocketAddr>>;

#[derive(Default)]
struct Shared {
    route: Option<(ConnId, Inbox)>,
    hosts: Option<HostAddrs>,
    transactions: Transactions,
}

/// A bound socket handed over from hole punching, with the remote it reached.
#[derive(Debug)]
pub struct PunchedSocket {
    pub socket: std::net::UdpSocket,
    pub remote: SocketAddr,
}

pub struct ConnSocket {
    socket: Arc<UdpSocket>,
    punched_peer: Option<SocketAddr>,
    shared: Arc<Mutex<Shared>>,
    reader: JoinHandle<()>,
}

impl ConnSocket {
    /// Binds a fresh IPv4 socket (`0.0.0.0:0` binds every interface).
    pub fn bind(addr: SocketAddrV4) -> io::Result<Self> {
        Ok(Self::start(bind_udp4(addr)?, None))
    }

    /// Takes ownership of a punched socket. Its authenticated tuple stays
    /// usable because the connection keeps this exact socket.
    pub fn adopt(punched: PunchedSocket) -> io::Result<Self> {
        Ok(Self::start(adopt(punched.socket)?, Some(punched.remote)))
    }

    fn start(socket: UdpSocket, punched_peer: Option<SocketAddr>) -> Self {
        let socket = Arc::new(socket);
        let shared = Arc::new(Mutex::new(Shared::default()));
        let reader = tokio::spawn(read(socket.clone(), shared.clone()));
        Self {
            socket,
            punched_peer,
            shared,
            reader,
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }

    pub fn egress(&self) -> Egress {
        Egress::new(self.socket.clone())
    }

    /// Takes a fresh interface snapshot for this gathering and returns the
    /// host candidates for this socket.
    pub fn gather_hosts(&self, include_loopback: bool) -> io::Result<Vec<str0m::Candidate>> {
        let local = self.local_addr()?;
        let bind = match local {
            SocketAddr::V4(v4) => *v4.ip(),
            SocketAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
        };
        let hosts = HostAddrs::gather(bind, include_loopback)?;
        let candidates = hosts.candidates(local.port());
        self.set_hosts(hosts);
        Ok(candidates)
    }

    /// Sets the host snapshot directly.
    pub fn set_hosts(&self, hosts: HostAddrs) {
        lock(&self.shared).hosts = Some(hosts);
    }

    /// Routes this socket's media to one connection of a shard. Datagrams
    /// before attachment are dropped.
    pub fn attach(&self, conn: ConnId, inbox: Inbox) {
        lock(&self.shared).route = Some((conn, inbox));
    }

    pub fn punched_peer(&self) -> Option<SocketAddr> {
        self.punched_peer
    }

    /// The remote candidate for the punched peer.
    pub fn punched_candidate(&self) -> Option<IceCandidateInit> {
        let peer = self.punched_peer?;
        Some(IceCandidateInit {
            candidate: format!(
                "candidate:sp0 1 udp {SURVEY_CANDIDATE_PRIORITY} {} {} typ srflx",
                peer.ip(),
                peer.port()
            ),
            sdp_mid: Some("0".into()),
            ..Default::default()
        })
    }

    /// One STUN Binding transaction to `server` from this socket. Returns the
    /// mapped address, or `None` on timeout.
    pub async fn query_stun(&self, server: SocketAddr, timeout: Duration) -> Option<SocketAddr> {
        let id: [u8; 12] = rand::random();
        let (sender, mut receiver) = oneshot::channel();
        {
            let mut shared = lock(&self.shared);
            if shared.transactions.len() >= MAX_TRANSACTIONS {
                return None;
            }
            shared.transactions.insert(id, sender);
        }
        let request =
            Message::<Attribute>::new(MessageClass::Request, BINDING, TransactionId::new(id));
        let bytes = MessageEncoder::new().encode_into_bytes(request).ok()?;
        let deadline = tokio::time::Instant::now() + timeout;
        let mut rto = STUN_RTO;
        let mapped = loop {
            let _ = self.socket.send_to(&bytes, server).await;
            let wake = (tokio::time::Instant::now() + rto).min(deadline);
            match tokio::time::timeout_at(wake, &mut receiver).await {
                Ok(Ok(mapped)) => break Some(mapped),
                Ok(Err(_)) => break None,
                Err(_) if wake >= deadline => break None,
                Err(_) => rto *= 2,
            }
        };
        lock(&self.shared).transactions.remove(&id);
        mapped
    }
}

impl Drop for ConnSocket {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// A Binding success response to one of Piik's own transactions.
fn survey_response(contents: &[u8], transactions: &Transactions) -> Option<([u8; 12], SocketAddr)> {
    // Cheap prefilter: Binding success, magic cookie, known transaction.
    if contents.len() < 20 || contents[0..2] != [0x01, 0x01] {
        return None;
    }
    let id: [u8; 12] = contents[8..20].try_into().ok()?;
    if !transactions.contains_key(&id) {
        return None;
    }
    let message = MessageDecoder::<Attribute>::new()
        .decode_from_bytes(contents)
        .ok()?
        .ok()?;
    let mapped = message.get_attribute::<XorMappedAddress>()?.address();
    Some((id, mapped))
}

async fn read(socket: Arc<UdpSocket>, shared: Arc<Mutex<Shared>>) {
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
        let mut state = lock(&shared);
        if let Some((id, mapped)) = survey_response(contents, &state.transactions) {
            if let Some(sender) = state.transactions.remove(&id) {
                let _ = sender.send(mapped);
            }
            continue;
        }
        let Some((conn, inbox)) = state.route.clone() else {
            continue;
        };
        let destination = state
            .hosts
            .as_ref()
            .and_then(|hosts| hosts.destination(source, local.port()))
            .unwrap_or(local);
        drop(state);
        let datagram = Datagram {
            received_at,
            source,
            destination,
            contents: contents.to_vec(),
        };
        let _ = inbox.try_send((conn, datagram));
    }
}
