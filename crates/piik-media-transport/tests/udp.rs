//! Real 127.0.0.1 UDP: SharedUdp routing, ConnSocket isolation and STUN
//! survey demux, and one str0m client connecting through a spawned shard.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytecodec::EncodeExt;
use piik_input_route::InputChannel;
use piik_media_transport::sdp::{VideoCodec, codec_config};
use piik_media_transport::str0m::change::SdpAnswer;
use piik_media_transport::str0m::net::{Protocol, Receive};
use piik_media_transport::str0m::{Candidate, Event, Input, Output, Rtc};
use piik_media_transport::{
    ConnEvent, ConnId, ConnSocket, Connection, Connections, Datagram, HostAddrs, PcState, Shard,
    ShardApp, SharedRoute, SharedUdp, spawn,
};
use stun_codec::rfc5389::attributes::Username;
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::{Message, MessageClass, MessageEncoder, TransactionId};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const LOCALHOST: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
const WAIT: Duration = Duration::from_secs(2);

fn binding_request(username: &str) -> Vec<u8> {
    let mut message =
        Message::<Attribute>::new(MessageClass::Request, BINDING, TransactionId::new([7; 12]));
    message.add_attribute(Username::new(username.to_owned()).unwrap());
    MessageEncoder::new().encode_into_bytes(message).unwrap()
}

/// Two ids from real connections, so they are unique like production ones.
fn conn_ids() -> (ConnId, ConnId) {
    let now = Instant::now();
    (
        Connection::new(Rtc::builder(), now).id(),
        Connection::new(Rtc::builder(), now).id(),
    )
}

async fn recv(rx: &mut mpsc::Receiver<(ConnId, Datagram)>) -> Option<(ConnId, Datagram)> {
    tokio::time::timeout(WAIT, rx.recv()).await.ok().flatten()
}

async fn nothing(rx: &mut mpsc::Receiver<(ConnId, Datagram)>) -> bool {
    tokio::time::timeout(Duration::from_millis(200), rx.recv())
        .await
        .is_err()
}

#[tokio::test]
async fn shared_udp_routes_by_ufrag_then_learned_address() {
    let shared = SharedUdp::bind(LOCALHOST, HostAddrs::from_addrs([Ipv4Addr::LOCALHOST])).unwrap();
    let port = shared.local_addr().unwrap();
    let (a, b) = conn_ids();
    let (inbox, mut rx) = mpsc::channel(16);
    shared.register(
        "ufragA",
        SharedRoute {
            conn: a,
            inbox: inbox.clone(),
        },
    );
    shared.register(
        "ufragB",
        SharedRoute {
            conn: b,
            inbox: inbox.clone(),
        },
    );
    assert_eq!(shared.len(), 2);

    let peer_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let peer_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    // Unknown address: media and unknown ufrags are dropped.
    peer_a.send_to(b"\x80\x60media", port).await.unwrap();
    peer_a
        .send_to(&binding_request("nobody:x"), port)
        .await
        .unwrap();
    assert!(nothing(&mut rx).await);

    // A binding request admits its address for the named connection.
    peer_a
        .send_to(&binding_request("ufragA:remote"), port)
        .await
        .unwrap();
    let (conn, datagram) = recv(&mut rx).await.unwrap();
    assert_eq!(conn, a);
    assert_eq!(datagram.source, peer_a.local_addr().unwrap());
    assert_eq!(
        datagram.destination, port,
        "labelled with the host candidate"
    );
    peer_b
        .send_to(&binding_request("ufragB:remote"), port)
        .await
        .unwrap();
    assert_eq!(recv(&mut rx).await.unwrap().0, b);

    // Learned addresses carry any datagram to their own connection.
    peer_b.send_to(b"\x80\x60to-b", port).await.unwrap();
    peer_a.send_to(b"\x80\x60to-a", port).await.unwrap();
    let first = recv(&mut rx).await.unwrap();
    let second = recv(&mut rx).await.unwrap();
    assert_eq!((first.0, &first.1.contents[2..]), (b, &b"to-b"[..]));
    assert_eq!((second.0, &second.1.contents[2..]), (a, &b"to-a"[..]));

    // ICE restart: the new ufrag routes, the old one no longer does.
    shared.register(
        "ufragA2",
        SharedRoute {
            conn: a,
            inbox: inbox.clone(),
        },
    );
    let peer_c = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer_c
        .send_to(&binding_request("ufragA:remote"), port)
        .await
        .unwrap();
    assert!(nothing(&mut rx).await);
    peer_c
        .send_to(&binding_request("ufragA2:remote"), port)
        .await
        .unwrap();
    assert_eq!(recv(&mut rx).await.unwrap().0, a);
    // The address learned before the restart still belongs to `a`.
    peer_a.send_to(b"\x80\x60still-a", port).await.unwrap();
    assert_eq!(recv(&mut rx).await.unwrap().0, a);

    // Unregister drops both the ufrag and the learned addresses.
    shared.unregister(a);
    peer_a.send_to(b"\x80\x60gone", port).await.unwrap();
    peer_c
        .send_to(&binding_request("ufragA2:remote"), port)
        .await
        .unwrap();
    assert!(nothing(&mut rx).await);
    assert_eq!(shared.len(), 1);
}

#[tokio::test]
async fn shared_udp_survives_connection_reset() {
    let shared = SharedUdp::bind(LOCALHOST, HostAddrs::from_addrs([Ipv4Addr::LOCALHOST])).unwrap();
    let port = shared.local_addr().unwrap();
    let (a, _) = conn_ids();
    let (inbox, mut rx) = mpsc::channel(16);
    shared.register("ufragA", SharedRoute { conn: a, inbox });
    let closed = std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap();
    let egress = shared.egress();
    for _ in 0..3 {
        egress.send(closed, b"to a closed port");
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let peer = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    peer.send_to(&binding_request("ufragA:x"), port)
        .await
        .unwrap();
    assert_eq!(recv(&mut rx).await.unwrap().0, a, "reader kept running");
}

/// Two connections whose remote is the same ip:port stay apart because each
/// owns its socket. The STUN survey's own response never reaches media.
#[tokio::test]
async fn conn_sockets_isolate_same_remote_and_demux_survey() {
    let stun = piik_stun::listen(&["127.0.0.1:0"]).await.unwrap();
    let server = stun.addresses()[0];
    let one = ConnSocket::bind(LOCALHOST).unwrap();
    let two = ConnSocket::bind(LOCALHOST).unwrap();
    let (a, b) = conn_ids();
    let (inbox, mut rx) = mpsc::channel(16);
    one.attach(a, inbox.clone());
    two.attach(b, inbox.clone());
    let candidates = one.gather_hosts(true).unwrap();
    assert_eq!(candidates.len(), 1, "a concrete bind is its only host");
    assert_eq!(candidates[0].addr(), one.local_addr().unwrap());

    let remote = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    remote
        .send_to(b"\x80\x60one", one.local_addr().unwrap())
        .await
        .unwrap();
    remote
        .send_to(b"\x80\x60two", two.local_addr().unwrap())
        .await
        .unwrap();
    let mut got = [recv(&mut rx).await.unwrap(), recv(&mut rx).await.unwrap()];
    got.sort_by_key(|(conn, _)| *conn);
    assert_eq!((got[0].0, &got[0].1.contents[2..]), (a, &b"one"[..]));
    assert_eq!((got[1].0, &got[1].1.contents[2..]), (b, &b"two"[..]));
    assert_eq!(got[0].1.source, got[1].1.source, "same remote ip:port");
    assert_eq!(got[0].1.destination, one.local_addr().unwrap());

    let mapped = one.query_stun(server, WAIT).await;
    assert_eq!(mapped, Some(one.local_addr().unwrap()));
    assert!(nothing(&mut rx).await, "the survey response is not media");

    // A foreign Binding response (unknown transaction) is media for str0m.
    let mut response = Message::<Attribute>::new(
        MessageClass::SuccessResponse,
        BINDING,
        TransactionId::new([9; 12]),
    );
    response.add_attribute(stun_codec::rfc5389::attributes::XorMappedAddress::new(
        remote.local_addr().unwrap(),
    ));
    let bytes = MessageEncoder::new().encode_into_bytes(response).unwrap();
    remote
        .send_to(&bytes, one.local_addr().unwrap())
        .await
        .unwrap();
    assert_eq!(recv(&mut rx).await.unwrap().0, a);

    // An unanswered query times out without blocking the socket.
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let started = Instant::now();
    assert_eq!(
        one.query_stun(silent.local_addr().unwrap(), Duration::from_millis(300))
            .await,
        None
    );
    assert!(started.elapsed() < WAIT);
    stun.close().await;
}

#[tokio::test]
async fn adopted_punched_socket_keeps_its_tuple() {
    let raw = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let local = raw.local_addr().unwrap();
    let remote: SocketAddr = "198.51.100.9:4000".parse().unwrap();
    let socket = ConnSocket::adopt(piik_media_transport::PunchedSocket {
        socket: raw,
        remote,
    })
    .unwrap();
    assert_eq!(socket.local_addr().unwrap(), local);
    assert_eq!(socket.punched_peer(), Some(remote));
    let candidate = socket.punched_candidate().unwrap();
    assert_eq!(
        candidate.candidate,
        "candidate:sp0 1 udp 1694498815 198.51.100.9 4000 typ srflx"
    );
    assert_eq!(candidate.sdp_mid.as_deref(), Some("0"));
}

// ---------------------------------------------------------------------------
// End to end: a spawned shard behind SharedUdp and a str0m client.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Echo {
    states: Vec<PcState>,
    report: Option<mpsc::UnboundedSender<PcState>>,
}

enum Command {
    Insert(Box<Connection>),
}

impl ShardApp for Echo {
    type Command = Command;

    fn on_command(&mut self, _now: Instant, command: Command, conns: &mut Connections) {
        let Command::Insert(conn) = command;
        conns.insert(*conn);
    }

    fn on_event(
        &mut self,
        _now: Instant,
        _conn: ConnId,
        event: ConnEvent,
        _conns: &mut Connections,
    ) {
        match event {
            ConnEvent::State(state) => {
                self.states.push(state);
                if let Some(report) = &self.report {
                    let _ = report.send(state);
                }
            }
            ConnEvent::ChannelData(channel, data) => {
                channel.send_text(&format!("echo:{}", String::from_utf8_lossy(&data.data)));
            }
            _ => {}
        }
    }
}

/// Drives a str0m client on a real socket until it receives `want`.
async fn run_client(mut rtc: Rtc, socket: UdpSocket, send: &str, want: &str) {
    let mut buffer = vec![0u8; 2048];
    let local = socket.local_addr().unwrap();
    let mut channel = None;
    let mut sent = false;
    loop {
        let timeout = loop {
            match rtc.poll_output().unwrap() {
                Output::Timeout(at) => break at,
                Output::Transmit(t) => {
                    let _ = socket.send_to(&t.contents, t.destination).await;
                }
                Output::Event(Event::ChannelOpen(id, _)) => channel = Some(id),
                Output::Event(Event::ChannelData(data)) => {
                    if data.data == want.as_bytes() {
                        return;
                    }
                }
                Output::Event(_) => {}
            }
        };
        if let (Some(id), false) = (channel, sent) {
            sent = rtc
                .channel(id)
                .unwrap()
                .write(false, send.as_bytes())
                .unwrap();
            continue;
        }
        let wait = tokio::time::sleep_until(timeout.max(Instant::now()).into());
        tokio::select! {
            received = socket.recv_from(&mut buffer) => {
                let (count, source) = received.unwrap();
                let receive = Receive::new(Protocol::Udp, source, local, &buffer[..count]).unwrap();
                rtc.handle_input(Input::Receive(Instant::now(), receive)).unwrap();
            }
            () = wait => rtc.handle_input(Input::Timeout(Instant::now())).unwrap(),
        }
    }
}

#[tokio::test]
async fn spawned_shard_serves_a_client_through_shared_udp() {
    let shared =
        Arc::new(SharedUdp::bind(LOCALHOST, HostAddrs::from_addrs([Ipv4Addr::LOCALHOST])).unwrap());
    let (report, mut states) = mpsc::unbounded_channel();
    let (handle, task) = spawn(Shard::new(Echo {
        report: Some(report),
        ..Default::default()
    }));

    let client_socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut client = Rtc::builder().build(Instant::now());
    client
        .add_local_candidate(Candidate::host(client_socket.local_addr().unwrap(), "udp").unwrap());
    let mut api = client.sdp_api();
    api.add_channel("piik".into());
    let (offer, pending) = api.apply().unwrap();

    let mut conn = Connection::new(
        codec_config(&[VideoCodec::Vp8], false).set_ice_lite(true),
        Instant::now(),
    );
    for candidate in shared.candidates().unwrap() {
        conn.add_local_candidate(candidate);
    }
    let answer = conn.accept_offer(&offer.to_sdp_string()).unwrap();
    conn.set_egress(shared.egress());
    shared.register(
        &conn.local_ufrag(),
        SharedRoute {
            conn: conn.id(),
            inbox: handle.inbox.clone(),
        },
    );
    handle
        .commands
        .send(Command::Insert(Box::new(conn)))
        .await
        .unwrap();
    client
        .sdp_api()
        .accept_answer(pending, SdpAnswer::from_sdp_string(&answer).unwrap())
        .unwrap();

    tokio::time::timeout(
        Duration::from_secs(10),
        run_client(client, client_socket, "hello", "echo:hello"),
    )
    .await
    .expect("client receives the shard's echo");
    assert_eq!(states.recv().await, Some(PcState::Connecting));
    assert_eq!(states.recv().await, Some(PcState::Connected));

    // Dropping every command sender stops the shard and closes connections.
    drop(handle);
    let app = tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
    assert_eq!(
        app.states,
        [PcState::Connecting, PcState::Connected, PcState::Closed]
    );
}
