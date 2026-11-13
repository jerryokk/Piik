//! STUN listeners and the wire ICE configuration.

use std::net::{Ipv4Addr, SocketAddr};

use piik_protocol::{IceConfig, IceServer, IceServerUrls};

use crate::hosted::socket_address;
use crate::{Config, ConfigError, Result};

const NAT_PREDICTION_BASE_PORT: u16 = 3478;

/// The two listeners a NAT-prediction STUN deployment binds beside the base
/// port; their answers reveal the peer's port-allocation stride.
const NAT_PREDICTION_AUXILIARY_PORTS: [u16; 2] =
    [NAT_PREDICTION_BASE_PORT + 1, NAT_PREDICTION_BASE_PORT + 2];

pub(crate) fn stun_listen_addresses(host: &str, prediction: bool) -> Result<Vec<SocketAddr>> {
    let mut host = host.trim();
    if host.is_empty() {
        host = "0.0.0.0";
    }
    let address: Ipv4Addr = host
        .parse()
        .map_err(|_| ConfigError::new("STUN_LISTEN_HOST must be an IPv4 address"))?;
    let mut listeners = vec![socket_address(address, NAT_PREDICTION_BASE_PORT)];
    if prediction {
        listeners.extend(
            NAT_PREDICTION_AUXILIARY_PORTS
                .iter()
                .map(|port| socket_address(address, *port)),
        );
    }
    Ok(listeners)
}

/// The two auxiliary listeners derived from the first ordinary STUN
/// authority on UDP 3478 (explicit or implicit), or none.
pub fn nat_prediction_stun_urls(stun_urls: &[String]) -> Vec<String> {
    stun_urls
        .iter()
        .find_map(|stun_url| base_listener_host(stun_url))
        .map(|host| {
            NAT_PREDICTION_AUXILIARY_PORTS
                .iter()
                .map(|port| format!("stun:{host}:{port}"))
                .collect()
        })
        .unwrap_or_default()
}

/// The lowercase host of a STUN URL on the base port, IPv6 bracketed as the
/// WHATWG serializer keeps it. The port is compared numerically, so `03478`
/// is the base port while an explicit `80` is not.
fn base_listener_host(stun_url: &str) -> Option<String> {
    // Without a scheme separator the whole text is the authority.
    let authority = stun_url.split_once(':').map_or(stun_url, |(_, rest)| rest);
    let authority = authority.split(['/', '?', '#']).next().unwrap_or_default();
    let authority = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let (host, port) = if let Some(bracketed) = authority.strip_prefix('[') {
        let (host, rest) = bracketed.split_once(']')?;
        let port = match rest {
            "" => "",
            _ => rest.strip_prefix(':')?,
        };
        (host, port)
    } else {
        authority.rsplit_once(':').unwrap_or((authority, ""))
    };
    if host.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if !port.is_empty() && port.parse::<u64>().ok()? != u64::from(NAT_PREDICTION_BASE_PORT) {
        return None;
    }
    let host = host.to_lowercase();
    Some(if host.contains(':') {
        format!("[{host}]")
    } else {
        host
    })
}

/// The wire ICE configuration. A configured empty prediction list (Local
/// without prediction) is kept; only an absent one is derived.
pub fn ice_config(config: &Config) -> IceConfig {
    let mut ice = IceConfig::default();
    if !config.stun_urls.is_empty() {
        ice.ice_servers.push(IceServer {
            urls: IceServerUrls::List(config.stun_urls.clone()),
        });
    }
    if config.nat_prediction_enabled {
        ice.nat_prediction_stun_urls = config
            .nat_prediction_stun_urls
            .clone()
            .unwrap_or_else(|| nat_prediction_stun_urls(&config.stun_urls));
    }
    ice
}
