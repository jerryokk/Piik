//! Port Control Protocol (RFC 6887) gateways: an IPv4 NAT mapping and an
//! optional IPv6 firewall pinhole. MAP requests use `crab_nat`; discovery uses
//! the ANNOUNCE opcode, which that crate does not provide.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::num::NonZeroU16;
use std::time::Duration;

use crab_nat::{GatewayAddress, InternetProtocol, PortMappingOptions, TimeoutConfig};
use tokio::net::UdpSocket;
use tokio::sync::Mutex;

use crate::route::{Route4, Route6};
use crate::{BoxFuture, Error, Gateway, Scope};

const PORT: u16 = 5351;
const VERSION: u8 = 2;
const OP_ANNOUNCE: u8 = 0;
const OP_REPLY: u8 = 0x80;
const HEADER_SIZE: usize = 24;
const RESPONSE_BUFFER_SIZE: usize = 128;
/// RFC 6887 section 8.1.1 retransmission schedule: 3 s initial wait,
/// doubling, four attempts in total. Callers' scopes usually end it sooner.
const TIMEOUTS: TimeoutConfig = TimeoutConfig {
    initial_timeout: Duration::from_secs(3),
    max_retries: 3,
    max_retry_timeout: Some(Duration::from_secs(1024)),
};
/// RFC 6887 default requested lifetime for positive durations under a second.
const DEFAULT_LIFETIME: u32 = 7200;

type Nonce = [u32; 3];

/// One PCP server and the nonces of the mappings this process created.
pub(crate) struct Client {
    gateway: GatewayAddress,
    local: IpAddr,
    /// RFC 6887 requires renewals and deletes to reuse a mapping's nonce. The
    /// lock is held across each request so a delete cannot drop the nonce of a
    /// create still in flight for the same port.
    nonces: Mutex<HashMap<u16, Nonce>>,
}

impl Client {
    pub(crate) fn v4(route: Route4) -> Self {
        Self::new(GatewayAddress::IpV4(route.gateway), IpAddr::V4(route.local))
    }

    pub(crate) fn v6(route: Route6) -> Self {
        let scope_id = (route.scope_id != 0).then_some(route.scope_id);
        Self::new(
            GatewayAddress::IpV6(route.gateway, scope_id),
            IpAddr::V6(route.local),
        )
    }

    fn new(gateway: GatewayAddress, local: IpAddr) -> Self {
        Self {
            gateway,
            local,
            nonces: Mutex::new(HashMap::new()),
        }
    }

    fn server(&self) -> SocketAddr {
        match self.gateway {
            GatewayAddress::IpV4(ip) => SocketAddr::new(IpAddr::V4(ip), PORT),
            GatewayAddress::IpV6(ip, scope) => {
                SocketAddr::V6(SocketAddrV6::new(ip, PORT, 0, scope.unwrap_or(0)))
            }
        }
    }

    /// Confirms PCP support with ANNOUNCE and returns the server epoch.
    pub(crate) async fn announce(&self, scope: &Scope) -> Result<u32, Error> {
        let request = announce_request(self.local);
        let server = self.server();
        let bind: SocketAddr = match server {
            SocketAddr::V4(_) => (Ipv4Addr::UNSPECIFIED, 0).into(),
            SocketAddr::V6(_) => (Ipv6Addr::UNSPECIFIED, 0).into(),
        };
        let socket = UdpSocket::bind(bind).await?;
        let mut wait = TIMEOUTS.initial_timeout;
        let mut last = Error::DeadlineExceeded;
        for attempt in 0..=TIMEOUTS.max_retries {
            let timeout = scope
                .remaining()
                .map_or(wait, |remaining| remaining.min(wait));
            match scope
                .run(exchange(&socket, server, &request, timeout))
                .await
            {
                Ok(response) => return parse_announce(&response),
                Err(error @ (Error::Canceled | Error::DeadlineExceeded)) if scope.is_done() => {
                    return Err(error);
                }
                Err(error) => last = error,
            }
            if attempt < TIMEOUTS.max_retries {
                wait = (wait * 2).min(Duration::from_secs(1024));
            }
        }
        Err(Error::gateway(std::io::Error::other(format!(
            "PCP request failed after {} retries: {last}",
            TIMEOUTS.max_retries + 1
        ))))
    }

    pub(crate) async fn add(
        &self,
        scope: &Scope,
        local_port: u16,
        lease: Duration,
    ) -> Result<u16, Error> {
        let port = NonZeroU16::new(local_port).ok_or(Error::InvalidPort(local_port))?;
        let lifetime = match u32::try_from(lease.as_secs()).unwrap_or(u32::MAX) {
            0 => DEFAULT_LIFETIME,
            seconds => seconds,
        };
        let mut nonces = self.nonces.lock().await;
        // Store the nonce before sending: if the server creates the mapping
        // but the response is lost, a later delete must present it.
        let nonce = *nonces
            .entry(local_port)
            .or_insert_with(|| [rand::random(), rand::random(), rand::random()]);
        let options = PortMappingOptions {
            external_port: Some(port),
            lifetime_seconds: Some(lifetime),
            timeout_config: Some(TIMEOUTS),
        };
        let request = crab_nat::pcp::BaseMapRequest::new(
            self.gateway,
            self.local,
            InternetProtocol::Udp,
            port,
        );
        let mapping = scope
            .run(async {
                crab_nat::pcp::port_mapping(request, Some(nonce), None, options)
                    .await
                    .map_err(Error::gateway)
            })
            .await?;
        Ok(mapping.external_port().get())
    }

    /// Deletes this process's mapping. A mapping created by another process
    /// has an unknown nonce; the server treats that as a no-op and the mapping
    /// expires on its own.
    pub(crate) async fn delete(&self, scope: &Scope, local_port: u16) -> Result<(), Error> {
        let port = NonZeroU16::new(local_port).ok_or(Error::InvalidPort(local_port))?;
        let mut nonces = self.nonces.lock().await;
        let nonce = nonces
            .get(&local_port)
            .copied()
            .unwrap_or_else(|| [rand::random(), rand::random(), rand::random()]);
        scope
            .run(async {
                crab_nat::pcp::try_drop_mapping(
                    self.gateway,
                    self.local,
                    nonce,
                    crab_nat::pcp::DropMappingRange::Single {
                        protocol: InternetProtocol::Udp,
                        internal_port: port,
                    },
                    Some(TIMEOUTS),
                )
                .await
                .map_err(Error::gateway)
            })
            .await?;
        nonces.remove(&local_port);
        Ok(())
    }
}

impl Gateway for Client {
    fn add_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        _description: &'a str,
        lease: Duration,
    ) -> BoxFuture<'a, Result<u16, Error>> {
        Box::pin(async move {
            self.add(scope, local_port, lease).await.map_err(|error| {
                Error::gateway(std::io::Error::other(format!("add mapping: {error}")))
            })
        })
    }

    fn delete_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.delete(scope, local_port).await.map_err(|error| {
                Error::gateway(std::io::Error::other(format!("delete mapping: {error}")))
            })
        })
    }
}

fn mapped(ip: IpAddr) -> [u8; 16] {
    match ip {
        IpAddr::V4(ip) => ip.to_ipv6_mapped().octets(),
        IpAddr::V6(ip) => ip.octets(),
    }
}

fn announce_request(client: IpAddr) -> [u8; HEADER_SIZE] {
    let mut request = [0; HEADER_SIZE];
    request[0] = VERSION;
    request[1] = OP_ANNOUNCE;
    request[8..24].copy_from_slice(&mapped(client));
    request
}

fn parse_announce(response: &[u8]) -> Result<u32, Error> {
    let fail = |text: String| Error::gateway(std::io::Error::other(text));
    if response.len() < HEADER_SIZE {
        return Err(fail(format!(
            "response too short: {} bytes",
            response.len()
        )));
    }
    if response[0] != VERSION {
        return Err(fail(format!("unsupported PCP version: {}", response[0])));
    }
    if response[1] != OP_REPLY | OP_ANNOUNCE {
        return Err(fail(format!(
            "unexpected PCP reply opcode: 0x{:02x}",
            response[1]
        )));
    }
    // Byte 2 is reserved; byte 3 is the result code (RFC 6887 section 7.2).
    if response[3] != 0 {
        return Err(fail(format!("PCP ANNOUNCE failed: result {}", response[3])));
    }
    Ok(u32::from_be_bytes([
        response[8],
        response[9],
        response[10],
        response[11],
    ]))
}

/// Sends one request and returns the first reply from the server itself
/// (RFC 6887 section 8.3); other senders are ignored.
async fn exchange(
    socket: &UdpSocket,
    server: SocketAddr,
    request: &[u8],
    timeout: Duration,
) -> Result<Vec<u8>, Error> {
    socket.send_to(request, server).await?;
    let read = async {
        let mut buffer = [0; RESPONSE_BUFFER_SIZE];
        loop {
            let (length, from) = socket.recv_from(&mut buffer).await?;
            if from.ip() == server.ip() {
                return Ok::<_, Error>(buffer[..length].to_vec());
            }
        }
    };
    tokio::time::timeout(timeout, read)
        .await
        .map_err(|_| Error::gateway(std::io::Error::other("read: i/o timeout")))?
}

/// Finds and confirms the default IPv4 PCP server.
pub(crate) async fn discover_v4(scope: &Scope) -> Result<Client, Error> {
    scope.error().map_or(Ok(()), Err)?;
    let route = crate::route::default_route4().map_err(|error| {
        Error::gateway(std::io::Error::other(format!(
            "get default gateway: {error}"
        )))
    })?;
    let client = Client::v4(route);
    client
        .announce(scope)
        .await
        .map_err(|error| Error::gateway(std::io::Error::other(format!("PCP announce: {error}"))))?;
    Ok(client)
}

/// Finds and confirms the default IPv6 PCP server for firewall pinholes.
pub(crate) async fn discover_v6(scope: &Scope) -> Result<Client, Error> {
    scope.error().map_or(Ok(()), Err)?;
    let route = crate::route::default_route6().map_err(|error| {
        Error::gateway(std::io::Error::other(format!(
            "get default IPv6 gateway: {error}"
        )))
    })?;
    let client = Client::v6(route);
    client.announce(scope).await.map_err(|error| {
        Error::gateway(std::io::Error::other(format!("PCP IPv6 announce: {error}")))
    })?;
    Ok(client)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announce_request_maps_ipv4_clients() {
        let request = announce_request(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 4)));
        assert_eq!(request[0], 2);
        assert_eq!(request[1], 0);
        assert_eq!(&request[2..8], &[0; 6]);
        assert_eq!(&request[8..18], &[0; 10]);
        assert_eq!(&request[18..24], &[0xff, 0xff, 192, 168, 1, 4]);
    }

    #[test]
    fn announce_response_is_validated() {
        let mut response = [0u8; HEADER_SIZE];
        response[0] = 2;
        response[1] = 0x80;
        response[8..12].copy_from_slice(&42u32.to_be_bytes());
        assert_eq!(parse_announce(&response).unwrap(), 42);
        assert!(parse_announce(&response[..23]).is_err());
        let mut wrong = response;
        wrong[0] = 0;
        assert!(parse_announce(&wrong).is_err());
        let mut wrong = response;
        wrong[1] = 0;
        assert!(parse_announce(&wrong).is_err());
        let mut wrong = response;
        wrong[3] = 2;
        assert!(parse_announce(&wrong).is_err());
    }

    #[tokio::test]
    async fn announce_ignores_other_senders_and_accepts_the_server() {
        let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let address = server.local_addr().unwrap();
        let client = Client::new(
            GatewayAddress::IpV4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::LOCALHOST),
        );
        let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let reply = tokio::spawn(async move {
            let mut buffer = [0u8; 64];
            let (_, from) = server.recv_from(&mut buffer).await.unwrap();
            let mut response = [0u8; HEADER_SIZE];
            response[0] = 2;
            response[1] = 0x80;
            response[8..12].copy_from_slice(&7u32.to_be_bytes());
            server.send_to(&response, from).await.unwrap();
        });
        let response = exchange(
            &socket,
            address,
            &announce_request(client.local),
            Duration::from_secs(2),
        )
        .await
        .unwrap();
        assert_eq!(parse_announce(&response).unwrap(), 7);
        reply.await.unwrap();
    }
}
