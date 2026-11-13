//! Default-route lookup: the next hop and the local source address the
//! operating system would use toward the unspecified destination.

use std::net::{Ipv4Addr, Ipv6Addr};

use crate::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Route4 {
    pub gateway: Ipv4Addr,
    pub local: Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Route6 {
    pub gateway: Ipv6Addr,
    pub local: Ipv6Addr,
    /// Interface index for a link-local gateway; zero otherwise.
    pub scope_id: u32,
}

mod platform {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute2, MIB_IPFORWARD_ROW2};
    use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6, SOCKADDR_INET};

    use super::{Route4, Route6};
    use crate::Error;

    fn best_route(
        family: windows::Win32::Networking::WinSock::ADDRESS_FAMILY,
    ) -> Result<(MIB_IPFORWARD_ROW2, SOCKADDR_INET), Error> {
        let mut destination = SOCKADDR_INET::default();
        destination.si_family = family;
        let mut route = MIB_IPFORWARD_ROW2::default();
        let mut source = SOCKADDR_INET::default();
        // SAFETY: every pointer refers to a live, initialized local value of the
        // type GetBestRoute2 documents; the call does not retain them.
        let status =
            unsafe { GetBestRoute2(None, 0, None, &destination, 0, &mut route, &mut source) };
        if status != ERROR_SUCCESS {
            return Err(Error::Io(std::io::Error::from_raw_os_error(
                status.0 as i32,
            )));
        }
        Ok((route, source))
    }

    pub(super) fn default_route4() -> Result<Route4, Error> {
        let (route, source) = best_route(AF_INET)?;
        // SAFETY: an AF_INET lookup fills the Ipv4 member of both unions.
        let (family, gateway, local) = unsafe {
            (
                route.NextHop.si_family,
                route.NextHop.Ipv4.sin_addr.S_un.S_addr,
                source.Ipv4.sin_addr.S_un.S_addr,
            )
        };
        if family != AF_INET {
            return Err(Error::NoNatFound);
        }
        // S_addr holds the address in network byte order.
        let gateway = Ipv4Addr::from(u32::from_be(gateway));
        let local = Ipv4Addr::from(u32::from_be(local));
        if gateway.is_unspecified() {
            return Err(Error::NoNatFound);
        }
        Ok(Route4 { gateway, local })
    }

    pub(super) fn default_route6() -> Result<Route6, Error> {
        let (route, source) = best_route(AF_INET6)?;
        // SAFETY: an AF_INET6 lookup fills the Ipv6 member of both unions.
        let (family, gateway, local) = unsafe {
            (
                route.NextHop.si_family,
                route.NextHop.Ipv6.sin6_addr.u.Byte,
                source.Ipv6.sin6_addr.u.Byte,
            )
        };
        if family != AF_INET6 {
            return Err(Error::NoNatFound);
        }
        let gateway = Ipv6Addr::from(gateway);
        if gateway.is_unspecified() {
            return Err(Error::NoNatFound);
        }
        let scope_id = if gateway.is_unicast_link_local() {
            route.InterfaceIndex
        } else {
            0
        };
        Ok(Route6 {
            gateway,
            local: Ipv6Addr::from(local),
            scope_id,
        })
    }
}

/// The default IPv4 next hop and preferred local source address.
pub(crate) fn default_route4() -> Result<Route4, Error> {
    let route = platform::default_route4()?;
    if route.local.is_unspecified() {
        return Err(Error::gateway(std::io::Error::other("no internal address")));
    }
    Ok(route)
}

/// The default IPv6 next hop, preferred local source address and gateway zone.
pub(crate) fn default_route6() -> Result<Route6, Error> {
    let route = platform::default_route6()?;
    if route.local.is_unspecified() {
        return Err(Error::gateway(std::io::Error::other("no internal address")));
    }
    Ok(route)
}
