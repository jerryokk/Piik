//! The stateless site-access cookie (`docs/standards/rooms-access.md`).
//!
//! A configured site password is exchanged for an HMAC-signed
//! `v1.<expiresAt>.<signature>` cookie. The signing key is random per process,
//! so a cookie never permits offline password guessing and dies with the
//! process that issued it.

use std::collections::HashSet;
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use piik_protocol::MAX_SAFE_INTEGER;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::Clock;

pub(crate) const DEFAULT_SESSION_TTL_SECONDS: i64 = 24 * 60 * 60;
const COOKIE_VERSION: &str = "v1";
const SECURE_COOKIE_NAME: &str = "__Host-piik-site-access";
const PLAIN_COOKIE_NAME: &str = "piik-site-access";
/// Base64url of a SHA-256 MAC without padding.
const SIGNATURE_LENGTH: usize = 43;

/// Cookie lifetime, signing and transport policy.
pub(crate) struct SiteAccessOptions {
    /// Empty means site access is not required.
    pub password: String,
    /// Selects the `__Host-` cookie name and appends `Secure`.
    pub secure: bool,
    pub now: Clock,
    /// Zero selects the default 24-hour lifetime.
    pub ttl_seconds: i64,
}

/// Issues and verifies the stateless site-access cookie. Clones share the
/// process key; request adapters only change `secure`.
#[derive(Clone)]
pub(crate) struct SiteAccessGate {
    password: String,
    signing_key: [u8; 32],
    secure: bool,
    now: Clock,
    ttl_seconds: i64,
}

impl std::fmt::Debug for SiteAccessGate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SiteAccessGate")
            .field("required", &self.required())
            .field("secure", &self.secure)
            .field("ttl_seconds", &self.ttl_seconds)
            .finish_non_exhaustive()
    }
}

impl SiteAccessGate {
    pub(crate) fn new(options: SiteAccessOptions) -> Result<Self, String> {
        let ttl_seconds = if options.ttl_seconds == 0 {
            DEFAULT_SESSION_TTL_SECONDS
        } else {
            options.ttl_seconds
        };
        if !(0..=MAX_SAFE_INTEGER).contains(&ttl_seconds) {
            return Err("Access session TTL must be a positive integer".to_owned());
        }
        // A user-chosen password is not a signing key: site authentication is
        // scoped to this process.
        let mut signing_key = [0; 32];
        rand::TryRng::try_fill_bytes(&mut rand::rngs::SysRng, &mut signing_key)
            .map_err(|error| format!("site access key could not be created: {error}"))?;
        Ok(Self {
            password: options.password,
            signing_key,
            secure: options.secure,
            now: options.now,
            ttl_seconds,
        })
    }

    /// The same authority with another cookie transport.
    pub(crate) fn with_secure(&self, secure: bool) -> Self {
        Self {
            secure,
            ..self.clone()
        }
    }

    pub(crate) fn secure(&self) -> bool {
        self.secure
    }

    pub(crate) fn required(&self) -> bool {
        !self.password.is_empty()
    }

    /// An unset password permits entry; a configured one requires an exact match.
    pub(crate) fn password_matches(&self, value: &str) -> bool {
        !self.required() || (!value.is_empty() && secret_matches(value, &self.password))
    }

    /// Verifies the cookie signature and expiry when access is required.
    pub(crate) fn is_authenticated(&self, cookie_header: &str) -> bool {
        if !self.required() {
            return true;
        }
        let value = read_cookie(cookie_header, self.cookie_name());
        let segments: Vec<&str> = value.split('.').collect();
        let [version, expires, signature] = segments[..] else {
            return false;
        };
        if version != COOKIE_VERSION
            || expires.is_empty()
            || !expires.bytes().all(|byte| byte.is_ascii_digit())
            || signature.len() != SIGNATURE_LENGTH
            || !signature
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        {
            return false;
        }
        // Expiry is already decimal; parsing still enforces integer bounds.
        let Ok(expires_at) = expires.parse::<i64>() else {
            return false;
        };
        if expires_at > MAX_SAFE_INTEGER || expires_at <= floor_seconds((self.now)()) {
            return false;
        }
        // Sign the received text, not a re-rendered payload.
        secret_matches(signature, &self.sign(&format!("{version}.{expires}")))
    }

    /// A `Set-Cookie` value, or `None` when access is not required.
    pub(crate) fn create_cookie(&self) -> Option<String> {
        if !self.required() {
            return None;
        }
        let expires_at = floor_seconds((self.now)()).saturating_add(self.ttl_seconds);
        let payload = format!("{COOKIE_VERSION}.{expires_at}");
        let signature = self.sign(&payload);
        let mut cookie = format!(
            "{}={payload}.{signature}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict",
            self.cookie_name(),
            self.ttl_seconds
        );
        if self.secure {
            cookie.push_str("; Secure");
        }
        Some(cookie)
    }

    fn cookie_name(&self) -> &'static str {
        if self.secure {
            SECURE_COOKIE_NAME
        } else {
            PLAIN_COOKIE_NAME
        }
    }

    fn sign(&self, payload: &str) -> String {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&self.signing_key)
            .expect("HMAC accepts any key length");
        mac.update(payload.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }
}

/// Selects the cookie contract by the configured destination of a request,
/// never by its initiating Origin or an untrusted forwarded-protocol header.
/// One Local authority serves HTTPS public links and HTTP LAN/loopback origins.
#[derive(Debug)]
pub(crate) struct AccessPolicy {
    gate: SiteAccessGate,
    allowed_origins: HashSet<String>,
    /// `PUBLIC_BASE_URL`'s `host[:port]`.
    public_host: String,
}

impl AccessPolicy {
    pub(crate) fn new(
        gate: SiteAccessGate,
        allowed_origins: HashSet<String>,
        public_host: String,
    ) -> Self {
        Self {
            gate,
            allowed_origins,
            public_host,
        }
    }

    /// The gate for a request naming `host` over plain HTTP or TLS.
    pub(crate) fn for_request(&self, host: &str, tls: bool) -> SiteAccessGate {
        if !self.gate.secure() || tls {
            return self.gate.clone();
        }
        let allows = |scheme: &str| {
            let origin = piik_config::origin_from_host(scheme, host);
            !origin.is_empty() && self.allowed_origins.contains(&origin)
        };
        if !allows("http") || allows("https") || host.eq_ignore_ascii_case(&self.public_host) {
            return self.gate.clone();
        }
        self.gate.with_secure(false)
    }

    pub(crate) fn origin_allowed(&self, origin: &str) -> bool {
        self.allowed_origins.contains(origin)
    }
}

/// `Math.floor(milliseconds / 1000)`.
pub(crate) fn floor_seconds(milliseconds: i64) -> i64 {
    milliseconds.div_euclid(1_000)
}

/// The first cookie named `name`; an empty value counts as absent.
pub(crate) fn read_cookie<'a>(header: &'a str, name: &str) -> &'a str {
    header
        .split(';')
        .find_map(|part| {
            let (key, value) = part.split_once('=')?;
            (key.trim() == name).then(|| value.trim())
        })
        .unwrap_or_default()
}

/// Compares SHA-256 digests so the comparison is constant time regardless of
/// length.
pub(crate) fn secret_matches(actual: &str, expected: &str) -> bool {
    let actual = Sha256::digest(actual.as_bytes());
    let expected = Sha256::digest(expected.as_bytes());
    bool::from(actual.ct_eq(&expected))
}

pub(crate) fn wall_clock() -> Clock {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX))
            .unwrap_or_default()
    })
}
