// Sources, output failure, relay ownership and relay capacity.

use std::time::{Instant, SystemTime};

use piik_media_forwarding::SourceView;
use piik_native_capture::VideoProfile;

use super::*;
use crate::receiver::ReceivedRtp;
use crate::source::{SharedDemand, shared_plan};

fn profile() -> VideoProfile {
    VideoProfile {
        width: 1280,
        height: 720,
        framerate: 30,
        bitrate: 3_000_000,
        preference: "balanced".into(),
    }
}

/// A relay source without a capture process; derivation never starts.
async fn relay_source(engine: &Engine, capacity: usize) -> VideoSource {
    engine
        .new_source_with("vp8", capacity, 2, Some(RelayOptions::default()), None)
        .await
        .unwrap()
}

/// Pending recovery as the callback would receive it, and the frame-plan
/// recovery mask.
async fn recovery(engine: &Engine, source: &VideoSource) -> (Option<Vec<i32>>, u32) {
    let key = source.key;
    engine
        .call(move |app, _, _| {
            let state = app.sources.get_mut(&key).unwrap();
            (state.take_recovery(), state.groups.recovery())
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn recovery_coalesces_targets_without_waking_unrelated_outputs() {
    for all in [false, true] {
        let engine = loopback_engine();
        let (callback, requests) = recorder();
        let source = engine
            .new_source("vp8", 2, 2, Some(callback))
            .await
            .unwrap();
        // Queue several requests within one shard step.
        let key = source.key;
        engine
            .call(move |app, _, _| {
                let state = app.sources.get_mut(&key).unwrap();
                state.request_layer_keyframe(0);
                state.request_layer_keyframe(2);
                state.request_layer_keyframe(0);
                if all {
                    state.request_recovery(None);
                }
            })
            .await
            .unwrap();
        let want = if all { vec![-1] } else { vec![0, 2] };
        assert_eq!(
            *requests.lock().unwrap(),
            vec![want],
            "one coalesced callback"
        );
        assert_eq!(
            recovery(&engine, &source).await.1,
            0,
            "direct capture queued a second recovery"
        );
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn recovery_retains_local_plan_without_direct_capture_callback() {
    for relay in [false, true] {
        let engine = loopback_engine();
        let source = if relay {
            let (callback, _) = recorder();
            engine
                .new_source_with("vp8", 2, 2, Some(RelayOptions::default()), Some(callback))
                .await
                .unwrap()
        } else {
            engine.new_source("vp8", 2, 2, None).await.unwrap()
        };
        let key = source.key;
        let (pending, mask) = engine
            .call(move |app, _, _| {
                let state = app.sources.get_mut(&key).unwrap();
                state.request_layer_keyframe(0);
                state.request_layer_keyframe(2);
                let pending = state.pending_recovery();
                state.request_recovery(None);
                (pending, (state.groups.recovery(), state.pending_recovery()))
            })
            .await
            .unwrap();
        assert_eq!(
            mask.0, 5,
            "local recovery was lost when the callback cannot reach its encoders"
        );
        if relay {
            assert_eq!(
                pending, 5,
                "relay recovery did not also request its upstream input"
            );
            assert_eq!(
                mask.1,
                u32::MAX,
                "upstream wake widened or lost the targets"
            );
        } else {
            assert_eq!(pending, 0);
        }
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn startup_recovery_uses_the_capture_or_relay_owner() {
    // Startup feedback requests recovery: a str0m-owned source has no
    // asynchronous startup PLI, so the first edge connection is the startup
    // feedback. A relay additionally keeps the plan's local target.
    for relay in [false, true] {
        let engine = loopback_engine();
        let (callback, requests) = recorder();
        let source = engine
            .new_source_with(
                "vp8",
                2,
                2,
                relay.then(RelayOptions::default),
                Some(callback),
            )
            .await
            .unwrap();
        source
            .configure_outputs(vec![150_000, 600_000])
            .await
            .unwrap();
        let key = source.key;
        engine
            .call(move |app, _, now| {
                let state = app.sources.get_mut(&key).unwrap();
                state.keyframe(
                    crate::source::ORIGINAL,
                    piik_media_session::UpstreamKeyframe {
                        layer: 1,
                        force: true,
                    },
                    now,
                );
            })
            .await
            .unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            vec![vec![1]],
            "startup recovery identity"
        );
        assert_eq!(
            recovery(&engine, &source).await.1 != 0,
            relay,
            "wrong capture/relay owner"
        );
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn pli_uses_physical_output_identity() {
    let engine = loopback_engine();
    let (callback, requests) = recorder();
    let source = engine
        .new_source("vp8", 2, 2, Some(callback))
        .await
        .unwrap();
    source
        .configure_outputs(vec![300_000, 1_200_000, 300_000])
        .await
        .unwrap();
    let key = source.key;
    let group = engine
        .call(move |app, _, _| {
            app.plan_groups(key, &[]).unwrap();
            app.new_test_group(key, 2)
        })
        .await
        .unwrap();
    for (media, layer, physical) in [
        (crate::source::ORIGINAL, 0, 0),
        (crate::source::ORIGINAL, 1, 1),
        (group, 0, 2),
        (group, 1, 1),
    ] {
        requests.lock().unwrap().clear();
        engine
            .call(move |app, _, now| {
                let state = app.sources.get_mut(&key).unwrap();
                state.keyframe(
                    media,
                    piik_media_session::UpstreamKeyframe { layer, force: true },
                    now,
                );
            })
            .await
            .unwrap();
        assert_eq!(
            *requests.lock().unwrap(),
            vec![vec![physical]],
            "logical {layer} of {media:?}"
        );
        assert_eq!(
            recovery(&engine, &source).await.1,
            0,
            "PLI requested the capture twice"
        );
    }
    engine.close().await.unwrap();
}

#[test]
fn single_rendition_follows_shared_remote_budget() {
    // Over the planner the shard runs at BeginFrame. Committed transport
    // budgets come from str0m GoogCC; this test fixes them directly.
    let ceiling = [1_000_000];
    let plan = |demands: &[SharedDemand]| {
        let plan = shared_plan(&ceiling, 1, demands, |budget| budget);
        (plan.active[0], plan.bitrates[0])
    };
    let preview = SharedDemand {
        required: 1,
        remote: false,
        budget: 3_000_000,
    };
    let child = |budget| SharedDemand {
        required: usize::from(budget > 0),
        remote: true,
        budget,
    };
    assert_eq!(plan(&[]), (false, 1_000_000), "no consumers");
    assert_eq!(plan(&[preview]), (true, 1_000_000), "preview alone");
    assert_eq!(
        plan(&[preview, child(100_000)]),
        (true, 100_000),
        "preview does not mask a weak child"
    );
    assert_eq!(
        plan(&[preview, child(100_000), child(700_000)]),
        (true, 700_000),
        "weak child does not lower a healthy sibling"
    );
    assert_eq!(
        plan(&[preview, child(100_000), child(0)]),
        (true, 100_000),
        "parked sibling"
    );
    assert_eq!(
        plan(&[preview, child(100_000), child(900_000)]),
        (true, 900_000),
        "reactivated"
    );
    let publication = SharedDemand {
        required: 1,
        remote: true,
        budget: 200_000,
    };
    assert_eq!(plan(&[publication]), (true, 200_000), "SFU upload budget");
    let parked = SharedDemand {
        required: 0,
        ..publication
    };
    assert_eq!(plan(&[parked]), (false, 1_000_000), "SFU upload parked");
    let low = shared_plan(&[150_000], 1, &[publication], |budget| budget);
    assert_eq!(low.bitrates, vec![150_000], "new generation ceiling");
    let floor = shared_plan(&ceiling, 1, &[child(500)], |budget| budget);
    assert_eq!(floor.bitrates, vec![1000], "codec floor");
}

#[tokio::test]
async fn single_rendition_plans_from_live_consumers() {
    let engine = Engine::new(EngineOptions {
        bind_address: "127.0.0.1:0".into(),
        include_loopback: true,
        initial_bitrate: 200_000,
        ..EngineOptions::default()
    })
    .unwrap();
    let source = engine.new_source("vp8", 2, 1, None).await.unwrap();
    source.set_format(0, 640, 360).await.unwrap();
    source.configure_outputs(vec![1_000_000]).await.unwrap();
    source.set_lowest_layer_rate_controlled(true).await;
    let plan = source.begin_frame(Duration::ZERO).await.unwrap();
    assert_eq!(
        (plan.active, plan.bitrates),
        (vec![false], vec![1_000_000]),
        "no consumers"
    );
    let peer = Peer::connect(&engine, &source, "child").await;
    let plan = source.begin_frame(Duration::from_millis(33)).await.unwrap();
    assert_eq!(plan.active, vec![true]);
    assert!(plan.bitrates[0] <= 1_000_000 && plan.bitrates[0] >= 1000);
    peer.edge.close().await;
    let plan = source.begin_frame(Duration::from_millis(66)).await.unwrap();
    assert_eq!(
        (plan.active, plan.bitrates),
        (vec![false], vec![1_000_000]),
        "retired child"
    );
    peer.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn frame_survives_retiring_edge() {
    for layers in [1usize, 2] {
        let engine = loopback_engine();
        let source = engine.new_source("vp8", 2, layers, None).await.unwrap();
        source
            .configure_outputs([300_000, 1_200_000][..layers].to_vec())
            .await
            .unwrap();
        let healthy = Peer::connect(&engine, &source, "healthy").await;
        let retiring = Peer::connect(&engine, &source, "retiring").await;
        // Retire one transport between its connected observation and the
        // next input, as a racing close does.
        let key = retiring.edge.key;
        engine
            .call(move |app, conns, _| {
                let edge = app.edges.get_mut(&key).unwrap();
                if let (Some(conn), Some(transport)) =
                    (conns.get_mut(edge.conn), &mut edge.transport)
                {
                    transport.close(conn.rtc());
                }
            })
            .await
            .unwrap();
        for index in 0..3u32 {
            let pts = Duration::from_secs(1) / 30 * index;
            source
                .begin_frame(pts)
                .await
                .expect("edge close stopped source");
            source
                .write_video(layers - 1, frame(VP8_KEY_8X8, pts, true))
                .await
                .unwrap();
        }
        assert_eq!(
            retiring.edge.state(),
            PcState::Closed,
            "retirement did not isolate the failed edge"
        );
        assert_eq!(healthy.edge.state(), PcState::Connected);
        healthy.wait_packets(1).await;
        source.close().await;
        assert!(matches!(
            source.begin_frame(Duration::from_secs(1)).await,
            Err(Error::Closed)
        ));
        healthy.close().await;
        retiring.close().await;
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn output_plan_retires_only_failed_layer_consumers() {
    let engine = loopback_engine();
    let (callback, requests) = recorder();
    let source = engine
        .new_source("vp8", 2, 2, Some(callback))
        .await
        .unwrap();
    source
        .configure_outputs(vec![150_000, 600_000])
        .await
        .unwrap();
    let high = Peer::connect(&engine, &source, "high").await;
    let low = Peer::connect(&engine, &source, "low").await;
    source.begin_frame(Duration::ZERO).await.unwrap();
    requests.lock().unwrap().clear();
    low.edge.set_target_layer(0).await.unwrap();
    assert!(
        !requests.lock().unwrap().is_empty(),
        "quiet source target-change wakeup"
    );
    let plan = source
        .begin_frame(Duration::from_secs(1) / 30)
        .await
        .unwrap();
    assert_eq!(plan.active, vec![true, true], "{plan:?}");
    assert_eq!(plan.bitrates.len(), 2);
    source.mark_layer_unavailable(0).await.unwrap();
    assert_eq!(low.edge.state(), PcState::Closed, "failed-layer consumer");
    assert_eq!(high.edge.state(), PcState::Connected, "healthy sibling");
    assert!(
        high.edge.set_target_layer(0).await.is_err(),
        "failed output accepted a new demand"
    );
    let pts = Duration::from_secs(2) / 30;
    let plan = source.begin_frame(pts).await.unwrap();
    assert_eq!(plan.active, vec![false, true], "{plan:?}");
    assert_eq!(plan.bitrates, vec![0, 600_000]);
    assert!(
        source
            .write_video(0, frame(VP8_KEY_8X8, pts, true))
            .await
            .is_err(),
        "late output revived a failed encoder slot"
    );
    source
        .configure_outputs(vec![150_000, 600_000])
        .await
        .unwrap();
    high.edge
        .set_target_layer(0)
        .await
        .expect("validated output restored the slot");
    high.close().await;
    low.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn format_adaptation_preserves_live_layers() {
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 2, None).await.unwrap();
    source.set_format(0, 640, 360).await.unwrap();
    source.set_format(1, 1280, 720).await.unwrap();
    source
        .configure_outputs(vec![300_000, 1_200_000])
        .await
        .unwrap();
    let key = source.key;
    let period = Duration::from_secs(1) / 25;
    let mut pts = Duration::ZERO;
    let write = async |pts: Duration| {
        // This check owns input explicitly; no consumer requests codec
        // activation through the separate demand planner.
        engine
            .call(move |app, _, _| {
                app.sources
                    .get_mut(&key)
                    .unwrap()
                    .begin_group_frame(pts, SystemTime::now())
            })
            .await
            .unwrap()
            .unwrap();
        for layer in 0..2 {
            source
                .write_video(layer, frame(VP8_KEY_8X8, pts, true))
                .await
                .unwrap();
        }
    };
    let rates = async || {
        engine
            .call(move |app, _, now| {
                let state = app.sources.get_mut(&key).unwrap();
                state.media.source_mut().poll(now);
                let (_, rates) = state.media.source().layered_bitrate();
                (rates[0][0], rates[1][0])
            })
            .await
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        assert!(
            Instant::now() < deadline,
            "live layers produced no measured bitrate: {:?}",
            rates().await
        );
        write(pts).await;
        pts += period;
        tokio::time::sleep(period).await;
        let (low, high) = rates().await;
        if low > 0 && high > 0 {
            break;
        }
    }
    let stats = async || {
        engine
            .call(move |app, _, _| {
                let state = &app.sources[&key];
                (
                    state.media.layer_stats(0).unwrap(),
                    state.format(0),
                    state.format(1),
                )
            })
            .await
            .unwrap()
    };
    for width in [320u32, 320, 640] {
        let (before, _, _) = stats().await;
        source.set_format(0, width, width * 9 / 16).await.unwrap();
        let (unchanged, low, high) = stats().await;
        assert_eq!(
            low,
            (width, width * 9 / 16),
            "adapted dimensions did not reach metadata"
        );
        assert_eq!(high, (1280, 720), "dimension update escaped its layer");
        assert_eq!(
            unchanged, before,
            "dimension update changed the source counters"
        );
        let (low_rate, high_rate) = rates().await;
        assert!(
            low_rate > 0 && high_rate > 0,
            "dimension update reset a live layer"
        );
        write(pts).await;
        pts += period;
        let (after, _, _) = stats().await;
        assert!(after.packets > before.packets && after.octets > before.octets);
    }
    for (layer, width, height) in [(2usize, 320, 180), (0, 0, 180), (0, 320, 0)] {
        assert!(
            source.set_format(layer, width, height).await.is_err(),
            "invalid {layer} {width}x{height}"
        );
    }
    source.close().await;
    assert!(matches!(
        source.set_format(0, 640, 360).await,
        Err(Error::Closed)
    ));
    engine.close().await.unwrap();
}

#[tokio::test]
async fn relay_format_adaptation_preserves_live_layers() {
    let engine = loopback_engine();
    let source = relay_source(&engine, 2).await;
    source.set_relay_profile(profile()).await.unwrap();
    source.set_format(0, 640, 360).await.unwrap();
    source.set_format(1, 1280, 720).await.unwrap();
    let key = source.key;
    // The received original carries a sender report, so derived groups share
    // its clock.
    let mut packet = ReceivedRtp {
        sequence_number: 0,
        ext_seq: 0,
        ext_timestamp: 0,
        timestamp: 0,
        marker: true,
        payload: [&[0x10][..], VP8_KEY_8X8].concat().into(),
        arrival: Instant::now(),
        size: 12 + VP8_KEY_8X8.len() + 1,
    };
    for width in [640u16, 1280] {
        packet.sequence_number += 1;
        packet.ext_seq += 1;
        packet.timestamp += 3600;
        packet.ext_timestamp += 3600;
        packet.marker = false;
        let mut payload = packet.payload.to_vec();
        payload[7..9].copy_from_slice(&width.to_le_bytes());
        payload[9..11].copy_from_slice(&(width * 9 / 16).to_le_bytes());
        packet.payload = payload.into();
        packet.arrival = Instant::now();
        let written = packet.clone();
        let formats = engine
            .call(move |app, conns, now| {
                app.write_received_rtp(conns, key, &written, now).unwrap();
                let state = &app.sources[&key];
                (state.format(0), state.format(1), state.relay_running())
            })
            .await
            .unwrap();
        assert_eq!(
            formats.1,
            (u32::from(width), u32::from(width) * 9 / 16),
            "received dimensions"
        );
        assert_eq!(
            formats.0,
            (640, 360),
            "derived output followed the received input"
        );
        assert!(!formats.2, "dimension metadata alone started a decoder");
    }
    engine.close().await.unwrap();
}

#[tokio::test]
async fn relay_failure_cannot_retire_reconfigured_profile() {
    let engine = loopback_engine();
    let source = relay_source(&engine, 1).await;
    source.set_format(1, 640, 360).await.unwrap();
    source.set_relay_profile(profile()).await.unwrap();
    let low = Peer::connect(&engine, &source, "relay-failure").await;
    low.edge.set_target_layer(0).await.unwrap();
    let key = source.key;
    // An old run failed, then the profile changed and returned to equal
    // settings with a new owner.
    let old = engine
        .call(move |app, _, _| app.install_test_run(key, true))
        .await
        .unwrap();
    let mut changed = profile();
    changed.preference = "maintain-resolution".into();
    source.set_relay_profile(changed).await.unwrap();
    source.set_relay_profile(profile()).await.unwrap();
    engine
        .call(move |app, conns, now| app.relay_test_failure(conns, key, old, now))
        .await
        .unwrap();
    let bitrate = async || {
        engine
            .call(move |app, _, _| app.sources[&key].output_bitrates[0])
            .await
            .unwrap()
    };
    assert!(
        bitrate().await != 0,
        "late failed decoder retired reconfigured output"
    );
    assert_eq!(low.edge.state(), PcState::Connected);
    let current = engine
        .call(move |app, _, _| app.install_test_run(key, true))
        .await
        .unwrap();
    engine
        .call(move |app, conns, now| app.relay_test_failure(conns, key, current, now))
        .await
        .unwrap();
    assert_eq!(
        bitrate().await,
        0,
        "current decoder failure was not retired"
    );
    assert_eq!(low.edge.state(), PcState::Closed);
    low.close().await;
    engine.close().await.unwrap();
}

#[tokio::test]
async fn retired_relay_failure_cannot_disable_later_demand() {
    let engine = loopback_engine();
    let source = relay_source(&engine, 2).await;
    source.set_format(1, 640, 360).await.unwrap();
    source.set_relay_profile(profile()).await.unwrap();
    let high = Peer::connect(&engine, &source, "retiring-relay-high").await;
    let low = Peer::connect(&engine, &source, "retiring-relay-low").await;
    low.edge.set_target_layer(0).await.unwrap();
    let key = source.key;
    let run = engine
        .call(move |app, _, _| app.install_test_run(key, false))
        .await
        .unwrap();
    low.edge.close().await;
    let cancelled = engine
        .call(move |app, _, _| app.relay_test_cancelled(key))
        .await
        .unwrap();
    assert!(
        cancelled,
        "last lower consumer did not retire the derivation normally"
    );
    // A failure already read from the old process arrives after cancellation.
    engine
        .call(move |app, conns, now| app.relay_test_failure(conns, key, run, now))
        .await
        .unwrap();
    let bitrate = engine
        .call(move |app, _, _| app.sources[&key].output_bitrates[0])
        .await
        .unwrap();
    assert_ne!(
        bitrate, 0,
        "normal retirement disabled a reusable output slot"
    );
    let rejoined = Peer::connect(&engine, &source, "retiring-relay-rejoined").await;
    rejoined
        .edge
        .set_target_layer(0)
        .await
        .expect("later lower demand was rejected");
    assert_eq!(rejoined.edge.state(), PcState::Connected);
    assert_eq!(high.edge.state(), PcState::Connected);
    for peer in [high, low, rejoined] {
        peer.close().await;
    }
    engine.close().await.unwrap();
}

#[tokio::test]
async fn volunteer_fanout_shares_bounded_outputs_and_releases_capacity() {
    let engine = loopback_engine();
    let capacity = physical_source_capacity(MAX_RELAY_CHILDREN);
    assert_eq!(capacity, MAX_RELAY_CHILDREN + 1);
    assert_eq!(physical_source_capacity(3), 3);
    let source = engine.new_source("vp8", capacity, 2, None).await.unwrap();
    source
        .configure_outputs(vec![150_000, 600_000])
        .await
        .unwrap();
    let mut edges = Vec::new();
    for index in 0..capacity {
        edges.push(
            engine
                .new_edge(
                    &source,
                    EdgeOptions {
                        connection_id: format!("child-{index}"),
                        ..EdgeOptions::default()
                    },
                )
                .await
                .unwrap(),
        );
    }
    let excess = engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "excess".into(),
                ..EdgeOptions::default()
            },
        )
        .await;
    assert!(matches!(excess, Err(Error::SourceCapacity)));
    let plan = source.begin_frame(Duration::ZERO).await.unwrap();
    assert_eq!(
        plan.active,
        vec![false, false],
        "unconnected edges demand no output"
    );
    edges.pop().unwrap().close().await;
    engine
        .new_edge(
            &source,
            EdgeOptions {
                connection_id: "replacement".into(),
                ..EdgeOptions::default()
            },
        )
        .await
        .expect("released physical capacity");
    assert!(
        engine
            .new_source("vp8", MAX_RELAY_CHILDREN + 2, 1, None)
            .await
            .is_err()
    );
    assert!(engine.new_source("vp8", 1, 4, None).await.is_err());
    assert!(engine.new_source("av1", 1, 1, None).await.is_err());
    engine.close().await.unwrap();
}

#[tokio::test]
async fn sender_report_clock_reaches_active_groups() {
    // The deferred SR clock check: a received correlation is shared with an
    // active derived group without its own delay estimate.
    let engine = loopback_engine();
    let source = relay_source(&engine, 2).await;
    source.set_relay_profile(profile()).await.unwrap();
    source.set_format(1, 1280, 720).await.unwrap();
    let key = source.key;
    let group = engine
        .call(move |app, _, _| {
            app.configure_relay_formats(key).unwrap();
            let group = app.new_test_group(key, 2);
            app.sources
                .get_mut(&key)
                .unwrap()
                .groups
                .group_mut(2)
                .unwrap()
                .active = true;
            group
        })
        .await
        .unwrap();
    let ntp = SystemTime::now();
    let at = Instant::now();
    let (original, derived) = engine
        .call(move |app, _, _| {
            app.sender_report(
                key,
                piik_media_forwarding::SenderReport {
                    rtp_timestamp: 90_000,
                    ntp_time: piik_media_forwarding::ntp_from_system_time(ntp),
                    packets: 1,
                    octets: 100,
                    at,
                    at_adjusted: at,
                },
            )
            .unwrap();
            let state = &app.sources[&key];
            (
                state.media.source().reference_time(1, 90_000 + 9_000),
                state
                    .source_of(group)
                    .unwrap()
                    .reference_time(1, 90_000 + 9_000),
            )
        })
        .await
        .unwrap();
    assert!(original.is_some(), "received correlation was not stored");
    assert_eq!(
        original, derived,
        "derived group did not share the received clock"
    );
    let expected = ntp + Duration::from_millis(100);
    let delta = original
        .unwrap()
        .duration_since(expected)
        .unwrap_or_else(|error| error.duration());
    assert!(
        delta < Duration::from_millis(1),
        "reference time off by {delta:?}"
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn generation_restart_keeps_group_clock_monotonic() {
    // The deferred generation check: a new capture generation's origin
    // starts after the previous input, and a group created later inherits
    // the established clock.
    let engine = loopback_engine();
    let source = engine.new_source("vp8", 2, 2, None).await.unwrap();
    source
        .configure_outputs(vec![150_000, 600_000, 150_000])
        .await
        .unwrap();
    let origin = SystemTime::now();
    source
        .begin_frame_at(Duration::from_secs(5), origin)
        .await
        .unwrap();
    source.begin_generation().await;
    let key = source.key;
    source
        .begin_frame_at(Duration::from_millis(10), origin - Duration::from_secs(1))
        .await
        .unwrap();
    let input = engine
        .call(move |app, _, _| app.sources[&key].input_at)
        .await
        .unwrap()
        .unwrap();
    assert!(
        input > origin,
        "new generation went backwards: {input:?} <= {origin:?}"
    );
    assert!(input < origin + Duration::from_millis(1));
    engine.close().await.unwrap();
}
