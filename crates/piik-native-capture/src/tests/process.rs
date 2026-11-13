//! Real child-process lifecycle tests against `examples/capture_fixture.rs`.
//! `cargo test` builds examples before running unit tests.

use std::path::PathBuf;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::stream::{CAPTURE_STOP_TIMEOUT, StreamSettings, start_stream};
use crate::{Error, ExitCause, Failure, Stream};

fn fixture() -> PathBuf {
    let executable = std::env::current_exe().expect("test executable");
    // target/<profile>/deps/<test>.exe -> target/<profile>/examples/capture_fixture.exe
    let fixture = executable
        .parent()
        .and_then(|deps| deps.parent())
        .map(|profile| profile.join("examples").join("capture_fixture.exe"))
        .expect("target directory");
    assert!(
        fixture.exists(),
        "build the fixture with `cargo test -p piik-native-capture`: {}",
        fixture.display()
    );
    fixture
}

async fn start(parent: &CancellationToken, environment: &[(&str, String)]) -> Stream {
    start_stream(
        parent,
        &fixture(),
        &[],
        environment,
        StreamSettings::default(),
    )
    .await
    .expect("start fixture")
}

fn mode(value: &str) -> (&'static str, String) {
    ("PIIK_CAPTURE_STOP_FIXTURE", value.to_owned())
}

#[tokio::test]
async fn capture_failure_survives_process_retirement_without_debug_logging() {
    let parent = CancellationToken::new();
    let stream = start(&parent, &[mode("startup-stage")]).await;
    assert!(
        stream.read().await.is_err(),
        "failed process produced a ready frame"
    );
    let error = stream.close().await.expect_err("process failure");
    assert_eq!(
        error.failure_details(),
        Some(&Failure {
            stage: "mft-output-timeout".into(),
            hresult: "0x80004005".into(),
            exit_code: 2
        }),
        "process failure lost structured codes"
    );
    let text = error.to_string();
    assert!(
        text.contains("mft-output-timeout") && text.contains("0x80004005"),
        "lost native stage: {text}"
    );
    assert!(
        !text.contains("private"),
        "native failure exposed raw stderr: {text}"
    );
    let Error::Process { cause, .. } = &error else {
        panic!("untyped failure: {error}")
    };
    assert_eq!(
        cause.exit_code(),
        2,
        "native failure lost its process exit cause"
    );
    assert!(
        Error::Invalid("stage=forged hresult=0x80004005")
            .failure_details()
            .is_none()
    );

    let stream = start(&parent, &[mode("startup-exit")]).await;
    assert!(
        stream.read().await.is_err(),
        "uncoded process failure produced a frame"
    );
    let error = stream.close().await.expect_err("uncoded failure");
    assert_eq!(
        error.failure_details(),
        Some(&Failure {
            exit_code: 3,
            ..Failure::default()
        })
    );
    assert!(matches!(stream.done().await, Err(ExitCause::Status(_))));
}

#[tokio::test]
async fn capture_stop_uses_one_bounded_retirement_path() {
    for mode_name in [
        "explicit",
        "parent",
        "both",
        "unresponsive",
        "blocked-output-explicit",
        "blocked-output-parent",
        "blocked-output-both",
    ] {
        let parent = CancellationToken::new();
        let directory = tempfile::tempdir().expect("temp dir");
        let marker = directory.path().join("released");
        let marker_value = marker.to_string_lossy().into_owned();
        let stream = start(
            &parent,
            &[mode(mode_name), ("PIIK_CAPTURE_STOP_MARKER", marker_value)],
        )
        .await;
        stream
            .read()
            .await
            .unwrap_or_else(|error| panic!("{mode_name}: ready frame: {error}"));
        let stop = mode_name.trim_start_matches("blocked-output-");
        if stop != "explicit" {
            parent.cancel();
        }
        if stop == "explicit" || stop == "both" {
            let closer = stream.clone();
            tokio::spawn(async move { closer.close().await });
        }
        tokio::time::timeout(CAPTURE_STOP_TIMEOUT + Duration::from_secs(3), stream.done())
            .await
            .unwrap_or_else(|_| panic!("{mode_name}: capture retirement exceeded its stop budget"))
            .ok();
        if mode_name == "unresponsive" {
            assert!(
                !marker.exists(),
                "unresponsive capture unexpectedly released"
            );
        } else {
            assert!(
                marker.exists(),
                "{mode_name}: capture was killed before receiving its stop command"
            );
        }
        let _ = stream.close().await;
    }
}

#[tokio::test]
async fn remote_input_does_not_block_on_a_stalled_capture() {
    let parent = CancellationToken::new();
    let settings = StreamSettings {
        remote_input: true,
        ..StreamSettings::default()
    };
    let stream = start_stream(&parent, &fixture(), &[], &[mode("unresponsive")], settings)
        .await
        .expect("start fixture");
    stream.read().await.expect("ready frame");
    assert!(stream.remote_input());
    let sender = stream.clone();
    let result = tokio::time::timeout(CAPTURE_STOP_TIMEOUT + Duration::from_secs(3), async move {
        for _ in 0..8192 {
            sender
                .send_remote_input("M 0.500000000 0.500000000")
                .await?;
        }
        Ok::<(), Error>(())
    })
    .await
    .expect("input held its grant lock beyond the capture stop budget");
    assert!(
        result.is_err(),
        "unresponsive helper accepted input after its pipe filled"
    );
    assert!(!stream.remote_input(), "stalled capture was not retired");
    let _ = stream.close().await;
}

#[tokio::test]
async fn audio_startup_requires_an_explicit_ready_state() {
    let parent = CancellationToken::new();
    let arguments = ["--capture-audio", "display", "0", "0"];
    let stream = crate::stream::start_audio_stream_with(
        &parent,
        &fixture(),
        &arguments,
        &[mode("audio-ready")],
        Duration::from_secs(3),
    )
    .await
    .expect("ready audio");
    stream.close().await.expect("clean stop");
    let error = crate::stream::start_audio_stream_with(
        &parent,
        &fixture(),
        &arguments,
        &[mode("audio-invalid")],
        Duration::from_secs(3),
    )
    .await
    .expect_err("inactive audio accepted");
    assert_eq!(
        error.to_string(),
        "native audio capture returned an invalid ready state"
    );
    let error = crate::stream::start_audio_stream_with(
        &parent,
        &fixture(),
        &arguments,
        &[mode("unresponsive-audio")],
        Duration::from_secs(3),
    )
    .await
    .expect_err("non-status ready accepted");
    assert_eq!(
        error.to_string(),
        "native audio capture returned an invalid ready state"
    );
    parent.cancel();
    let error = crate::stream::start_audio_stream_with(
        &parent,
        &fixture(),
        &arguments,
        &[mode("audio-ready")],
        Duration::from_secs(3),
    )
    .await
    .expect_err("canceled startup");
    assert!(matches!(error, Error::Canceled));
}

/// Opt-in check against the real helper: set `PIIK_NATIVE_CAPTURE` and
/// `PIIK_ADAPTIVE_INPUT` (JSON lines of `{Width,Height,Recovery,DataHex}`).
#[tokio::test]
#[ignore = "requires the real piik-capture.exe and an encoded fixture (PIIK_NATIVE_CAPTURE, PIIK_ADAPTIVE_INPUT)"]
async fn adaptive_output_fixture() {
    use crate::protocol::{Frame, FrameKind, MAX_MEDIA_BYTES};
    use crate::stream::{EncodedVideoOptions, OutputProfile, start_encoded_video};

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "PascalCase")]
    struct Row {
        width: u32,
        height: u32,
        recovery: bool,
        data_hex: String,
    }

    let (Some(executable), Some(fixture)) = (
        std::env::var_os("PIIK_NATIVE_CAPTURE"),
        std::env::var_os("PIIK_ADAPTIVE_INPUT"),
    ) else {
        panic!("set PIIK_NATIVE_CAPTURE and PIIK_ADAPTIVE_INPUT");
    };
    let codec = std::env::var("PIIK_ADAPTIVE_CODEC").unwrap_or_else(|_| "vp8".into());
    let kind = if codec == "h264" {
        FrameKind::H264
    } else {
        FrameKind::Vp8
    };
    let text = std::fs::read_to_string(fixture).expect("fixture");
    let mut frames = Vec::new();
    for row in serde_json::Deserializer::from_str(&text).into_iter::<Row>() {
        let row = row.expect("bounded input");
        assert!(
            frames.len() < 180 && row.data_hex.len() <= MAX_MEDIA_BYTES * 2,
            "invalid bounded input"
        );
        assert!(
            row.width == 640 && row.height == 360,
            "expected 640x360 encoded input"
        );
        let data = hex::decode(&row.data_hex).expect("hex");
        frames.push(Frame {
            kind,
            key_frame: row.recovery,
            width: row.width,
            height: row.height,
            data,
            ..Frame::default()
        });
    }
    assert!(
        frames.first().is_some_and(|frame| frame.key_frame),
        "input must begin with recovery"
    );
    let parent = CancellationToken::new();
    let options = EncodedVideoOptions {
        codec: codec.clone(),
        preference: "maintain-framerate".into(),
        outputs: vec![OutputProfile {
            width: 640,
            height: 360,
            framerate: 30,
            bitrate: 2_000_000,
        }],
        ..EncodedVideoOptions::default()
    };
    let stream = start_encoded_video(&parent, std::path::Path::new(&executable), &options)
        .await
        .expect("start");
    stream.set_output_active(0, true).await.expect("activate");
    let reader = stream.clone();
    let output = tokio::spawn(async move {
        let (mut begin, mut previous, mut output) = (Duration::ZERO, Duration::ZERO, Vec::new());
        loop {
            let frame = match reader.read().await {
                Ok(frame) => frame,
                Err(_) => return Ok(output),
            };
            if frame.kind == FrameKind::Begin && begin.is_zero() {
                begin = frame.timestamp;
            }
            if frame.kind == FrameKind::LayerUnavailable {
                return Err(format!(
                    "capture layer unavailable: {}",
                    String::from_utf8_lossy(&frame.data)
                ));
            }
            if frame.kind != kind {
                continue;
            }
            if frame.timestamp < begin || frame.timestamp <= previous || output.len() >= 2400 {
                return Err("adaptive output violated its input timeline".into());
            }
            previous = frame.timestamp;
            output.push((frame.timestamp, frame.width));
        }
    });
    let mut ticker = tokio::time::interval(Duration::from_secs(1) / 30);
    for index in 0..76 * 30u32 {
        ticker.tick().await;
        assert!(!output.is_finished(), "capture stopped during adaptation");
        if index == 6 * 30 || index == 21 * 30 {
            let budget = if index == 21 * 30 { 2_000_000 } else { 100_000 };
            stream.set_output_bitrate(0, budget).await.expect("bitrate");
        }
        let mut frame = frames[index as usize % frames.len()].clone();
        // Preserve a non-microsecond-aligned input anchor through WebRTC.
        frame.timestamp = Duration::from_secs(1) / 30 * (index + 1) + Duration::from_nanos(100);
        frame.duration = Duration::from_secs(1) / 30;
        stream.write_frame(&frame).await.expect("write");
    }
    stream.close().await.expect("close");
    let output = output.await.expect("reader").expect("timeline");
    let six = Duration::from_secs(6);
    let twenty_one = Duration::from_secs(21);
    let healthy = output
        .iter()
        .filter(|(time, width)| *time < six && *width == 640)
        .count();
    let reduced = output
        .iter()
        .filter(|(time, width)| *time >= six && *time < twenty_one && *width < 640)
        .count();
    let restored = output
        .iter()
        .filter(|(time, width)| *time >= twenty_one && *width == 640)
        .count();
    println!(
        "codec={codec} healthy={healthy} reduced={reduced} restored={restored} total={}",
        output.len()
    );
    assert!(
        healthy > 0 && reduced > 0 && restored > 0,
        "real output did not preserve delivery through adapt/recover"
    );
}
