//! WHATWG origin parsing for configured HTTP(S) origins and request checks.
//!
//! The `url` crate implements the WHATWG URL standard the original TypeScript
//! used, so host casing, IDNA, IPv6 and default-port rules come from it. The
//! extra syntax gate is stricter than WHATWG parsing: it requires a literal
//! `scheme://` authority and no whitespace, control characters or backslashes.

use url::{ParseError, Url};

/// Why a value is not a usable HTTP(S) URL. Callers choose the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OriginError {
    /// Not an absolute `scheme://authority` URL.
    Syntax,
    /// The scheme is not one of the accepted schemes.
    Scheme,
    /// The host or port is invalid.
    Host,
}

/// Parses an absolute URL with one of `schemes` and a valid host. Path,
/// query, fragment and userinfo are left for the caller to judge.
pub fn parse_url(value: &str, schemes: &[&str]) -> Result<Url, OriginError> {
    let Some((scheme, rest)) = value.split_once("://") else {
        return Err(OriginError::Syntax);
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if !valid_scheme(scheme)
        || authority.is_empty()
        || value
            .chars()
            .any(|c| c <= ' ' || c == '\u{7f}' || c == '\\')
    {
        return Err(OriginError::Syntax);
    }
    let scheme = scheme.to_ascii_lowercase();
    if !schemes.contains(&scheme.as_str()) {
        return Err(OriginError::Scheme);
    }
    let parsed = Url::parse(value).map_err(|error| fault_of(error, authority))?;
    if origin(&parsed).is_empty() {
        return Err(OriginError::Host);
    }
    Ok(parsed)
}

/// Reports whether `parsed` carries nothing beyond scheme, host and port.
/// An empty `?`, `#` or `@` is not observable through WHATWG getters.
pub fn is_bare_origin(parsed: &Url) -> bool {
    parsed.username().is_empty()
        && parsed.password().is_none_or(str::is_empty)
        && parsed.path() == "/"
        && parsed.query().is_none_or(str::is_empty)
        && parsed.fragment().is_none_or(str::is_empty)
}

/// The URL whose serialization is `origin + "/"`.
pub(crate) fn origin_url(parsed: &Url) -> Url {
    Url::parse(&format!("{}/", origin(parsed))).expect("a serialized origin parses")
}

/// The WHATWG `origin` serialization for a URL with a host:
/// `https://Example.com:443/` gives `https://example.com`. Hosts outside the
/// accepted origin grammar give the empty string.
pub fn origin(parsed: &Url) -> String {
    let Some(host) = parsed.host_str() else {
        return String::new();
    };
    // WHATWG no longer forbids `*`, but configured and request origins do.
    if host.is_empty() || host.contains('*') {
        return String::new();
    }
    match parsed.port() {
        Some(port) => format!("{}://{host}:{port}", parsed.scheme()),
        None => format!("{}://{host}", parsed.scheme()),
    }
}

/// The origin a request `Host` header names under `scheme`, or the empty
/// string when the header is not a plain `host[:port]`.
pub fn origin_from_host(scheme: &str, host: &str) -> String {
    if host.is_empty()
        || host
            .chars()
            .any(|c| c <= ' ' || c == '\u{7f}' || matches!(c, '/' | '?' | '#' | '@' | '\\'))
    {
        return String::new();
    }
    Url::parse(&format!("{scheme}://{host}"))
        .map(|parsed| origin(&parsed))
        .unwrap_or_default()
}

fn valid_scheme(scheme: &str) -> bool {
    let mut characters = scheme.chars();
    characters.next().is_some_and(|c| c.is_ascii_alphabetic())
        && characters.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// A malformed but numeric port is a host fault, as is any host rejection;
/// a non-numeric port is a syntax fault.
fn fault_of(error: ParseError, authority: &str) -> OriginError {
    match error {
        ParseError::InvalidPort => {
            let port = authority.rsplit_once(':').map_or("", |(_, port)| port);
            if !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
                OriginError::Host
            } else {
                OriginError::Syntax
            }
        }
        ParseError::EmptyHost
        | ParseError::IdnaError
        | ParseError::InvalidIpv4Address
        | ParseError::InvalidIpv6Address
        | ParseError::InvalidDomainCharacter => OriginError::Host,
        _ => OriginError::Syntax,
    }
}
