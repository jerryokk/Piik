//! Embedded SFU media.
//!
//! With `config.sfu` set, [`crate::Server::listen`] binds the str0m SFU
//! ([`piik_sfu::Media`]) on its `udp4` listener after STUN, and shutdown
//! closes it after signaling. Its physical callbacks reach room authority
//! through [`MediaEvents`], which the server attaches to signaling once room
//! authority is published. [`crate::Options::sfu`] replaces the embedded
//! runtime with an injected one; its owner keeps its sockets.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, RwLock, Weak};
use std::time::Duration;

use piik_config::SfuConfig;
use piik_media_transport::{HostAddrs, SharedUdp};
use piik_sfu::runtime::{MediaError, MediaEvent, Runtime};

/// Delivers SFU media callbacks to the current signaling owner. Events
/// raised before startup completes, or after shutdown, are dropped.
#[derive(Clone, Default)]
pub struct MediaEvents(Arc<RwLock<Weak<piik_signal::Server>>>);

impl std::fmt::Debug for MediaEvents {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("MediaEvents")
    }
}

impl MediaEvents {
    pub fn new() -> Self {
        Self::default()
    }

    /// Hands one physical-handle callback to room authority.
    pub fn deliver(&self, event: MediaEvent) {
        let signaling = self
            .0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .upgrade();
        if let Some(signaling) = signaling {
            signaling.handle_sfu_media_event(event);
        }
    }

    pub(crate) fn attach(&self, signaling: &Arc<piik_signal::Server>) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Arc::downgrade(signaling);
    }
}

/// The literal IPv4 address new media connections advertise as their host
/// candidate (Pion's NAT 1:1 address). The server resolves `SFU_PUBLIC_IP`
/// before startup and every minute for a DNS name, and hands each value to
/// the embedded SFU's host snapshot. Empty advertises the listener address.
#[derive(Debug, Clone, Default)]
pub struct SfuPublicAddress(Arc<RwLock<String>>);

impl SfuPublicAddress {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self) -> String {
        self.0
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn set(&self, address: String) {
        *self
            .0
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = address;
    }
}

/// An injected SFU media runtime for a configuration with `SFU_UDP_PORT`,
/// replacing the embedded one; room voice is [`Runtime::voice`].
pub struct SfuMedia {
    pub runtime: Arc<dyn Runtime>,
    /// The sink the runtime reports its callbacks to.
    pub events: MediaEvents,
    /// The resolved advertised address.
    pub public_address: SfuPublicAddress,
    /// Zero selects the router default.
    pub prepare_timeout_ms: i64,
}

/// The server-owned SFU on `config.sfu`'s UDP listener.
pub(crate) struct EmbeddedSfu {
    pub(crate) media: piik_sfu::Media,
    /// The interface snapshot taken at bind; refreshes only replace its
    /// NAT 1:1 address.
    hosts: HostAddrs,
}

impl EmbeddedSfu {
    /// Binds `udp4` only. Loopback candidates are offered only for a loopback
    /// listener. Requires a Tokio runtime; the SFU's shards
    /// run on it.
    pub(crate) fn bind(config: &SfuConfig, public_ip: &str) -> io::Result<Self> {
        let hosts = HostAddrs::gather(config.listen_host, config.listen_host.is_loopback())?;
        let udp = SharedUdp::bind(
            SocketAddrV4::new(config.listen_host, config.port),
            hosts.clone().with_nat_1to1(nat_1to1(public_ip)),
        )?;
        Ok(Self {
            media: piik_sfu::Media::new(Arc::new(udp)),
            hosts,
        })
    }

    /// New connections advertise `public_ip`; existing ones keep theirs.
    pub(crate) fn set_public_ip(&self, public_ip: &str) {
        self.media
            .udp()
            .set_hosts(self.hosts.clone().with_nat_1to1(nat_1to1(public_ip)));
    }

    /// The injection the signaling owner consumes, reporting callbacks to
    /// `events`.
    pub(crate) fn runtime(&self, events: &MediaEvents, address: &SfuPublicAddress) -> SfuMedia {
        let sink = events.clone();
        self.media
            .set_event_sink(Arc::new(move |event| sink.deliver(event)));
        SfuMedia {
            runtime: Arc::new(self.media.clone()),
            events: events.clone(),
            public_address: address.clone(),
            prepare_timeout_ms: 0,
        }
    }

    /// Closes every media connection and waits for their shards (blocking).
    pub(crate) fn close(&self) -> Result<(), MediaError> {
        self.media.close()
    }
}

/// Resolved values are IPv4 literals; empty advertises the listener.
fn nat_1to1(public_ip: &str) -> Option<Ipv4Addr> {
    public_ip.parse().ok()
}

/// Resolves `SFU_PUBLIC_IP` to the literal IPv4 candidate address media
/// advertises: a literal is kept, a DNS name resolves within five seconds,
/// and an empty value stays empty.
pub async fn resolve_sfu_public_ip(host: &str) -> Result<String, String> {
    if host.is_empty() {
        return Ok(String::new());
    }
    if let Ok(address) = host.parse::<IpAddr>() {
        return Ok(address.to_string());
    }
    let lookup = tokio::time::timeout(Duration::from_secs(5), tokio::net::lookup_host((host, 0)));
    let addresses = match lookup.await {
        Ok(Ok(addresses)) => addresses,
        Ok(Err(error)) => return Err(format!("SFU_PUBLIC_IP could not resolve to IPv4: {error}")),
        Err(_) => {
            return Err("SFU_PUBLIC_IP could not resolve to IPv4: lookup timed out".to_owned());
        }
    };
    addresses
        .map(|address| address.ip())
        .find_map(|address| match address {
            IpAddr::V4(address) => Some(address.to_string()),
            IpAddr::V6(address) => address.to_ipv4_mapped().map(|address| address.to_string()),
        })
        .ok_or_else(|| format!("SFU_PUBLIC_IP has no IPv4 address: {host}"))
}
