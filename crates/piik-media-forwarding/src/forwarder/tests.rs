use std::time::{Duration, Instant};

use super::*;

const FRAME_TICKS: u64 = 3000;

/// VP8 packet: T descriptor (TID/Y) and optional 15-bit PictureID/TL0PICIDX.
struct Vp8Packet {
    layer: i32,
    seq: u64,
    timestamp: u64,
    tid: u8,
    sync: bool,
    key: bool,
    picture: Option<(u16, u8)>,
    marker: bool,
}

impl Vp8Packet {
    fn new(layer: i32, seq: u64, timestamp: u64) -> Self {
        Self {
            layer,
            seq,
            timestamp,
            tid: 0,
            sync: false,
            key: false,
            picture: None,
            marker: true,
        }
    }

    fn build(&self, at: Instant) -> InboundPacket {
        let mut payload = vec![0x90];
        match self.picture {
            Some((id, tl0)) => payload.extend([0xe0, 0x80 | (id >> 8) as u8, id as u8, tl0]),
            None => payload.push(0x20),
        }
        payload.push(self.tid << 6 | u8::from(self.sync) << 5);
        payload.push(u8::from(!self.key)); // VP8 header P bit.
        InboundPacket::new(
            CodecKind::Vp8,
            self.layer,
            self.seq,
            self.timestamp,
            &payload,
            at,
        )
        .marker(self.marker)
    }
}

fn rates(layers: &[[i64; 3]]) -> Bitrates {
    let mut table = Bitrates::default();
    for (spatial, row) in layers.iter().enumerate() {
        table[spatial][..3].copy_from_slice(row);
    }
    table
}

#[test]
fn vp8_temporal_recovery_requires_decodable_frames() {
    let mut forwarder = Forwarder::new(CodecKind::Vp8);
    forwarder.set_max_spatial_layer(0);
    forwarder.set_max_published_layer(0);
    forwarder.set_max_temporal_layer_seen(2);
    let table = rates(&[[100_000, 200_000, 300_000]]);
    let now = Instant::now();
    for (index, (name, target, tid, want, key, sync)) in [
        ("start at base layer", 0, 0, 0, true, false),
        ("dependent frame cannot raise layer", 2, 1, 0, false, false),
        (
            "sync cannot skip an intermediate layer",
            2,
            2,
            0,
            false,
            true,
        ),
        ("first sync restores middle layer", 2, 1, 1, false, true),
        ("highest dependent frame still waits", 2, 2, 1, false, false),
        ("next sync restores highest layer", 2, 2, 2, false, true),
        ("reduce at frame boundary", 0, 2, 0, false, false),
        (
            "keyframe restores all layers without sync bit",
            2,
            0,
            2,
            true,
            false,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        forwarder.set_max_temporal_layer(target);
        forwarder.allocate_optimal(&[0], &table, false, false);
        let sequence = index as u64 + 1;
        let packet = Vp8Packet {
            tid,
            sync,
            key,
            ..Vp8Packet::new(0, sequence, sequence * FRAME_TICKS)
        };
        forwarder.forward(&packet.build(now));
        assert_eq!(forwarder.current_layer().temporal, want, "{name}");
    }
}

// Pure part of TestForwardingPreservesVP8TemporalLayers: L1T3 input with one
// spatial encoding keeps every temporal layer, even when T0 arrives first.
#[test]
fn forwarding_preserves_vp8_temporal_layers() {
    let mut forwarder = Forwarder::new(CodecKind::Vp8);
    forwarder.set_max_spatial_layer(0);
    forwarder.set_max_published_layer(0);
    forwarder.set_max_temporal_layer_seen(0);
    forwarder.allocate_optimal(&[0], &Bitrates::default(), false, false);
    let now = Instant::now();
    let mut received = [0; 3];
    for frame in 0..40u64 {
        let tid = [0u8, 2, 1, 2][(frame % 4) as usize];
        forwarder.set_max_temporal_layer_seen(i32::from(tid));
        let packet = Vp8Packet {
            tid,
            sync: true,
            key: frame % 20 == 0,
            picture: Some((frame as u16, (frame / 4) as u8)),
            ..Vp8Packet::new(0, frame + 1, (frame + 1) * FRAME_TICKS)
        };
        if forwarder.forward(&packet.build(now)).is_some() {
            received[usize::from(tid)] += 1;
        }
    }
    assert!(
        received.iter().all(|count| *count >= 4),
        "forwarding discarded temporal layers: {received:?}"
    );
}

#[test]
fn simulcast_switch_waits_for_target_keyframe_and_keeps_output_continuous() {
    let mut forwarder = Forwarder::new(CodecKind::Vp8);
    forwarder.set_max_published_layer(2);
    let start = Instant::now();
    let table = rates(&[[100_000; 3], [300_000; 3], [900_000; 3]]);
    forwarder.allocate_optimal(&[0, 1, 2], &table, false, false);
    assert_eq!(forwarder.target_layer().spatial, 2);
    assert_eq!(forwarder.keyframe_request(), Some(2));
    // Inputs: each layer has its own sequence, timestamp and PictureID space.
    let bases = [
        (10_000u64, 1_000_000u64, 100u16),
        (40_000, 7_000_000, 9000),
        (65_000, 50_000, 0x7ff0),
    ];
    let packet = |layer: usize, frame: u64, key: bool| {
        let (seq, ts, picture) = bases[layer];
        let at = start + Duration::from_millis(33 * frame);
        Vp8Packet {
            key,
            picture: Some((picture.wrapping_add(frame as u16) & 0x7fff, frame as u8)),
            ..Vp8Packet::new(layer as i32, seq + frame, ts + frame * FRAME_TICKS)
        }
        .build(at)
    };
    // Delta frames never start a layer.
    assert!(forwarder.forward(&packet(2, 0, false)).is_none());
    // Opportunistic start on a lower keyframe while waiting for the target.
    let first = forwarder
        .forward(&packet(0, 1, true))
        .expect("lower keyframe latches");
    assert!(first.switched && first.ext_seq < 1 << 16);
    assert_eq!(
        forwarder.keyframe_request(),
        Some(2),
        "still waiting for the target layer"
    );
    let mut outputs = vec![first];
    outputs.push(forwarder.forward(&packet(0, 2, false)).expect("low delta"));
    assert!(
        forwarder.forward(&packet(1, 2, false)).is_none(),
        "delta on another layer"
    );
    outputs.push(
        forwarder
            .forward(&packet(2, 3, true))
            .expect("target keyframe"),
    );
    assert!(
        forwarder.forward(&packet(0, 3, false)).is_none(),
        "old layer after the switch"
    );
    outputs.push(forwarder.forward(&packet(2, 4, false)).expect("high delta"));
    assert_eq!(forwarder.current_layer().spatial, 2);
    assert_eq!(forwarder.keyframe_request(), None);
    // Downgrade under a lower subscription limit waits for the low keyframe.
    forwarder.set_max_spatial_layer(0);
    forwarder.allocate_optimal(&[0, 1, 2], &table, false, false);
    assert_eq!(forwarder.target_layer().spatial, 0);
    outputs.push(
        forwarder
            .forward(&packet(2, 5, false))
            .expect("current continues until switch"),
    );
    outputs.push(
        forwarder
            .forward(&packet(1, 6, true))
            .expect("keyframe toward the target"),
    );
    outputs.push(
        forwarder
            .forward(&packet(0, 7, true))
            .expect("target keyframe"),
    );
    for pair in outputs.windows(2) {
        assert_eq!(
            pair[1].ext_seq,
            pair[0].ext_seq + 1,
            "sequence gap across switches: {pair:?}"
        );
        assert!(
            pair[1].ext_timestamp > pair[0].ext_timestamp,
            "timestamp regressed: {pair:?}"
        );
        let ids = (
            pair[0].vp8.and_then(|r| r.picture_id),
            pair[1].vp8.and_then(|r| r.picture_id),
        );
        let (Some(a), Some(b)) = ids else {
            panic!("missing PictureID")
        };
        assert_eq!(b, (a + 1) & 0x7fff, "PictureID discontinuity: {pair:?}");
    }
}
