//! Audio mix and source audio tests.

use std::time::Duration;

use piik_native_audio::FRAME_BYTES;
use piik_native_capture::CaptureTarget;

use super::{Fixture, audio_state, events, next_event, options, start};
use crate::audio_mix::mix_pcm;

fn frame(value: i16) -> Vec<u8> {
    value.to_le_bytes().repeat(FRAME_BYTES / 2)
}

fn sample(pcm: &[u8], index: usize) -> i16 {
    i16::from_le_bytes([pcm[index * 2], pcm[index * 2 + 1]])
}

#[test]
fn mix_pcm_retains_source_gain_and_saturates_stereo() {
    let mut output = frame(0);
    mix_pcm(&mut output, Some(&frame(1000)), None, 0.0, 0.0);
    assert_eq!(sample(&output, 0), 1000, "source was attenuated");
    mix_pcm(
        &mut output,
        Some(&frame(1000)),
        Some(&frame(2000)),
        0.0,
        2.0,
    );
    let last = output.len() / 2 - 1;
    assert!(
        sample(&output, 0) == sample(&output, 1)
            && sample(&output, last) == 5000
            && sample(&output, 0) < 1100,
        "stereo gain did not ramp"
    );
    for value in [-30000i16, 30000] {
        mix_pcm(
            &mut output,
            Some(&frame(value)),
            Some(&frame(value)),
            2.0,
            2.0,
        );
        let expected = if value < 0 { -32768 } else { 32767 };
        assert_eq!(
            sample(&output, 0),
            expected,
            "sum wrapped instead of saturating"
        );
    }
    mix_pcm(&mut output, None, None, 2.0, 0.0);
    assert!(
        output.iter().all(|value| *value == 0),
        "missing input replayed old audio"
    );
}

fn excluded() -> CaptureTarget {
    CaptureTarget {
        kind: "window".into(),
        source_id: "123".into(),
        pid: 456,
        creation_time: "789".into(),
        title: "Voice fixture".into(),
    }
}

async fn mixed(
    fixture: &Fixture,
) -> (
    crate::Session,
    tokio::sync::mpsc::Receiver<piik_native_control::media::MediaEvent>,
    tokio_util::sync::CancellationToken,
) {
    let (sink, rx) = events();
    let mut options = options(fixture, sink);
    options.share_id = "mixed_fixture".into();
    options.microphone_mixing = true;
    let (session, parent) = start(options).await;
    (session, rx, parent)
}

#[tokio::test]
async fn audio_exclusion_failure_mutes_only_source_and_never_falls_back() {
    let fixture = Fixture::new("media");
    let (session, mut rx, _parent) = mixed(&fixture).await;
    session
        .set_microphone(Some(true), None, None)
        .await
        .unwrap();
    let options = super::video_options();
    for fault in ["PIIK_VIDEO_FAIL", "PIIK_EXCLUSION_FAIL"] {
        session
            .replace_source(options.clone(), true, None)
            .await
            .unwrap();
        let video = session.inner.lock().stream.clone();
        fixture.set(fault, "1");
        let mut attempt = options.clone();
        if fault == "PIIK_VIDEO_FAIL" {
            attempt.target.source_id = "2".into();
        }
        let error = session
            .replace_source(attempt, true, Some(excluded()))
            .await
            .expect_err("failed exclusion was accepted");
        if fault == "PIIK_VIDEO_FAIL" {
            // The joined process cause is not asserted; the wire carries its codes.
            let _ = error;
        }
        let state = session.inner.lock();
        assert_eq!(session.source_audio(), Some(false), "{fault}");
        assert!(
            state.audio_stream.is_none(),
            "{fault}: retained unfiltered audio"
        );
        assert!(
            std::sync::Arc::ptr_eq(&state.stream, &video),
            "{fault}: retired video"
        );
        drop(state);
        fixture.set(fault, "");
    }
    let marker = fixture.path("excluded");
    fixture.set("PIIK_EXCLUSION_STARTED", &marker);
    session
        .replace_source(options, true, Some(excluded()))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "456:789");
    assert_eq!(session.source_audio(), Some(true));
    let source_audio = session.inner.lock().audio_stream.clone().unwrap();
    let mut profile = session.inner.lock().profile.clone();
    profile.audio_bitrate = 128_000;
    session.update_profile(profile).await.unwrap();
    session.set_paused(true);
    session.set_paused(false);
    let current = session.inner.lock().audio_stream.clone().unwrap();
    assert!(
        std::sync::Arc::ptr_eq(&current, &source_audio),
        "profile/pause replaced exclusion"
    );
    while rx.try_recv().is_ok() {}
    source_audio.close().await.unwrap();
    let state = next_event(&mut rx, Duration::from_secs(2), audio_state)
        .await
        .expect("source loss was not reported");
    assert_eq!(state, (false, true, false), "source loss was misreported");
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn same_video_source_audio_changes_preserve_capture() {
    let fixture = Fixture::new("media");
    let (session, _rx, _parent) = mixed(&fixture).await;
    let video = session.inner.lock().stream.clone();
    let options = super::video_options();
    session
        .replace_source(options.clone(), true, None)
        .await
        .unwrap();
    assert_eq!(session.source_audio(), Some(true));
    session
        .replace_source(options.clone(), false, None)
        .await
        .unwrap();
    assert_eq!(session.source_audio(), Some(false));
    session
        .replace_source(options, true, Some(excluded()))
        .await
        .unwrap();
    let current = session.inner.lock().stream.clone();
    assert!(
        std::sync::Arc::ptr_eq(&current, &video),
        "audio change replaced the capture"
    );
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn native_microphone_preserves_media_owners_across_input_changes() {
    let fixture = Fixture::new("media");
    let (session, mut rx, _parent) = mixed(&fixture).await;
    let output = session.inner.lock().audio_source.clone().unwrap();
    session
        .set_microphone(Some(true), Some(1.5), None)
        .await
        .unwrap();
    let state = next_event(&mut rx, Duration::from_secs(2), |event| {
        audio_state(event).filter(|state| state.1)
    })
    .await;
    assert_eq!(state, Some((false, true, false)));
    // A missing device fails without retiring the current microphone.
    assert!(
        session
            .set_microphone(None, None, Some("missing".into()))
            .await
            .is_err()
    );
    session
        .set_microphone(None, None, Some("other".into()))
        .await
        .unwrap();
    session
        .set_microphone(Some(false), None, None)
        .await
        .unwrap();
    assert!(
        session.set_microphone(None, Some(3.0), None).await.is_err(),
        "invalid gain was accepted"
    );
    let current = session.inner.lock().audio_source.clone().unwrap();
    assert_eq!(current, output, "input changes replaced the share output");
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn stop_cancels_pending_microphone_without_reviving_share() {
    let fixture = Fixture::new("media");
    let marker = fixture.path("microphone");
    fixture.set("PIIK_MICROPHONE_WAIT", &marker);
    let (session, _rx, _parent) = mixed(&fixture).await;
    let pending = {
        let session = session.clone();
        tokio::spawn(async move { session.set_microphone(Some(true), None, None).await })
    };
    super::eventually("microphone wait", || std::path::Path::new(&marker).exists()).await;
    session.close_session().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), pending)
        .await
        .expect("pending microphone outlived the share")
        .unwrap();
    assert!(result.is_err(), "pending microphone revived the share");
    // A cancelled mixer cannot register a replacement reader: retirement
    // drained every reader and admits none afterward.
    let mixer = session.inner.mixer().unwrap();
    assert_eq!(mixer.readers(), 0, "a reader outlived retirement");
    mixer.set_source(Some(session.inner.lock().stream.clone()));
    assert_eq!(mixer.readers(), 0, "cancelled mixer registered a reader");
}

#[tokio::test]
async fn fixed_source_audio_starts_with_share() {
    let fixture = Fixture::new("media");
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    let (session, _parent) = start(options).await;
    assert!(session.has_audio(), "fixed share lost its source audio");
    assert_eq!(
        session.source_audio(),
        None,
        "legacy share reported mixed facts"
    );
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn fixed_source_audio_keeps_encoding_past_the_first_frame() {
    let fixture = Fixture::new("media");
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    let (session, _parent) = start(options).await;
    let source = session
        .inner
        .lock()
        .audio_source
        .clone()
        .expect("fixed share has no audio source");
    let before = source.encoded_bytes().await.unwrap();
    let mut encoded = before;
    // One frame alone is not a live source: require sustained encoding.
    for _ in 0..500 {
        encoded = source.encoded_bytes().await.unwrap();
        if encoded >= before + 3 * 300 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        encoded >= before + 3 * 300,
        "fixed source audio stopped after its first frame ({} bytes)",
        encoded - before
    );
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn audio_startup_does_not_require_samples_or_stop_video() {
    for mode in ["silent", "no-ready"] {
        let fixture = Fixture::new("media");
        fixture.set("PIIK_AUDIO_STARTUP", mode);
        let (sink, _rx) = events();
        let mut options = options(&fixture, sink);
        options.audio_enabled = true;
        let (session, _parent) = start(options).await;
        // A silent source still starts; an input that never readies leaves
        // a video-only share.
        assert_eq!(session.has_audio(), mode == "silent", "{mode}");
        assert!(
            !session.inner.ctx.is_cancelled(),
            "{mode}: audio stopped video"
        );
        session.close_session().await.unwrap();
    }
}

#[tokio::test]
async fn audio_eof_waits_for_explicit_source_replacement() {
    let fixture = Fixture::new("media");
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    let (session, _parent) = start(options).await;
    let audio = session.inner.lock().audio_stream.clone().unwrap();
    audio.close().await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(session.has_audio(), "audio EOF changed the output shape");
    assert!(
        !session.inner.ctx.is_cancelled(),
        "audio EOF ended the share"
    );
    session
        .replace_source(super::video_options(), true, None)
        .await
        .unwrap();
    let replaced = session.inner.lock().audio_stream.clone().unwrap();
    assert!(!std::sync::Arc::ptr_eq(&replaced, &audio));
    session.close_session().await.unwrap();
}
