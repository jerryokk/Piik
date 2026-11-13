//! UDP listeners for in-process STUN discovery.
//!
//! The server is STUN-only: Binding requests receive a success response with
//! XOR-MAPPED-ADDRESS and FINGERPRINT, every other request method (including
//! TURN Allocate) receives a 400 error response, and indications, responses
//! and non-STUN datagrams are dropped. Nothing is logged because packets carry
//! remote addresses.

use std::io;
use std::net::SocketAddr;

use bytecodec::{DecodeExt, EncodeExt};
use stun_codec::rfc5389::attributes::{ErrorCode, Fingerprint, XorMappedAddress};
use stun_codec::rfc5389::errors::BadRequest;
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

/// Matches Pion's inbound MTU; larger datagrams are truncated and then dropped.
const RECEIVE_BUFFER_BYTES: usize = 1600;

/// Running STUN listeners. [`Server::close`] releases every socket; dropping the
/// server also stops the listeners, but releases the sockets asynchronously.
#[derive(Debug)]
pub struct Server {
    addresses: Vec<SocketAddr>,
    tasks: Vec<JoinHandle<()>>,
}

/// Binds the ordinary endpoint or the three-destination NAT survey on IPv4 UDP.
/// On failure every socket bound by this call is released; the caller owns the
/// returned server's lifecycle.
pub async fn listen<A: AsRef<str>>(addresses: &[A]) -> io::Result<Server> {
    if addresses.len() != 1 && addresses.len() != 3 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "STUN requires one or three UDP listeners",
        ));
    }
    // Sockets bound so far are dropped (closed) if a later bind fails.
    let mut sockets = Vec::with_capacity(addresses.len());
    for address in addresses {
        let address = tokio::net::lookup_host(address.as_ref())
            .await?
            .find(SocketAddr::is_ipv4)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "STUN listener must be IPv4")
            })?;
        sockets.push(UdpSocket::bind(address).await?);
    }
    let addresses = sockets
        .iter()
        .map(UdpSocket::local_addr)
        .collect::<io::Result<Vec<_>>>()?;
    let tasks = sockets
        .into_iter()
        .map(|socket| tokio::spawn(serve(socket)))
        .collect();
    Ok(Server { addresses, tasks })
}

impl Server {
    /// Bound local addresses in the order they were requested.
    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    /// Stops every listener and returns once all sockets are closed.
    pub async fn close(mut self) {
        for task in std::mem::take(&mut self.tasks) {
            task.abort();
            // A cancelled task has dropped its socket when the join completes.
            let _ = task.await;
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

async fn serve(socket: UdpSocket) {
    let mut buffer = [0; RECEIVE_BUFFER_BYTES];
    loop {
        let (count, source) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            // Windows reports an earlier send's ICMP port-unreachable on the
            // next receive; it concerns one remote peer, not this listener.
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionRefused
                ) =>
            {
                continue;
            }
            Err(_) => return,
        };
        if let Some(response) = respond(&buffer[..count], source) {
            // UDP is best effort; a failed send is the client's retransmission.
            let _ = socket.send_to(&response, source).await;
        }
    }
}

/// Builds the response for one datagram, or `None` when it must be ignored.
fn respond(datagram: &[u8], source: SocketAddr) -> Option<Vec<u8>> {
    let (class, method, transaction_id) = match MessageDecoder::<Attribute>::new()
        .decode_from_bytes(datagram)
        .ok()?
    {
        Ok(message) => (message.class(), message.method(), message.transaction_id()),
        // A well-framed request with an unreadable attribute is still answerable.
        Err(broken) => (broken.class(), broken.method(), broken.transaction_id()),
    };
    if class != MessageClass::Request {
        return None;
    }
    let mut response = if method == BINDING {
        let mut response =
            Message::<Attribute>::new(MessageClass::SuccessResponse, method, transaction_id);
        response.add_attribute(XorMappedAddress::new(source));
        response
    } else {
        let mut response = Message::new(MessageClass::ErrorResponse, method, transaction_id);
        response.add_attribute(ErrorCode::from(BadRequest));
        response
    };
    let fingerprint = Fingerprint::new(&response).ok()?;
    response.add_attribute(fingerprint);
    MessageEncoder::new().encode_into_bytes(response).ok()
}

#[cfg(test)]
mod tests;
