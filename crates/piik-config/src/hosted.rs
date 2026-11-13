//! Hosted configuration from the process environment.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};

use piik_protocol::{
    DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY, MAX_ENDPOINT_MEDIA_COPY_CAPACITY, MAX_ICE_SERVER_URLS,
    MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS, MAX_SAFE_INTEGER, MAX_VIEWERS_PER_ROOM_LIMIT,
    is_js_whitespace, valid_stun_url_value,
};
use url::{Host, Url};

use crate::ice::{nat_prediction_stun_urls, stun_listen_addresses};
use crate::number::js_number;
use crate::origin::{OriginError, is_bare_origin, origin, origin_from_host, origin_url, parse_url};
use crate::{Config, ConfigError, DirectTls, Environment, Result, SfuConfig, TlsSource};

pub(crate) const DEFAULT_PORT: u16 = 8787;
const MAX_ROOM_EMPTY_TIMEOUT_SECONDS: u32 = 31_536_000;

const REMOVED_TURN_REASON: &str = "ordinary ICE accepts STUN_URLS only";

/// Settings this build does not read, with the replacement where one exists.
/// Rejecting them keeps a stale environment file from silently starting with a
/// weaker posture, such as `NODE_ENV=production` leaving `PIIK_ENV` unset.
const REPLACED_ENVIRONMENT_VARIABLES: [(&str, &str); 18] = [
    ("TURN_URLS", ""),
    ("TURN_SHARED_SECRET", ""),
    ("TURN_CREDENTIAL_TTL_SECONDS", ""),
    ("PEER_ICE_TURN_URLS", ""),
    ("PEER_ICE_TURN_SHARED_SECRET", ""),
    ("PEER_ICE_TURN_CREDENTIAL_TTL_SECONDS", ""),
    ("SELECTED_EDGE_TURN_URLS", ""),
    ("SELECTED_EDGE_TURN_SHARED_SECRET", ""),
    ("SELECTED_EDGE_TURN_CREDENTIAL_TTL_SECONDS", ""),
    ("SELECTED_EDGE_TURN_ALLOCATION_CAPACITY", ""),
    (
        "PEER_ASSISTED_ROOM_IDS",
        "peer-assisted media applies to every room",
    ),
    (
        "PEER_ASSISTED_MEDIA",
        "peer-assisted media is always enabled",
    ),
    ("HOST_ADMISSION_PASSWORD", "use SITE_ACCESS_PASSWORD"),
    (
        "MAX_PEER_RELAY_DOWNSTREAM_EDGES",
        "use ENDPOINT_MEDIA_COPY_CAPACITY",
    ),
    ("ROOM_TTL_SECONDS", "use ROOM_EMPTY_TIMEOUT_SECONDS"),
    ("ROOM_LEASE_SECONDS", "use ROOM_EMPTY_TIMEOUT_SECONDS"),
    ("ACCESS_PASSWORD", "use SITE_ACCESS_PASSWORD"),
    ("NODE_ENV", "use PIIK_ENV"),
];

/// Validates the whole process environment. Presence, not emptiness,
/// distinguishes an explicit value from an unset one where that distinction
/// is part of the contract. Direct TLS is validated as configuration only.
pub fn load(env: &HashMap<String, String>) -> Result<Config> {
    let get = |name: &str| value_of(env, name);
    for (name, replacement) in REPLACED_ENVIRONMENT_VARIABLES {
        if env.contains_key(name) {
            let detail = if replacement.is_empty() {
                REMOVED_TURN_REASON
            } else {
                replacement
            };
            return Err(ConfigError::new(format!(
                "{name} is no longer read; {detail}"
            )));
        }
    }

    let environment = match env.get("PIIK_ENV").map(String::as_str) {
        None | Some("development") => Environment::Development,
        Some("production") => Environment::Production,
        Some(_) => {
            return Err(ConfigError::new(
                "PIIK_ENV must be development or production",
            ));
        }
    };
    let port = parse_positive_integer(get("PORT"), i64::from(DEFAULT_PORT), "PORT")?;
    let port = u16::try_from(port)
        .map_err(|_| ConfigError::new(format!("PORT must be at most {}", u16::MAX)))?;
    let mut listen_host = get("LISTEN_HOST").trim_matches(is_js_whitespace).to_owned();
    if listen_host.is_empty() {
        listen_host = "0.0.0.0".to_owned();
    }

    // Only an absent variable takes the default; an explicit empty value is
    // invalid below.
    let raw_public_base_url = env
        .get("PUBLIC_BASE_URL")
        .cloned()
        .unwrap_or_else(|| format!("http://localhost:{port}"));
    let public_base_url = parse_origin_url(&raw_public_base_url, "PUBLIC_BASE_URL")?;
    if environment == Environment::Production && public_base_url.scheme() != "https" {
        return Err(ConfigError::new(
            "PUBLIC_BASE_URL must use https in production",
        ));
    }

    let site_access_password = get("SITE_ACCESS_PASSWORD").to_owned();
    let room_database_path = parse_room_database_path(get("ROOM_DATABASE_PATH"))?;
    let room_empty_timeout_seconds = parse_room_empty_timeout(get("ROOM_EMPTY_TIMEOUT_SECONDS"))?;
    let stun_urls = parse_stun_urls(get("STUN_URLS"), "STUN_URLS")?;
    let nat_prediction_enabled =
        parse_boolean(get("NAT_PREDICTION_ENABLED"), "NAT_PREDICTION_ENABLED")?;
    if nat_prediction_enabled && nat_prediction_stun_urls(&stun_urls).is_empty() {
        return Err(ConfigError::new(
            "NAT_PREDICTION_ENABLED requires a STUN_URLS entry on UDP 3478",
        ));
    }
    if nat_prediction_enabled
        && stun_urls.len() + MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS > MAX_ICE_SERVER_URLS
    {
        return Err(ConfigError::new(format!(
            "STUN_URLS must contain at most {} URLs when NAT_PREDICTION_ENABLED=true",
            MAX_ICE_SERVER_URLS - MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS
        )));
    }
    let stun_listen_addresses = if stun_urls.is_empty() {
        Vec::new()
    } else {
        stun_listen_addresses(get("STUN_LISTEN_HOST"), nat_prediction_enabled)?
    };
    let max_viewers_per_room = parse_bounded_integer(
        get("MAX_VIEWERS_PER_ROOM"),
        MAX_VIEWERS_PER_ROOM_LIMIT as i64,
        "MAX_VIEWERS_PER_ROOM",
        1,
        MAX_VIEWERS_PER_ROOM_LIMIT as i64,
    )?;
    let endpoint_media_copy_capacity = parse_bounded_integer(
        get("ENDPOINT_MEDIA_COPY_CAPACITY"),
        DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
        "ENDPOINT_MEDIA_COPY_CAPACITY",
        1,
        MAX_ENDPOINT_MEDIA_COPY_CAPACITY,
    )?;
    let sfu = parse_sfu(env)?;

    if environment == Environment::Production && stun_urls.is_empty() {
        return Err(ConfigError::new("STUN is required in production"));
    }

    let allowed_origins = parse_origins(get("ALLOWED_ORIGINS"), &origin(&public_base_url))?;
    let tls = parse_direct_tls(env, &public_base_url)?;

    Ok(Config {
        env: environment,
        port,
        listen_host,
        public_base_url,
        tls,
        allowed_origins,
        site_access_password,
        room_database_path,
        room_empty_timeout_seconds,
        max_viewers_per_room: max_viewers_per_room as usize,
        endpoint_media_copy_capacity,
        sfu,
        stun_urls,
        stun_listen_addresses,
        nat_prediction_enabled,
        nat_prediction_stun_urls: None,
    })
}

/// Defaults an absent value to false.
pub(crate) fn parse_boolean(value: &str, name: &str) -> Result<bool> {
    match value {
        "" | "false" => Ok(false),
        "true" => Ok(true),
        _ => Err(ConfigError::new(format!("{name} must be true or false"))),
    }
}

/// Preserves `Number.isSafeInteger`'s bounds over `Number(string)`.
fn parse_positive_integer(value: &str, fallback: i64, name: &str) -> Result<i64> {
    if value.is_empty() {
        return Ok(fallback);
    }
    match js_number(value) {
        Some(number)
            if number == number.trunc() && number > 0.0 && number <= MAX_SAFE_INTEGER as f64 =>
        {
            Ok(number as i64)
        }
        _ => Err(ConfigError::new(format!(
            "{name} must be a positive integer"
        ))),
    }
}

fn parse_bounded_integer(
    value: &str,
    fallback: i64,
    name: &str,
    minimum: i64,
    maximum: i64,
) -> Result<i64> {
    let parsed = parse_positive_integer(value, fallback, name)?;
    if parsed < minimum || parsed > maximum {
        return Err(ConfigError::new(format!(
            "{name} must be between {minimum} and {maximum}"
        )));
    }
    Ok(parsed)
}

/// Hosted defaults to durable room authority in its working directory.
/// Explicit paths stay absolute so a deployment cannot relocate an existing
/// database when its working directory changes. `:memory:` selects the
/// in-memory room store.
fn parse_room_database_path(value: &str) -> Result<Option<PathBuf>> {
    let path = value.trim_matches(is_js_whitespace);
    if path.is_empty() {
        return std::path::absolute("rooms.sqlite")
            .map(Some)
            .map_err(|error| ConfigError::new(error.to_string()));
    }
    if path == ":memory:" {
        return Ok(None);
    }
    if path.contains('\0') || !Path::new(path).is_absolute() {
        return Err(ConfigError::new(
            "ROOM_DATABASE_PATH must be an absolute file path",
        ));
    }
    Ok(Some(PathBuf::from(path)))
}

fn parse_room_empty_timeout(value: &str) -> Result<u32> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(0);
    }
    // strconv.Atoi semantics: an optional sign, decimal digits only.
    value
        .parse::<i64>()
        .ok()
        .and_then(|seconds| u32::try_from(seconds).ok())
        .filter(|seconds| *seconds <= MAX_ROOM_EMPTY_TIMEOUT_SECONDS)
        .ok_or_else(|| {
            ConfigError::new("ROOM_EMPTY_TIMEOUT_SECONDS must be an integer between 0 and 31536000")
        })
}

fn parse_sfu(env: &HashMap<String, String>) -> Result<Option<SfuConfig>> {
    let get = |name: &str| value_of(env, name);
    let only = parse_boolean(get("SFU_ONLY"), "SFU_ONLY")?;
    let raw_port = get("SFU_UDP_PORT").trim();
    if raw_port.is_empty() {
        if only {
            return Err(ConfigError::new("SFU_ONLY requires SFU_UDP_PORT"));
        }
        return Ok(None);
    }
    let port = parse_bounded_integer(raw_port, 0, "SFU_UDP_PORT", 1, i64::from(u16::MAX))?;
    let mut listen_host = get("SFU_LISTEN_HOST").trim();
    if listen_host.is_empty() {
        listen_host = "0.0.0.0";
    }
    let listen_host: Ipv4Addr = listen_host
        .parse()
        .map_err(|_| ConfigError::new("SFU_LISTEN_HOST must be an IPv4 address"))?;
    let public_ip = parse_sfu_public_ip(get("SFU_PUBLIC_IP").trim())
        .ok_or_else(|| ConfigError::new("SFU_PUBLIC_IP must be an IPv4 address or DNS hostname"))?;
    Ok(Some(SfuConfig {
        listen_host,
        port: port as u16,
        public_ip,
        only,
    }))
}

/// An IPv4 address, or a hostname normalized without resolving it.
fn parse_sfu_public_ip(value: &str) -> Option<String> {
    if value.is_empty() {
        return Some(String::new());
    }
    if let Ok(address) = value.parse::<IpAddr>() {
        return address.is_ipv4().then(|| address.to_string());
    }
    if value.contains(':') {
        return None;
    }
    let host = origin_from_host("http", value);
    let host = host.strip_prefix("http://")?;
    (!host.is_empty()).then(|| host.to_owned())
}

fn parse_url_list(value: &str, name: &str) -> Result<Vec<String>> {
    if value.trim_matches(is_js_whitespace).is_empty() {
        return Ok(Vec::new());
    }
    value
        .split(',')
        .map(|entry| {
            let trimmed = entry.trim_matches(is_js_whitespace);
            if trimmed.is_empty() {
                Err(ConfigError::new(format!("{name} contains an empty URL")))
            } else {
                Ok(trimmed.to_owned())
            }
        })
        .collect()
}

/// Validates a comma-separated discovery list for Server or App.
pub fn parse_stun_urls(value: &str, name: &str) -> Result<Vec<String>> {
    let values = parse_url_list(value, name)?;
    if values.len() > MAX_ICE_SERVER_URLS {
        return Err(ConfigError::new(format!(
            "{name} must contain at most {MAX_ICE_SERVER_URLS} URLs"
        )));
    }
    if !values.iter().all(|value| valid_stun_url_value(value)) {
        return Err(ConfigError::new(format!(
            "{name} contains an invalid STUN URL"
        )));
    }
    Ok(values)
}

fn parse_origins(value: &str, fallback: &str) -> Result<HashSet<String>> {
    let mut origins = parse_url_list(value, "ALLOWED_ORIGINS")?;
    if origins.is_empty() {
        origins.push(fallback.to_owned());
    }
    origins.iter().map(|entry| to_origin(entry)).collect()
}

/// Validates one allowed HTTP(S) origin. A path is ignored.
fn to_origin(value: &str) -> Result<String> {
    match parse_url(value, &["http", "https"]) {
        Ok(parsed) => Ok(origin(&parsed)),
        Err(OriginError::Syntax) => Err(ConfigError::new(
            "Allowed origins must be valid http or https URLs",
        )),
        Err(OriginError::Scheme) => Err(ConfigError::new("Allowed origins must use http or https")),
        Err(OriginError::Host) => Err(ConfigError::new(
            "Allowed origins must have a valid host and port",
        )),
    }
}

/// Validates an HTTP(S) origin and returns its WHATWG-normalized URL
/// (lowercase host, default port removed, path `/`).
pub(crate) fn parse_origin_url(value: &str, name: &str) -> Result<Url> {
    let parsed = parse_url(value, &["http", "https"]).map_err(|error| {
        ConfigError::new(match error {
            OriginError::Syntax => format!("{name} must be a valid http or https origin"),
            OriginError::Scheme => format!("{name} must use http or https"),
            OriginError::Host => format!("{name} must have a valid host and port"),
        })
    })?;
    if !is_bare_origin(&parsed) {
        return Err(ConfigError::new(format!(
            "{name} must be an origin without credentials, path, query, or fragment"
        )));
    }
    Ok(origin_url(&parsed))
}

/// Selects the direct TLS source. Loading and verifying the certificate
/// against `server_name` and its validity window belongs to the TLS owner.
fn parse_direct_tls(env: &HashMap<String, String>, origin: &Url) -> Result<Option<DirectTls>> {
    let get = |name: &str| value_of(env, name);
    let self_signed = parse_boolean(get("TLS_SELF_SIGNED"), "TLS_SELF_SIGNED")?;
    let certificate = get("TLS_CERT_FILE").trim();
    let key = get("TLS_KEY_FILE").trim();
    if certificate.is_empty() && key.is_empty() && !self_signed {
        return Ok(None);
    }
    if origin.scheme() != "https" {
        return Err(ConfigError::new(
            "direct TLS requires PUBLIC_BASE_URL to use https",
        ));
    }
    let source = if self_signed {
        if !certificate.is_empty() || !key.is_empty() {
            return Err(ConfigError::new(
                "choose TLS_SELF_SIGNED or TLS_CERT_FILE with TLS_KEY_FILE",
            ));
        }
        TlsSource::SelfSigned
    } else {
        if certificate.is_empty() || key.is_empty() {
            return Err(ConfigError::new(
                "TLS_CERT_FILE and TLS_KEY_FILE must be configured together",
            ));
        }
        TlsSource::PemFiles {
            certificate: PathBuf::from(certificate),
            key: PathBuf::from(key),
        }
    };
    let server_name = match origin.host() {
        Some(Host::Ipv6(address)) => address.to_string(),
        _ => origin.host_str().unwrap_or_default().to_owned(),
    };
    Ok(Some(DirectTls {
        source,
        server_name,
    }))
}

pub(crate) fn socket_address(host: Ipv4Addr, port: u16) -> SocketAddr {
    SocketAddr::from((host, port))
}

/// An absent variable reads as empty, which most settings treat as unset.
fn value_of<'a>(env: &'a HashMap<String, String>, name: &str) -> &'a str {
    env.get(name).map_or("", String::as_str)
}
