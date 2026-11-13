//! LAN invitation addresses. This describes where a Viewer can reach the Host;
//! it does not choose or constrain the media interfaces, which remain owned by
//! ICE and the operating system.

use std::collections::BTreeMap;
use std::net::IpAddr;

use serde::{Deserialize, Serialize};

const NO_ADDRESS: &str = "no active LAN IPv4 address is available";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Address {
    pub address: String,
    /// The interface's user-visible (friendly) name.
    pub name: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("list network interfaces: {0}")]
    Interfaces(#[source] std::io::Error),
    #[error("{NO_ADDRESS}")]
    NoAddress,
    #[error("selected LAN address is not active")]
    Inactive,
    #[error(
        "multiple LAN addresses are active; restart with --local --lan-address <address>, choosing from: {0}; public-link mode does not require this selection"
    )]
    Ambiguous(String),
}

/// One interface address as reported by the operating system.
struct Candidate {
    name: String,
    up: bool,
    ip: IpAddr,
}

/// Lists active, non-loopback IPv4 addresses, one entry per address, sorted
/// by address text.
pub fn list() -> Result<Vec<Address>, Error> {
    let interfaces = if_addrs::get_if_addrs().map_err(Error::Interfaces)?;
    collect(interfaces.into_iter().map(|interface| Candidate {
        up: interface.is_oper_up() && !interface.is_loopback(),
        ip: interface.ip(),
        name: interface.name,
    }))
}

fn collect(candidates: impl IntoIterator<Item = Candidate>) -> Result<Vec<Address>, Error> {
    // Several interfaces can report one address; keep the smallest name.
    let mut unique: BTreeMap<String, String> = BTreeMap::new();
    for candidate in candidates {
        let IpAddr::V4(ip) = candidate.ip else {
            continue;
        };
        if !candidate.up || ip.is_loopback() || ip.is_unspecified() {
            continue;
        }
        let value = ip.to_string();
        match unique.get_mut(&value) {
            Some(previous) if candidate.name < *previous => *previous = candidate.name,
            Some(_) => {}
            None => {
                unique.insert(value, candidate.name);
            }
        }
    }
    if unique.is_empty() {
        return Err(Error::NoAddress);
    }
    Ok(unique
        .into_iter()
        .map(|(address, name)| Address { address, name })
        .collect())
}

/// Active invitation address values, sorted by address text.
pub fn addresses() -> Result<Vec<String>, Error> {
    Ok(list()?.into_iter().map(|address| address.address).collect())
}

/// Chooses the invitation address: an explicit active `preferred` address,
/// the only address, or the only private address among several.
pub fn select(addresses: &[String], preferred: &str) -> Result<String, Error> {
    let preferred = preferred.trim();
    if !preferred.is_empty() {
        return addresses
            .iter()
            .find(|address| *address == preferred)
            .cloned()
            .ok_or(Error::Inactive);
    }
    match addresses {
        [] => return Err(Error::NoAddress),
        [only] => return Ok(only.clone()),
        _ => {}
    }
    let mut private = addresses.iter().filter(|address| is_private(address));
    if let (Some(only), None) = (private.next(), private.next()) {
        return Ok(only.clone());
    }
    Err(Error::Ambiguous(addresses.join(", ")))
}

/// RFC 1918 IPv4 and RFC 4193 IPv6 ranges.
fn is_private(address: &str) -> bool {
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => ip.is_private(),
        Ok(IpAddr::V6(ip)) => ip.segments()[0] & 0xfe00 == 0xfc00,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }

    #[test]
    fn select_uses_one_address_or_an_explicit_active_address() {
        assert_eq!(
            select(&strings(&["192.168.1.4"]), "").unwrap(),
            "192.168.1.4"
        );
        assert_eq!(
            select(&strings(&["10.0.0.2", "192.168.1.4"]), "192.168.1.4").unwrap(),
            "192.168.1.4"
        );
        assert_eq!(
            select(&strings(&["192.168.1.4", "198.18.0.1"]), "").unwrap(),
            "192.168.1.4"
        );
        let ambiguous = select(&strings(&["10.0.0.2", "192.168.1.4"]), "").unwrap_err();
        assert!(
            ambiguous
                .to_string()
                .contains("--local --lan-address <address>"),
            "ambiguous selection must explain the actual CLI option: {ambiguous}"
        );
        assert!(
            select(&strings(&["10.0.0.2"]), "192.168.1.4").is_err(),
            "inactive selection was accepted"
        );
    }

    #[test]
    fn select_reports_no_available_address() {
        let error = select(&[], "").unwrap_err();
        assert_eq!(error.to_string(), "no active LAN IPv4 address is available");
    }

    #[test]
    fn list_keeps_active_ipv4_once_with_the_smallest_name() {
        let candidate = |name: &str, up: bool, ip: &str| Candidate {
            name: name.to_owned(),
            up,
            ip: ip.parse().unwrap(),
        };
        let listed = collect([
            candidate("Wi-Fi", true, "192.168.1.4"),
            candidate("Ethernet", true, "192.168.1.4"),
            candidate("Ethernet", true, "10.0.0.2"),
            candidate("Down", false, "172.16.0.3"),
            candidate("Loopback", true, "127.0.0.1"),
            candidate("Unspecified", true, "0.0.0.0"),
            candidate("Ethernet", true, "fe80::1"),
        ])
        .unwrap();
        assert_eq!(
            listed,
            vec![
                Address {
                    address: "10.0.0.2".to_owned(),
                    name: "Ethernet".to_owned()
                },
                Address {
                    address: "192.168.1.4".to_owned(),
                    name: "Ethernet".to_owned()
                },
            ]
        );
        assert!(matches!(
            collect([candidate("Down", false, "10.0.0.2")]),
            Err(Error::NoAddress)
        ));
    }

    #[test]
    fn host_addresses_are_active_ipv4() {
        // The host may legitimately have no LAN address; only check shape.
        if let Ok(values) = addresses() {
            assert!(!values.is_empty());
            for value in values {
                let ip: std::net::Ipv4Addr = value.parse().unwrap();
                assert!(!ip.is_loopback() && !ip.is_unspecified());
            }
        }
    }
}
