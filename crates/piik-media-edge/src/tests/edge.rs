// Edges, local bridges and quality samples.

use std::time::SystemTime;

use super::*;
use crate::quality::selected_nat_traversal_path;
use crate::signal::MAX_PENDING_CANDIDATES;

#[tokio::test]
async fn one_encoded_source_feeds_two_independent_edges() {
    let engine = loopback_engine();
    let (callback, requests) = recorder();
    let source = engine
        .new_source("vp8", 2, 1, Some(callback))
        .await
        .unwrap();
    let a = Peer::connect(&engine, &source, "edge-a").await;
    let b = Peer::connect(&engine, &source, "edge-b").await;
    let (key_a, key_b) = (a.edge.key, b.edge.key);
    let (port_a, port_b) = engine
        .call(move |app, _, _| {
            let port = |key| {
                app.edges
                    .get(&key)
                    .map(|edge| edge.signal.link.local_port())
            };
            (port(key_a), port(key_b))
        })
        .await
        .unwrap();
    assert!(
        port_a.is_some() && port_a != port_b,
        "media edges did not keep independent sockets"
    );
    let third = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "edge-c".into(),
                ..EdgeOptions::default()
            },
        )
        .await;
    assert!(
        matches!(third, Err(Error::SourceCapacity)),
        "third edge: {third:?}"
    );
    eventually("connected edge recovery request", async || {
        !requests.lock().unwrap().is_empty()
    })
    .await;

    write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(1))
        .await
        .unwrap();
    let first = a.wait_packets(1).await;
    b.wait_packets(1).await;
    // The VP8 payload descriptor starts the partition; the frame is intact.
    let descriptor = piik_media_forwarding::Vp8Descriptor::parse(&first[0].1).unwrap();
    assert!(descriptor.start && descriptor.partition == 0);
    assert_eq!(&first[0].1[descriptor.header_len..], VP8_KEY_8X8);

    // A subscriber PLI reaches the shared source.
    requests.lock().unwrap().clear();
    tokio::time::sleep(Duration::from_millis(600)).await;
    a.receiver.request_key_frame().await;
    eventually("PLI reached the shared source", async || {
        !requests.lock().unwrap().is_empty()
    })
    .await;

    a.edge.close().await;
    let replacement = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "edge-c".into(),
                ..EdgeOptions::default()
            },
        )
        .await
        .expect("released source slot was not reusable");
    replacement.close().await;
    a.close().await;
    b.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn remote_candidates_are_bounded_until_the_answer() {
    let engine = loopback_engine();
    let source = engine.new_source("h264", 1, 1, None).await.unwrap();
    let edge = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "edge".into(),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let pending = async || {
        engine
            .call(|app, _, _| {
                app.edges
                    .values()
                    .next()
                    .map_or(0, |edge| edge.signal.pending_candidates())
            })
            .await
            .unwrap()
    };
    edge.add_remote_candidate(Some(IceCandidateInit {
        candidate: "candidate:garbage".into(),
        ..IceCandidateInit::default()
    }))
    .await
    .expect("malformed candidate escaped the edge boundary");
    assert_eq!(pending().await, 0, "malformed candidate was queued");
    let host = IceCandidateInit {
        candidate: "candidate:1 1 udp 1 127.0.0.1 9 typ host".into(),
        ..IceCandidateInit::default()
    };
    for _ in 0..=MAX_PENDING_CANDIDATES {
        edge.add_remote_candidate(Some(host.clone())).await.unwrap();
    }
    assert_eq!(
        pending().await,
        MAX_PENDING_CANDIDATES,
        "queue grew beyond its bound"
    );
    engine.close().await.unwrap();
}

#[test]
fn selected_nat_traversal_path_keeps_only_anonymous_provenance() {
    assert_eq!(
        selected_nat_traversal_path(&["ordinary", "sp3"]),
        "predicted"
    );
    assert_eq!(
        selected_nat_traversal_path(&["ordinary", "sm1"]),
        "predicted"
    );
    // Only `s` + `p`/`m` matches: a mapped `mp` foundation alone is ordinary.
    assert_eq!(
        selected_nat_traversal_path(&["ordinary", "mp1"]),
        "ordinary"
    );
    assert_eq!(
        selected_nat_traversal_path(&["ordinary", "remote"]),
        "ordinary"
    );
    assert_eq!(selected_nat_traversal_path(&["", ""]), "unknown");
}

#[tokio::test]
async fn repeated_answer_is_idempotent() {
    let engine = loopback_engine();
    let viewer = loopback_engine();
    let source = engine.new_source("h264", 1, 1, None).await.unwrap();
    let edge = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "repeat".into(),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let offer = edge.create_offer().await.unwrap();
    let (_receiver, answer) = viewer
        .new_receiver(ReceiverOptions {
            offer,
            edge_capacity: 1,
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    edge.set_answer(&answer, "").await.unwrap();
    edge.set_answer(&answer, "")
        .await
        .expect("repeated answer was not idempotent");
    assert!(
        edge.set_answer(&answer, "unexpected").await.is_err(),
        "trickled edge accepted an offer id"
    );
    viewer.close().await.unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn capture_timestamps_drive_the_rtp_clock() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let peer = Peer::connect(&engine, &source, "timed-edge").await;
    let start = Duration::from_secs(10);
    let interval = Duration::from_secs(1) / 30;
    write_source_frame(&source, VP8_KEY_8X8, start)
        .await
        .unwrap();
    let first = peer.wait_packets(1).await[0].0;
    write_source_frame(&source, VP8_KEY_8X8, start + interval)
        .await
        .unwrap();
    let second = peer.wait_packets(2).await[1].0;
    let delta = second.wrapping_sub(first);
    assert!(
        (2_999..=3_001).contains(&delta),
        "steady timestamp delta = {delta}"
    );
    write_source_frame(&source, VP8_KEY_8X8, start + Duration::from_secs(5))
        .await
        .unwrap();
    let third = peer.wait_packets(3).await[2].0;
    let delta = third.wrapping_sub(second);
    assert!(
        (446_999..=447_001).contains(&delta),
        "sparse timestamp delta = {delta}"
    );
    peer.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn new_capture_generation_keeps_the_rtp_clock_continuous() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let peer = Peer::connect(&engine, &source, "generation-edge").await;
    let origin = SystemTime::now();
    let pts = Duration::from_secs(50);
    source.begin_frame_at(pts, origin).await.unwrap();
    source
        .write_video(0, frame(VP8_KEY_8X8, pts, true))
        .await
        .unwrap();
    let first = peer.wait_packets(1).await[0].0;
    source.begin_generation().await;
    let pts = Duration::from_millis(100);
    source
        .begin_frame_at(pts, origin + Duration::from_millis(200))
        .await
        .unwrap();
    source
        .write_video(0, frame(VP8_KEY_8X8, pts, true))
        .await
        .unwrap();
    let second = peer.wait_packets(2).await[1].0;
    let delta = second.wrapping_sub(first);
    assert!(
        (17_999..=18_001).contains(&delta),
        "generation timestamp delta = {delta}"
    );
    peer.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn one_local_bridge_does_not_consume_route_capacity() {
    let engine = loopback_engine();
    let source = engine.new_source("h264", 1, 1, None).await.unwrap();
    let audio = engine.new_audio_source(1, 128_000).await.unwrap();
    let options = |id: &str, local: bool, audio: Option<AudioSource>| EdgeOptions {
        connection_id: id.into(),
        local,
        audio,
        ..EdgeOptions::default()
    };
    let local = engine
        .new_edge(&source, options("local-preview", true, Some(audio.clone())))
        .await
        .unwrap();
    let route = engine
        .new_edge(&source, options("route-edge", false, Some(audio.clone())))
        .await
        .unwrap();
    let second_local = engine
        .new_edge(&source, options("second-local", true, None))
        .await;
    assert!(
        matches!(second_local, Err(Error::SourceCapacity)),
        "{second_local:?}"
    );
    let second_route = engine
        .new_edge(&source, options("second-route", false, None))
        .await;
    assert!(
        matches!(second_route, Err(Error::SourceCapacity)),
        "{second_route:?}"
    );
    local.close().await;
    route.close().await;
    let replacement = engine
        .new_edge(
            &source,
            options("replacement-local", true, Some(audio.clone())),
        )
        .await
        .expect("released local slot was not reusable");
    replacement.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn bandwidth_observer_produces_a_healthy_quality_sample() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let peer = Peer::connect(&engine, &source, "quality-edge").await;
    source.set_format(0, 1280, 720).await.unwrap();
    let started = SystemTime::now();
    assert!(
        peer.edge.quality_sample(started).await.is_none(),
        "first quality sample did not establish a baseline"
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut pts = Duration::from_secs(1);
    let mut sampled = started;
    while tokio::time::Instant::now() < deadline {
        write_source_frame(&source, VP8_KEY_8X8, pts).await.unwrap();
        pts += Duration::from_secs(1) / 30;
        tokio::time::sleep(Duration::from_secs(1) / 30).await;
        let now = SystemTime::now();
        if now.duration_since(sampled).unwrap() < Duration::from_millis(1200) {
            continue;
        }
        sampled = now;
        let Some(sample) = peer.edge.quality_sample(now).await else {
            continue;
        };
        if sample.state == "unknown" {
            continue;
        }
        assert_eq!(sample.state, "healthy", "{sample:?}");
        assert_eq!(sample.reason.as_deref(), Some("none"));
        assert!(sample.interval_frames_sent > 0 && sample.available_outgoing_kbps > 0.0);
        assert!(!sample.rtp_stats_id.is_empty() && !sample.track_identifier.is_empty());
        assert_eq!((sample.width, sample.height), (1280, 720));
        peer.close().await;
        engine.close().await.unwrap();
        return;
    }
    panic!("transport feedback did not produce a bandwidth estimate");
}

#[tokio::test]
async fn quality_sample_rejects_windows_outside_its_bound() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let edge = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "window".into(),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let start = SystemTime::now();
    assert!(edge.quality_sample(start).await.is_none());
    assert!(
        edge.quality_sample(start + Duration::from_millis(500))
            .await
            .is_none(),
        "short window"
    );
    let sample = edge
        .quality_sample(start + Duration::from_millis(1500))
        .await
        .expect("bounded window");
    assert_eq!(sample.state, "unknown", "an unconnected edge is unknown");
    assert!(sample.reason.is_none());
    assert!(
        edge.quality_sample(start + Duration::from_secs(10))
            .await
            .is_none(),
        "long window"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn audio_uses_the_same_connection_and_capacity_as_video() {
    let engine = loopback_engine();
    let video = engine.new_source("h264", 1, 1, None).await.unwrap();
    let audio = engine.new_audio_source(1, 128_000).await.unwrap();
    let edge = engine
        .new_edge(
            &video,
            EdgeOptions {
                connection_id: "audio-edge".into(),
                audio: Some(audio.clone()),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let video_only = engine
        .new_edge(
            &video,
            EdgeOptions {
                connection_id: "video-only".into(),
                ..EdgeOptions::default()
            },
        )
        .await;
    assert!(
        matches!(video_only, Err(Error::SourceCapacity)),
        "video capacity was not shared"
    );
    let audio_only = engine
        .new_edge(
            &video,
            EdgeOptions {
                connection_id: "audio-only".into(),
                audio: Some(audio.clone()),
                ..EdgeOptions::default()
            },
        )
        .await;
    assert!(
        matches!(audio_only, Err(Error::SourceCapacity)),
        "audio capacity was not shared"
    );
    let offer = edge.create_offer().await.unwrap();
    assert!(
        offer.contains("m=audio") && offer.to_ascii_lowercase().contains("opus/48000"),
        "{offer}"
    );
    assert!(
        offer.contains("transport-cc"),
        "offer did not negotiate transport feedback"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn audio_source_delivers_opus_on_the_video_connection() {
    let engine = loopback_engine();
    let video = engine.new_source("vp8", 2, 1, None).await.unwrap();
    let audio = engine.new_audio_source(2, 128_000).await.unwrap();
    let peer = Peer::connect_with(&engine, &video, "audio-edge", Some(audio.clone())).await;
    assert!(peer.receiver.has_audio());

    // An answer below the shared source ceiling is rejected before it applies.
    let limited = engine
        .new_edge(
            &video,
            EdgeOptions {
                connection_id: "limited".into(),
                audio: Some(audio.clone()),
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    let offer = limited.create_offer().await.unwrap();
    let other = loopback_engine();
    let (_receiver, answer) = other
        .new_receiver(ReceiverOptions {
            offer,
            edge_capacity: 1,
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    assert!(
        answer.contains("maxaveragebitrate=510000") && answer.contains("stereo=1"),
        "{answer}"
    );
    let reduced = answer.replace("maxaveragebitrate=510000", "maxaveragebitrate=192000");
    assert!(
        limited.set_answer(&reduced, "").await.is_err(),
        "insufficient receiver limit accepted"
    );
    limited.close().await;
    assert_eq!(
        audio.configured_bitrate(),
        128_000,
        "admission changed the shared setting"
    );

    let audio_bytes = async || {
        let key = audio.key;
        engine
            .call(move |app, _, _| app.audios.get(&key).map_or(0, |state| state.bytes))
            .await
            .unwrap()
    };
    let receiver_audio = peer.receiver.audio_source().unwrap().key;
    let relayed = async || {
        peer.viewer
            .call(move |app, _, _| {
                app.audios
                    .get(&receiver_audio)
                    .map_or(0, |state| state.bytes)
            })
            .await
            .unwrap()
    };
    let pcm = vec![0u8; piik_native_audio::FRAME_BYTES];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while relayed().await == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for Opus RTP"
        );
        audio
            .write_pcm(&pcm, Duration::from_millis(20))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(audio_bytes().await > 0);
    peer.close().await;
    other.close().await.unwrap();
    engine.close().await.unwrap();
}

#[tokio::test]
async fn shutdown_closes_every_sibling_transport() {
    for owner in ["source", "engine"] {
        let engine = loopback_engine();
        let source = engine.new_source("vp8", 2, 1, None).await.unwrap();
        let mut sinks = Vec::new();
        for index in 0..2 {
            let mut events = events();
            engine
                .new_edge(
                    &source,
                    EdgeOptions {
                        connection_id: format!("shutdown-{owner}-{index}"),
                        events: Some(events.sink.clone()),
                        ..EdgeOptions::default()
                    },
                )
                .await
                .unwrap();
            sinks.push(std::mem::replace(
                &mut events.receiver,
                mpsc::unbounded_channel().1,
            ));
        }
        if owner == "source" {
            source.close().await;
        } else {
            engine.close().await.unwrap();
        }
        for receiver in &mut sinks {
            let closed = wait_event(receiver, Duration::from_secs(5), |event| {
                matches!(
                    event,
                    EdgeEvent::State {
                        state: PcState::Closed,
                        ..
                    }
                )
            })
            .await;
            assert!(closed.is_some(), "{owner}: sibling transport did not close");
        }
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn delayed_connection_state_is_not_current() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 2, None).await.unwrap();
    source
        .configure_outputs(vec![150_000, 600_000])
        .await
        .unwrap();
    let peer = Peer::connect(&engine, &source, "delayed-state").await;
    // A connected event queued behind its consumer carries this live watch.
    let current = peer.edge.state_watch();
    assert!(
        current.is(PcState::Connected),
        "live connection was not current"
    );
    peer.edge.close().await;
    assert!(
        !current.is(PcState::Connected),
        "delayed connected event survived closure"
    );
    assert!(
        current.is(PcState::Closed),
        "predicate retained a historical snapshot"
    );
    peer.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn edge_reports_its_selected_path_once_connected() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let mut peer = Peer::connect(&engine, &source, "path").await;
    let path = wait_event(
        &mut peer.edge_events.receiver,
        Duration::from_secs(5),
        |event| matches!(event, EdgeEvent::Path { .. }),
    )
    .await;
    let Some(EdgeEvent::Path { pair, current }) = path else {
        panic!("no selected path");
    };
    assert!(current.is(PcState::Connected));
    assert_eq!(pair.local, CandidateType::Host);
    assert_eq!(pair.remote, CandidateType::Host);
    assert_eq!(pair.nat_traversal_path, "ordinary");
    peer.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn local_restart_retains_media_and_rejects_stale_answers() {
    let engine = loopback_engine();
    let viewer = loopback_engine();
    let source = engine.new_source("vp8", 1, 1, None).await.unwrap();
    let edge = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "local".into(),
                local: true,
                ..EdgeOptions::default()
            },
        )
        .await
        .unwrap();
    assert!(
        edge.create_offer().await.is_err(),
        "local media needs an identified offer"
    );
    assert!(edge.create_local_offer("", false).await.is_err());
    assert!(
        edge.create_local_offer("first", true).await.is_err(),
        "restart before the first answer"
    );
    let offer = edge.create_local_offer("first", false).await.unwrap();
    assert!(
        offer.contains("a=candidate:"),
        "local offer carries its candidates"
    );
    assert!(!offer.contains("a=end-of-candidates"));
    assert!(
        edge.create_local_offer("second", false).await.is_err(),
        "second initial offer"
    );
    let (receiver, answer) = viewer
        .new_receiver(ReceiverOptions {
            offer,
            edge_capacity: 1,
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    assert!(
        edge.set_answer(&answer, "").await.is_err(),
        "unidentified local answer"
    );
    assert!(
        edge.set_answer(&answer, "other").await.is_err(),
        "stale local answer"
    );
    edge.set_answer(&answer, "first").await.unwrap();
    assert!(
        edge.add_remote_candidate(None).await.is_err(),
        "local candidates belong in the SDP answer"
    );
    eventually("local bridge connects", async || {
        edge.state() == PcState::Connected && receiver.state() == PcState::Connected
    })
    .await;
    assert!(
        edge.create_local_offer("first", true).await.is_err(),
        "reused offer id"
    );
    let restart = edge.create_local_offer("restart", true).await.unwrap();
    assert!(
        edge.set_answer(&answer, "first").await.is_err(),
        "stale answer after restart"
    );
    let answer = receiver
        .renegotiate(&restart, Vec::new())
        .await
        .unwrap()
        .expect("same media shape");
    edge.set_answer(&answer, "restart").await.unwrap();
    write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(1))
        .await
        .unwrap();
    viewer.close().await.unwrap();
    engine.close().await.unwrap();
}
