//! UPnP Internet Gateway Device through `igd-next`. Discovery searches by
//! multicast SSDP and by unicast M-SEARCH to the default gateway, for networks
//! that filter multicast. `igd-next` only follows a description URL that names
//! the responder's own address, so a rogue responder cannot redirect discovery.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use igd_next::aio::Gateway as IgdGateway;
use igd_next::aio::tokio::Tokio;
use igd_next::{PortMappingProtocol, SearchOptions};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::natpmp::random_port;
use crate::{BoxFuture, Error, Gateway, Scope};

const SSDP_PORT: u16 = 1900;
const RANDOM_ATTEMPTS: usize = 3;
/// Bound for one search when the caller's scope has no deadline.
const SEARCH_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) struct Client {
    gateway: IgdGateway<Tokio>,
    /// Held across the device call and cache update so two callers cannot both
    /// miss the cache and create competing mappings.
    ports: Mutex<HashMap<u16, u16>>,
}

impl Client {
    /// The local address the routing table would use toward the device.
    async fn internal_address(&self) -> Result<IpAddr, Error> {
        let bind: SocketAddr = match self.gateway.addr {
            SocketAddr::V4(_) => (std::net::Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(_) => (std::net::Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(bind).await?;
        socket.connect(self.gateway.addr).await?;
        Ok(socket.local_addr()?.ip())
    }
}

impl Gateway for Client {
    fn add_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        description: &'a str,
        lease: Duration,
    ) -> BoxFuture<'a, Result<u16, Error>> {
        Box::pin(async move {
            if local_port == 0 {
                return Err(Error::InvalidPort(local_port));
            }
            let ip = scope.run(self.internal_address()).await.map_err(|error| {
                Error::gateway(std::io::Error::other(format!(
                    "get internal address: {error}"
                )))
            })?;
            let local = SocketAddr::new(ip, local_port);
            let lease = u32::try_from(lease.as_secs()).unwrap_or(u32::MAX);
            let add = |external: u16| {
                scope.run(async move {
                    self.gateway
                        .add_port(
                            PortMappingProtocol::UDP,
                            external,
                            local,
                            lease,
                            description,
                        )
                        .await
                        .map_err(Error::gateway)
                })
            };
            let mut ports = self.ports.lock().await;
            if let Some(&external) = ports.get(&local_port)
                && add(external).await.is_ok()
            {
                return Ok(external);
            }
            let mut last = Error::NoNatFound;
            for _ in 0..RANDOM_ATTEMPTS {
                let external = random_port();
                match add(external).await {
                    Ok(()) => {
                        ports.insert(local_port, external);
                        return Ok(external);
                    }
                    Err(error) => last = error,
                }
            }
            Err(last)
        })
    }

    fn delete_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let mut ports = self.ports.lock().await;
            let Some(&external) = ports.get(&local_port) else {
                return Ok(());
            };
            scope
                .run(async {
                    self.gateway
                        .remove_port(PortMappingProtocol::UDP, external)
                        .await
                        .map_err(Error::gateway)
                })
                .await?;
            // Drop the entry only once the device confirms, so a failed delete
            // leaves the mapping addressable for a retry.
            ports.remove(&local_port);
            Ok(())
        })
    }
}

fn options(scope: &Scope, broadcast: Option<SocketAddr>) -> SearchOptions {
    let timeout = scope.remaining().unwrap_or(SEARCH_TIMEOUT);
    let mut options = SearchOptions::default();
    options.timeout = Some(timeout);
    options.single_search_timeout = Some(timeout);
    if let Some(broadcast) = broadcast {
        options.broadcast_address = broadcast;
    }
    options
}

async fn search(scope: &Scope, broadcast: Option<SocketAddr>) -> Result<Client, Error> {
    let gateway = scope
        .run(async {
            igd_next::aio::tokio::search_gateway(options(scope, broadcast))
                .await
                .map_err(Error::gateway)
        })
        .await?;
    Ok(Client {
        gateway,
        ports: Mutex::new(HashMap::new()),
    })
}

/// Multicast SSDP search for a WAN connection service.
pub(crate) async fn discover_multicast(scope: &Scope) -> Result<Client, Error> {
    search(scope, None).await
}

/// Unicast M-SEARCH to the default IPv4 gateway.
pub(crate) async fn discover_unicast(scope: &Scope) -> Result<Client, Error> {
    let route = crate::route::default_route4()?;
    let gateway = SocketAddr::new(IpAddr::V4(route.gateway), SSDP_PORT);
    search(scope, Some(gateway)).await.map_err(|error| {
        Error::gateway(std::io::Error::other(format!(
            "no UPnP gateway found at {gateway}: {error}"
        )))
    })
}
