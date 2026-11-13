//! The per-connection socket and SDP/candidate owner shared by edges,
//! publications and receivers.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use piik_media_transport::str0m::Candidate;
use piik_media_transport::str0m::change::{SdpApi, SdpPendingOffer};
use piik_media_transport::str0m::stats::PeerStats;
use piik_media_transport::{ConnSocket, Connection, IceCandidateInit, PcState, PunchedSocket};
use piik_portmapping::Mapping;
use tokio_util::sync::CancellationToken;

use crate::engine::Settings;
use crate::error::{Error, Result};
use crate::events::{EdgeEvent, EventSink, StateWatch};
use crate::gather::{self, Gatherings, IceServerUrls};
use crate::quality::{CandidateType, SelectedPair, selected_nat_traversal_path};

/// Remote candidates held until the answer.
pub const MAX_PENDING_CANDIDATES: usize = 64;

/// One connection's socket, gateway mapping and gathering owners. A
/// connection keeps its socket for life; ICE restart keeps it too.
pub(crate) struct Link {
    pub socket: Arc<ConnSocket>,
    pub mapping: Option<Arc<Mapping>>,
    pub hosts: Vec<Candidate>,
    pub gatherings: Gatherings,
    /// Applied remote candidates by address, with type and foundation.
    pub remote: HashMap<SocketAddr, (CandidateType, String)>,
}

impl Link {
    /// Binds a fresh socket or adopts a punched one, and takes this
    /// connection's host snapshot. Must run inside the Tokio runtime.
    pub fn open(
        settings: &Settings,
        punched: Option<PunchedSocket>,
        servers: &[IceServerUrls],
        mapping: bool,
    ) -> Result<Self> {
        let socket = match punched {
            Some(punched) => ConnSocket::adopt(punched)?,
            None => ConnSocket::bind(settings.bind)
                .map_err(|_| Error::Invalid("native media UDP socket is unavailable"))?,
        };
        let hosts = socket.gather_hosts(settings.include_loopback)?;
        let port = socket.local_addr()?.port();
        let mapping = (settings.port_mapping && mapping && gather::has_stun(servers))
            .then(|| Arc::new(Mapping::start(port)));
        Ok(Self {
            socket: Arc::new(socket),
            mapping,
            hosts,
            gatherings: Gatherings::default(),
            remote: HashMap::new(),
        })
    }

    /// Routes this socket to `conn` on the engine's shard and installs the
    /// host candidates.
    pub fn bind(&self, inbox: piik_media_transport::Inbox, conn: &mut Connection) {
        self.socket.attach(conn.id(), inbox);
        conn.set_egress(self.socket.egress());
        for host in &self.hosts {
            conn.add_local_candidate(host.clone());
        }
    }

    pub fn local_port(&self) -> u16 {
        self.socket.local_addr().map_or(0, |addr| addr.port())
    }

    /// The proved tuple becomes a remote candidate.
    pub fn add_punched_peer(&mut self, conn: &mut Connection) -> Result<()> {
        if let Some(candidate) = self.socket.punched_candidate() {
            self.apply_remote(conn, &candidate)?;
        }
        Ok(())
    }

    pub fn apply_remote(&mut self, conn: &mut Connection, init: &IceCandidateInit) -> Result<()> {
        conn.add_ice_candidate(init)?;
        if let Some((addr, kind, foundation)) = describe(&init.candidate) {
            self.remote.insert(addr, (kind, foundation));
        }
        Ok(())
    }

    /// Starts one gathering generation; the previous one stops delivering.
    pub fn start_gathering(
        &self,
        parent: &CancellationToken,
        servers: &[IceServerUrls],
        mapping: bool,
        mid: &str,
        ufrag: &str,
        sink: EventSink,
    ) {
        let (generation, cancel) = self.gatherings.next(parent);
        let hosts = self
            .hosts
            .iter()
            .map(|host| IceCandidateInit::local(host, mid, ufrag))
            .collect();
        let job = gather::GatherJob {
            socket: self.socket.clone(),
            hosts,
            servers: gather::stun_urls(servers),
            mapping: self
                .mapping
                .clone()
                .filter(|_| mapping)
                .map(gather::prepare_mapping),
            mid: mid.to_owned(),
            ufrag: ufrag.to_owned(),
            local_port: self.local_port(),
            sink,
            generation,
            cancel,
        };
        tokio::spawn(gather::run(job));
    }

    /// The selected pair from one stats snapshot. str0m reports addresses
    /// only; the remote type and foundation come from the applied candidates,
    /// and an unknown remote is peer-reflexive.
    pub fn selected_pair(&self, remote: SocketAddr) -> SelectedPair {
        let (remote_type, foundation) = self
            .remote
            .get(&remote)
            .cloned()
            .unwrap_or((CandidateType::Prflx, String::new()));
        SelectedPair {
            local: CandidateType::Host,
            remote: remote_type,
            nat_traversal_path: selected_nat_traversal_path(&["host", &foundation]),
        }
    }

    /// Stops gathering and releases the socket after gateway cleanup settles,
    /// so a late deletion cannot remove a replacement connection's mapping on
    /// a recycled port.
    pub fn close(&mut self) {
        self.gatherings.close();
        if let Some(mapping) = self.mapping.take() {
            let socket = self.socket.clone();
            tokio::spawn(async move {
                mapping.close().await;
                drop(socket);
            });
        }
    }
}

/// `(address, type, foundation)` of one candidate line.
pub(crate) fn describe(line: &str) -> Option<(SocketAddr, CandidateType, String)> {
    let value = line.strip_prefix("candidate:").unwrap_or(line);
    let fields: Vec<&str> = value.split_whitespace().collect();
    if fields.len() < 8 || fields[6] != "typ" {
        return None;
    }
    let ip = fields[4].parse().ok()?;
    let port = fields[5].parse().ok()?;
    Some((
        SocketAddr::new(ip, port),
        CandidateType::parse(fields[7])?,
        fields[0].to_owned(),
    ))
}

/// Content no ICE parser accepts. Hostname (mDNS) candidates are discarded
/// later, not rejected here.
pub(crate) fn malformed_candidate(init: &IceCandidateInit) -> bool {
    if init.candidate.is_empty() {
        return false;
    }
    let value = init
        .candidate
        .strip_prefix("candidate:")
        .unwrap_or(&init.candidate);
    let fields: Vec<&str> = value.split_whitespace().collect();
    if fields.len() < 8 || fields[6] != "typ" {
        return true;
    }
    if fields[4].parse::<std::net::IpAddr>().is_err() {
        return false;
    }
    Candidate::from_sdp_string(&format!("candidate:{value}")).is_err()
}

/// Removes candidate lines (trickled separately) and any end marker (owned
/// by the merged gathering).
pub(crate) fn trickle_sdp(sdp: &str) -> String {
    sdp.split_inclusive("\r\n")
        .filter(|line| {
            let line = line.trim_end();
            !line.starts_with("a=candidate:") && line != "a=end-of-candidates"
        })
        .collect()
}

/// The SDP without its end-of-candidates marker.
pub(crate) fn strip_end_of_candidates(sdp: &str) -> String {
    sdp.replace("a=end-of-candidates\r\n", "")
}

/// One connection's negotiation and lifecycle projection.
pub(crate) struct Signal {
    pub link: Link,
    pub sink: EventSink,
    pub servers: Vec<IceServerUrls>,
    pub state: StateWatch,
    pub local: bool,
    /// Whether this connection reports its selected path (publications
    /// report none).
    pub report_path: bool,
    pub path_reported: bool,
    pending: Option<SdpPendingOffer>,
    pub remote_described: bool,
    pending_candidates: Vec<IceCandidateInit>,
    pub channels: Vec<String>,
    pub offered: bool,
    pub local_offer_id: String,
    pub closed: bool,
}

impl Signal {
    pub fn new(
        link: Link,
        sink: EventSink,
        servers: Vec<IceServerUrls>,
        state: StateWatch,
    ) -> Self {
        Self {
            link,
            sink,
            servers,
            state,
            local: false,
            report_path: true,
            path_reported: false,
            pending: None,
            remote_described: false,
            pending_candidates: Vec::new(),
            channels: Vec::new(),
            offered: false,
            local_offer_id: String::new(),
            closed: false,
        }
    }

    #[cfg(test)]
    pub fn pending_candidates(&self) -> usize {
        self.pending_candidates.len()
    }

    pub fn emit(&self, event: EdgeEvent) {
        (self.sink)(event);
    }

    /// Creates an offer. The first offer adds `media` and the requested data
    /// channels; `restart` replaces ICE credentials and keeps the candidates.
    pub fn offer(
        &mut self,
        conn: &mut Connection,
        restart: bool,
        media: impl FnOnce(&mut SdpApi),
    ) -> Result<String> {
        if self.closed {
            return Err(Error::Closed);
        }
        let first = !self.offered;
        let channels = &self.channels;
        let (offer, pending) = conn
            .create_offer(|api| {
                if first {
                    media(api);
                    for label in channels {
                        api.add_channel(label.clone());
                    }
                }
                if restart {
                    api.ice_restart(true);
                }
            })
            .ok_or(Error::Invalid("native media offer is unavailable"))?;
        self.offered = true;
        self.pending = Some(pending);
        self.remote_described = false;
        Ok(offer)
    }

    /// Applies the remote answer. A repeated answer to the same offer is a
    /// retransmission; its first applied description stays authoritative.
    pub fn answer(
        &mut self,
        conn: &mut Connection,
        answer: &str,
        offer_id: &str,
        audio: bool,
    ) -> Result<()> {
        if self.local && (offer_id.is_empty() || offer_id != self.local_offer_id)
            || !self.local && !offer_id.is_empty()
        {
            return Err(Error::Invalid(
                "native media answer does not match its offer",
            ));
        }
        if self.closed {
            return Err(Error::Invalid("native media edge is closed"));
        }
        if self.remote_described {
            return Ok(());
        }
        if audio {
            piik_media_forwarding::check_audio_receive_limit(
                answer,
                piik_native_audio::MAX_BITRATE,
            )
            .map_err(piik_media_session::Error::from)?;
        }
        let pending = self
            .pending
            .take()
            .ok_or(Error::Invalid("native media edge requires an SDP answer"))?;
        conn.accept_answer(pending, answer)?;
        self.link.add_punched_peer(conn)?;
        self.remote_described = true;
        for candidate in std::mem::take(&mut self.pending_candidates) {
            // Candidates are disposable edge input; the connection state
            // remains the authority for a real media failure.
            let _ = self.link.apply_remote(conn, &candidate);
        }
        Ok(())
    }

    /// Malformed, stale and excess candidates are dropped without failing the
    /// shared control session.
    pub fn candidate(&mut self, conn: &mut Connection, candidate: IceCandidateInit) -> Result<()> {
        if self.local {
            return Err(Error::Invalid(
                "local media candidates belong in the SDP answer",
            ));
        }
        if malformed_candidate(&candidate) || self.closed {
            return Ok(());
        }
        if !self.remote_described {
            if self.pending_candidates.len() < MAX_PENDING_CANDIDATES {
                self.pending_candidates.push(candidate);
            }
            return Ok(());
        }
        let _ = self.link.apply_remote(conn, &candidate);
        Ok(())
    }

    /// Projects a connection state change to the owner's sink.
    pub fn on_state(&mut self, state: PcState, transport: bool) {
        self.state.set(state);
        if state != PcState::Connected {
            self.path_reported = false;
        }
        if transport {
            self.emit(EdgeEvent::InputTransport(state == PcState::Connected));
        }
        self.emit(EdgeEvent::State {
            state,
            current: self.state.clone(),
        });
    }

    /// Reports the selected path once per connected period from the first
    /// transport statistics.
    pub fn on_stats(&mut self, stats: &PeerStats) {
        if !self.report_path || self.path_reported || !self.state.is(PcState::Connected) {
            return;
        }
        let Some(pair) = &stats.selected_candidate_pair else {
            return;
        };
        self.path_reported = true;
        let pair = self.link.selected_pair(pair.remote.addr);
        self.emit(EdgeEvent::Path {
            pair,
            current: self.state.clone(),
        });
    }

    /// Retires signaling; a final `closed` state is reported once.
    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.pending = None;
        self.pending_candidates.clear();
        self.link.close();
        if !self.state.is(PcState::Closed) {
            self.state.set(PcState::Closed);
            self.emit(EdgeEvent::State {
                state: PcState::Closed,
                current: self.state.clone(),
            });
        }
    }
}
