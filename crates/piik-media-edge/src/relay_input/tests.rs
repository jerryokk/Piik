// Relay input decoder configuration and timing (assembly cases live in
// access_unit/tests.rs).

use super::*;
use crate::access_unit::{Assembler, Codec, RtpPacket};

// H.264 2x2 keyframe fixture.
const SPS: &[u8] = &[
    0x67, 0x42, 0xc0, 0x1f, 0x0f, 0xd9, 0x1f, 0x88, 0x88, 0x84, 0x00, 0x00, 0x03, 0x00, 0x04, 0x00,
    0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc9, 0x20,
];
const PPS: &[u8] = &[0x68, 0x87, 0xcb, 0x83, 0xcb, 0x20];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x0a, 0xf2, 0x62, 0x80, 0x00, 0xa7, 0xbe];

fn annexb(nals: &[&[u8]]) -> Vec<u8> {
    nals.iter()
        .flat_map(|nal| [&[0u8, 0, 0, 1][..], nal].concat())
        .collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[test]
fn keeps_recovery_metadata_for_a_complete_quiet_frame() {
    let key = hex::decode("1000009d012aa0005a00").unwrap();
    let mut assembler = Assembler::new(Codec::Vp8);
    let mut payload = vec![0x10];
    payload.extend(&key);
    assembler.push(RtpPacket {
        sequence_number: 10,
        timestamp: 0xffff_fff0,
        marker: true,
        payload,
    });
    let unit = assembler
        .pop()
        .expect("complete quiet frame waited for future input");
    assert_eq!(unit.timestamp, 0xffff_fff0);
    assert_eq!(unit.data, key);
    let mut input = RelayInput::new(VideoCodec::Vp8);
    let frame = input.frame(unit.data).unwrap();
    assert!(frame.key_frame);
    assert_eq!((frame.width, frame.height), (160, 90));
    let frame = input.frame(vec![1, 0, 0, 1]).unwrap();
    assert!(!frame.key_frame, "delta frame is not recovery");
    assert_eq!(
        (frame.width, frame.height),
        (160, 90),
        "delta frame lost the stream's dimensions"
    );

    let mut input = RelayInput::new(VideoCodec::H264);
    let configuration = annexb(&[SPS, PPS]);
    input.frame(configuration.clone()).unwrap();
    let frame = input.frame(vec![0, 0, 0, 1, 0x65, 0x88, 0x84]).unwrap();
    assert!(frame.key_frame);
    assert!(
        contains(&frame.data, &configuration[..4 + SPS.len()]) && contains(&frame.data, PPS),
        "H264 IDR lost preceding decoder configuration"
    );
}

#[test]
fn h264_idr_without_parameter_sets_is_not_recovery() {
    let mut input = RelayInput::new(VideoCodec::H264);
    let frame = input.frame(annexb(&[IDR])).unwrap();
    assert!(!frame.key_frame);
    let frame = input.frame(annexb(&[SPS, PPS, IDR])).unwrap();
    assert!(frame.key_frame);
    assert!(frame.width > 0 && frame.height > 0);
}

#[test]
fn replacement_recovery_keeps_its_decoder_configuration() {
    // After a retired loss, the H.264 replacement keyframe decodes with its
    // in-band configuration.
    let mut stap = vec![0x78];
    for nal in [SPS, PPS, IDR] {
        stap.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        stap.extend_from_slice(nal);
    }
    let mut assembler = Assembler::new(Codec::H264);
    let mut input = RelayInput::new(VideoCodec::H264);
    let mut push = |assembler: &mut Assembler, sequence, timestamp, marker, payload: &[u8]| {
        for nal in assembler.push(RtpPacket {
            sequence_number: sequence,
            timestamp,
            marker,
            payload: payload.to_vec(),
        }) {
            input.frame(nal).unwrap();
        }
    };
    push(&mut assembler, 10, 0, true, &stap);
    assert!(assembler.pop().is_some(), "no initial recovery");
    push(&mut assembler, 11, 3000, false, &[0x7c, 0x81, 1]);
    push(&mut assembler, 13, 6000, true, &[0x61, 1, 0]);
    assert!(assembler.pop().is_none(), "incomplete frame escaped");
    push(&mut assembler, 14, 9000, true, &stap);
    assert_eq!(
        assembler.take_losses(),
        0,
        "retired loss requested recovery"
    );
    let unit = assembler
        .pop()
        .expect("replacement keyframe did not recover");
    assert_eq!(unit.timestamp, 9000);
    let frame = input.frame(unit.data).unwrap();
    assert!(frame.key_frame && frame.width > 0 && frame.height > 0);
}

#[test]
fn rejects_unsafe_h264_configuration() {
    // Generated Constrained Baseline SPS: ID 0 is 5120x2880; ID 1 is
    // 160x90. A decoder may use either, regardless of which arrived last.
    const OVERSIZED: &str = "6742c034da0050005a64";
    const SMALL: &str = "6742c015570a37e4c05a8303035280000003008000000301478b17c0";
    for (name, nals) in [
        ("oversize-hidden-by-small-SPS", vec![OVERSIZED, SMALL]),
        ("oversize-after-small-SPS", vec![SMALL, OVERSIZED]),
        ("truncated-SPS", vec![SMALL, "6742c015"]),
        ("truncated-VUI", vec!["6742c01eda0280b7fe5c0505050200"]),
    ] {
        let nals: Vec<Vec<u8>> = nals.iter().map(|nal| hex::decode(nal).unwrap()).collect();
        let refs: Vec<&[u8]> = nals.iter().map(Vec::as_slice).collect();
        let mut input = RelayInput::new(VideoCodec::H264);
        assert!(
            input.frame(annexb(&refs)).is_err(),
            "{name}: unsafe configuration reached the native decoder boundary"
        );
    }
    let mut input = RelayInput::new(VideoCodec::H264);
    let frame = input
        .frame(hex::decode(format!("00000001{SMALL}")).unwrap())
        .unwrap();
    assert_eq!((frame.width, frame.height), (160, 90), "valid cropped SPS");
}

#[test]
fn h264_allocation_bounds() {
    // Minimal Constrained Baseline parameter sets, including valid codec
    // padding and coded dimensions hidden behind a small crop.
    for (name, sps, width, height) in [
        ("1440p", "6742c033da00a002d640", 2560, 1440),
        ("padded-1080p", "6742c033da01e0089f95", 1920, 1080),
        ("portrait-within-bound", "6742c033da01100b5e5d", 1080, 1440),
        ("4k", "6742c033da00f0010f90", 3840, 2160),
        ("portrait-4k", "6742c033da008701e190", 2160, 3840),
        ("level-5.2-4k", "6742c034da00f0010f90", 3840, 2160),
        ("level-5.2-portrait-4k", "6742c034da008701e190", 2160, 3840),
        ("level-5.2-dci", "6742c034da0040004864", 4096, 2304),
        ("level-5.2-ultrawide", "6742c034da005000b590", 5120, 1440),
        (
            "level-5.2-portrait-ultrawide",
            "6742c034da0168028190",
            1440,
            5120,
        ),
        ("padded-4k", "6742c033da00f00111f894", 3840, 2160),
        (
            "4k-reference-buffer-limit",
            "6742c033f300780087c8",
            3840,
            2160,
        ),
        ("4k-reference-buffer-overflow", "6742c033f380780087c8", 0, 0),
        (
            "level-5.2-reference-buffer-overflow",
            "6742c034f380780087c8",
            0,
            0,
        ),
        (
            "level-5.2-cropped-storage-overflow",
            "6742c034da00f001e1f8034940",
            0,
            0,
        ),
        ("large-cropped-storage", "6742c033da00f001e1f8034940", 0, 0),
        (
            "crop-overflow",
            "6742c033da0a37e000000300100000030004",
            0,
            0,
        ),
        (
            "coded-size-overflow",
            "6742c033da000003000040000003000d90",
            0,
            0,
        ),
        ("excessive-references", "6742c033d84828d9", 0, 0),
    ] {
        let result = relay_h264_size(&hex::decode(sps).unwrap());
        if width == 0 {
            assert!(result.is_err(), "{name}: accepted {result:?}");
        } else {
            assert_eq!(result.ok(), Some((width, height)), "{name}");
        }
    }
}

#[test]
fn h264_size_never_accepts_an_unbounded_picture() {
    // Deterministic fuzzing: mutate the seeds.
    let seeds = [
        "6742c033da00f0010f90",
        "6742c033da008701e190",
        "6742c033da00f00111f894",
        "6742c015",
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64;
    for seed in seeds {
        let seed = hex::decode(seed).unwrap();
        for _ in 0..2000 {
            let mut data = seed.clone();
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let index = 4 + (state as usize) % data.len().saturating_sub(4).max(1);
            if index < data.len() {
                data[index] ^= (state >> 32) as u8;
            } else {
                data.push((state >> 40) as u8);
            }
            if let Ok((width, height)) = relay_h264_size(&data) {
                assert!(valid_video_size(width, height), "{width}x{height}");
            }
        }
    }
}

#[test]
fn frame_timing_uses_one_source_clock_across_wrap() {
    let anchor = 0xffff_f000u32;
    let mut clock = RelayClock::new(anchor);
    let second = Duration::from_secs(1);
    for (ticks, pts, duration) in [
        // The first frame keeps the caller's nominal fallback.
        (3000u32, second / 30, Duration::ZERO),
        (6000, second / 15, second / 30),
        // Reordering cannot create a negative interval.
        (3000, second / 30, Duration::ZERO),
        (9000, Duration::from_millis(100), second / 30),
    ] {
        let timing = clock.timing(anchor.wrapping_add(ticks));
        assert_eq!(timing, Some((pts, duration)), "source ticks {ticks}");
    }
    assert_eq!(
        clock.timing(anchor.wrapping_sub(1)),
        None,
        "frame before the source anchor was accepted"
    );
}

#[test]
fn payload_size_reads_sps_and_vp8_keyframes() {
    let mut stap = vec![0x78];
    for nal in [SPS, PPS] {
        stap.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        stap.extend_from_slice(nal);
    }
    let size = payload_size(VideoCodec::H264, &stap);
    assert!(size.is_some_and(|(width, height)| width > 0 && height > 0));
    assert_eq!(payload_size(VideoCodec::H264, SPS), size);
    assert_eq!(payload_size(VideoCodec::H264, IDR), None);
    let mut vp8 = vec![0x10];
    vp8.extend(hex::decode("1000009d012aa0005a00").unwrap());
    assert_eq!(payload_size(VideoCodec::Vp8, &vp8), Some((160, 90)));
    assert_eq!(payload_size(VideoCodec::Vp8, &[0x10, 1, 0]), None);
}

#[test]
fn annex_b_splits_three_and_four_byte_start_codes() {
    let data = [0, 0, 0, 1, 0x67, 1, 0, 0, 1, 0x68, 2, 0, 0, 0, 1, 0x65, 3];
    assert_eq!(
        annex_b(&data),
        vec![&[0x67, 1][..], &[0x68, 2][..], &[0x65, 3][..]]
    );
}
