//! ICE candidate JSON (Pion `ICECandidateInit`), remote candidate admission
//! and host candidate gathering.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use serde::{Deserialize, Serialize};
use str0m::Candidate;

/// `RTCIceCandidateInit` exactly as Pion encodes it: every key present, `null`
/// for an absent value. An empty `candidate` is end-of-candidates.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IceCandidateInit {
    #[serde(default)]
    pub candidate: String,
    #[serde(default, rename = "sdpMid")]
    pub sdp_mid: Option<String>,
    #[serde(default, rename = "sdpMLineIndex")]
    pub sdp_m_line_index: Option<u16>,
    #[serde(default, rename = "usernameFragment")]
    pub username_fragment: Option<String>,
}

impl IceCandidateInit {
    /// A local candidate in the shape Piik's gathering emits: bundled on the
    /// first m-line and scoped to the current local ufrag.
    pub fn local(candidate: &Candidate, mid: &str, ufrag: &str) -> Self {
        Self {
            candidate: candidate.to_sdp_string(),
            sdp_mid: Some(mid.to_owned()),
            sdp_m_line_index: Some(0),
            username_fragment: Some(ufrag.to_owned()),
        }
    }

    pub fn is_end_of_candidates(&self) -> bool {
        self.candidate.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CandidateError {
    /// Lifecycle: no remote description yet (Pion `InvalidStateError`).
    #[error("remote description is not set")]
    NoRemoteDescription,
    /// Lifecycle: the connection is closed.
    #[error("connection is closed")]
    Closed,
    /// Disposable content error (Pion `OperationError`); later candidates
    /// remain usable.
    #[error("ICE candidate is invalid: {0}")]
    Invalid(String),
}

/// Remote candidate content rules after a remote description exists.
///
/// - Empty: end-of-candidates, `Ok(None)`.
/// - Unknown type or transport, and hostname (mDNS) addresses: discarded with
///   `Ok(None)`, as Pion discards them. Piik learns those peers as
///   peer-reflexive instead.
/// - Any other malformed line, or a `usernameFragment` that is not one SDP
///   token: [`CandidateError::Invalid`].
/// - `usernameFragment` becomes the candidate's `ufrag` extension, so str0m
///   drops candidates of another ICE generation.
/// - TCP candidates parse but are discarded: Piik media is UDP only.
pub(crate) fn parse_remote(init: &IceCandidateInit) -> Result<Option<Candidate>, CandidateError> {
    if init.candidate.is_empty() {
        return Ok(None);
    }
    let value = init
        .candidate
        .strip_prefix("candidate:")
        .unwrap_or(&init.candidate);
    let fields: Vec<&str> = value.split_whitespace().collect();
    if fields.len() < 8 || fields[6] != "typ" {
        return Err(CandidateError::Invalid("malformed candidate".into()));
    }
    if !matches!(fields[7], "host" | "srflx" | "prflx" | "relay") {
        return Ok(None);
    }
    let udp = fields[2].eq_ignore_ascii_case("udp");
    if !udp && !fields[2].eq_ignore_ascii_case("tcp") {
        return Ok(None);
    }
    if fields[4].parse::<IpAddr>().is_err() {
        return Ok(None);
    }
    if let Some(fragment) = &init.username_fragment {
        let mut tokens = fragment.split_whitespace();
        if tokens.next() != Some(fragment.as_str()) || tokens.next().is_some() {
            return Err(CandidateError::Invalid(
                "ICE username fragment is not an SDP token".into(),
            ));
        }
    }
    let mut candidate = Candidate::from_sdp_string(&format!("candidate:{value}"))
        .map_err(|error| CandidateError::Invalid(error.to_string()))?;
    if let Some(fragment) = &init.username_fragment {
        candidate.set_ufrag(fragment);
    }
    Ok(udp.then_some(candidate))
}

/// One snapshot of local IPv4 host addresses. Take a new snapshot for every
/// gathering: interfaces change while the process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostAddrs {
    /// `(address, netmask)` in enumeration order.
    addrs: Vec<(Ipv4Addr, Ipv4Addr)>,
    nat_1to1: Option<Ipv4Addr>,
}

impl HostAddrs {
    /// The addresses a socket bound to `bind` receives on. A concrete bind
    /// address is the only host address; a wildcard bind enumerates every
    /// operational IPv4 interface. Loopback is included only on request,
    /// link-local never (str0m rejects it).
    pub fn gather(bind: Ipv4Addr, include_loopback: bool) -> io::Result<Self> {
        let interfaces = if_addrs::get_if_addrs()?;
        let mut addrs = Vec::new();
        for interface in interfaces {
            let up = interface.is_oper_up();
            let if_addrs::IfAddr::V4(v4) = interface.addr else {
                continue;
            };
            let ip = v4.ip;
            let usable = up
                && !ip.is_unspecified()
                && !ip.is_link_local()
                && (include_loopback || !ip.is_loopback());
            let wanted = if bind.is_unspecified() {
                usable
            } else {
                ip == bind
            };
            if wanted && !addrs.iter().any(|(known, _)| *known == ip) {
                addrs.push((ip, v4.netmask));
            }
        }
        if !bind.is_unspecified() && addrs.is_empty() {
            addrs.push((bind, Ipv4Addr::BROADCAST));
        }
        Ok(Self {
            addrs,
            nat_1to1: None,
        })
    }

    /// Fixed addresses, for tests and callers that know their interfaces.
    pub fn from_addrs(addrs: impl IntoIterator<Item = Ipv4Addr>) -> Self {
        Self {
            addrs: addrs
                .into_iter()
                .map(|ip| (ip, Ipv4Addr::BROADCAST))
                .collect(),
            nat_1to1: None,
        }
    }

    /// Pion `SetNAT1To1IPs([ip], Host)`: every host candidate advertises the
    /// public address instead, which collapses them into one.
    pub fn with_nat_1to1(mut self, public: Option<Ipv4Addr>) -> Self {
        self.nat_1to1 = public;
        self
    }

    pub fn is_empty(&self) -> bool {
        self.addrs.is_empty() && self.nat_1to1.is_none()
    }

    /// Host candidates for a socket on `port`.
    pub fn candidates(&self, port: u16) -> Vec<Candidate> {
        let ips: Vec<Ipv4Addr> = match self.nat_1to1 {
            Some(public) => vec![public],
            None => self.addrs.iter().map(|(ip, _)| *ip).collect(),
        };
        ips.into_iter()
            .filter_map(|ip| Candidate::host(SocketAddr::from((ip, port)), "udp").ok())
            .collect()
    }

    /// The host candidate a datagram from `source` most likely arrived on.
    /// A wildcard socket does not report its destination address, but str0m
    /// only answers binding requests that name one of its host candidates.
    /// Prefer the interface on the source's subnet, else the first one.
    pub fn destination(&self, source: SocketAddr, port: u16) -> Option<SocketAddr> {
        if let Some(public) = self.nat_1to1 {
            return Some(SocketAddr::from((public, port)));
        }
        let IpAddr::V4(source) = source.ip() else {
            return self.addrs.first().map(|(ip, _)| (*ip, port).into());
        };
        let same_subnet = |(ip, mask): &&(Ipv4Addr, Ipv4Addr)| {
            u32::from(*ip) & u32::from(*mask) == u32::from(source) & u32::from(*mask)
        };
        self.addrs
            .iter()
            .find(same_subnet)
            .or_else(|| self.addrs.first())
            .map(|(ip, _)| (*ip, port).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = "candidate:probe 1 udp 2122260223 127.0.0.1 49152 typ host";

    fn init(candidate: &str, fragment: Option<&str>) -> IceCandidateInit {
        IceCandidateInit {
            candidate: candidate.into(),
            username_fragment: fragment.map(str::to_owned),
            ..Default::default()
        }
    }

    #[test]
    fn json_matches_pion_shape() {
        let json = r#"{"candidate":"candidate:1 1 udp 1 192.0.2.1 9 typ host","sdpMid":"0","sdpMLineIndex":0,"usernameFragment":"abcd"}"#;
        let parsed: IceCandidateInit = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.sdp_mid.as_deref(), Some("0"));
        assert_eq!(parsed.sdp_m_line_index, Some(0));
        assert_eq!(parsed.username_fragment.as_deref(), Some("abcd"));
        assert_eq!(serde_json::to_string(&parsed).unwrap(), json);

        // Pion writes null for every absent pointer.
        let end = IceCandidateInit::default();
        assert!(end.is_end_of_candidates());
        assert_eq!(
            serde_json::to_string(&end).unwrap(),
            r#"{"candidate":"","sdpMid":null,"sdpMLineIndex":null,"usernameFragment":null}"#
        );
        // Browsers may omit keys.
        let sparse: IceCandidateInit = serde_json::from_str(r#"{"candidate":""}"#).unwrap();
        assert_eq!(sparse, end);
    }

    #[test]
    fn local_candidate_json() {
        let candidate = Candidate::host("192.0.2.1:5000".parse().unwrap(), "udp").unwrap();
        let json = IceCandidateInit::local(&candidate, "0", "frag");
        assert!(json.candidate.starts_with("candidate:"));
        assert!(json.candidate.ends_with("192.0.2.1 5000 typ host"));
        assert_eq!(json.sdp_mid.as_deref(), Some("0"));
        assert_eq!(json.sdp_m_line_index, Some(0));
        assert_eq!(json.username_fragment.as_deref(), Some("frag"));
        // It parses back as a remote candidate of the same generation.
        let remote = parse_remote(&json).unwrap().unwrap();
        assert_eq!(remote.addr(), candidate.addr());
        assert_eq!(remote.ufrag(), Some("frag"));
    }

    #[test]
    fn content_errors_follow_go_add_ice_candidate() {
        for (candidate, fragment) in [
            ("malformed", None),
            ("malformed", Some("current")),
            (VALID, Some("not a token")),
            (VALID, Some("")),
            (VALID, Some(" lead")),
            ("candidate:1 1 udp nope 127.0.0.1 9 typ host", None),
            ("candidate:1 1 udp 1 127.0.0.1 99999 typ host", None),
            ("candidate:1 1 udp 1 127.0.0.1 9 type host", None),
        ] {
            assert!(
                matches!(
                    parse_remote(&init(candidate, fragment)),
                    Err(CandidateError::Invalid(_))
                ),
                "{candidate:?} {fragment:?}"
            );
        }
    }

    #[test]
    fn discarded_candidates_are_not_errors() {
        for candidate in [
            "",
            "candidate:1 1 udp 1 127.0.0.1 9 typ bogus",
            "candidate:1 1 sctp 1 127.0.0.1 9 typ host",
            "candidate:1 1 udp 1 4f1d2a3b-0c9e.local 9 typ host",
            "candidate:1 1 tcp 1 127.0.0.1 9 typ host tcptype passive",
        ] {
            assert_eq!(
                parse_remote(&init(candidate, None)),
                Ok(None),
                "{candidate}"
            );
        }
    }

    #[test]
    fn valid_candidates_carry_their_generation() {
        let parsed = parse_remote(&init(VALID, None)).unwrap().unwrap();
        assert_eq!(parsed.addr(), "127.0.0.1:49152".parse().unwrap());
        assert_eq!(parsed.ufrag(), None);
        // The prefix is optional, and usernameFragment overrides the line.
        let line = "probe 1 udp 2122260223 127.0.0.1 49152 typ host ufrag old";
        let parsed = parse_remote(&init(line, Some("new"))).unwrap().unwrap();
        assert_eq!(parsed.ufrag(), Some("new"));
        let srflx =
            "candidate:ns1 1 udp 1694498815 203.0.113.7 40000 typ srflx raddr 0.0.0.0 rport 5000";
        assert!(parse_remote(&init(srflx, Some("frag"))).unwrap().is_some());
    }

    #[test]
    fn host_addrs_pick_destination_and_nat() {
        let hosts = HostAddrs {
            addrs: vec![
                (
                    "192.168.1.10".parse().unwrap(),
                    "255.255.255.0".parse().unwrap(),
                ),
                ("10.0.0.5".parse().unwrap(), "255.0.0.0".parse().unwrap()),
            ],
            nat_1to1: None,
        };
        let port = 7000;
        assert_eq!(hosts.candidates(port).len(), 2);
        assert_eq!(
            hosts.destination("10.9.8.7:1".parse().unwrap(), port),
            Some("10.0.0.5:7000".parse().unwrap())
        );
        assert_eq!(
            hosts.destination("203.0.113.1:1".parse().unwrap(), port),
            Some("192.168.1.10:7000".parse().unwrap())
        );
        let public: Ipv4Addr = "198.51.100.2".parse().unwrap();
        let nat = hosts.with_nat_1to1(Some(public));
        let candidates = nat.candidates(port);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].addr(), "198.51.100.2:7000".parse().unwrap());
        assert_eq!(
            nat.destination("10.9.8.7:1".parse().unwrap(), port),
            Some("198.51.100.2:7000".parse().unwrap())
        );
    }

    #[test]
    fn gathering_excludes_loopback_unless_requested() {
        let any = HostAddrs::gather(Ipv4Addr::UNSPECIFIED, false).unwrap();
        assert!(any.addrs.iter().all(|(ip, _)| !ip.is_loopback()));
        let with = HostAddrs::gather(Ipv4Addr::UNSPECIFIED, true).unwrap();
        assert!(with.addrs.iter().any(|(ip, _)| ip.is_loopback()));
        let bound = HostAddrs::gather(Ipv4Addr::LOCALHOST, true).unwrap();
        assert_eq!(bound.candidates(1).len(), 1);
        assert_eq!(
            bound.candidates(1)[0].addr(),
            "127.0.0.1:1".parse().unwrap()
        );
    }
}
