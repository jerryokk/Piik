//! Hosted and App Local server configuration and ICE discovery.
//!
//! Callers provide configuration inputs; loading opens no runtime resources.
//! Direct TLS is described as validated configuration data ([`DirectTls`]);
//! reading and verifying the certificate belongs to the TLS owner at startup.

#![forbid(unsafe_code)]

mod hosted;
mod ice;
mod local;
mod number;
mod origin;

use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use piik_protocol::{DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY, Int, MAX_VIEWERS_PER_ROOM_LIMIT};
use thiserror::Error;
use url::Url;

pub use hosted::{load, parse_stun_urls};
pub use ice::{ice_config, nat_prediction_stun_urls};
pub use local::{LocalOptions, local};
pub use origin::{OriginError, is_bare_origin, origin, origin_from_host, parse_url};

/// A configuration failure. The text is the operator-facing message.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0}")]
pub struct ConfigError(String);

impl ConfigError {
    pub(crate) fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

pub(crate) type Result<T> = std::result::Result<T, ConfigError>;

/// Selects development-only behavior. A stale unit file must fail closed
/// rather than fall back to development.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Environment {
    #[default]
    Development,
    Production,
}

/// Enables the embedded UDP media listener.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SfuConfig {
    pub listen_host: Ipv4Addr,
    pub port: u16,
    /// An IPv4 address or normalized DNS hostname; empty when not advertised.
    pub public_ip: String,
    pub only: bool,
}

/// Where direct TLS takes its certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TlsSource {
    /// `TLS_SELF_SIGNED=true`.
    SelfSigned,
    /// `TLS_CERT_FILE` with `TLS_KEY_FILE`, both PEM.
    PemFiles { certificate: PathBuf, key: PathBuf },
}

/// Optional direct HTTPS. Reverse-proxy deployments leave it unset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectTls {
    pub source: TlsSource,
    /// The `PUBLIC_BASE_URL` host the certificate must cover, without IPv6
    /// brackets.
    pub server_name: String,
}

/// Runtime configuration. The default is an open development site with
/// process-memory rooms, no STUN and no SFU fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub env: Environment,
    pub port: u16,
    pub listen_host: String,
    /// A normalized origin with path `/`.
    pub public_base_url: Url,
    pub tls: Option<DirectTls>,
    /// A membership test only.
    pub allowed_origins: HashSet<String>,
    pub site_access_password: String,
    /// `None` selects the in-memory room store.
    pub room_database_path: Option<PathBuf>,
    pub room_empty_timeout_seconds: u32,
    pub max_viewers_per_room: usize,
    pub endpoint_media_copy_capacity: Int,
    pub sfu: Option<SfuConfig>,
    pub stun_urls: Vec<String>,
    /// Belongs to the application lifecycle. Pure LAN binds its selected
    /// interface; public-link discovery URLs never create listeners.
    pub stun_listen_addresses: Vec<SocketAddr>,
    pub nat_prediction_enabled: bool,
    /// `None` when unset, which [`ice_config`] distinguishes from a
    /// configured empty list.
    pub nat_prediction_stun_urls: Option<Vec<String>>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            env: Environment::Development,
            port: hosted::DEFAULT_PORT,
            listen_host: "0.0.0.0".to_owned(),
            public_base_url: Url::parse("http://localhost:8787/").expect("constant URL parses"),
            tls: None,
            allowed_origins: HashSet::new(),
            site_access_password: String::new(),
            room_database_path: None,
            room_empty_timeout_seconds: 0,
            max_viewers_per_room: MAX_VIEWERS_PER_ROOM_LIMIT,
            endpoint_media_copy_capacity: DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
            sfu: None,
            stun_urls: Vec::new(),
            stun_listen_addresses: Vec::new(),
            nat_prediction_enabled: false,
            nat_prediction_stun_urls: None,
        }
    }
}
