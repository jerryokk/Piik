//! Local candidate gathering per ICE generation.
//!
//! Host candidates are emitted first. A STUN survey on the connection's own
//! socket and an optional gateway mapping add server-reflexive candidates;
//! neither delays the SDP or host candidates. End-of-candidates follows both.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use piik_media_transport::{
    ConnSocket, IceCandidateInit, STUN_SURVEY_TIMEOUT, SURVEY_CANDIDATE_PRIORITY,
};
use piik_portmapping::Mapping;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::events::{Current, EdgeEvent, EventSink};

/// One STUN server's URLs, as Piik signaling carries them.
pub type IceServerUrls = Vec<String>;

pub const MAPPED_CANDIDATE_PRIORITY: u32 = SURVEY_CANDIDATE_PRIORITY - (1 << 8);

/// One observed mapping. `server_index` is the one-based position in the
/// filtered STUN URL list, independent of response order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedAddress {
    pub address: IpAddr,
    pub port: u16,
    pub server_index: usize,
}

/// A `stun:` UDP destination of the survey.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StunUrl {
    /// One-based position among the `stun:` URLs, usable or not.
    pub index: usize,
    pub host: String,
    pub port: u16,
}

/// Only UDP `stun:` URLs, in order.
/// An unusable `stun:` URL keeps its position so later identities hold.
pub(crate) fn stun_urls(servers: &[IceServerUrls]) -> Vec<StunUrl> {
    servers
        .iter()
        .flatten()
        .filter(|url| {
            url.get(..5)
                .is_some_and(|scheme| scheme.eq_ignore_ascii_case("stun:"))
        })
        .enumerate()
        .filter_map(|(index, url)| parse_stun(url, index + 1))
        .collect()
}

fn parse_stun(url: &str, index: usize) -> Option<StunUrl> {
    let (scheme, rest) = url.split_once(':')?;
    if !scheme.eq_ignore_ascii_case("stun") {
        return None;
    }
    let (address, query) = rest.split_once('?').unwrap_or((rest, ""));
    if !query.is_empty()
        && query
            .split('&')
            .any(|pair| pair.eq_ignore_ascii_case("transport=tcp"))
    {
        return None;
    }
    let (host, port) = if let Some(stripped) = address.strip_prefix('[') {
        let (host, port) = stripped.split_once(']')?;
        (host, port.strip_prefix(':').unwrap_or("3478"))
    } else {
        match address.rsplit_once(':') {
            Some((host, port)) => (host, port),
            None => (address, "3478"),
        }
    };
    let port: u16 = port.parse().ok()?;
    (!host.is_empty() && port != 0).then(|| StunUrl {
        index,
        host: host.to_owned(),
        port,
    })
}

pub(crate) fn has_stun(servers: &[IceServerUrls]) -> bool {
    !stun_urls(servers).is_empty()
}

/// Queries every destination concurrently from `socket` within one bound and
/// calls `emit` per IPv4 observation as it arrives.
pub(crate) async fn survey(
    socket: &ConnSocket,
    servers: &[StunUrl],
    cancel: &CancellationToken,
    mut emit: impl FnMut(MappedAddress),
) {
    let deadline = tokio::time::Instant::now() + STUN_SURVEY_TIMEOUT;
    let mut lookups = JoinSet::new();
    for server in servers {
        let server = server.clone();
        // Resolve each destination independently: one broken resolver must
        // not stall healthy STUN.
        lookups.spawn(async move {
            let address = tokio::time::timeout_at(
                deadline,
                tokio::net::lookup_host((server.host.as_str(), server.port)),
            )
            .await
            .ok()?
            .ok()?
            .find(SocketAddr::is_ipv4)?;
            Some((server.index, address))
        });
    }
    let mut queries = futures_unordered();
    // Duplicate destinations share one observation within this gathering.
    let mut queried: Vec<(SocketAddr, Vec<usize>)> = Vec::new();
    // Ends when every destination resolved and answered or failed.
    while !lookups.is_empty() || !queries.is_empty() {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep_until(deadline) => return,
            resolved = lookups.join_next(), if !lookups.is_empty() => {
                let Some(Ok(Some((index, address)))) = resolved else { continue };
                if let Some((_, indexes)) = queried.iter_mut().find(|(known, _)| *known == address) {
                    indexes.push(index);
                    continue;
                }
                queried.push((address, vec![index]));
                let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
                queries.push(query(socket, address, timeout));
            }
            observed = next(&mut queries), if !queries.is_empty() => {
                let Some((server, Some(mapped))) = observed else { continue };
                if !mapped.is_ipv4() {
                    continue;
                }
                let indexes = queried
                    .iter()
                    .find(|(known, _)| *known == server)
                    .map(|(_, indexes)| indexes.clone())
                    .unwrap_or_default();
                for server_index in indexes {
                    emit(MappedAddress { address: mapped.ip(), port: mapped.port(), server_index });
                }
            }
        }
    }
}

type Query<'a> =
    std::pin::Pin<Box<dyn Future<Output = (SocketAddr, Option<SocketAddr>)> + Send + 'a>>;

fn futures_unordered<'a>() -> Vec<Query<'a>> {
    Vec::new()
}

fn query(socket: &ConnSocket, server: SocketAddr, timeout: Duration) -> Query<'_> {
    Box::pin(async move { (server, socket.query_stun(server, timeout).await) })
}

/// Polls the first ready query of a small set.
async fn next(queries: &mut Vec<Query<'_>>) -> Option<(SocketAddr, Option<SocketAddr>)> {
    std::future::poll_fn(|context| {
        for index in 0..queries.len() {
            if let std::task::Poll::Ready(result) = queries[index].as_mut().poll(context) {
                drop(queries.swap_remove(index));
                return std::task::Poll::Ready(Some(result));
            }
        }
        std::task::Poll::Pending
    })
    .await
}

/// One gathering generation's delivery authority.
#[derive(Clone)]
pub(crate) struct Generation {
    counter: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    mine: u64,
}

impl Generation {
    pub fn is_current(&self) -> bool {
        !self.closed.load(Ordering::Acquire) && self.counter.load(Ordering::Acquire) == self.mine
    }

    pub fn current(&self) -> Current {
        let generation = self.clone();
        Arc::new(move || generation.is_current())
    }
}

/// The owner side of successive gatherings on one connection.
#[derive(Clone, Default)]
pub(crate) struct Gatherings {
    counter: Arc<AtomicU64>,
    closed: Arc<AtomicBool>,
    cancel: Arc<std::sync::Mutex<Option<CancellationToken>>>,
}

impl Gatherings {
    /// Retires the previous generation and returns the next one.
    pub fn next(&self, parent: &CancellationToken) -> (Generation, CancellationToken) {
        let mine = self.counter.fetch_add(1, Ordering::AcqRel) + 1;
        let cancel = parent.child_token();
        let previous = self
            .cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .replace(cancel.clone());
        if let Some(previous) = previous {
            previous.cancel();
        }
        (
            Generation {
                counter: self.counter.clone(),
                closed: self.closed.clone(),
                mine,
            },
            cancel,
        )
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(cancel) = self
            .cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .take()
        {
            cancel.cancel();
        }
    }
}

/// Confirms one gateway mapping for a gathering and returns its external
/// port.
pub(crate) type PrepareMapping =
    Arc<dyn Fn() -> std::pin::Pin<Box<dyn Future<Output = Option<u16>> + Send>> + Send + Sync>;

pub(crate) fn prepare_mapping(mapping: Arc<Mapping>) -> PrepareMapping {
    Arc::new(move || {
        let mapping = mapping.clone();
        Box::pin(async move { mapping.prepare().await })
    })
}

pub(crate) struct GatherJob {
    pub socket: Arc<ConnSocket>,
    pub hosts: Vec<IceCandidateInit>,
    pub servers: Vec<StunUrl>,
    pub mapping: Option<PrepareMapping>,
    pub mid: String,
    pub ufrag: String,
    pub local_port: u16,
    pub sink: EventSink,
    pub generation: Generation,
    pub cancel: CancellationToken,
}

fn candidate(line: String, mid: &str, ufrag: &str) -> IceCandidateInit {
    IceCandidateInit {
        candidate: line,
        sdp_mid: Some(mid.to_owned()),
        sdp_m_line_index: Some(0),
        username_fragment: Some(ufrag.to_owned()),
    }
}

/// Runs one gathering to completion or retirement.
pub(crate) async fn run(job: GatherJob) {
    let GatherJob {
        socket,
        hosts,
        servers,
        mapping,
        mid,
        ufrag,
        local_port,
        sink,
        generation,
        cancel,
    } = job;
    let emit = |value: Option<IceCandidateInit>| {
        if generation.is_current() {
            sink(EdgeEvent::LocalCandidate {
                candidate: value,
                current: generation.current(),
            });
        }
    };
    for host in hosts {
        emit(Some(host));
    }
    let mapping = mapping.filter(|_| !servers.is_empty());
    let mut mapped_port: Option<u16> = None;
    let mut observed: Vec<MappedAddress> = Vec::new();
    let mut mapped: HashSet<(IpAddr, u16)> = HashSet::new();
    let emit_mapped = |port: u16, value: &MappedAddress, mapped: &mut HashSet<(IpAddr, u16)>| {
        if port == 0 || value.port == port || !mapped.insert((value.address, port)) {
            return;
        }
        emit(Some(candidate(
            format!(
                "candidate:mp{} 1 udp {MAPPED_CANDIDATE_PRIORITY} {} {port} typ srflx raddr 0.0.0.0 rport {local_port}",
                mapped.len(),
                value.address
            ),
            &mid,
            &ufrag,
        )));
    };
    let mut prepare = mapping.map(|prepare| prepare());
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
    // The closure owns the sender; it drops when the survey completes.
    let mut surveying = std::pin::pin!(survey(&socket, &servers, &cancel, move |value| {
        let _ = sender.send(value);
    }));
    let (mut survey_done, mut channel_open) = (false, true);
    // Gateway discovery and STUN are additive; end-of-candidates follows both.
    while channel_open || prepare.is_some() {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = &mut surveying, if !survey_done => survey_done = true,
            port = async { prepare.as_mut().expect("mapping pending").await }, if prepare.is_some() => {
                prepare = None;
                let port = port.unwrap_or(0);
                mapped_port = Some(port);
                for value in std::mem::take(&mut observed) {
                    emit_mapped(port, &value, &mut mapped);
                }
            }
            value = receiver.recv(), if channel_open => {
                let Some(value) = value else {
                    channel_open = false;
                    continue;
                };
                emit(Some(candidate(
                    format!(
                        "candidate:ns{} 1 udp {SURVEY_CANDIDATE_PRIORITY} {} {} typ srflx raddr 0.0.0.0 rport {local_port}",
                        value.server_index, value.address, value.port
                    ),
                    &mid,
                    &ufrag,
                )));
                match mapped_port {
                    Some(port) => emit_mapped(port, &value, &mut mapped),
                    None => observed.push(value),
                }
            }
        }
    }
    if !cancel.is_cancelled() {
        emit(None);
    }
}
