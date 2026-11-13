use std::net::{SocketAddr, UdpSocket};
use std::time::Duration;

use bytecodec::{DecodeExt, EncodeExt};
use stun_codec::rfc5389::attributes::{Fingerprint, XorMappedAddress};
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::rfc5766::methods::ALLOCATE;
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder, Method, TransactionId};

use super::listen;

#[tokio::test]
async fn three_stun_listeners_without_relay_and_with_complete_shutdown() {
    let server = listen(&["127.0.0.1:0", "127.0.0.1:0", "127.0.0.1:0"])
        .await
        .unwrap();
    let addresses = server.addresses().to_vec();
    let client = UdpSocket::bind("127.0.0.1:0").unwrap();
    for &address in &addresses {
        let response = exchange(&client, address, MessageClass::Request, BINDING)
            .await
            .unwrap();
        assert_eq!(response.class(), MessageClass::SuccessResponse);
        let mapped = response
            .get_attribute::<XorMappedAddress>()
            .expect("binding address");
        assert_eq!(
            mapped.address(),
            client.local_addr().unwrap(),
            "STUN returned a different socket's address"
        );
        assert!(response.get_attribute::<Fingerprint>().is_some());

        let response = exchange(&client, address, MessageClass::Request, ALLOCATE)
            .await
            .unwrap();
        assert_eq!(
            response.class(),
            MessageClass::ErrorResponse,
            "STUN-only listener allowed TURN allocation"
        );

        // Indications are never answered.
        assert!(
            exchange(&client, address, MessageClass::Indication, BINDING)
                .await
                .is_none()
        );
        client.send_to(b"not stun", address).unwrap();
        assert!(receive(&client, address).await.is_none());
    }
    server.close().await;
    for address in addresses {
        UdpSocket::bind(address).expect("STUN socket remained open after shutdown");
    }
}

#[tokio::test]
async fn occupied_listener_rolls_back_only_new_sockets() {
    let first = UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let occupied = UdpSocket::bind("127.0.0.1:0").unwrap();
    let occupied_address = occupied.local_addr().unwrap().to_string();
    let result = listen(&[first.to_string().as_str(), &occupied_address, "127.0.0.1:0"]).await;
    assert!(result.is_err(), "occupied listener was accepted");
    UdpSocket::bind(first).expect("failed startup retained an earlier socket");
    occupied
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("failed startup closed a foreign socket");
    assert_eq!(occupied.local_addr().unwrap().to_string(), occupied_address);
}

#[tokio::test]
async fn listener_count_must_be_one_or_three() {
    assert!(listen::<&str>(&[]).await.is_err());
    assert!(listen(&["127.0.0.1:0", "127.0.0.1:0"]).await.is_err());
    let single = listen(&["127.0.0.1:0"]).await.unwrap();
    assert_eq!(single.addresses().len(), 1);
    single.close().await;
}

async fn exchange(
    client: &UdpSocket,
    destination: SocketAddr,
    class: MessageClass,
    method: Method,
) -> Option<Message<Attribute>> {
    let transaction_id = TransactionId::new(rand_id());
    let request = Message::<Attribute>::new(class, method, transaction_id);
    let bytes = MessageEncoder::new().encode_into_bytes(request).unwrap();
    client.send_to(&bytes, destination).unwrap();
    let response = receive(client, destination).await?;
    assert_eq!(
        response.transaction_id(),
        transaction_id,
        "STUN response identity did not match"
    );
    Some(response)
}

/// Waits briefly for one reply from `destination`; `None` means no reply.
async fn receive(client: &UdpSocket, destination: SocketAddr) -> Option<Message<Attribute>> {
    let client = client.try_clone().unwrap();
    tokio::task::spawn_blocking(move || {
        client
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut buffer = [0; 1500];
        let (count, source) = client.recv_from(&mut buffer).ok()?;
        assert_eq!(source, destination, "STUN response identity did not match");
        let message = MessageDecoder::<Attribute>::new()
            .decode_from_bytes(&buffer[..count])
            .unwrap()
            .expect("valid STUN response");
        Some(message)
    })
    .await
    .unwrap()
}

fn rand_id() -> [u8; 12] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .to_le_bytes();
    let mut id = [0; 12];
    id.copy_from_slice(&nanos[..12]);
    id
}
