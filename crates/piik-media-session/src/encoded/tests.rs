//! Encoded-source and source-clock tests. Time is supplied explicitly, so
//! bitrate windows need no wall-clock sleeps.

use std::time::{Duration, Instant, SystemTime};

use piik_encoded::Frame;
use piik_media_forwarding::{
    Codec, CodecKind, InboundPacket, LayerFormat, SenderReport, Source, SourceOptions, SourceView,
    ntp_from_system_time,
};

use super::*;

/// Shaped like a VP8 keyframe (P bit clear); payloads exercise RTP only.
pub(crate) const VP8_KEY: [u8; 12] = [0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 8, 0, 8, 0, 0, 0];
const PERIOD: Duration = Duration::from_millis(40);

pub(crate) fn vp8_delta() -> Vec<u8> {
    let mut delta = VP8_KEY.to_vec();
    delta[0] |= 1;
    delta
}

pub(crate) fn clock_options(id: &str) -> SourceOptions {
    SourceOptions {
        id: id.into(),
        stream_id: "clock".into(),
        codec: Codec {
            kind: CodecKind::Vp8,
            payload_type: 96,
            fmtp: String::new(),
            channels: 0,
        },
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
    }
}

fn frame(data: &[u8], pts: Duration, recovery: bool) -> Frame {
    Frame {
        data: data.to_vec(),
        pts,
        duration: PERIOD,
        recovery,
    }
}

fn local(anchor: ClockAnchor) -> InputClock {
    InputClock::Local(anchor)
}

#[test]
fn codec_budget_uses_actual_source_framing() {
    let anchor = ClockAnchor::now();
    let mut input = EncodedSource::new(clock_options("framing"), local(anchor)).unwrap();
    assert_eq!(
        input.codec_budget(0, 120_000),
        120_000,
        "cold source invented overhead"
    );
    let period = Duration::from_secs(1) / 30;
    let mut frame_index = 0u32;
    loop {
        let (_, rates) = input.source().layered_bitrate();
        if rates[0][0] > 0 && rates[1][0] > 0 {
            break;
        }
        assert!(frame_index < 400, "measurement window produced no bitrate");
        let pts = period * frame_index;
        input.begin_frame(pts, anchor.system + pts).unwrap();
        for (layer, size) in [(0, 80), (1, 4000)] {
            let mut data = vec![0u8; size];
            data[..VP8_KEY.len()].copy_from_slice(&VP8_KEY);
            input
                .write_frame(layer, &frame(&data, pts, true), None)
                .unwrap();
        }
        frame_index += 1;
    }
    let (_, rates) = input.source().layered_bitrate();
    for (layer, rate) in rates.iter().enumerate().take(2) {
        let stats = input.layer_stats(layer).unwrap();
        assert!(
            stats.rtp_bytes - stats.codec_bytes > u64::from(stats.packets) * 12,
            "codec descriptors were omitted from framing cost"
        );
        let overhead = (rate[0] as f64 * (stats.rtp_bytes - stats.codec_bytes) as f64
            / stats.rtp_bytes as f64)
            .ceil() as i64;
        let budget = input.codec_budget(layer, 120_000);
        assert_eq!(budget, (120_000 - overhead).max(0), "layer {layer}");
        assert!(budget < 120_000);
    }
    let rtp = input.layer_stats(0).unwrap().rtp_bytes;
    input.set_codec_bytes(0, rtp + 1); // Annex-B delimiters can be removed.
    assert_eq!(
        input.codec_budget(0, 120_000),
        120_000,
        "removed codec framing increased the supplied budget"
    );
    input.begin_generation();
    let stats = input.layer_stats(1).unwrap();
    assert_eq!(input.codec_budget(1, 120_000), 120_000);
    assert_eq!(
        (stats.codec_bytes, stats.rtp_bytes),
        (0, 0),
        "stale framing ratio"
    );
}

fn annex_b(nalus: &[&[u8]]) -> Vec<u8> {
    nalus
        .iter()
        .flat_map(|nalu| [&[0u8, 0, 0, 1][..], nalu].concat())
        .collect()
}

#[test]
fn h264_metadata_preserves_correlation() {
    let anchor = ClockAnchor::now();
    let mut options = clock_options("h264-metadata");
    options.codec = Codec {
        kind: CodecKind::H264,
        payload_type: 96,
        fmtp: "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42c01f".into(),
        channels: 0,
    };
    options.formats.truncate(1);
    let mut input = EncodedSource::new(options, local(anchor)).unwrap();
    let sps: &[u8] = &[0x67, 0x42, 0xc0, 0x1f];
    let pps: &[u8] = &[0x68, 0xce, 0x3c, 0x80];
    let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x33];
    let write = |input: &mut EncodedSource, index: u32, data: Vec<u8>| {
        let pts = PERIOD * index;
        input.begin_frame(pts, anchor.system + pts).unwrap();
        input
            .write_frame(0, &frame(&data, pts, index == 0), None)
            .unwrap()
    };
    write(&mut input, 0, annex_b(&[sps, pps, idr]));
    let first = input
        .source()
        .current_sender_report(0)
        .expect("IDR correlation");
    let stats = input.layer_stats(0).unwrap();
    assert!(stats.packets > 0);
    let mut codec_bytes = stats.codec_bytes;
    // SPS/PPS are cached by the payloader; AUD and filler produce no RTP.
    for (index, metadata) in [sps, pps, &[0x09, 0xf0], &[0x0c, 0xff, 0x80]]
        .into_iter()
        .enumerate()
    {
        let written = write(&mut input, index as u32 + 1, annex_b(&[metadata]));
        codec_bytes += annex_b(&[metadata]).len() as u64;
        let now = input.layer_stats(0).unwrap();
        assert!(written.packets.is_empty(), "metadata invented RTP");
        assert_eq!(
            (now.packets, now.octets, now.rtp_bytes),
            (stats.packets, stats.octets, stats.rtp_bytes)
        );
        assert_eq!(
            now.codec_bytes, codec_bytes,
            "accepted metadata lost codec bytes"
        );
        assert_eq!(input.source().current_sender_report(0), Some(first));
    }
    write(&mut input, 5, annex_b(&[idr]));
    let last = input.source().current_sender_report(0).unwrap();
    assert_eq!(
        input.layer_stats(0).unwrap().packets,
        stats.packets + 2,
        "cached SPS/PPS and the next IDR"
    );
    assert_eq!(last.rtp_timestamp.wrapping_sub(first.rtp_timestamp), 18_000);
    let captured = input
        .source()
        .reference_time(0, last.rtp_timestamp)
        .unwrap();
    let expected = anchor.system + PERIOD * 5;
    let error = captured
        .duration_since(expected)
        .or_else(|e| Ok::<_, ()>(e.duration()))
        .unwrap();
    assert!(
        error <= Duration::from_micros(1),
        "IDR correlation used metadata time"
    );
    let packets = input.layer_stats(0).unwrap().packets;
    write(&mut input, 6, annex_b(&[&[0x06, 0x05, 0x00, 0x80]]));
    assert_eq!(
        input.layer_stats(0).unwrap().packets,
        packets + 1,
        "SEI is RTP"
    );
}

fn near(a: SystemTime, b: SystemTime) -> bool {
    let error = a.duration_since(b).unwrap_or_else(|e| e.duration());
    error <= Duration::from_micros(1)
}

#[test]
fn generation_starts_each_layer_with_recovery() {
    let anchor = ClockAnchor::now();
    let mut input = EncodedSource::new(clock_options("generation"), local(anchor)).unwrap();
    let delta = vp8_delta();
    for generation in 0..2u32 {
        if generation != 0 {
            input.begin_generation();
        }
        let at = anchor.system + Duration::from_secs(5) * generation;
        let packets: Vec<_> = (0..2)
            .map(|l| input.layer_stats(l).unwrap().packets)
            .collect();
        let reports: Vec<_> = (0..2)
            .map(|l| input.source().current_sender_report(l))
            .collect();
        let write = |input: &mut EncodedSource, layer: usize, pts: Duration, recovery: bool| {
            let data = if recovery {
                VP8_KEY.to_vec()
            } else {
                delta.clone()
            };
            input
                .write_frame(layer, &frame(&data, pts, recovery), None)
                .unwrap();
        };
        input.begin_frame(Duration::ZERO, at).unwrap();
        for layer in 0..2 {
            write(&mut input, layer, Duration::ZERO, false);
            let stats = input.layer_stats(layer).unwrap();
            assert_eq!(
                stats.packets, packets[layer],
                "delta crossed a fresh generation"
            );
            assert_eq!((stats.rtp_bytes, stats.codec_bytes), (0, 0));
            assert_eq!(input.source().current_sender_report(layer), reports[layer]);
        }
        input.begin_frame(PERIOD, at + PERIOD).unwrap();
        write(&mut input, 0, PERIOD, true);
        write(&mut input, 1, PERIOD, false);
        assert_eq!(input.layer_stats(0).unwrap().packets, packets[0] + 1);
        assert_eq!(
            input.layer_stats(1).unwrap().packets,
            packets[1],
            "one layer's recovery admitted another layer's delta"
        );
        input.begin_frame(PERIOD * 2, at + PERIOD * 2).unwrap();
        write(&mut input, 0, PERIOD * 2, false);
        write(&mut input, 1, PERIOD * 2, true);
        assert_eq!(input.layer_stats(0).unwrap().packets, packets[0] + 2);
        assert_eq!(input.layer_stats(1).unwrap().packets, packets[1] + 1);
        for layer in 0..2 {
            let report = input.source().current_sender_report(layer).unwrap();
            let captured = input
                .source()
                .reference_time(layer, report.rtp_timestamp)
                .unwrap();
            assert!(
                near(captured, at + PERIOD * 2),
                "recovery changed the source clock"
            );
        }
    }
    // Writes outside the current input or without one are rejected.
    assert!(
        input
            .write_frame(0, &frame(&VP8_KEY, PERIOD * 3, true), None)
            .is_err()
    );
    assert!(
        input.begin_frame(PERIOD, anchor.system).is_err(),
        "PTS must increase"
    );
}

/// Each layer's correlation maps its RTP clock to the capture instant, not to
/// when its encoder finished, so a late low output and a current high output
/// project the same capture onto one local clock.
#[test]
fn clock_ignores_encoder_delay() {
    let anchor = ClockAnchor::now();
    let mut input = EncodedSource::new(clock_options("local"), local(anchor)).unwrap();
    let origin = anchor.system;
    input.begin_frame(Duration::ZERO, origin).unwrap();
    let low = input
        .write_frame(0, &frame(&VP8_KEY, Duration::ZERO, true), None)
        .unwrap();
    let half = Duration::from_millis(500);
    input.begin_frame(half, origin + half).unwrap();
    let high = input
        .write_frame(1, &frame(&VP8_KEY, half, true), None)
        .unwrap();
    // The low output was encoded 500 ms late but carries its capture instant.
    assert_eq!(low.packets[0].info.arrival, anchor.local(origin).unwrap());
    assert_eq!(
        high.packets[0].info.arrival,
        anchor.local(origin + half).unwrap()
    );
    let later = Duration::from_millis(533);
    input.begin_frame(later, origin + later).unwrap();
    for layer in 0..2 {
        let written = input
            .write_frame(layer, &frame(&VP8_KEY, later, true), None)
            .unwrap();
        let packet = &written.packets[0].info;
        let source = input.source();
        let captured = source
            .reference_time(layer, packet.ext_timestamp as u32)
            .unwrap();
        assert!(near(captured, origin + later), "layer {layer}");
        assert_eq!(
            source.local_time(layer, captured),
            anchor.local(origin + later)
        );
    }
}

fn wallclock_of(source: &Source, packet: &InboundPacket) -> Instant {
    let layer = packet.layer as usize;
    let captured = source
        .reference_time(layer, packet.ext_timestamp as u32)
        .unwrap();
    source.local_time(layer, captured).unwrap()
}

#[test]
fn derived_source_shares_remote_clock() {
    for offset in [-7200i64, 0, 7200] {
        let start = Instant::now();
        let mut original = Source::new(clock_options("remote")).unwrap();
        original.bind_layer(1, 2).unwrap();
        let mut derived =
            EncodedSource::new(clock_options("derived"), InputClock::Source { layer: 1 }).unwrap();
        let remote_origin = if offset < 0 {
            SystemTime::now() - Duration::from_secs(offset.unsigned_abs())
        } else {
            SystemTime::now() + Duration::from_secs(offset as u64)
        };
        // The second report changes the estimated offset; derived output must
        // follow the original's current correlation.
        for (index, pts) in [Duration::ZERO, Duration::from_millis(540)]
            .into_iter()
            .enumerate()
        {
            let arrival = start + pts + Duration::from_millis(40 * index as u64);
            let timestamp = 900_000 + (pts.as_micros() * 90 / 1000) as u32;
            let mut payload = vec![0x10];
            payload.extend_from_slice(&VP8_KEY);
            let packet = InboundPacket::new(
                CodecKind::Vp8,
                1,
                index as u64 + 1,
                u64::from(timestamp),
                &payload,
                arrival,
            )
            .marker(true)
            .ssrc(2)
            .payload_type(96);
            original.write_rtp(1, &packet).unwrap();
            original
                .sender_report(
                    1,
                    2,
                    SenderReport {
                        rtp_timestamp: timestamp,
                        ntp_time: ntp_from_system_time(remote_origin + pts),
                        packets: index as u32 + 1,
                        octets: (index as u64 + 1) * 20,
                        at: arrival,
                        at_adjusted: arrival,
                    },
                )
                .unwrap();
            let captured = original.reference_time(1, timestamp).unwrap();
            derived.begin_frame(pts, captured).unwrap();
            let written = derived
                .write_frame(0, &frame(&VP8_KEY, pts, true), Some(&original))
                .unwrap();
            let high = wallclock_of(&original, &packet);
            let low = wallclock_of(derived.source(), &written.packets[0].info);
            assert_eq!(low, high, "offset {offset} frame {index}");
            assert_eq!(low, arrival);
        }
        // Without its clock source the derived write is refused.
        derived
            .begin_frame(Duration::from_secs(1), remote_origin)
            .unwrap();
        assert_eq!(
            derived
                .write_frame(0, &frame(&VP8_KEY, Duration::from_secs(1), true), None)
                .err(),
            Some(Error::NoClock)
        );
    }
}
