// Audio sources, observed through the source's written packet record and
// real edges.

use std::time::Duration;

use opus_pure::OpusDecoder;

use super::*;
use crate::{Engine, EngineOptions};

fn engine() -> Engine {
    Engine::new(EngineOptions {
        bind_address: "127.0.0.1:0".into(),
        include_loopback: true,
        ..EngineOptions::default()
    })
    .unwrap()
}

async fn written(engine: &Engine, source: &AudioSource) -> Vec<Arc<[u8]>> {
    let key = source.key;
    engine
        .call(move |app, _, _| std::mem::take(&mut app.audios.get_mut(&key).unwrap().written))
        .await
        .unwrap()
}

#[tokio::test]
async fn source_volume_applies_before_the_shared_opus_encoder() {
    let engine = engine();
    let source = engine.new_audio_source(2, 128_000).await.unwrap();
    let mut decoder = OpusDecoder::new(SAMPLE_RATE as i32, CHANNELS).unwrap();
    let mut pcm = vec![0u8; FRAME_BYTES];
    for sample in 0..FRAME_SAMPLES {
        for (channel, frequency) in [500.0f64, 1500.0].into_iter().enumerate() {
            let value = (2000.0
                * (channel + 1) as f64
                * (2.0 * std::f64::consts::PI * frequency * sample as f64 / f64::from(SAMPLE_RATE))
                    .sin()) as i16;
            let offset = (sample * CHANNELS + channel) * 2;
            pcm[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
        }
    }
    let original = pcm.clone();
    let mut baseline = [0.0f64; 2];
    let mut decoded = vec![0i16; FRAME_SAMPLES * CHANNELS];
    for gain in [1.0, 2.0, 0.0, 1.0] {
        source.set_volume(gain).await.unwrap();
        let mut energy = [0.0f64; 2];
        for block in 0..20 {
            source
                .write_pcm(&pcm, Duration::from_millis(20))
                .await
                .unwrap();
            let packets = written(&engine, &source).await;
            assert_eq!(
                packets.len(),
                1,
                "one encoded packet per frame for every binding"
            );
            let count = decoder
                .decode_s16(&packets[0], FRAME_SAMPLES, &mut decoded)
                .unwrap();
            assert_eq!(count, FRAME_SAMPLES);
            if block < 5 {
                continue; // The gain ramp and codec settling.
            }
            for (index, value) in decoded.iter().enumerate() {
                energy[index % 2] += f64::from(*value) * f64::from(*value);
            }
        }
        assert_eq!(pcm, original, "gain modified capture-owned PCM");
        for channel in 0..2 {
            let level = energy[channel].sqrt();
            if baseline[channel] == 0.0 {
                baseline[channel] = level;
            }
            let ratio = level / baseline[channel];
            assert!(
                (ratio - gain).abs() <= 0.1,
                "channel {channel} gain {gain} decoded as {ratio:.3}"
            );
        }
    }
    for invalid in [-0.1, 2.1, f64::NAN, f64::INFINITY] {
        assert!(
            source.set_volume(invalid).await.is_err(),
            "volume {invalid}"
        );
    }
    engine.close().await.unwrap();
}

#[test]
fn scale_pcm_ramps_and_clips() {
    let mut input = vec![0u8; FRAME_BYTES];
    for chunk in input.chunks_exact_mut(2) {
        chunk.copy_from_slice(&20_000i16.to_le_bytes());
    }
    let mut output = vec![0u8; FRAME_BYTES];
    scale_pcm(&mut output, &input, 1.0, 2.0);
    let sample = |index: usize| i16::from_le_bytes([output[index * 2], output[index * 2 + 1]]);
    assert!(
        sample(0) > 20_000 && sample(0) < 20_100,
        "ramp starts near the old gain"
    );
    assert_eq!(
        sample(FRAME_SAMPLES * CHANNELS - 1),
        32_767,
        "clipped at the new gain"
    );
}

#[tokio::test]
async fn audio_source_rejects_invalid_and_closed_writes() {
    let engine = engine();
    let source = engine.new_audio_source(2, 128_000).await.unwrap();
    let pcm = vec![0u8; FRAME_BYTES];
    for _ in 0..2 {
        source
            .write_pcm(&pcm, Duration::from_millis(20))
            .await
            .expect("write");
    }
    assert!(
        source
            .write_pcm(&pcm[..1], Duration::from_millis(20))
            .await
            .is_err(),
        "invalid PCM"
    );
    assert!(
        source.write_pcm(&pcm, Duration::ZERO).await.is_err(),
        "zero duration"
    );
    source.set_bitrate(64_000).await.unwrap();
    assert_eq!(source.configured_bitrate(), 64_000);
    source.close().await;
    assert!(
        source
            .write_pcm(&pcm, Duration::from_millis(20))
            .await
            .is_err(),
        "closed source"
    );
    assert!(source.set_bitrate(64_000).await.is_err());
    let relayed = engine.new_relayed_audio_source(1).await.unwrap();
    assert!(relayed.is_relayed());
    assert_eq!(relayed.configured_bitrate(), 0);
    assert!(
        relayed
            .write_pcm(&pcm, Duration::from_millis(20))
            .await
            .is_err(),
        "relayed audio takes RTP"
    );
    assert!(
        engine.new_audio_source(0, 128_000).await.is_err(),
        "capacity bound"
    );
    assert!(
        engine
            .new_audio_source(MAX_RELAY_CHILDREN + 2, 128_000)
            .await
            .is_err()
    );
    engine.close().await.unwrap();
}

#[tokio::test]
async fn dropped_frames_advance_the_rtp_clock() {
    let engine = engine();
    let source = engine.new_audio_source(1, 128_000).await.unwrap();
    let pcm = vec![0u8; FRAME_BYTES];
    source
        .write_pcm(&pcm, Duration::from_millis(20))
        .await
        .unwrap();
    source
        .write_pcm_with_dropped(&pcm, Duration::from_millis(20), 2)
        .await
        .unwrap();
    let encoding = source.encoding.as_ref().unwrap();
    let (sequence, timestamp) = {
        let encoding = lock(encoding);
        (encoding.sequence, encoding.timestamp)
    };
    assert_eq!(sequence, 2);
    assert_eq!(timestamp, 960 * 4, "two dropped frames advanced the clock");
    engine.close().await.unwrap();
}
