//! The embedded SFU: server and addressing cases on the real str0m media, and
//! one str0m publisher negotiating through `/signal`.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use piik_media_transport::str0m::change::{SdpAnswer, SdpPendingOffer};
use piik_media_transport::str0m::media::{Direction, MediaKind};
use piik_media_transport::str0m::net::{Protocol, Receive};
use piik_media_transport::str0m::{Candidate, Input, Output, Rtc};
use piik_sfu::runtime::{
    LayerFormat, PublicationMedia, ResourceFence, Runtime as _, VoiceConnectionPlan,
    VoiceEventSink, VoicePublication,
};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
use url::Url;

use super::{
    ALLOWED_ORIGIN, CLOSE_TIMEOUT, Harness, Req, RoomRequest, free_port, new_server, options,
    sfu_config, start, test_config,
};

const WAIT: Duration = Duration::from_secs(10);

fn sfu_options(public_ip: &str, only: bool) -> crate::Options {
    let mut config = test_config();
    config.sfu = Some(sfu_config(public_ip, only));
    options(config)
}

fn sfu_address(h: &Harness) -> SocketAddr {
    h.server
        .inner()
        .embedded_sfu()
        .expect("embedded SFU")
        .udp()
        .local_addr()
        .expect("SFU address")
}

fn assert_udp_free(address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UdpSocket::bind(address) {
            Ok(_) => return,
            Err(error) if Instant::now() >= deadline => {
                panic!("UDP {address} remained owned: {error}")
            }
            Err(_) => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sfu_listener_follows_application_close_and_end() {
    for ending in [false, true] {
        let h = start(sfu_options("", false)).await;
        let address = sfu_address(&h);
        assert!(h.server.inner().has_signaling());
        h.send(Req::get("/api/capabilities"))
            .await
            .expect(200, r#"{"sfu":true,"natPrediction":false}"#);
        if ending {
            h.server.end(CLOSE_TIMEOUT).await.expect("end");
        } else {
            h.close().await;
        }
        assert!(h.server.inner().embedded_sfu().is_none());
        assert_udp_free(address);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn occupied_sfu_port_rolls_back_before_opening_the_database() {
    let occupied = UdpSocket::bind("127.0.0.1:0").expect("occupied UDP");
    let directory = tempfile::tempdir().expect("tempdir");
    let mut config = test_config();
    config.port = free_port();
    config.room_database_path = Some(directory.path().join("rooms.sqlite"));
    config.stun_listen_addresses = vec!["127.0.0.1:0".parse().expect("address")];
    let mut sfu = sfu_config("", false);
    sfu.port = occupied.local_addr().expect("address").port();
    config.sfu = Some(sfu);
    let port = config.port;
    let database = config.room_database_path.clone().expect("path");
    let server = new_server(options(config));
    match server.listen().await {
        Err(crate::Error::Sfu(message)) => {
            assert!(message.contains("Piik SFU listener failed"), "{message}");
        }
        other => panic!("occupied media listener was accepted: {:?}", other.ok()),
    }
    assert!(
        !database.exists(),
        "SFU bind failure touched room persistence"
    );
    super::assert_port_free(port);
    assert!(server.inner().stun_addresses().is_empty());
    assert!(server.inner().embedded_sfu().is_none());
    occupied
        .set_read_timeout(Some(Duration::from_millis(10)))
        .expect("startup rollback closed another listener");
}

fn vp8_publication() -> PublicationMedia {
    PublicationMedia {
        codec: "vp8".into(),
        rids: vec![String::new()],
        formats: vec![LayerFormat {
            width: 640,
            height: 360,
            bitrate: 1_000_000,
        }],
        ..PublicationMedia::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sfu_public_hostname_starts_the_existing_media_runtime() {
    let h = start(sfu_options("localhost", false)).await;
    let media = h.server.inner().embedded_sfu().expect("embedded SFU");
    let fence = ResourceFence {
        room_id: "1234".into(),
        share_generation: "share_12345678".into(),
        publication_generation: "publication_12345678".into(),
    };
    tokio::task::spawn_blocking(move || {
        media
            .prepare_publication(&fence, "publisher_12345678", &vp8_publication())
            .expect("hostname-backed media initialization");
        assert!(media.has_publication(&fence, "publisher_12345678"));
    })
    .await
    .expect("prepare");
    h.close().await;
}

/// The host candidate a new voice connection advertises.
async fn voice_candidate(media: &piik_sfu::Media, connection: &str) -> (String, u16) {
    let media = media.clone();
    let connection = connection.to_owned();
    tokio::task::spawn_blocking(move || {
        // str0m's ICE-lite offer carries its host candidates in the SDP.
        let (sender, receiver) = std::sync::mpsc::sync_channel(8);
        let emit: VoiceEventSink = Arc::new(move |event| {
            let offered = event.description.and_then(|description| {
                description
                    .sdp
                    .sdp
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("a=")
                            .filter(|l| l.starts_with("candidate:"))
                    })
                    .map(str::to_owned)
            });
            if let Some(candidate) = event.candidate.map(|c| c.candidate).or(offered) {
                let _ = sender.try_send(candidate);
            }
        });
        let plan = VoiceConnectionPlan {
            room_id: "1234".into(),
            peer_id: connection.clone(),
            session_id: connection.clone(),
            connection_id: connection.clone(),
            publish: vec![VoicePublication {
                id: connection.clone(),
                peer_id: connection.clone(),
            }],
            subscribe: Vec::new(),
        };
        let voice = media.voice().expect("room voice");
        if let Some(work) = voice.configure_voice(plan, emit).expect("configure voice") {
            work().expect("voice offer");
        }
        let candidate = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("voice connection did not gather a real ICE candidate");
        let fields: Vec<&str> = candidate.split_whitespace().collect();
        (
            fields[4].to_owned(),
            fields[5].parse().expect("candidate port"),
        )
    })
    .await
    .expect("voice candidate")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sfu_public_dns_refresh_changes_new_voice_candidates() {
    // A literal resolves to itself, so refreshing with a new literal is the
    // DNS answer changing; an unresolvable name is the DNS failure.
    let h = start(sfu_options("198.51.100.5", false)).await;
    let media = h.server.inner().embedded_sfu().expect("embedded SFU");
    let initial = voice_candidate(&media, "initial").await;
    assert_eq!(initial.0, "198.51.100.5", "initial candidate {initial:?}");
    assert_eq!(initial.1, sfu_address(&h).port());
    h.server.inner().refresh_sfu("198.51.100.9").await;
    let updated = voice_candidate(&media, "updated").await;
    assert_eq!(
        updated,
        ("198.51.100.9".to_owned(), initial.1),
        "a new connection must use the refreshed IP on the same UDP listener"
    );
    h.server.inner().refresh_sfu("unresolved.invalid").await;
    let retained = voice_candidate(&media, "dns-failed").await;
    assert_eq!(
        retained, updated,
        "DNS failure lost the last usable address"
    );
    h.close().await;
    assert!(!h.server.inner().has_dns_refresh());
}

/// One str0m publisher on its own loopback socket, driven by a thread.
struct Publisher {
    rtc: Arc<Mutex<Rtc>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Publisher {
    fn new() -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").expect("publisher socket");
        socket.set_nonblocking(true).expect("nonblocking");
        let local = socket.local_addr().expect("publisher address");
        let mut rtc = Rtc::builder().build(Instant::now());
        rtc.add_local_candidate(Candidate::host(local, "udp").expect("host candidate"));
        let rtc = Arc::new(Mutex::new(rtc));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (rtc, stop) = (rtc.clone(), stop.clone());
            std::thread::spawn(move || drive(&rtc, &stop, &socket, local))
        };
        Self {
            rtc,
            stop,
            thread: Some(thread),
        }
    }

    fn offer(&self) -> (String, SdpPendingOffer) {
        let mut rtc = self.rtc.lock().expect("rtc");
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        let (offer, pending) = api.apply().expect("offer");
        (offer.to_sdp_string(), pending)
    }

    fn accept(&self, pending: SdpPendingOffer, answer: &str) {
        self.rtc
            .lock()
            .expect("rtc")
            .sdp_api()
            .accept_answer(pending, SdpAnswer::from_sdp_string(answer).expect("answer"))
            .expect("accept answer");
    }

    fn connected(&self) -> bool {
        self.rtc.lock().expect("rtc").is_connected()
    }
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn drive(rtc: &Mutex<Rtc>, stop: &AtomicBool, socket: &UdpSocket, local: SocketAddr) {
    let mut buffer = vec![0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        {
            let mut rtc = rtc.lock().expect("rtc");
            loop {
                match rtc.poll_output() {
                    Ok(Output::Timeout(at)) if at <= Instant::now() => {
                        let _ = rtc.handle_input(Input::Timeout(Instant::now()));
                    }
                    Ok(Output::Timeout(_)) => break,
                    Ok(Output::Transmit(transmit)) => {
                        let _ = socket.send_to(&transmit.contents, transmit.destination);
                    }
                    Ok(Output::Event(_)) => {}
                    Err(_) => return,
                }
            }
        }
        let mut received = false;
        while let Ok((count, source)) = socket.recv_from(&mut buffer) {
            received = true;
            if let Ok(receive) = Receive::new(Protocol::Udp, source, local, &buffer[..count]) {
                let _ = rtc
                    .lock()
                    .expect("rtc")
                    .handle_input(Input::Receive(Instant::now(), receive));
            }
        }
        if !received {
            std::thread::sleep(Duration::from_millis(1));
            let _ = rtc
                .lock()
                .expect("rtc")
                .handle_input(Input::Timeout(Instant::now()));
        }
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn signal_socket(h: &Harness, authenticate: Value) -> Socket {
    let mut request = format!("ws://127.0.0.1:{}/signal", h.port)
        .into_client_request()
        .expect("upgrade request");
    request
        .headers_mut()
        .insert("origin", ALLOWED_ORIGIN.parse().expect("origin"));
    let cookie = h.cookie().await;
    request
        .headers_mut()
        .insert("cookie", cookie.parse().expect("cookie"));
    let (mut socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("upgrade /signal");
    socket
        .send(Message::text(authenticate.to_string()))
        .await
        .expect("authenticate");
    next_message(&mut socket, |message| message["type"] == "authenticated").await;
    socket
}

/// The next server message `matches` accepts; others are skipped.
async fn next_message(socket: &mut Socket, matches: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let frame = tokio::time::timeout_at(deadline, socket.next())
            .await
            .expect("server message in time")
            .expect("open socket")
            .expect("frame");
        if let Message::Text(text) = frame {
            let message: Value = serde_json::from_str(&text).expect("server message");
            if matches(&message) {
                return message;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn str0m_publisher_connects_to_the_embedded_sfu_through_signaling() {
    let h = start(sfu_options("", true)).await;
    let cookie = h.cookie().await;
    let room = h
        .create_room(RoomRequest {
            cookie: &cookie,
            ..RoomRequest::default()
        })
        .await
        .expect_status(201)
        .room();
    let protocol = piik_protocol::SIGNALING_PROTOCOL;
    let mut host = signal_socket(
        &h,
        json!({"type": "authenticate", "protocol": protocol, "roomId": room.room_id,
            "role": "host", "clientId": "sfu_host_client_1234", "token": room.host_token,
            "shareGeneration": "sfu_share_generation_1234"}),
    )
    .await;
    let invite = Url::parse(&room.invite_url).expect("invite");
    let grant = invite
        .fragment()
        .unwrap_or_default()
        .trim_start_matches("v=")
        .to_owned();
    let mut viewer = json!({"type": "authenticate", "protocol": protocol,
        "roomId": room.room_id, "role": "viewer", "clientId": "sfu_viewer_client_1234"});
    if !grant.is_empty() {
        viewer["viewerGrant"] = grant.into();
    }
    let _viewer = signal_socket(&h, viewer).await;

    let config = next_message(&mut host, |message| message["type"] == "sfu-config").await;
    let publisher = Publisher::new();
    let (offer, pending) = publisher.offer();
    let fence = json!({"type": "sfu-signal", "revision": config["revision"],
        "publicationGeneration": config["publicationGeneration"],
        "connectionId": config["connectionId"]});
    let mut message = fence.clone();
    message["kind"] = "description".into();
    message["description"] = json!({"type": "offer", "sdp": offer});
    message["media"] = json!({"codec": "vp8",
        "layers": [{"rid": "", "width": 640, "height": 360, "bitrate": 1_000_000}],
        "audio": false, "audioBitrate": 0});
    host.send(Message::text(message.to_string()))
        .await
        .expect("publisher offer");
    let answer = next_message(&mut host, |message| {
        message["type"] == "sfu-signal"
            && message["kind"] == "description"
            && message["description"]["type"] == "answer"
    })
    .await;
    let answer = answer["description"]["sdp"].as_str().expect("answer SDP");
    assert!(answer.contains("a=ice-lite"), "{answer}");
    let port = sfu_address(&h).port();
    assert!(
        answer.contains(&format!("127.0.0.1 {port} typ host")),
        "the answer must advertise the SFU listener: {answer}"
    );
    publisher.accept(pending, answer);
    let deadline = Instant::now() + WAIT;
    while !publisher.connected() {
        assert!(
            Instant::now() < deadline,
            "the publisher did not connect to the SFU"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    drop(publisher);
    h.server.end(CLOSE_TIMEOUT).await.expect("end");
    assert_udp_free(SocketAddr::from((Ipv4Addr::LOCALHOST, port)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sfu_loopback_listener_advertises_its_address() {
    let h = start(sfu_options("", false)).await;
    let media = h.server.inner().embedded_sfu().expect("embedded SFU");
    let candidate = voice_candidate(&media, "loopback").await;
    assert_eq!(
        candidate,
        (Ipv4Addr::LOCALHOST.to_string(), sfu_address(&h).port())
    );
    h.close().await;
}
