use super::*;

fn vp8() -> Codec {
    Codec {
        kind: CodecKind::Vp8,
        payload_type: 96,
        fmtp: String::new(),
        channels: 0,
    }
}

pub(crate) fn clock_source(id: &str) -> Source {
    Source::new(SourceOptions {
        id: id.into(),
        stream_id: "clock".into(),
        codec: vp8(),
        formats: vec![
            LayerFormat {
                width: 8,
                height: 8,
                bitrate: 100_000,
            },
            LayerFormat {
                width: 16,
                height: 16,
                bitrate: 1_000_000,
            },
        ],
        max_packets: 0,
    })
    .expect("source")
}

/// VP8 keyframe payload with a plain descriptor.
fn packet(layer: i32, ssrc: u32, seq: u64, timestamp: u64, at: Instant) -> InboundPacket {
    InboundPacket::new(
        CodecKind::Vp8,
        layer,
        seq,
        timestamp,
        &[0x10, 0, 0, 0, 0x9d],
        at,
    )
    .ssrc(ssrc)
    .payload_type(96)
    .marker(true)
}

fn report(timestamp: u32, ntp: SystemTime, at: Instant) -> SenderReport {
    SenderReport {
        rtp_timestamp: timestamp,
        ntp_time: ntp_from_system_time(ntp),
        packets: 1,
        octets: 20,
        at,
        at_adjusted: at,
    }
}

#[test]
fn source_validates_options() {
    let mut options = SourceOptions {
        id: "s".into(),
        stream_id: "screen".into(),
        codec: vp8(),
        formats: vec![LayerFormat::default()],
        max_packets: 0,
    };
    assert_eq!(
        Source::new(options.clone()).map(|s| s.max_packets()),
        Ok(piik_encoded::MAX_PACKET_WINDOW)
    );
    options.max_packets = 10;
    assert_eq!(
        Source::new(options.clone()).err(),
        Some(SourceError::PacketBound)
    );
    options.max_packets = 0;
    options.formats = vec![LayerFormat {
        width: 1,
        height: 0,
        bitrate: 0,
    }];
    assert_eq!(
        Source::new(options.clone()).err(),
        Some(SourceError::LayerFormat)
    );
    options.formats = vec![LayerFormat::default(); 4];
    assert_eq!(
        Source::new(options.clone()).err(),
        Some(SourceError::LayerCount)
    );
    options.formats.clear();
    assert_eq!(
        Source::new(options.clone()).err(),
        Some(SourceError::LayerCount)
    );
    options.codec.kind = CodecKind::Opus;
    assert_eq!(Source::new(options.clone()).map(|s| s.layer_count()), Ok(1));
    options.formats = vec![LayerFormat::default()];
    assert_eq!(
        Source::new(options.clone()).err(),
        Some(SourceError::AudioLayers)
    );
    options.id.clear();
    assert_eq!(Source::new(options).err(), Some(SourceError::Invalid));
}

#[test]
fn source_layer_binding_metadata_and_close() {
    let mut source = Source::new(SourceOptions {
        id: "source".into(),
        stream_id: "screen".into(),
        codec: vp8(),
        formats: vec![
            LayerFormat::default(),
            LayerFormat {
                width: 640,
                height: 360,
                bitrate: 1_200_000,
            },
        ],
        max_packets: 0,
    })
    .expect("source");
    source.set_lowest_layer_rate_controlled(true);
    assert!(
        source.rate_controlled(0) && !source.rate_controlled(1),
        "codec ownership escaped lowest output"
    );
    assert_eq!(
        source.formats()[0],
        LayerFormat::default(),
        "unknown metadata was guessed"
    );
    assert_eq!(source.bind_layer(2, 1000), Err(SourceError::InvalidLayer));
    assert_eq!(source.bind_layer(0, 0), Err(SourceError::InvalidLayer));
    source.bind_layer(0, 1000).expect("bind");
    assert_eq!(source.bind_layer(0, 1001), Err(SourceError::DuplicateLayer));
    assert_eq!(source.bind_layer(1, 1000), Err(SourceError::SharedSsrc));
    source.bind_layer(1, 1001).expect("bind");
    assert_eq!(
        source.update_formats(vec![LayerFormat {
            width: 320,
            height: 180,
            bitrate: 300_000
        }]),
        Err(SourceError::LayerCountChanged)
    );
    source
        .update_formats(vec![
            LayerFormat {
                width: 320,
                height: 180,
                bitrate: 300_000,
            },
            LayerFormat {
                width: 1280,
                height: 720,
                bitrate: 3_000_000,
            },
        ])
        .expect("formats");
    assert_eq!(
        (source.formats()[1].width, source.layer_ceilings()),
        (1280, vec![300_000, 3_000_000])
    );
    assert_eq!(
        source.layer_ssrc(0),
        Some(1000),
        "metadata replaced the binding"
    );
    let now = Instant::now();
    let mut first = packet(0, 1000, 100, 9000, now);
    assert_eq!(
        source.write_rtp(1, &first),
        Err(SourceError::PacketMismatch),
        "RTP crossed its layer"
    );
    first.layer = 0;
    let changes = source.write_rtp(0, &first).expect("write");
    assert!(changes.layers && changes.max_published == Some(0));
    let ntp = SystemTime::now();
    source
        .sender_report(0, 1000, report(9000, ntp, now))
        .expect("report");
    assert_eq!(
        source.sender_report(0, 1001, report(9000, ntp, now)),
        Err(SourceError::ReportMismatch)
    );
    let stored = source.current_sender_report(0).expect("stored");
    assert_eq!(
        (stored.rtp_timestamp, stored.ntp_time),
        (9000, ntp_from_system_time(ntp))
    );
    source.update_dimensions(1, 1920, 1080).expect("dimensions");
    assert_eq!(
        source.formats()[1].bitrate,
        3_000_000,
        "dimensions changed the ceiling"
    );
    assert_eq!(
        source.update_dimensions(2, 1, 1),
        Err(SourceError::Dimensions)
    );
    assert_eq!(
        source.update_dimensions(0, 0, 1),
        Err(SourceError::Dimensions)
    );
    source.close();
    assert_eq!(source.write_rtp(0, &first), Err(SourceError::Closed));
    assert!(
        !source.rate_controlled(0),
        "closed source retained codec ownership"
    );
    assert_eq!(source.bind_layer(0, 2000), Err(SourceError::Closed));
    assert_eq!(
        source.update_formats(vec![LayerFormat::default(); 2]),
        Err(SourceError::Closed)
    );
    assert_eq!(source.layered_bitrate().0, Vec::<i32>::new());
}

#[test]
fn stream_tracking_measures_layers_and_stops_dry_layers() {
    let mut source = clock_source("tracking");
    source.bind_layer(0, 1).expect("bind");
    source.bind_layer(1, 2).expect("bind");
    let start = Instant::now();
    let frame = Duration::from_secs(1) / 30;
    let mut changes = SourceChanges::default();
    // Layer 0: 1000-byte T0 and 500-byte T1 frames; layer 1: 4000-byte T0.
    for index in 0..150u32 {
        let at = start + frame * index;
        let mut low = packet(0, 1, u64::from(index), u64::from(index) * 3000, at).size(1000);
        low.temporal = (index % 2) as i32;
        if low.temporal == 1 {
            low.size = 500;
        }
        changes.merge(source.write_rtp(0, &low).expect("low"));
        changes.merge(
            source
                .write_rtp(1, &packet(1, 2, u64::from(index), 0, at).size(4000))
                .expect("high"),
        );
    }
    assert!(
        changes.bitrates
            && changes.max_published == Some(1)
            && changes.max_temporal_seen == Some(1)
    );
    let (available, rates) = source.layered_bitrate();
    assert_eq!(available, vec![0, 1]);
    // 15 fps of each temporal layer over one window.
    let expected_t0 = 15.0 * 1000.0 * 8.0;
    assert!(
        (rates[0][0] as f64 - expected_t0).abs() < expected_t0 * 0.05,
        "{rates:?}"
    );
    assert!(
        (rates[0][1] as f64 - expected_t0 * 1.5).abs() < expected_t0 * 0.05,
        "{rates:?}"
    );
    assert_eq!(rates[0][2], 0, "unseen temporal layer reported a rate");
    assert!((rates[1][0] as f64 - 30.0 * 4000.0 * 8.0).abs() < 30.0 * 4000.0 * 8.0 * 0.05);
    // Padding neither starts nor feeds a layer.
    let quiet = start + frame * 150;
    let padding = InboundPacket::new(CodecKind::Vp8, 1, 200, 0, &[], quiet)
        .ssrc(2)
        .payload_type(96);
    source.write_rtp(1, &padding).expect("padding");
    assert!(source.poll_deadline().is_some());
    // Only the low layer continues; the high layer stops after the timeout.
    for index in 150..(150 + 6 * 30u32) {
        let at = start + frame * index;
        source
            .write_rtp(0, &packet(0, 1, u64::from(index), 0, at))
            .expect("low");
    }
    let changes = source.poll(start + frame * (150 + 6 * 30));
    assert_eq!(changes.max_published, None, "low layer stays published");
    let (available, rates) = source.layered_bitrate();
    assert_eq!(available, vec![0]);
    assert_eq!(rates[1], [0; MAX_TEMPORAL_LAYERS]);
    assert_eq!(source.max_published_layer(), 0);
    let stopped = source.poll(start + frame * (150 + 6 * 30) + STREAM_STOP_TIMEOUT);
    assert!(stopped.layers && stopped.max_published == Some(-1));
}

#[test]
fn ntp_conversion_round_trips() {
    let time = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
    let back = system_time_from_ntp(ntp_from_system_time(time));
    let error = back
        .duration_since(time)
        .unwrap_or_else(|error| error.duration());
    assert!(error <= Duration::from_nanos(1), "{error:?}");
}

// A derived layer copies the original's current remote correlation, and each
// layer keeps its own RTP clock mapping to the same capture time.
#[test]
fn derived_source_shares_remote_clock() {
    for offset_secs in [-7200i64, 0, 7200] {
        let mut original = clock_source("remote");
        original.bind_layer(1, 2).expect("bind");
        let mut derived = clock_source("derived");
        derived.bind_layer(0, 1).expect("bind");
        let local = Instant::now();
        let shift = Duration::from_secs(offset_secs.unsigned_abs());
        let remote_origin = if offset_secs >= 0 {
            SystemTime::now() + shift
        } else {
            SystemTime::now() - shift
        };
        for (index, pts_ms) in [0u64, 540].into_iter().enumerate() {
            let pts = Duration::from_millis(pts_ms);
            let timestamp = (900_000 + pts_ms * 90) as u32;
            let arrival = local + pts + Duration::from_millis(40 * index as u64);
            original
                .write_rtp(
                    1,
                    &packet(1, 2, index as u64 + 1, u64::from(timestamp), arrival),
                )
                .expect("write");
            // The second report carries a changed delay estimate.
            let estimate = Duration::from_millis(10 + 30 * index as u64);
            let mut received = report(timestamp, remote_origin + pts, arrival);
            received.at_adjusted = arrival - estimate;
            original.sender_report(1, 2, received).expect("report");
            let captured = original.reference_time(1, timestamp).expect("reference");
            let skew = captured
                .duration_since(remote_origin + pts)
                .unwrap_or_else(|e| e.duration());
            assert!(
                skew < Duration::from_micros(1),
                "reference changed the correlation"
            );
            // An encoder derived from this frame finished 25 ms later; its
            // correlation uses the original's projection, not completion time.
            let projected = original.local_time(1, captured).expect("local");
            derived
                .set_correlation(
                    0,
                    SenderReport {
                        rtp_timestamp: 77_000 + index as u32 * 48_600,
                        ntp_time: ntp_from_system_time(captured),
                        packets: index as u32 + 1,
                        octets: 0,
                        at: projected,
                        at_adjusted: projected + Duration::from_millis(25),
                    },
                )
                .expect("correlation");
            for at in [captured, captured + Duration::from_millis(33)] {
                let low = derived.local_time(0, at).expect("derived local");
                let high = original.local_time(1, at).expect("original local");
                // NTP fixed point and platform clock resolution bound agreement.
                assert!(
                    low.max(high) - low.min(high) < Duration::from_micros(1),
                    "frame {index}"
                );
            }
        }
        // An internal copy shares the delay-adjusted clock and owns no estimate.
        let mut copy = clock_source("copy");
        copy.bind_layer(1, 9).expect("bind");
        assert_eq!(
            copy.copy_correlation(0, &original),
            Err(SourceError::InvalidLayer)
        );
        copy.copy_correlation(1, &original).expect("copy");
        let base = original.current_sender_report(1).expect("report");
        let copied = copy.current_sender_report(1).expect("copied");
        assert_eq!(
            (copied.at, copied.at_adjusted),
            (base.at_adjusted, base.at_adjusted)
        );
        // A later received report on the copy keeps its known input clock.
        copy.sender_report(
            1,
            9,
            SenderReport {
                at_adjusted: base.at,
                ..copied
            },
        )
        .expect("report");
        let after = copy.current_sender_report(1).expect("after");
        assert_eq!(after.at_adjusted, after.at);
    }
}

#[test]
fn reference_time_uses_nearest_wrapped_delta() {
    let mut source = clock_source("wrap");
    source.bind_layer(0, 1).expect("bind");
    let ntp = SystemTime::now();
    source
        .sender_report(0, 1, report(u32::MAX - 899, ntp, Instant::now()))
        .expect("report");
    // 900 ticks after the report crosses the 32-bit wrap: +10 ms.
    let after = source.reference_time(0, 0).expect("after");
    let delta = after.duration_since(ntp).expect("forward");
    assert!(
        delta.abs_diff(Duration::from_millis(10)) < Duration::from_micros(1),
        "{delta:?}"
    );
    let before = source.reference_time(0, u32::MAX - 1799).expect("before");
    let delta = ntp.duration_since(before).expect("backward");
    assert!(
        delta.abs_diff(Duration::from_millis(10)) < Duration::from_micros(1),
        "{delta:?}"
    );
    let missing = source.current_sender_report(1);
    assert!(missing.is_none() && source.reference_time(1, 0).is_none());
}
