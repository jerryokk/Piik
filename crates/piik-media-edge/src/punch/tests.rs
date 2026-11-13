// NAT punch preparation. Sequential mappings across virtual NATs need a NAT
// lab: these run on 127.0.0.1, where every mapping is stable (step 0),
// through the engine's private-address test seam.

use std::time::Duration;

use super::*;
use crate::{EdgeOptions, EngineOptions, PcState, ReceiverOptions};

const SECRET: &str = "4242424242424242424242424242424242424242424242424242424242424242";

fn engine() -> Engine {
    Engine::with_private_punch(EngineOptions {
        bind_address: "127.0.0.1:0".into(),
        include_loopback: true,
        ..EngineOptions::default()
    })
    .unwrap()
}

async fn survey_servers() -> (piik_stun::Server, Vec<IceServerUrls>) {
    let server = piik_stun::listen(&["127.0.0.1:0", "127.0.0.1:0", "127.0.0.1:0"])
        .await
        .unwrap();
    let urls = server
        .addresses()
        .iter()
        .map(|address| format!("stun:{address}"))
        .collect();
    (server, vec![urls])
}

#[test]
fn prediction_rejects_unsafe_destinations_and_overflow() {
    let prediction = |address: &str, port: i64, step: i64| PunchPrediction {
        address: address.into(),
        port,
        step,
    };
    for unsafe_prediction in [
        prediction("127.0.0.1", 5000, 1),
        prediction("10.0.0.1", 5000, 1),
        prediction("224.0.0.1", 5000, 1),
        prediction("::1", 5000, 1),
        prediction("198.51.100.1", i64::MAX, 1),
        prediction("198.51.100.1", 65534, 1),
        prediction("198.51.100.1", 1000, 1),
        prediction("198.51.100.1", 5000, 17),
        prediction("10.0.0.1", 5000, 0),
        prediction("198.51.100.1", 5000, -1),
        prediction("not-an-address", 5000, 0),
    ] {
        assert!(
            unsafe_prediction.target().is_err(),
            "accepted {unsafe_prediction:?}"
        );
    }
    assert_eq!(
        prediction("198.51.100.1", 5000, 2).target().unwrap(),
        "198.51.100.1:5008".parse().unwrap(),
        "four allocations ahead"
    );
    let json: PunchPrediction =
        serde_json::from_str(r#"{"address":"198.51.100.1","port":5000,"step":1}"#).unwrap();
    assert_eq!(json.target().unwrap().port(), 5004);
}

#[tokio::test]
async fn begin_punch_validates_its_inputs_and_owns_one_slot() {
    let engine = engine();
    for (id, secret) in [("", SECRET), ("x", "42"), ("x", &SECRET[..62]), ("x", "zz")] {
        assert!(
            engine.begin_punch(id, None, Vec::new(), secret).is_err(),
            "{id:?} {secret:?}"
        );
    }
    let long = "x".repeat(257);
    assert!(engine.begin_punch(&long, None, Vec::new(), SECRET).is_err());
    let preparation = engine
        .begin_punch("current", None, Vec::new(), SECRET)
        .unwrap();
    assert!(
        engine
            .begin_punch("other", None, Vec::new(), SECRET)
            .is_err(),
        "one slot per engine"
    );
    assert!(
        engine
            .punch("current")
            .is_some_and(|current| current.same(&preparation))
    );
    assert!(engine.punch("other").is_none());
    assert!(
        engine.take_punched_socket("current", None).is_none(),
        "nothing proved yet"
    );
    engine.close_punch("other");
    assert!(
        engine.punch("current").is_some(),
        "another identity retired the slot"
    );
    engine.close_punch("current");
    assert!(engine.punch("current").is_none());
    engine
        .begin_punch("replacement", None, Vec::new(), SECRET)
        .expect("slot reusable");
    engine.close().await.unwrap();
    assert!(
        engine
            .begin_punch("closed", None, Vec::new(), SECRET)
            .is_err()
    );
}

#[tokio::test]
async fn preparation_expires_without_sdp() {
    let engine = engine();
    engine
        .begin_punch("expiring", None, Vec::new(), SECRET)
        .unwrap();
    tokio::time::sleep(PREPARATION_LIFETIME + Duration::from_millis(100)).await;
    assert!(
        engine.punch("expiring").is_none(),
        "preparation outlived its bound"
    );
}

#[tokio::test]
async fn prepare_needs_three_distinct_destinations() {
    let engine = engine();
    let one = piik_stun::listen(&["127.0.0.1:0"]).await.unwrap();
    let url = format!("stun:{}", one.addresses()[0]);
    let preparation = engine
        .begin_punch(
            "few",
            None,
            vec![vec![url.clone(), url.clone(), url]],
            SECRET,
        )
        .unwrap();
    assert!(preparation.prepare().await.is_err());
    assert!(
        preparation.prepare().await.is_err(),
        "a retired step cannot run again"
    );
    one.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn stable_observation_retains_its_socket_until_retired() {
    let (stun, servers) = survey_servers().await;
    for scenario in ["connection", "source", "engine", "unreachable"] {
        let engine = engine();
        let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
        let preparation = engine
            .begin_punch("stable", Some(&source), servers.clone(), SECRET)
            .unwrap();
        let prediction = preparation.prepare().await.unwrap();
        assert_eq!(prediction.step, 0, "loopback mappings are stable");
        assert!(
            lock(&preparation.inner.state).socket.is_some(),
            "stable observation retains its socket"
        );
        assert!(
            engine
                .take_punched_socket("stable", Some(&source))
                .is_none(),
            "observation alone authorized adoption"
        );
        match scenario {
            "connection" => engine.close_punch("stable"),
            "source" => source.close().await,
            "engine" => engine.close().await.unwrap(),
            _ => {
                // A tuple nobody answers on.
                let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
                let port = i64::from(silent.local_addr().unwrap().port());
                let target = PunchPrediction {
                    address: "127.0.0.1".into(),
                    port,
                    step: 0,
                };
                assert!(
                    !preparation.run(target).await,
                    "unreachable peer completed preparation"
                );
                engine.close_punch("stable");
            }
        }
        assert!(
            lock(&preparation.inner.state).socket.is_none(),
            "retired preparation kept its socket"
        );
        assert!(engine.punch("stable").is_none());
        engine.close().await.unwrap();
    }
    stun.close().await;
}

#[tokio::test]
async fn punch_rejects_another_generation() {
    let (stun, servers) = survey_servers().await;
    let (left_engine, right_engine) = (engine(), engine());
    let left = left_engine
        .begin_punch("left", None, servers.clone(), SECRET)
        .unwrap();
    let other = "2424242424242424242424242424242424242424242424242424242424242424";
    let right = right_engine
        .begin_punch("right", None, servers, other)
        .unwrap();
    let (left_prediction, right_prediction) = (
        left.prepare().await.unwrap(),
        right.prepare().await.unwrap(),
    );
    let (a, b) = tokio::join!(left.run(right_prediction), right.run(left_prediction));
    assert!(!a && !b, "another generation's packets authorized adoption");
    assert!(left_engine.take_punched_socket("left", None).is_none());
    assert!(right_engine.take_punched_socket("right", None).is_none());
    stun.close().await;
}

#[tokio::test]
async fn socket_array_bounds_resources_on_failure() {
    let bind = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0);
    let silent = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let remote = silent.local_addr().unwrap();
    assert!(
        punch_socket_array(bind, remote, "", None).await.is_err(),
        "invalid secret"
    );
    assert!(
        punch_socket_array(bind, "[::1]:9".parse().unwrap(), SECRET, None)
            .await
            .is_err(),
        "IPv6 peer"
    );
    let started = tokio::time::Instant::now();
    assert!(
        punch_socket_array(bind, remote, SECRET, None)
            .await
            .is_err(),
        "silent peer"
    );
    assert!(
        started.elapsed() < ARRAY_BUDGET + Duration::from_millis(500),
        "budget exceeded"
    );
    // The fanout stays within its shared request budget.
    silent.set_nonblocking(true).unwrap();
    let mut buffer = [0u8; 1500];
    let mut requests = 0;
    while silent.recv_from(&mut buffer).is_ok() {
        requests += 1;
    }
    assert!(
        requests > 0 && requests <= ARRAY_REQUESTS,
        "requests={requests}"
    );
}

#[tokio::test]
async fn punch_adopts_the_proved_tuple_into_media_and_feedback() {
    let (stun, servers) = survey_servers().await;
    let (host, viewer) = (engine(), engine());
    let (callback, requests) = crate::tests::recorder();
    let source = host.new_source("vp8", 1, 1, Some(callback)).await.unwrap();
    let left = host
        .begin_punch("connection", Some(&source), servers.clone(), SECRET)
        .unwrap();
    let right = viewer
        .begin_punch("connection", None, servers, SECRET)
        .unwrap();
    let left_prediction = left.prepare().await.unwrap();
    let right_prediction = right.prepare().await.unwrap();
    // A delayed peer start still proves the tuple through triggered checks.
    let (a, b) = tokio::join!(left.run(right_prediction), async {
        tokio::time::sleep(Duration::from_millis(250)).await;
        right.run(left_prediction).await
    });
    assert!(a && b, "authenticated preparation failed");
    let socket = viewer
        .take_punched_socket("connection", None)
        .expect("inbound winner");
    let edge = host
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "connection".into(),
                punched: true,
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    assert!(
        host.punch("connection").is_none() && viewer.punch("connection").is_none(),
        "adoption kept an owner"
    );
    let offer = edge.create_offer().await.unwrap();
    let (receiver, answer) = viewer
        .new_receiver(ReceiverOptions {
            offer,
            edge_capacity: 1,
            punched_socket: Some(socket),
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    edge.set_answer(&answer, "").await.unwrap();
    // No candidates are exchanged: the punched tuple alone connects.
    crate::tests::eventually("adopted pair connects", async || {
        edge.state() == PcState::Connected && receiver.state() == PcState::Connected
    })
    .await;
    let key = receiver.key;
    let mut pts = Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        crate::tests::write_source_frame(&source, crate::tests::VP8_KEY_8X8, pts)
            .await
            .unwrap();
        pts += Duration::from_secs(1) / 30;
        let received = viewer
            .call(move |app, _, _| {
                app.receivers
                    .get(&key)
                    .map_or(0, |receiver| receiver.video_packets)
            })
            .await
            .unwrap();
        if received > 0 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "adopted socket carried no SRTP video"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(600)).await;
    requests.lock().unwrap().clear();
    receiver.request_key_frame().await;
    crate::tests::eventually("reverse SRTCP feedback", async || {
        !requests.lock().unwrap().is_empty()
    })
    .await;
    host.close().await.unwrap();
    viewer.close().await.unwrap();
    stun.close().await;
}

#[tokio::test]
async fn punched_edge_requires_a_completed_preparation() {
    let engine = engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let edge = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "unprepared".into(),
                punched: true,
                ..EdgeOptions::default()
            },
        )
        .await;
    assert!(edge.is_err(), "edge adopted a missing preparation");
    engine.close().await.unwrap();
}
