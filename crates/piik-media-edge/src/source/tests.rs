// Engine options and encoded output groups.

use std::net::UdpSocket;
use std::time::Duration;

use crate::tests::{Peer, VP8_KEY_8X8, frame, loopback_engine, write_source_frame};
use crate::{CandidateType, EdgeEvent, Engine, EngineOptions, Error, PcState};

#[tokio::test]
async fn engine_media_address_families() {
    // The media socket is IPv4-only by design (piik-media-transport
    // `bind_udp4`), so IPv6 binds are rejected.
    assert!(matches!(
        Engine::new(EngineOptions {
            bind_address: "[::1]:0".into(),
            ..EngineOptions::default()
        }),
        Err(Error::Invalid(_))
    ));
    assert!(
        Engine::new(EngineOptions {
            bind_address: "nonsense".into(),
            ..EngineOptions::default()
        })
        .is_err()
    );
    for bind in ["", "127.0.0.1:0"] {
        let engine = Engine::new(EngineOptions {
            bind_address: bind.into(),
            include_loopback: true,
            ..EngineOptions::default()
        })
        .unwrap();
        let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
        let mut peer = Peer::connect(&engine, &source, "family").await;
        let key = peer.edge.key;
        let port = engine
            .call(move |app, _, _| {
                app.edges
                    .get(&key)
                    .map(|edge| edge.signal.link.local_port())
            })
            .await
            .unwrap()
            .unwrap();
        let path = crate::tests::wait_event(
            &mut peer.edge_events.receiver,
            Duration::from_secs(5),
            |event| matches!(event, EdgeEvent::Path { .. }),
        )
        .await;
        assert!(
            matches!(path, Some(EdgeEvent::Path { pair, .. }) if pair.local == CandidateType::Host)
        );
        write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(1))
            .await
            .unwrap();
        let packets = peer.wait_packets(1).await;
        assert!(
            !packets[0].1.is_empty(),
            "direct ICE/DTLS transport did not deliver"
        );
        engine.close().await.unwrap();
        assert_eq!(
            peer.edge.state(),
            PcState::Closed,
            "engine shutdown retained its media edge"
        );
        // The listener is released once the shard and socket readers stop.
        crate::tests::eventually("engine released its UDP listener", async || {
            UdpSocket::bind(("127.0.0.1", port)).is_ok()
        })
        .await;
        peer.close().await;
        assert!(
            engine.new_source("vp8", 1, 1, None).await.is_err(),
            "closed engine admitted a source"
        );
    }
}

#[tokio::test]
async fn closed_engine_rejects_new_owners() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    engine.close().await.unwrap();
    engine.close().await.unwrap();
    assert!(engine.is_closed());
    assert!(engine.new_audio_source(1, 128_000).await.is_err());
    assert!(
        engine
            .new_edge(
                &source,
                crate::EdgeOptions {
                    connection_id: "late".into(),
                    ..Default::default()
                }
            )
            .await
            .is_err()
    );
    assert!(source.is_closed().await);
}

/// 96 real VP8 outputs over two unchanged connections. The bounded fixture is
/// produced by the native codec gate and is not in testdata.
#[tokio::test]
#[ignore = "requires PIIK_ENCODED_FIXTURE (the bounded 96-row VP8 JSONL from the native codec gate); not in testdata"]
async fn encoded_group_vp8_fixture() {
    let path = std::env::var("PIIK_ENCODED_FIXTURE").expect("set PIIK_ENCODED_FIXTURE");
    let text = std::fs::read_to_string(path).unwrap();
    struct Row {
        index: u32,
        layer: usize,
        pts: Duration,
        recovery: bool,
        data: Vec<u8>,
    }
    // Rows are JSON objects: {"FrameIndex","Layer","Width","Height","PTS","Recovery","DataHex"}.
    let field = |line: &str, name: &str| -> String {
        let start = line.find(&format!("\"{name}\":")).expect(name) + name.len() + 3;
        let rest = &line[start..];
        let end = rest.find([',', '}']).unwrap();
        rest[..end].trim_matches('"').to_owned()
    };
    let rows: Vec<Row> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Row {
            index: field(line, "FrameIndex").parse().unwrap(),
            layer: field(line, "Layer").parse().unwrap(),
            pts: Duration::from_nanos(field(line, "PTS").parse().unwrap()),
            recovery: field(line, "Recovery") == "true",
            data: hex::decode(field(line, "DataHex")).unwrap(),
        })
        .collect();
    assert_eq!(rows.len(), 96, "fixture must contain 96 access units");
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 3, None).await.unwrap();
    for (layer, (width, height)) in [(160, 90), (320, 180), (640, 360)].into_iter().enumerate() {
        source.set_format(layer, width, height).await.unwrap();
    }
    source
        .configure_outputs(vec![90_000, 300_000, 1_200_000])
        .await
        .unwrap();
    let peers = [
        Peer::connect(&engine, &source, "group-a").await,
        Peer::connect(&engine, &source, "group-b").await,
    ];
    for warm in 0..45u32 {
        let pts = Duration::from_secs(1) / 30 * (warm + 1);
        source.begin_frame(pts).await.unwrap();
        for row in &rows[..3] {
            source
                .write_video(row.layer, frame(&row.data, pts, true))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_secs(1) / 30).await;
    }
    let baseline = [
        peers[0].received().await.len(),
        peers[1].received().await.len(),
    ];
    source.begin_generation().await;
    let targets = [[2usize, 2], [2, 0], [0, 0], [1, 1], [2, 2]];
    let mut last = None;
    for row in &rows {
        let wanted = targets[row.index as usize / 8];
        if last != Some(row.index) {
            for (peer, layer) in peers.iter().zip(wanted) {
                peer.edge.set_target_layer(layer).await.unwrap();
            }
            let plan = source.begin_frame(row.pts).await.unwrap();
            assert!(
                plan.active[wanted[0]] && plan.active[wanted[1]],
                "{plan:?} for {wanted:?}"
            );
            last = Some(row.index);
        }
        source
            .write_video(
                row.layer,
                piik_encoded::Frame {
                    data: row.data.clone(),
                    pts: row.pts,
                    duration: Duration::from_secs(1) / 30,
                    recovery: row.recovery,
                },
            )
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (child, peer) in peers.iter().enumerate() {
        let received = peer.received().await;
        let frames = received[baseline[child]..]
            .iter()
            .filter(|(_, payload)| !payload.is_empty())
            .map(|(timestamp, _)| *timestamp)
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        assert_eq!(frames, 40, "child {child} selected frames");
    }
    engine.close().await.unwrap();
}
