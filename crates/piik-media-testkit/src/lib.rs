//! Dev-only harness for driving str0m peers deterministically in virtual time.
//!
//! A [`Network`] owns every [`Peer`], one [`VirtualClock`] and one impaired
//! link per `(source, destination)` address pair. [`Network::run_until`] always
//! processes the earliest pending peer timeout or link delivery, so a test run
//! takes no wall time and its virtual timeline depends only on the inputs.
//! Link impairment (latency, jitter, loss, reorder, duplicate, rate limit) is
//! str0m's own sans-IO emulator, re-exported as [`netem`].
//!
//! Peers bound to the same address share one "socket": datagrams for it are
//! demultiplexed with [`Rtc::accepts`], as an SFU on a single UDP port would.
//! An optional 1:1 NAT rewrites a private address to a public one so
//! peer-reflexive learning can be exercised.
//!
//! str0m and DTLS still draw ICE credentials, SSRCs and keys from their own
//! randomness, so only time and impairment are reproducible run to run.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub use str0m;
pub use str0m_netem as netem;

use netem::{Netem, NetemConfig};
use str0m::change::{SdpAnswer, SdpApi, SdpOffer};
use str0m::net::{Protocol, Receive, Transmit};
use str0m::{Candidate, Event, Input, Output, Rtc, RtcConfig, RtcError};

/// Virtual time shared by every peer and link of one [`Network`].
#[derive(Debug, Clone, Copy)]
pub struct VirtualClock {
    start: Instant,
    now: Instant,
}

impl VirtualClock {
    fn new() -> Self {
        let start = Instant::now();
        Self { start, now: start }
    }

    /// The instant every peer was built at.
    pub fn start(&self) -> Instant {
        self.start
    }

    /// Current virtual time.
    pub fn now(&self) -> Instant {
        self.now
    }

    /// Virtual time elapsed since [`Self::start`].
    pub fn elapsed(&self) -> Duration {
        self.now - self.start
    }
}

/// Index of a peer inside its [`Network`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PeerId(usize);

/// One str0m instance with the events it has emitted so far.
pub struct Peer {
    /// The driven instance. Mutations made through it are drained by the next
    /// [`Network::run_until`].
    pub rtc: Rtc,
    /// Every emitted event with the virtual time it was polled at.
    pub events: Vec<(Instant, Event)>,
    addr: SocketAddr,
    next_timeout: Instant,
}

impl Peer {
    /// Local address the peer was added with.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }
}

/// Delivery counters for the shared-address demultiplexer.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NetStats {
    /// Datagrams handed to a peer.
    pub delivered: u64,
    /// Datagrams with no bound peer, or that no peer sharing the address accepted.
    pub undelivered: u64,
    /// Datagrams on a shared address that more than one peer accepted.
    pub ambiguous: u64,
}

/// Cleartext RTP header fields. SRTP leaves the fixed header unencrypted, so
/// tests can target exact packets on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WireRtp {
    pub payload_type: u8,
    pub sequence_number: u16,
    pub ssrc: u32,
}

impl WireRtp {
    /// Parse an RTP (not RTCP, RFC 5761) fixed header from a datagram.
    pub fn parse(datagram: &[u8]) -> Option<Self> {
        if datagram.len() < 12 || datagram[0] >> 6 != 2 {
            return None;
        }
        let payload_type = datagram[1] & 0x7f;
        if (64..=95).contains(&payload_type) {
            return None;
        }
        Some(Self {
            payload_type,
            sequence_number: u16::from_be_bytes([datagram[2], datagram[3]]),
            ssrc: u32::from_be_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        })
    }
}

/// Decides whether a datagram `(source, destination, contents)` is dropped
/// before it enters its link. Applied after NAT.
pub type DropFilter = Box<dyn FnMut(SocketAddr, SocketAddr, &[u8]) -> bool>;

#[derive(Clone)]
struct Datagram {
    proto: Protocol,
    source: SocketAddr,
    destination: SocketAddr,
    contents: Vec<u8>,
}

impl AsRef<[u8]> for Datagram {
    fn as_ref(&self) -> &[u8] {
        &self.contents
    }
}

#[derive(Clone, Copy)]
enum Next {
    Peer(usize),
    Link((SocketAddr, SocketAddr)),
}

/// In-memory network connecting str0m peers by [`SocketAddr`].
pub struct Network {
    clock: VirtualClock,
    peers: Vec<Peer>,
    bindings: BTreeMap<SocketAddr, Vec<usize>>,
    nat_out: BTreeMap<SocketAddr, SocketAddr>,
    nat_in: BTreeMap<SocketAddr, SocketAddr>,
    links: BTreeMap<(SocketAddr, SocketAddr), Netem<Datagram>>,
    link_bytes: BTreeMap<(SocketAddr, SocketAddr), u64>,
    drop_filter: Option<DropFilter>,
    min_step: Duration,
    stats: NetStats,
}

impl Default for Network {
    fn default() -> Self {
        Self::new()
    }
}

impl Network {
    /// An empty network with unimpaired links and a 1 ms minimum step.
    pub fn new() -> Self {
        Self {
            clock: VirtualClock::new(),
            peers: Vec::new(),
            bindings: BTreeMap::new(),
            nat_out: BTreeMap::new(),
            nat_in: BTreeMap::new(),
            links: BTreeMap::new(),
            link_bytes: BTreeMap::new(),
            drop_filter: None,
            min_step: Duration::from_millis(1),
            stats: NetStats::default(),
        }
    }

    /// Smallest advance applied when a peer asks to be woken at or before now.
    /// Lower it below the packet interval of the fastest simulated sender.
    pub fn set_min_step(&mut self, step: Duration) {
        self.min_step = step;
    }

    /// Impairment for datagrams from `source` (public address after NAT) to
    /// `destination`. Packets already queued on the link keep their schedule.
    pub fn set_link(&mut self, source: SocketAddr, destination: SocketAddr, config: NetemConfig) {
        match self.links.get_mut(&(source, destination)) {
            Some(link) => link.set_config(config),
            None => {
                self.links.insert((source, destination), Netem::new(config));
            }
        }
    }

    /// Install or clear the deterministic drop filter.
    pub fn set_drop_filter(&mut self, filter: Option<DropFilter>) {
        self.drop_filter = filter;
    }

    /// Bytes delivered from `source` (public address after NAT) to `destination`.
    pub fn link_bytes(&self, source: SocketAddr, destination: SocketAddr) -> u64 {
        self.link_bytes
            .get(&(source, destination))
            .copied()
            .unwrap_or(0)
    }

    /// Build a peer at the clock start, bind it to `addr` and add `addr` as its
    /// local host candidate.
    pub fn add_peer(&mut self, config: RtcConfig, addr: SocketAddr) -> PeerId {
        let id = self.add_peer_without_candidate(config, addr);
        let candidate = Candidate::host(addr, "udp").expect("valid host candidate");
        self.peers[id.0].rtc.add_local_candidate(candidate);
        id
    }

    /// Like [`Self::add_peer`] but leaves candidates to the caller.
    pub fn add_peer_without_candidate(&mut self, config: RtcConfig, addr: SocketAddr) -> PeerId {
        let index = self.peers.len();
        self.peers.push(Peer {
            rtc: config.build(self.clock.start),
            events: Vec::new(),
            addr,
            next_timeout: self.clock.now,
        });
        self.bindings.entry(addr).or_default().push(index);
        PeerId(index)
    }

    /// Translate traffic from `private` to appear from `public`, and traffic to
    /// `public` back to `private` (1:1 NAT).
    pub fn add_nat(&mut self, private: SocketAddr, public: SocketAddr) {
        self.nat_out.insert(private, public);
        self.nat_in.insert(public, private);
    }

    pub fn clock(&self) -> VirtualClock {
        self.clock
    }

    pub fn now(&self) -> Instant {
        self.clock.now
    }

    pub fn stats(&self) -> NetStats {
        self.stats
    }

    pub fn peer(&self, id: PeerId) -> &Peer {
        &self.peers[id.0]
    }

    pub fn peer_mut(&mut self, id: PeerId) -> &mut Peer {
        &mut self.peers[id.0]
    }

    /// Drive the network until `cond` holds or `max` virtual time has passed.
    ///
    /// Pending output of every peer is drained first, so API calls made since
    /// the previous run take effect. Returns whether `cond` was met; on `false`
    /// the clock stands exactly at the deadline.
    pub fn run_until(
        &mut self,
        mut cond: impl FnMut(&Network) -> bool,
        max: Duration,
    ) -> Result<bool, RtcError> {
        let deadline = self.clock.now + max;
        for index in 0..self.peers.len() {
            self.drain(index)?;
        }
        loop {
            if cond(self) {
                return Ok(true);
            }
            let Some((at, next)) = self.next_event() else {
                self.clock.now = deadline;
                return Ok(false);
            };
            if at > deadline {
                self.clock.now = deadline;
                return Ok(false);
            }
            self.clock.now = self.clock.now.max(at);
            self.step(next)?;
        }
    }

    /// Drive the network for exactly `duration` of virtual time.
    pub fn run_for(&mut self, duration: Duration) -> Result<(), RtcError> {
        self.run_until(|_| false, duration).map(|_| ())
    }

    fn next_event(&self) -> Option<(Instant, Next)> {
        let peers = self
            .peers
            .iter()
            .enumerate()
            .map(|(index, peer)| (peer.next_timeout, Next::Peer(index)));
        let links = self
            .links
            .iter()
            .filter(|(_, link)| !link.is_empty())
            .map(|(key, link)| (link.poll_timeout(), Next::Link(*key)));
        // Strict comparison keeps ties in peer-then-link-key order.
        peers
            .chain(links)
            .fold(None, |best, (at, next)| match best {
                Some((best_at, _)) if best_at <= at => best,
                _ => Some((at, next)),
            })
    }

    fn step(&mut self, next: Next) -> Result<(), RtcError> {
        let now = self.clock.now;
        match next {
            Next::Peer(index) => {
                self.peers[index].rtc.handle_input(Input::Timeout(now))?;
                self.drain(index)
            }
            Next::Link(key) => {
                let link = self.links.get_mut(&key).expect("link exists");
                link.handle_input(netem::Input::Timeout(now));
                let mut ready = Vec::new();
                while let Some(netem::Output::Packet(datagram)) = link.poll_output() {
                    ready.push(datagram);
                }
                for datagram in ready {
                    self.deliver(datagram)?;
                }
                Ok(())
            }
        }
    }

    fn drain(&mut self, index: usize) -> Result<(), RtcError> {
        let now = self.clock.now;
        loop {
            match self.peers[index].rtc.poll_output()? {
                Output::Timeout(at) => {
                    self.peers[index].next_timeout =
                        if at > now { at } else { now + self.min_step };
                    return Ok(());
                }
                Output::Transmit(transmit) => self.send(transmit),
                Output::Event(event) => self.peers[index].events.push((now, event)),
            }
        }
    }

    fn send(&mut self, transmit: Transmit) {
        let source = self
            .nat_out
            .get(&transmit.source)
            .copied()
            .unwrap_or(transmit.source);
        if let Some(filter) = &mut self.drop_filter
            && filter(source, transmit.destination, &transmit.contents)
        {
            return;
        }
        let datagram = Datagram {
            proto: transmit.proto,
            source,
            destination: transmit.destination,
            contents: transmit.contents.to_vec(),
        };
        self.links
            .entry((source, transmit.destination))
            .or_insert_with(|| Netem::new(NetemConfig::new()))
            .handle_input(netem::Input::Packet(self.clock.now, datagram));
    }

    fn deliver(&mut self, datagram: Datagram) -> Result<(), RtcError> {
        let now = self.clock.now;
        *self
            .link_bytes
            .entry((datagram.source, datagram.destination))
            .or_default() += datagram.contents.len() as u64;
        let destination = self
            .nat_in
            .get(&datagram.destination)
            .copied()
            .unwrap_or(datagram.destination);
        let input = Input::Receive(
            now,
            Receive {
                proto: datagram.proto,
                source: datagram.source,
                destination,
                contents: datagram.contents.as_slice().try_into()?,
            },
        );
        let bound = self.bindings.get(&destination).map(Vec::as_slice);
        let target = match bound {
            None | Some([]) => None,
            // A socket owned by a single peer hands it everything.
            Some([only]) => Some(*only),
            Some(shared) => {
                let accepting: Vec<usize> = shared
                    .iter()
                    .copied()
                    .filter(|index| self.peers[*index].rtc.accepts(&input))
                    .collect();
                if accepting.len() > 1 {
                    self.stats.ambiguous += 1;
                }
                accepting.first().copied()
            }
        };
        let Some(index) = target else {
            self.stats.undelivered += 1;
            return Ok(());
        };
        self.stats.delivered += 1;
        self.peers[index].rtc.handle_input(input)?;
        self.drain(index)
    }
}

/// Offer and answer text exchanged by [`negotiate_with`], after rewriting.
#[derive(Debug)]
pub struct Negotiated<R> {
    pub result: R,
    pub offer: String,
    pub answer: String,
}

/// One SDP offer/answer round from `offerer` to `answerer`.
///
/// `change` edits the offer. Both SDP texts pass through the map functions
/// before the remote side parses them, which is how SDP rewrites are tested.
pub fn negotiate_with<R>(
    net: &mut Network,
    offerer: PeerId,
    answerer: PeerId,
    change: impl FnOnce(&mut SdpApi) -> R,
    map_offer: impl FnOnce(String) -> String,
    map_answer: impl FnOnce(String) -> String,
) -> Result<Negotiated<R>, RtcError> {
    let mut api = net.peer_mut(offerer).rtc.sdp_api();
    let result = change(&mut api);
    let (offer, pending) = api.apply().expect("the change produces an offer");
    let offer = map_offer(offer.to_sdp_string());
    let answer = net
        .peer_mut(answerer)
        .rtc
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer)?)?;
    let answer = map_answer(answer.to_sdp_string());
    net.peer_mut(offerer)
        .rtc
        .sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer)?)?;
    Ok(Negotiated {
        result,
        offer,
        answer,
    })
}

/// [`negotiate_with`] without rewriting.
pub fn negotiate<R>(
    net: &mut Network,
    offerer: PeerId,
    answerer: PeerId,
    change: impl FnOnce(&mut SdpApi) -> R,
) -> Result<Negotiated<R>, RtcError> {
    negotiate_with(net, offerer, answerer, change, |o| o, |a| a)
}

/// Connect two peers through str0m's direct API without SDP: exchange ICE
/// credentials, DTLS fingerprints and host candidates, then start DTLS and
/// SCTP with `controlling` as the active side. Drive the network afterwards.
pub fn connect_direct(net: &mut Network, controlling: PeerId, controlled: PeerId) {
    let exchange = |net: &mut Network, id: PeerId| {
        let peer = net.peer_mut(id);
        let addr = peer.addr;
        let api = peer.rtc.direct_api();
        (
            api.local_dtls_fingerprint().clone(),
            api.local_ice_credentials(),
            addr,
        )
    };
    let (finger_a, creds_a, addr_a) = exchange(net, controlling);
    let (finger_b, creds_b, addr_b) = exchange(net, controlled);
    for (id, finger, creds, remote, active) in [
        (controlling, finger_b, creds_b, addr_b, true),
        (controlled, finger_a, creds_a, addr_a, false),
    ] {
        let rtc = &mut net.peer_mut(id).rtc;
        rtc.add_remote_candidate(Candidate::host(remote, "udp").expect("valid host candidate"));
        let mut api = rtc.direct_api();
        api.set_remote_fingerprint(finger);
        api.set_remote_ice_credentials(creds);
        api.set_ice_controlling(active);
        api.start_dtls(active).expect("DTLS starts once");
        api.start_sctp(active);
    }
}
