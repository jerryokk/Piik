// Relay input reorder and access-unit assembly. Decoder-configuration checks
// of `frame` stay with its future owner; these tests assert the assembled
// access units instead.

use piik_encoded::{H264Payloader, Payloader};

use super::*;

// H.264 2x2 keyframe fixture and a VP8 8x8 keyframe.
const SPS: &[u8] = &[
    0x67, 0x42, 0xc0, 0x1f, 0x0f, 0xd9, 0x1f, 0x88, 0x88, 0x84, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
    0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc9, 0x20,
];
const PPS: &[u8] = &[0x68, 0x87, 0xcb, 0x83, 0xcb, 0x20];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x0a, 0xf2, 0x62, 0x80, 0x00, 0xa7, 0xbe];
const VP8_KEY_8X8: &[u8] = &[
    0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x08, 0x00, 0x08, 0x00, 0x00, 0x47, 0x08, 0x85, 0x85, 0x88,
    0x85, 0x84, 0x88, 0x02, 0x02, 0x00, 0x0c, 0x0d, 0x60, 0x00, 0xfe, 0xff, 0xab, 0x50, 0x80,
];

fn vp8_key_160x90() -> Vec<u8> {
    hex::decode("1000009d012aa0005a00").unwrap()
}

fn stap(nals: &[&[u8]]) -> Vec<u8> {
    let mut payload = vec![0x78];
    for nal in nals {
        payload.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        payload.extend_from_slice(nal);
    }
    payload
}

fn annexb_of(nals: &[&[u8]]) -> Vec<u8> {
    nals.iter().flat_map(|nal| annexb(nal)).collect()
}

/// Feeds packets and counts loss notifications like `onPacketDropped`.
struct Input {
    assembler: Assembler,
    requests: usize,
}

impl Input {
    fn new(codec: Codec) -> Self {
        Self {
            assembler: Assembler::new(codec),
            requests: 0,
        }
    }

    fn push(
        &mut self,
        sequence_number: u16,
        timestamp: u32,
        marker: bool,
        payload: &[u8],
    ) -> Vec<Vec<u8>> {
        let retained = self.assembler.push(RtpPacket {
            sequence_number,
            timestamp,
            marker,
            payload: payload.to_vec(),
        });
        self.requests += self.assembler.take_losses();
        retained
    }

    fn push_packet(&mut self, packet: RtpPacket) {
        self.assembler.push(packet);
        self.requests += self.assembler.take_losses();
    }

    fn pop(&mut self) -> Option<AccessUnit> {
        let unit = self.assembler.pop();
        self.requests += self.assembler.take_losses();
        unit
    }

    fn drain(&mut self) -> Vec<AccessUnit> {
        std::iter::from_fn(|| self.pop()).collect()
    }
}

/// Recovery and dependent payloads per codec.
fn fixture(codec: Codec) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    match codec {
        Codec::Vp8 => {
            let mut key = vec![0x10];
            key.extend(vp8_key_160x90());
            (key, vec![0x10, 1, 0], vec![0x10, 1, 0])
        }
        Codec::H264 => (
            stap(&[SPS, PPS, IDR]),
            vec![0x61, 1, 0],
            vec![0x7c, 0x81, 1],
        ),
    }
}

#[test]
fn emits_a_complete_quiet_frame() {
    // A single complete access unit must not need a subsequent frame or timer.
    let key = vp8_key_160x90();
    let mut input = Input::new(Codec::Vp8);
    let mut payload = vec![0x10];
    payload.extend(&key);
    input.push(10, 0xffff_fff0, true, &payload);
    assert_eq!(
        input.pop(),
        Some(AccessUnit {
            data: key,
            timestamp: 0xffff_fff0
        })
    );
}

#[test]
fn bounds_stalled_assembly_by_media_time() {
    for codec in [Codec::Vp8, Codec::H264] {
        for partial in [false, true] {
            let mut input = Input::new(codec);
            let (key, delta, fragment) = fixture(codec);
            let origin = 0xffff_0000u32; // Exercise media-clock wrap as well.
            input.push(10, origin, true, &key);
            assert!(input.pop().is_some(), "no initial recovery");
            if partial {
                input.push(11, origin + 3000, false, &fragment);
            }
            // Sequence 12 never arrives. At low packet rates the memory-sized
            // reorder window must not postpone keyframe feedback for minutes.
            for frame in 2..=40u32 {
                input.push(
                    11 + frame as u16,
                    origin.wrapping_add(frame * 3000),
                    true,
                    &delta,
                );
                while input.pop().is_some() {
                    assert!(
                        input.requests > 0,
                        "dependent frame escaped before loss notification"
                    );
                }
                assert!(
                    frame >= 30 || input.requests == 0,
                    "{codec:?}/{partial}: discarded within the reorder allowance"
                );
            }
            assert!(
                input.requests > 0,
                "missing media still blocked recovery after a second"
            );
            input.push(100, origin.wrapping_add(123_000), true, &key);
            assert!(input.pop().is_some(), "recovery remained blocked");
        }
    }
}

#[test]
fn keeps_reordering_and_quiet_source() {
    let mut input = Input::new(Codec::Vp8);
    let mut key = vec![0x10];
    key.extend(vp8_key_160x90());
    input.push(10, 0, true, &key);
    assert!(input.pop().is_some());
    input.push(12, 6000, true, &[0x10, 1, 0]);
    assert!(input.pop().is_none(), "sequence gap was ignored");
    input.push(11, 3000, true, &[0x10, 1, 0]);
    for timestamp in [3000, 6000] {
        assert_eq!(input.pop().map(|unit| unit.timestamp), Some(timestamp));
    }
    input.push(13, 30 * VIDEO_CLOCK_RATE, true, &[0x10, 1, 0]);
    assert!(
        input.pop().is_some() && input.requests == 0,
        "quiet input became packet loss"
    );
}

#[test]
fn retired_loss_does_not_request_another_key_frame() {
    for codec in [Codec::Vp8, Codec::H264] {
        let mut input = Input::new(codec);
        let (_, delta, fragment) = fixture(codec);
        let key = match codec {
            Codec::Vp8 => VP8_KEY_8X8.to_vec(),
            Codec::H264 => stap(&[SPS, PPS, IDR]),
        };
        input.push(10, 0, true, &key);
        assert!(input.pop().is_some());
        input.push(11, 3000, false, &fragment);
        input.push(13, 6000, true, &delta); // The previous frame's final packet was lost.
        assert!(input.pop().is_none(), "incomplete frame escaped");
        input.push(14, 9000, true, &key);
        assert_eq!(
            input.requests, 0,
            "retiring old loss requested another keyframe"
        );
        let unit = input
            .pop()
            .expect("replacement keyframe did not recover immediately");
        assert_eq!(unit.timestamp, 9000);
        match codec {
            Codec::Vp8 => assert_eq!(unit.data, VP8_KEY_8X8[1..]),
            Codec::H264 => assert_eq!(unit.data, annexb_of(&[SPS, PPS, IDR])),
        }
        // A lost packet in a subsequent keyframe must still request recovery.
        input.push(15, 12000, false, &key);
        for index in 0..40u32 {
            input.push(17 + index as u16, 15000 + index * 3000, true, &delta);
            input.drain();
        }
        assert!(
            input.requests > 0,
            "{codec:?}: loss in the current keyframe no longer requests recovery"
        );
    }
}

#[test]
fn recovers_at_new_independent_frame() {
    let mut vp8 = vec![0x10];
    vp8.extend(vp8_key_160x90());
    let mut fu_start = vec![0x7c, 0x87];
    fu_start.extend_from_slice(&SPS[1..5]);
    let mut fu_end = vec![0x7c, 0x47];
    fu_end.extend_from_slice(&SPS[5..]);
    for (codec, recovery) in [
        (Codec::Vp8, vec![vp8, vec![0x00, 0x01]]),
        (Codec::H264, vec![SPS.to_vec(), PPS.to_vec(), IDR.to_vec()]),
        (Codec::H264, vec![stap(&[SPS, PPS, IDR])]),
        (
            Codec::H264,
            vec![fu_start, fu_end, PPS.to_vec(), IDR.to_vec()],
        ),
    ] {
        let mut input = Input::new(codec);
        let old = 0xffff_e000u32;
        let fresh = 0x1000u32;
        let last = recovery.len() - 1;
        for (index, payload) in recovery.iter().enumerate() {
            input.push(10 + index as u16, old, index == last, payload);
        }
        assert!(
            input.pop().is_some(),
            "initial complete recovery did not assemble"
        );
        input.drain();
        // A missing sequence and partial frame must not block later
        // independent recovery.
        let partial: &[u8] = if codec == Codec::H264 {
            &[0x7c, 0x81, 0x01]
        } else {
            &[0x10, 0x01, 0x00]
        };
        input.push(30, old + 3000, false, partial);
        assert!(
            input.pop().is_none(),
            "incomplete frame unexpectedly assembled"
        );
        for (index, payload) in recovery.iter().enumerate() {
            input.push(40 + index as u16, fresh, index == last, payload);
            if index == 0 {
                input.push(40, fresh, index == last, payload);
                input.push(10, old, false, payload);
            }
        }
        let units = input.drain();
        assert!(
            !units.is_empty(),
            "{codec:?}: new recovery remained blocked"
        );
        assert!(
            units.iter().all(|unit| unit.timestamp == fresh),
            "old frame was replayed"
        );
        let data = &units[0].data;
        match codec {
            Codec::Vp8 => assert!(data.starts_with(&vp8_key_160x90())),
            Codec::H264 => assert_eq!(*data, annexb_of(&[SPS, PPS, IDR])),
        }
    }
}

#[test]
fn rejects_truncated_h264_before_independent_recovery() {
    for leading_nal in [false, true] {
        let mut input = Input::new(Codec::H264);
        let mut payloader = H264Payloader::new();
        let key = annexb_of(&[SPS, PPS, IDR]);
        let mut sequence = 10u16;
        let mut push = |input: &mut Input, timestamp: u32, marker: bool, payload: &[u8]| {
            input.push(sequence, timestamp, marker, payload);
            sequence = sequence.wrapping_add(1);
        };
        let payloads = payloader.payload(1200, &key);
        for (index, payload) in payloads.iter().enumerate() {
            push(&mut input, 0, index + 1 == payloads.len(), payload);
        }
        let units = input.drain();
        assert_eq!(
            units.len(),
            1,
            "same-timestamp SPS/PPS/IDR was split before its marker"
        );
        assert_eq!(units[0].data, key);

        let mut partial = vec![0, 0, 0, 1];
        if leading_nal {
            partial.extend([0x61, 0x80, 0, 0, 0, 1]);
        }
        partial.push(0x61);
        partial.extend([0x55; 70]);
        let payloads = payloader.payload(16, &partial);
        assert!(
            payloads.len() >= 3 && payloads.last().unwrap()[1] & 0x40 != 0,
            "not a real FU"
        );
        // Chrome can abandon a FU without reserving its unsent tail sequence.
        for payload in &payloads[..payloads.len() - 1] {
            push(&mut input, 3000, false, payload);
            assert!(
                input.pop().is_none(),
                "unmarked FU escaped during incremental input"
            );
        }
        push(&mut input, 6000, true, &[0x61, 0x80]);
        assert!(
            input.drain().iter().all(|unit| unit.timestamp != 3000),
            "truncated AU escaped"
        );
        assert!(
            input.requests > 0,
            "truncated AU did not request independent recovery"
        );

        // An IDR NAL in the same damaged AU is not independent recovery.
        for payload in &payloads[..payloads.len() - 1] {
            push(&mut input, 7000, false, payload);
            assert!(input.pop().is_none());
        }
        push(&mut input, 7000, true, IDR);
        assert!(
            input.pop().is_none(),
            "same-timestamp IDR revived a truncated AU"
        );

        let payloads = payloader.payload(1200, &key);
        for (index, payload) in payloads.iter().enumerate() {
            push(&mut input, 9000, index + 1 == payloads.len(), payload);
        }
        let units = input.drain();
        assert!(
            units.len() == 1 && units[0].timestamp == 9000 && units[0].data == key,
            "fresh IDR did not recover immediately on its RTP time"
        );
        push(&mut input, 12000, false, &[0x61, 0x81]);
        assert!(
            input.pop().is_none(),
            "multi-NAL AU escaped before its marker"
        );
        push(&mut input, 12000, true, &[0x61, 0x82]);
        assert_eq!(
            input.pop(),
            Some(AccessUnit {
                data: vec![0, 0, 0, 1, 0x61, 0x81, 0, 0, 0, 1, 0x61, 0x82],
                timestamp: 12000
            })
        );

        // Drain after every arrival. Padding occupies a sequence between real
        // FU fragments but has no media timestamp.
        let requests_before = input.requests;
        let mut complete_nal = vec![0x61];
        complete_nal.extend([0x55; 70]);
        let payloads = payloader.payload(16, &complete_nal);
        let mut packets = Vec::new();
        for (index, payload) in payloads.iter().enumerate() {
            packets.push(RtpPacket {
                sequence_number: sequence,
                timestamp: 15000,
                marker: index + 1 == payloads.len(),
                payload: payload.clone(),
            });
            sequence = sequence.wrapping_add(1);
            if index == 0 {
                packets.push(RtpPacket {
                    sequence_number: sequence,
                    ..RtpPacket::default()
                });
                sequence = sequence.wrapping_add(1);
            }
        }
        // A middle fragment must wait behind its missing predecessor after
        // earlier packets have already been consumed.
        packets.swap(2, 3);
        let mut assembled = 0;
        for packet in packets {
            input.push_packet(packet);
            for unit in input.drain() {
                assembled += 1;
                assert!(unit.timestamp == 15000 && unit.data == annexb(&complete_nal));
            }
        }
        assert_eq!((assembled, input.requests), (1, requests_before));
    }
}

#[test]
fn retirement_returns_complete_configuration_nals() {
    let mut input = Input::new(Codec::H264);
    assert!(input.push(10, 0, false, SPS).is_empty());
    input.push(11, 0, false, PPS);
    // An abandoned FU must not contaminate the next independent NAL.
    input.push(12, 0, false, &[0x7c, 0x85, 1, 2]);
    let retained = input.push(13, 3000, true, &stap(&[SPS, PPS, IDR]));
    assert_eq!(retained, [annexb(SPS), annexb(PPS)]);
    assert_eq!(input.requests, 0);
    assert_eq!(input.pop().map(|unit| unit.timestamp), Some(3000));
}

#[test]
fn oversized_h264_access_unit_is_discarded() {
    let mut input = Input::new(Codec::H264);
    input.push(0, 0, true, &stap(&[SPS, PPS, IDR]));
    assert!(input.pop().is_some());
    for index in 0..=RELAY_PACKET_LIMIT as u16 {
        input.push(1 + index, 3000, false, &[0x61, 1]);
    }
    input.push(RELAY_PACKET_LIMIT as u16 + 2, 3000, true, &[0x61, 2]);
    assert!(
        input.drain().is_empty(),
        "AU above the packet window was assembled"
    );
    assert_eq!(input.requests, 1, "one discarded AU requests recovery once");
}

#[test]
fn recognizes_key_frames() {
    assert!(is_h264_key_frame(SPS) && !is_h264_key_frame(IDR) && !is_h264_key_frame(&[]));
    assert!(is_h264_key_frame(&stap(&[SPS, PPS])) && !is_h264_key_frame(&stap(&[PPS, IDR])));
    assert!(is_h264_key_frame(&[0x7c, 0x87, 0]) && !is_h264_key_frame(&[0x7c, 0x07, 0]));
    assert!(is_vp8_key_frame(VP8_KEY_8X8));
    assert!(!is_vp8_key_frame(&[0x10, 0x01]) && !is_vp8_key_frame(&[0x00, 0x00]));
    // Extended descriptor with a 15-bit PictureID.
    assert!(is_vp8_key_frame(&[0x90, 0x80, 0x81, 0x02, 0x00]));
    assert_eq!(
        vp8_payload(&[0x90, 0x80, 0x81, 0x02, 0x00, 7]),
        Some(&[0x00, 7][..])
    );
    assert_eq!(vp8_payload(&[0x90, 0x80]), None);
}
