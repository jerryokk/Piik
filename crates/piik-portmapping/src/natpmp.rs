//! NAT-PMP (RFC 6886) gateway through `crab_nat`: gateway-assigned ports are
//! kept, a cached port is renewed first, and deletion is sent even without a
//! cached reply.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::num::NonZeroU16;
use std::sync::Mutex;
use std::time::Duration;

use crab_nat::{GatewayAddress, InternetProtocol, PortMappingOptions, TimeoutConfig};

use crate::{BoxFuture, Error, Gateway, Scope};

/// RFC 6886 section 3.1: 250 ms initial wait, doubling, nine attempts. The
/// caller's scope bounds the schedule.
const TIMEOUTS: TimeoutConfig = TimeoutConfig {
    initial_timeout: Duration::from_millis(250),
    max_retries: 8,
    max_retry_timeout: None,
};
/// Fresh mapping attempts when the gateway rejects a suggested port.
const RANDOM_ATTEMPTS: usize = 3;

pub(crate) struct Client {
    gateway: Ipv4Addr,
    /// Internal port to the gateway-assigned external port.
    ports: Mutex<HashMap<u16, u16>>,
}

impl Client {
    fn address(&self) -> GatewayAddress {
        GatewayAddress::IpV4(self.gateway)
    }

    async fn request(
        &self,
        scope: &Scope,
        internal: NonZeroU16,
        external: u16,
        lifetime: u32,
    ) -> Result<u16, Error> {
        let options = PortMappingOptions {
            external_port: NonZeroU16::new(external),
            lifetime_seconds: Some(lifetime),
            timeout_config: Some(TIMEOUTS),
        };
        scope
            .run(async {
                crab_nat::natpmp::port_mapping(
                    self.address(),
                    InternetProtocol::Udp,
                    internal,
                    options,
                )
                .await
                .map(|mapping| mapping.external_port().get())
                .map_err(Error::gateway)
            })
            .await
    }

    fn cached(&self, internal: u16) -> Option<u16> {
        self.ports
            .lock()
            .ok()
            .and_then(|ports| ports.get(&internal).copied())
    }

    fn remember(&self, internal: u16, external: Option<u16>) {
        if let Ok(mut ports) = self.ports.lock() {
            match external {
                Some(external) => ports.insert(internal, external),
                None => ports.remove(&internal),
            };
        }
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
            let internal = NonZeroU16::new(local_port).ok_or(Error::InvalidPort(local_port))?;
            let lifetime = u32::try_from(lease.as_secs()).unwrap_or(u32::MAX);
            if let Some(external) = self.cached(local_port)
                && let Ok(assigned) = self.request(scope, internal, external, lifetime).await
            {
                self.remember(local_port, Some(assigned));
                return Ok(assigned);
            }
            let mut last = Error::NoNatFound;
            for _ in 0..RANDOM_ATTEMPTS {
                if let Some(error) = scope.error() {
                    return Err(error);
                }
                match self.request(scope, internal, random_port(), lifetime).await {
                    Ok(assigned) => {
                        self.remember(local_port, Some(assigned));
                        return Ok(assigned);
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
            let internal = NonZeroU16::new(local_port).ok_or(Error::InvalidPort(local_port))?;
            // RFC 6886 section 3.4: lifetime and suggested external port are both
            // zero. Send even without a cached reply: an earlier request may have
            // succeeded on the gateway while its reply was lost.
            scope
                .run(async {
                    crab_nat::natpmp::try_drop_mapping(
                        self.address(),
                        InternetProtocol::Udp,
                        Some(internal),
                        Some(TIMEOUTS),
                    )
                    .await
                    .map_err(Error::gateway)
                })
                .await?;
            self.remember(local_port, None);
            Ok(())
        })
    }
}

/// External port suggestion in [10000, 65535), as in the vendored client.
pub(crate) fn random_port() -> u16 {
    rand::random_range(10_000..u16::MAX)
}

/// Confirms a NAT-PMP gateway at the default IPv4 next hop.
pub(crate) async fn discover(scope: &Scope) -> Result<Client, Error> {
    let route = crate::route::default_route4()?;
    scope
        .run(async {
            crab_nat::natpmp::external_address(GatewayAddress::IpV4(route.gateway), Some(TIMEOUTS))
                .await
                .map_err(Error::gateway)
        })
        .await?;
    Ok(Client {
        gateway: route.gateway,
        ports: Mutex::new(HashMap::new()),
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn random_ports_stay_in_the_vendored_range() {
        for _ in 0..1000 {
            let port = super::random_port();
            assert!((10_000..u16::MAX).contains(&port));
        }
    }
}
