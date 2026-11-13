//! UDP socket setup shared by [`crate::SharedUdp`] and [`crate::ConnSocket`].

use std::io;
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;

use socket2::{Domain, Protocol, Socket, Type};
use tokio::net::UdpSocket;

/// Media socket receive buffer size.
pub const RECEIVE_BUFFER_BYTES: usize = 2 * 1024 * 1024;

/// Largest datagram read; larger ones are truncated by the OS and then
/// dropped by str0m's parser.
pub(crate) const MAX_DATAGRAM: usize = 2048;

/// Applies Piik's media socket options to an existing socket:
/// - a 2 MiB `SO_RCVBUF` (the OS may grant less);
/// - on Windows, `SIO_UDP_CONNRESET` off. Otherwise an ICMP port-unreachable
///   for one earlier send fails the next `recv_from` on a socket shared by
///   every peer.
pub fn configure_udp(socket: &Socket) -> io::Result<()> {
    socket.set_recv_buffer_size(RECEIVE_BUFFER_BYTES)?;
    disable_connection_reset(socket)?;
    Ok(())
}

/// Binds an IPv4 UDP media socket with [`configure_udp`] applied.
pub fn bind_udp4(addr: SocketAddrV4) -> io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    configure_udp(&socket)?;
    socket.set_nonblocking(true)?;
    socket.bind(&SocketAddr::V4(addr).into())?;
    UdpSocket::from_std(socket.into())
}

/// Adopts an already-bound socket, such as a punched one, as a media socket.
pub(crate) fn adopt(socket: std::net::UdpSocket) -> io::Result<UdpSocket> {
    let socket = Socket::from(socket);
    configure_udp(&socket)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// Whether a receive error concerns one remote peer rather than the socket.
pub(crate) fn transient(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionRefused
            | io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
    )
}

#[allow(unsafe_code)]
fn disable_connection_reset(socket: &Socket) -> io::Result<()> {
    use std::os::windows::io::AsRawSocket;

    use windows::Win32::Networking::WinSock::{
        SIO_UDP_CONNRESET, SOCKET, WSAGetLastError, WSAIoctl,
    };

    let disabled: u32 = 0;
    let mut returned = 0u32;
    // SAFETY: the socket handle is live for this borrow; the input buffer is
    // a valid u32 for its stated length; no output buffer, overlapped IO or
    // completion routine is passed.
    let result = unsafe {
        WSAIoctl(
            SOCKET(socket.as_raw_socket() as usize),
            SIO_UDP_CONNRESET,
            Some(std::ptr::from_ref(&disabled).cast()),
            size_of::<u32>() as u32,
            None,
            0,
            &mut returned,
            None,
            None,
        )
    };
    if result != 0 {
        // SAFETY: reads the calling thread's last WinSock error.
        let code = unsafe { WSAGetLastError() };
        return Err(io::Error::from_raw_os_error(code.0));
    }
    Ok(())
}

/// Where a connection's transmits go. A shard's tokio wrapper sends each
/// transmit with `try_send_to`; UDP is best effort, so a full send buffer is
/// a lost packet.
#[derive(Clone)]
pub struct Egress(pub(crate) Arc<UdpSocket>);

impl Egress {
    pub fn new(socket: Arc<UdpSocket>) -> Self {
        Self(socket)
    }

    pub fn send(&self, destination: SocketAddr, contents: &[u8]) {
        let _ = self.0.try_send_to(contents, destination);
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.0.local_addr()
    }
}

impl std::fmt::Debug for Egress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Egress")
            .field(&self.0.local_addr().ok())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;
    use std::time::Duration;

    use super::*;

    #[tokio::test]
    async fn receive_buffer_is_enlarged() {
        let socket = bind_udp4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let socket = socket2::SockRef::from(&socket);
        assert!(socket.recv_buffer_size().unwrap() >= RECEIVE_BUFFER_BYTES / 2);
    }

    /// A send to a closed port must not break the next receive from a live
    /// peer (Windows reports the ICMP error on the following `recv_from`).
    #[tokio::test]
    async fn connection_reset_is_tolerated() {
        let socket = bind_udp4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let closed = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let closed_addr = closed.local_addr().unwrap();
        drop(closed);
        let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        for _ in 0..3 {
            socket.send_to(b"gone", closed_addr).await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        peer.send_to(b"live", socket.local_addr().unwrap())
            .await
            .unwrap();
        let mut buffer = [0u8; 16];
        let (count, source) =
            tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut buffer))
                .await
                .unwrap()
                .expect("receive is not poisoned by the earlier ICMP error");
        assert_eq!(&buffer[..count], b"live");
        assert_eq!(source, peer.local_addr().unwrap());
    }
}
