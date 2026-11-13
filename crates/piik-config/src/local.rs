//! App-selected Local composition. It never reads the process environment.

use std::collections::HashSet;
use std::net::Ipv4Addr;

use piik_protocol::{
    DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY, MAX_ICE_SERVER_URLS,
    MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS, MAX_VIEWERS_PER_ROOM_LIMIT, is_js_whitespace,
    valid_stun_url_value,
};
use url::Url;

use crate::hosted::{DEFAULT_PORT, socket_address};
use crate::origin::{is_bare_origin, origin, origin_url, parse_url};
use crate::{Config, ConfigError, Environment, Result};

/// Inputs the App selects for its embedded Local server.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalOptions {
    /// 0 selects the default 8787.
    pub port: u16,
    pub public_address: String,
    pub public_origin: String,
    pub allowed_addresses: Vec<String>,
    pub site_access_password: String,
    /// Public-link discovery only. Pure LAN uses its selected address and port.
    pub stun_urls: Vec<String>,
    pub nat_prediction_stun_urls: Vec<String>,
}

pub fn local(options: LocalOptions) -> Result<Config> {
    let port = if options.port == 0 {
        DEFAULT_PORT
    } else {
        options.port
    };
    let mut public_address = None;
    if !options.public_address.is_empty() {
        let parsed = local_ipv4(&options.public_address, "public address")?;
        if parsed.is_unspecified() || parsed.octets()[0] == 127 {
            return Err(unreachable_address());
        }
        public_address = Some(parsed);
    } else if options.public_origin.is_empty() {
        return Err(unreachable_address());
    }
    let mut allowed_addresses: Vec<Ipv4Addr> = public_address.into_iter().collect();
    for address in &options.allowed_addresses {
        let normalized = local_ipv4(address, "allowed address")?;
        if !allowed_addresses.contains(&normalized) {
            allowed_addresses.push(normalized);
        }
    }

    let mut stun_urls = options.stun_urls;
    let mut nat_prediction_stun_urls = options.nat_prediction_stun_urls;
    let mut stun_listen_addresses = Vec::new();
    if let (true, Some(address)) = (options.public_origin.is_empty(), public_address) {
        // Routed LANs may not carry Browser mDNS. Discover the actual address
        // through the existing Binding-only service, without public STUN.
        let endpoint = socket_address(address, port);
        stun_urls = vec![format!("stun:{endpoint}")];
        stun_listen_addresses = vec![endpoint];
        nat_prediction_stun_urls = Vec::new();
    }
    let prediction_count = nat_prediction_stun_urls.len();
    if stun_urls.len() + prediction_count > MAX_ICE_SERVER_URLS
        || !stun_urls.iter().all(|url| valid_stun_url_value(url))
        || !nat_prediction_stun_urls
            .iter()
            .all(|url| valid_stun_url_value(url))
        || (prediction_count != 0 && prediction_count != MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS)
        || (prediction_count > 0 && stun_urls.is_empty())
    {
        return Err(ConfigError::new("Local STUN URLs are invalid"));
    }

    // Loopback keeps the Host's stable HTTP origin. Local invitations use TLS;
    // the public tunnel still forwards the existing plain HTTP service.
    let lan_scheme = if options.public_origin.is_empty() {
        "https"
    } else {
        "http"
    };
    let public_base_url = match public_address {
        Some(address) if options.public_origin.is_empty() => {
            Url::parse(&format!("https://{address}:{port}/")).expect("an IPv4 origin parses")
        }
        _ => public_https_origin(&options.public_origin)?,
    };

    let lan_origin = |scheme: &str, host: &str| format!("{scheme}://{host}:{port}");
    let mut allowed_origins = HashSet::from([
        lan_origin("http", "localhost"),
        lan_origin("http", "127.0.0.1"),
    ]);
    for address in &allowed_addresses {
        allowed_origins.insert(lan_origin(lan_scheme, &address.to_string()));
    }
    allowed_origins.insert(origin(&public_base_url));

    Ok(Config {
        env: Environment::Production,
        port,
        listen_host: "0.0.0.0".to_owned(),
        public_base_url,
        tls: None,
        allowed_origins,
        site_access_password: options.site_access_password,
        room_database_path: None,
        room_empty_timeout_seconds: 0,
        max_viewers_per_room: MAX_VIEWERS_PER_ROOM_LIMIT,
        endpoint_media_copy_capacity: DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
        sfu: None,
        stun_urls,
        stun_listen_addresses,
        nat_prediction_enabled: prediction_count == MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS,
        nat_prediction_stun_urls: Some(nat_prediction_stun_urls),
    })
}

fn unreachable_address() -> ConfigError {
    ConfigError::new("Local server public address must be reachable from the LAN")
}

/// One error message for all invalid origins.
pub(crate) fn public_https_origin(value: &str) -> Result<Url> {
    parse_url(value, &["https"])
        .ok()
        .filter(is_bare_origin)
        .map(|parsed| origin_url(&parsed))
        .ok_or_else(|| ConfigError::new("Local public origin must be an HTTPS origin"))
}

/// Node's `isIP(value) === 4` accepts only a dotted quad, so an IPv4-mapped
/// IPv6 literal is rejected as well.
fn local_ipv4(value: &str, name: &str) -> Result<Ipv4Addr> {
    value
        .trim_matches(is_js_whitespace)
        .parse()
        .map_err(|_| ConfigError::new(format!("Local server {name} must be an IPv4 address")))
}
