// Candidate gathering, STUN surveys and candidate generations.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicUsize, Ordering};

use bytecodec::{DecodeExt, EncodeExt};
use piik_media_transport::ConnSocket;
use stun_codec::rfc5389::attributes::XorMappedAddress;
use stun_codec::rfc5389::{Attribute, methods::BINDING};
use stun_codec::{Message, MessageClass, MessageDecoder, MessageEncoder, TransactionId};
use tokio::net::UdpSocket;
use tokio_util::sync::CancellationToken;

use super::*;
use crate::gather::{self, GatherJob, Gatherings, PrepareMapping, stun_urls, survey};

/// What a scripted Binding server does with one request.
enum Reply {
    /// Answer with this mapped port at the sender's address.
    Port(u16),
    /// Answer a different, unknown transaction first.
    Unmatched(u16),
    Silent,
}

/// A scripted STUN Binding server on 127.0.0.1.
struct Binding {
    address: SocketAddr,
    requests: Arc<AtomicUsize>,
    /// The sender of the latest request.
    sender: Arc<Mutex<Option<SocketAddr>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Binding {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn binding(respond: impl Fn(usize, [u8; 12]) -> Reply + Send + Sync + 'static) -> Binding {
    let socket = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));
    let sender = Arc::new(Mutex::new(None));
    let (counter, last) = (requests.clone(), sender.clone());
    let task = tokio::spawn(async move {
        let mut buffer = [0u8; 1500];
        loop {
            let Ok((size, from)) = socket.recv_from(&mut buffer).await else {
                return;
            };
            let Ok(Ok(request)) =
                MessageDecoder::<Attribute>::new().decode_from_bytes(&buffer[..size])
            else {
                continue;
            };
            if request.class() != MessageClass::Request {
                continue;
            }
            let count = counter.fetch_add(1, Ordering::SeqCst) + 1;
            *last.lock().unwrap() = Some(from);
            let id = *request.transaction_id().as_bytes();
            let (id, port) = match respond(count, id) {
                Reply::Port(port) => (id, port),
                Reply::Unmatched(port) => (rand::random(), port),
                Reply::Silent => continue,
            };
            let mut response = Message::<Attribute>::new(
                MessageClass::SuccessResponse,
                BINDING,
                TransactionId::new(id),
            );
            response.add_attribute(Attribute::XorMappedAddress(XorMappedAddress::new(
                SocketAddr::new(from.ip(), port),
            )));
            let bytes = MessageEncoder::new().encode_into_bytes(response).unwrap();
            let _ = socket.send_to(&bytes, from).await;
        }
    });
    Binding {
        address,
        requests,
        sender,
        task,
    }
}

fn url(binding: &Binding) -> String {
    format!("stun:{}", binding.address)
}

fn conn_socket() -> Arc<ConnSocket> {
    Arc::new(ConnSocket::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap())
}

/// Runs one gathering and collects its events.
fn start(
    socket: &Arc<ConnSocket>,
    gatherings: &Gatherings,
    servers: &[IceServerUrls],
    mapping: Option<PrepareMapping>,
    hosts: Vec<IceCandidateInit>,
    ufrag: &str,
) -> mpsc::UnboundedReceiver<EdgeEvent> {
    let Events { sink, receiver } = events();
    let (generation, cancel) = gatherings.next(&CancellationToken::new());
    tokio::spawn(gather::run(GatherJob {
        socket: socket.clone(),
        hosts,
        servers: stun_urls(servers),
        mapping,
        mid: "0".into(),
        ufrag: ufrag.into(),
        local_port: socket.local_addr().unwrap().port(),
        sink,
        generation,
        cancel,
    }));
    receiver
}

/// The next candidate event: `Some(line)`, or `None` at end-of-candidates.
async fn next(
    receiver: &mut mpsc::UnboundedReceiver<EdgeEvent>,
    timeout: Duration,
) -> Option<Option<IceCandidateInit>> {
    let event = tokio::time::timeout(timeout, receiver.recv())
        .await
        .ok()??;
    let EdgeEvent::LocalCandidate { candidate, .. } = event else {
        return None;
    };
    Some(candidate)
}

fn host() -> IceCandidateInit {
    IceCandidateInit {
        candidate: "candidate:1 1 udp 2130706431 127.0.0.1 9 typ host".into(),
        sdp_mid: Some("0".into()),
        sdp_m_line_index: Some(0),
        username_fragment: Some("gathering".into()),
    }
}

#[tokio::test]
async fn candidates_trickle_before_gateway_mapping_completes() {
    for close_first in [false, true] {
        let socket = conn_socket();
        let stun = binding(|_, _| Reply::Port(41000)).await;
        let (release, wait) = tokio::sync::oneshot::channel::<u16>();
        let wait = Arc::new(Mutex::new(Some(wait)));
        let started = Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        let mapping: PrepareMapping = Arc::new(move || {
            flag.store(true, Ordering::SeqCst);
            let wait = wait.lock().unwrap().take();
            Box::pin(async move { wait?.await.ok() })
        });
        let gatherings = Gatherings::default();
        let mut candidates = start(
            &socket,
            &gatherings,
            &[vec![url(&stun)]],
            Some(mapping),
            vec![host()],
            "gathering",
        );
        for _ in 0..2 {
            let candidate = next(&mut candidates, Duration::from_secs(1))
                .await
                .flatten()
                .expect("ordinary ICE waited for the gateway mapping");
            assert!(
                candidate.candidate.contains(" typ host")
                    || candidate.candidate.starts_with("candidate:ns1 "),
                "{candidate:?}"
            );
            assert_eq!(candidate.username_fragment.as_deref(), Some("gathering"));
        }
        assert!(started.load(Ordering::SeqCst), "mapping did not start");
        assert!(
            next(&mut candidates, Duration::from_millis(50))
                .await
                .is_none(),
            "gathering ended before its mapping owner finished"
        );
        if close_first {
            gatherings.close();
        }
        let port = socket.local_addr().unwrap().port() % 65535 + 1;
        let _ = release.send(port);
        if close_first {
            assert!(
                next(&mut candidates, Duration::from_millis(100))
                    .await
                    .is_none(),
                "retired generation emitted a late candidate"
            );
            continue;
        }
        let mapped = next(&mut candidates, Duration::from_secs(1))
            .await
            .flatten()
            .expect("mapped candidate");
        let fields: Vec<&str> = mapped.candidate.split_whitespace().collect();
        assert_eq!(fields[0], "candidate:mp1");
        assert_eq!(fields[3], "1694498559", "mapped priority");
        assert_eq!(fields[4], "127.0.0.1", "observed public address");
        assert_eq!(fields[5], port.to_string());
        assert_eq!(fields[7], "srflx");
        assert_eq!(mapped.username_fragment.as_deref(), Some("gathering"));
        assert_eq!(
            next(&mut candidates, Duration::from_secs(1)).await,
            Some(None),
            "end-of-candidates"
        );
        assert!(
            next(&mut candidates, Duration::from_millis(50))
                .await
                .is_none(),
            "duplicate end"
        );
    }
}

#[tokio::test]
async fn mapped_candidate_skips_an_unchanged_port() {
    // One mapped candidate per (address, external port); none when STUN
    // already sees the mapped port.
    let socket = conn_socket();
    let stun = binding(|_, _| Reply::Port(41000)).await;
    let ready =
        |port: u16| -> PrepareMapping { Arc::new(move || Box::pin(async move { Some(port) })) };
    let gatherings = Gatherings::default();
    let mut candidates = start(
        &socket,
        &gatherings,
        &[vec![url(&stun)]],
        Some(ready(41000)),
        Vec::new(),
        "u",
    );
    let mut lines = Vec::new();
    while let Some(Some(candidate)) = next(&mut candidates, Duration::from_secs(2)).await {
        lines.push(candidate.candidate);
    }
    assert_eq!(
        lines.len(),
        1,
        "a mapping on the observed port is not a new candidate: {lines:?}"
    );
    assert!(lines[0].starts_with("candidate:ns1 "));
}

#[tokio::test]
async fn native_gathering_refreshes_mapping_on_its_own_socket() {
    let port = Arc::new(AtomicU16::new(40000));
    let mapped = port.clone();
    let stun = binding(move |_, _| Reply::Port(mapped.load(Ordering::SeqCst))).await;
    for attempt in 0..3u16 {
        port.store(40000 + attempt, Ordering::SeqCst);
        let socket = conn_socket();
        let local = socket.local_addr().unwrap().port();
        let mut candidates = start(
            &socket,
            &Gatherings::default(),
            &[vec![url(&stun)]],
            None,
            Vec::new(),
            "u",
        );
        let candidate = next(&mut candidates, Duration::from_secs(2))
            .await
            .flatten()
            .expect("fresh mapping");
        let fields: Vec<&str> = candidate.candidate.split_whitespace().collect();
        assert_eq!(
            fields[5],
            (40000 + attempt).to_string(),
            "attempt {attempt} reused a mapping"
        );
        assert_eq!(
            fields.last().copied(),
            Some(local.to_string().as_str()),
            "rport is the media socket"
        );
        assert_eq!(
            stun.sender.lock().unwrap().map(|sender| sender.port()),
            Some(local),
            "STUN left the socket"
        );
    }
    assert!(
        stun.requests.load(Ordering::SeqCst) >= 3,
        "later gatherings did not query again"
    );
}

#[tokio::test]
async fn gathering_refresh_keeps_a_healthy_sibling() {
    let stun = binding(|_, _| Reply::Port(40000)).await;
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 1, None).await.unwrap();
    let sibling = Peer::connect(&engine, &source, "healthy").await;
    for attempt in 0..2u32 {
        let mut events = events();
        let edge = engine
            .new_edge(
                &source,
                EdgeOptions {
                    connection_id: format!("recover-{attempt}"),
                    ice_servers: vec![vec![url(&stun)]],
                    events: Some(events.sink.clone()),
                    ..EdgeOptions::default()
                },
            )
            .await
            .unwrap();
        edge.create_offer().await.unwrap();
        let survey = wait_event(&mut events.receiver, Duration::from_secs(3), |event| {
            matches!(event, EdgeEvent::LocalCandidate { candidate: Some(candidate), .. }
                if candidate.candidate.starts_with("candidate:ns"))
        })
        .await;
        assert!(
            survey.is_some(),
            "edge gathering produced no survey candidate"
        );
        edge.close().await;
        write_source_frame(
            &source,
            VP8_KEY_8X8,
            Duration::from_secs(u64::from(attempt) + 1),
        )
        .await
        .unwrap();
        sibling.wait_packets(attempt as usize + 1).await;
        assert_eq!(
            sibling.edge.state(),
            PcState::Connected,
            "refresh retired a healthy connection"
        );
    }
    sibling.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn survey_retries_lost_binding_and_ignores_unmatched_response() {
    let socket = conn_socket();
    let stun = binding(|count, _| {
        if count == 1 {
            Reply::Unmatched(40000)
        } else {
            Reply::Port(40001)
        }
    })
    .await;
    let mut observations = Vec::new();
    survey(
        &socket,
        &stun_urls(&[vec![url(&stun)]]),
        &CancellationToken::new(),
        |mapped| observations.push(mapped),
    )
    .await;
    assert!(
        stun.requests.load(Ordering::SeqCst) >= 2,
        "no retransmission"
    );
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations[0].port, 40001,
        "accepted an unmatched STUN transaction"
    );
}

#[tokio::test]
async fn concurrent_survey_cancellation_keeps_siblings() {
    let socket = conn_socket();
    let answer = Arc::new(AtomicBool::new(false));
    let gate = answer.clone();
    let stun = binding(move |_, _| {
        if gate.load(Ordering::SeqCst) {
            Reply::Port(40001)
        } else {
            Reply::Silent
        }
    })
    .await;
    let servers = stun_urls(&[vec![url(&stun)]]);
    let retiring = CancellationToken::new();
    let retired = {
        let (socket, servers, retiring) = (socket.clone(), servers.clone(), retiring.clone());
        tokio::spawn(async move {
            survey(&socket, &servers, &retiring, |_| {
                panic!("retired survey emitted a candidate")
            })
            .await;
        })
    };
    eventually("initial Binding request", async || {
        stun.requests.load(Ordering::SeqCst) > 0
    })
    .await;
    let mut siblings = Vec::new();
    for _ in 0..3 {
        let (socket, servers) = (socket.clone(), servers.clone());
        siblings.push(tokio::spawn(async move {
            let mut observed = Vec::new();
            survey(&socket, &servers, &CancellationToken::new(), |mapped| {
                observed.push(mapped.port)
            })
            .await;
            observed
        }));
    }
    retiring.cancel();
    tokio::time::timeout(Duration::from_secs(1), retired)
        .await
        .expect("retired gathering")
        .unwrap();
    answer.store(true, Ordering::SeqCst);
    for sibling in siblings {
        let observed = tokio::time::timeout(Duration::from_secs(6), sibling)
            .await
            .expect("sibling")
            .unwrap();
        assert_eq!(
            observed,
            vec![40001],
            "retiring a gathering cancelled a healthy sibling"
        );
    }
}

#[tokio::test]
async fn restarted_gathering_does_not_reuse_a_retired_transaction() {
    let socket = conn_socket();
    let first = Arc::new(Mutex::new(None::<[u8; 12]>));
    let seen = first.clone();
    let stun = binding(move |_, id| {
        let mut first = seen.lock().unwrap();
        match *first {
            None => {
                *first = Some(id);
                Reply::Silent
            }
            Some(previous) if previous == id => Reply::Silent,
            Some(_) => Reply::Port(40001),
        }
    })
    .await;
    let servers = [vec![url(&stun)]];
    let gatherings = Gatherings::default();
    let mut previous = start(&socket, &gatherings, &servers, None, Vec::new(), "previous");
    eventually("previous Binding request", async || {
        first.lock().unwrap().is_some()
    })
    .await;
    let mut current = start(&socket, &gatherings, &servers, None, Vec::new(), "current");
    let candidate = next(&mut current, Duration::from_secs(2))
        .await
        .flatten()
        .expect("new gathering waited");
    assert_eq!(candidate.candidate.split_whitespace().nth(5), Some("40001"));
    assert_eq!(candidate.username_fragment.as_deref(), Some("current"));
    assert!(
        next(&mut previous, Duration::from_millis(100))
            .await
            .is_none(),
        "retired gathering delivered"
    );
}

#[tokio::test]
async fn survey_healthy_stun_does_not_wait_for_slow_or_silent_destinations() {
    let socket = conn_socket();
    let local = socket.local_addr().unwrap().port();
    let silent = binding(|_, _| Reply::Silent).await;
    let healthy = binding(move |_, _| Reply::Port(local)).await;
    // An unresolvable name stands in for a blocked resolver.
    let servers = stun_urls(&[vec![
        "stun:unanswered.invalid.:3478".into(),
        url(&silent),
        url(&healthy),
    ]]);
    let cancel = CancellationToken::new();
    let (sender, mut observations) = mpsc::unbounded_channel();
    let started = tokio::time::Instant::now();
    let task = {
        let (socket, cancel) = (socket.clone(), cancel.clone());
        tokio::spawn(async move {
            survey(&socket, &servers, &cancel, |mapped| {
                let _ = sender.send(mapped);
            })
            .await;
        })
    };
    let mapped = tokio::time::timeout(Duration::from_secs(2), observations.recv())
        .await
        .expect("healthy STUN waited for a broken destination")
        .unwrap();
    assert_eq!(
        mapped.port, local,
        "STUN did not use the shared media socket"
    );
    assert_eq!(
        mapped.server_index, 3,
        "failed destinations shifted the identity"
    );
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .expect("retired survey kept waiting")
        .unwrap();
    assert!(started.elapsed() < STUN_SURVEY_TIMEOUT_FOR_TEST);
}

const STUN_SURVEY_TIMEOUT_FOR_TEST: Duration = piik_media_transport::STUN_SURVEY_TIMEOUT;

#[tokio::test]
async fn survey_candidate_identity_survives_response_order() {
    let socket = conn_socket();
    let release = Arc::new(AtomicBool::new(false));
    let gate = release.clone();
    let slow = binding(move |_, _| {
        if gate.load(Ordering::SeqCst) {
            Reply::Port(40000)
        } else {
            Reply::Silent
        }
    })
    .await;
    let fast = binding(|_, _| Reply::Port(40003)).await;
    let servers = [vec![
        "turn:unused.invalid:3478".into(),
        url(&slow),
        url(&fast),
    ]];
    let mut candidates = start(
        &socket,
        &Gatherings::default(),
        &servers,
        None,
        Vec::new(),
        "gathering",
    );
    for (foundation, port) in [("candidate:ns2", "40003"), ("candidate:ns1", "40000")] {
        let candidate = next(&mut candidates, Duration::from_secs(3))
            .await
            .flatten()
            .expect("STUN response");
        let fields: Vec<&str> = candidate.candidate.split_whitespace().collect();
        assert_eq!(
            (fields[0], fields[5]),
            (foundation, port),
            "{}",
            candidate.candidate
        );
        release.store(true, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn survey_preserves_destinations_with_duplicate_mapping() {
    for ports in [[40000u16, 40000, 40003, 40006], [40000; 4]] {
        let socket = conn_socket();
        let release = Arc::new(AtomicBool::new(false));
        let mut servers = Vec::new();
        let mut want = std::collections::HashMap::new();
        let mut listeners = Vec::new();
        for (index, port) in ports.into_iter().enumerate() {
            let gate = release.clone();
            // ns2 answers first; ns1/ns3/ns4 follow and share ns1's mapping.
            let listener = binding(move |_, _| {
                if index == 1 || gate.load(Ordering::SeqCst) {
                    Reply::Port(port)
                } else {
                    Reply::Silent
                }
            })
            .await;
            servers.push(url(&listener));
            want.insert(format!("candidate:ns{}", index + 1), port.to_string());
            listeners.push(listener);
        }
        let mut candidates = start(
            &socket,
            &Gatherings::default(),
            &[servers],
            None,
            vec![host()],
            "u",
        );
        loop {
            let candidate = next(&mut candidates, Duration::from_secs(6))
                .await
                .expect("survey did not complete");
            let Some(candidate) = candidate else {
                assert!(
                    want.is_empty(),
                    "deduplication lost destination observations: {want:?}"
                );
                break;
            };
            if !candidate.candidate.starts_with("candidate:ns") {
                continue;
            }
            let fields: Vec<&str> = candidate.candidate.split_whitespace().collect();
            assert_eq!(
                want.remove(fields[0]).as_deref(),
                Some(fields[5]),
                "{}",
                candidate.candidate
            );
            assert!(release.load(Ordering::SeqCst) || fields[0] == "candidate:ns2");
            release.store(true, Ordering::SeqCst);
        }
    }
}

#[tokio::test]
async fn native_remote_candidates_respect_json_generation() {
    // A malformed usernameFragment is dropped at the boundary; the session
    // survives.
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let peer = Peer::connect(&engine, &source, "generation").await;
    for fragment in ["two tokens", "stale"] {
        peer.edge
            .add_remote_candidate(Some(IceCandidateInit {
                candidate: "candidate:9 1 udp 1 127.0.0.1 9 typ host".into(),
                sdp_mid: Some("0".into()),
                sdp_m_line_index: Some(0),
                username_fragment: Some(fragment.into()),
            }))
            .await
            .expect("candidate generation failed the control session");
    }
    peer.receiver.add_remote_candidate(None).await.unwrap();
    assert_eq!(peer.edge.state(), PcState::Connected);
    peer.close().await;
    engine.close().await.unwrap();
}

#[test]
fn stun_urls_keep_only_udp_stun_destinations() {
    let urls = stun_urls(&[vec![
        "turn:relay.invalid:3478".into(),
        "stun:a.invalid".into(),
        "STUN:b.invalid:3479?transport=udp".into(),
        "stun:c.invalid:3478?transport=tcp".into(),
        "stun:[::1]:3478".into(),
        "stun:bad:port".into(),
    ]]);
    let summary: Vec<(usize, &str, u16)> = urls
        .iter()
        .map(|url| (url.index, url.host.as_str(), url.port))
        .collect();
    assert_eq!(
        summary,
        vec![
            (1, "a.invalid", 3478),
            (2, "b.invalid", 3479),
            (4, "::1", 3478)
        ]
    );
    assert!(!gather::has_stun(&[vec!["turn:x:1".into()]]));
}
