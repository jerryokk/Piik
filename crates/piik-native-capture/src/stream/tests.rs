use std::io;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use super::*;
use crate::protocol::{FrameKind, read_frame, write_frame};
use crate::sources::CaptureTarget;

fn profile(
    width: u32,
    height: u32,
    framerate: u32,
    bitrate: u32,
    preference: &str,
) -> VideoProfile {
    VideoProfile {
        width,
        height,
        framerate,
        bitrate,
        preference: preference.into(),
    }
}

fn output(width: u32, height: u32, framerate: u32, bitrate: u32) -> OutputProfile {
    OutputProfile {
        width,
        height,
        framerate,
        bitrate,
    }
}

fn clean_exit() -> Option<Outcome> {
    Some(Outcome {
        exit: Ok(()),
        failure: None,
    })
}

#[tokio::test]
async fn output_profiles_and_control_frames_share_one_bounded_contract() {
    let outputs = screen_share_outputs(&profile(854, 480, 30, 2_000_000, "balanced"));
    assert_eq!(
        outputs,
        [
            output(426, 240, 30, 500_000),
            output(854, 480, 30, 2_000_000)
        ]
    );
    assert!(outputs.iter().all(OutputProfile::valid));
    let derived = [output(160, 90, 30, 90_000), output(320, 180, 30, 300_000)];
    append_output_arguments(&mut vec!["--protocol-v7".into()], &derived).expect("derived outputs");
    for slots in [
        vec![derived[1], derived[0]],
        vec![derived[0], derived[0]],
        vec![
            derived[0], derived[1], derived[0], derived[0], derived[0], derived[0],
        ],
    ] {
        append_output_arguments(&mut Vec::new(), &slots).expect("independent output slots");
    }
    for invalid in [
        Vec::new(),
        vec![derived[0]; MAX_OUTPUTS + 1],
        vec![OutputProfile::default()],
    ] {
        assert!(
            append_output_arguments(&mut Vec::new(), &invalid).is_err(),
            "invalid derived output set"
        );
    }

    let (writer, mut reader) = tokio::io::duplex(64 * 1024);
    let (stream, done) = Stream::for_test(tokio::io::empty(), writer, outputs);
    let _ = done.send(clean_exit());
    let collected = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut reader, &mut bytes).await;
        bytes
    });
    stream.set_output_active(1, true).await.expect("A 1 1");
    stream.set_output_active(1, true).await.expect("repeat");
    stream.set_output_active(0, true).await.expect("A 0 1");
    stream.set_output_active(1, false).await.expect("A 1 0");
    stream.set_output_active(1, false).await.expect("repeat");
    stream.set_output_bitrate(0, 250_000).await.expect("B");
    stream.set_output_bitrate(0, 250_000).await.expect("repeat");
    stream.request_key_frame(0).await.expect("K 0");
    stream.request_key_frame(-1).await.expect("K -1");
    let input = Frame {
        kind: FrameKind::Vp8,
        width: 160,
        height: 90,
        timestamp: Duration::from_secs(1),
        duration: Duration::from_nanos(33_333_300),
        key_frame: true,
        data: vec![1, 2, 3],
        ..Frame::default()
    };
    stream.write_frame(&input).await.expect("encoded input");
    stream.close().await.expect("close");

    let bytes = collected.await.expect("collector");
    let mut frames = &bytes[..];
    let (mut commands, mut encoded) = (Vec::new(), 0);
    while !frames.is_empty() {
        let frame = read_frame(&mut frames).await.expect("frame");
        if frame.kind == FrameKind::Control {
            commands.push(String::from_utf8(frame.data).expect("ASCII"));
        } else {
            assert_eq!(frame, input, "encoded input changed");
            encoded += 1;
        }
    }
    assert_eq!(
        commands,
        ["A 1 1", "A 0 1", "A 1 0", "B 0 250000", "K 0", "K -1", "Q"]
    );
    assert_eq!(encoded, 1);
    assert!(matches!(
        stream.request_key_frame(0).await,
        Err(Error::Closed)
    ));
    assert!(matches!(
        stream.set_output_active(2, true).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        stream.set_output_active(0, false).await,
        Err(Error::Closed)
    ));
    assert!(
        stream.set_output_bitrate(0, 999).await.is_err()
            && stream.set_output_bitrate(1, 2_000_001).await.is_err()
    );
    assert!(
        stream.request_key_frame(2).await.is_err() && stream.request_key_frame(-2).await.is_err()
    );
    let status = Frame {
        kind: FrameKind::Status,
        data: b"{}".to_vec(),
        ..Frame::default()
    };
    assert!(
        stream.write_frame(&status).await.is_err(),
        "non-video encoded input accepted"
    );
}

#[tokio::test]
async fn capture_stop_preserves_an_in_flight_frame() {
    let (mut producer, input) = tokio::io::duplex(64);
    let (key, mut commands) = tokio::io::duplex(64);
    let (stream, done) = Stream::for_test(input, key, Vec::new());
    let expected = Frame {
        kind: FrameKind::Vp8,
        width: 1280,
        height: 720,
        duration: Duration::from_secs(1) / 30,
        data: vec![0x12; 1024 * 1024],
        ..Frame::default()
    };
    let mut encoded = Vec::new();
    write_frame(&mut encoded, &expected).await.expect("encode");
    let reader = stream.clone();
    let read = tokio::spawn(async move { reader.read().await });
    producer.write_all(&encoded[..1]).await.expect("first byte");
    // Let the read take the input lock and consume the partial header.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let closer = stream.clone();
    let closed = tokio::spawn(async move { closer.close().await });
    let stop = read_frame(&mut commands).await.expect("stop command");
    assert!(
        stop.kind == FrameKind::Control && stop.data == b"Q",
        "stop command = {stop:?}"
    );
    producer
        .write_all(&encoded[1..])
        .await
        .expect("rest of frame");
    let frame = read.await.expect("reader").expect("in-flight frame");
    assert!(
        frame.kind == expected.kind && frame.data == expected.data,
        "retirement discarded the in-flight frame"
    );
    drop(producer);
    let _ = done.send(clean_exit());
    closed.await.expect("closer").expect("close");
}

#[tokio::test]
async fn video_output_group_count_is_bounded_before_process_startup() {
    let mut options = VideoOptions {
        target: CaptureTarget {
            kind: "display".into(),
            source_id: "1".into(),
            title: "Display".into(),
            ..CaptureTarget::default()
        },
        codec: "vp8".into(),
        profile: profile(1280, 720, 30, 3_000_000, "balanced"),
        ..VideoOptions::default()
    };
    let token = CancellationToken::new();
    for groups in [MAX_OUTPUTS - 1, usize::MAX] {
        options.output_groups = groups;
        let error = start_video(&token, Path::new(""), &options)
            .await
            .expect_err("group count reached startup");
        assert_eq!(error.to_string(), "native video target is invalid");
    }
    options.output_groups = MAX_OUTPUT_GROUPS;
    let error = start_video(&token, Path::new(""), &options)
        .await
        .expect_err("missing helper");
    assert_eq!(error.to_string(), "native capture process is unavailable");
}

#[test]
fn audio_ready_state_is_strict_and_explicit() {
    let status = |data: &str| Frame {
        kind: FrameKind::Status,
        data: data.as_bytes().to_vec(),
        ..Frame::default()
    };
    validate_audio_ready_frame(&status(r#"{"state":"active","audio":true}"#)).expect("ready");
    for payload in [
        r#"{"state":"active","audio":false}"#,
        r#"{"state":"starting","audio":true}"#,
        r#"{"state":"active","audio":true,"extra":true}"#,
    ] {
        assert!(
            validate_audio_ready_frame(&status(payload)).is_err(),
            "invalid audio state accepted: {payload}"
        );
    }
    let pcm = Frame {
        kind: FrameKind::Pcm,
        ..status(r#"{"state":"active","audio":true}"#)
    };
    assert!(
        validate_audio_ready_frame(&pcm).is_err(),
        "audio PCM was accepted as a ready frame"
    );
}

#[test]
fn video_profile_uses_the_product_bounds() {
    for valid in [
        profile(1280, 720, 1, 100_000, "balanced"),
        profile(1280, 720, 5, 800_000, "balanced"),
        profile(3840, 2160, 60, 50_000_000, "balanced"),
        profile(854, 480, 15, 2_000_000, "maintain-resolution"),
        profile(1280, 720, 30, 3_000_000, "balanced"),
        profile(1920, 1080, 60, 8_000_000, "maintain-framerate"),
        profile(2560, 1440, 60, 12_000_000, "balanced"),
        profile(1920, 1200, 30, 5_000_000, "balanced"),
        profile(720, 1280, 30, 3_000_000, "balanced"),
        profile(3840, 2160, 30, 12_000_000, "balanced"),
        profile(2160, 3840, 30, 12_000_000, "balanced"),
        profile(3840, 2160, 60, 12_000_000, "balanced"),
        profile(2160, 3840, 60, 12_000_000, "balanced"),
        profile(3000, 2764, 30, 12_000_000, "balanced"),
        profile(1920, 1080, 240, 240_000_000, "balanced"),
        profile(2560, 1440, 144, 100_000_000, "balanced"),
        profile(5120, 1440, 60, 100_000_000, "balanced"),
        profile(1440, 5120, 60, 100_000_000, "balanced"),
        profile(4096, 2304, 56, 80_000_000, "balanced"),
        profile(4, 4, 15, 2_000_000, "balanced"),
    ] {
        assert!(valid.valid(), "valid profile rejected: {valid:?}");
        for derived in screen_share_outputs(&valid) {
            assert!(
                derived.valid() && derived.bitrate <= valid.bitrate,
                "invalid output {derived:?}"
            );
        }
    }
    for invalid in [
        profile(0, 720, 30, 3_000_000, "balanced"),
        profile(2, 720, 30, 3_000_000, "balanced"),
        profile(1280, 2, 30, 3_000_000, "balanced"),
        profile(1279, 720, 30, 3_000_000, "balanced"),
        profile(1280, 719, 30, 3_000_000, "balanced"),
        profile(8194, 100, 30, 3_000_000, "balanced"),
        profile(5120, 2880, 30, 3_000_000, "balanced"),
        profile(4096, 2306, 30, 3_000_000, "balanced"),
        profile(6562, 1438, 30, 3_000_000, "balanced"),
        profile(3840, 2160, 65, 3_000_000, "balanced"),
        profile(1920, 1080, 241, 3_000_000, "balanced"),
        profile(2160, 3840, 120, 3_000_000, "balanced"),
        profile(u32::MAX, u32::MAX, u32::MAX, 3_000_000, "balanced"),
        profile(1920, 1080, 0, 5_000_000, "balanced"),
        profile(1920, 1080, 30, 99_999, "balanced"),
        profile(1920, 1080, 30, 240_000_001, "balanced"),
        profile(1920, 1080, 30, 5_000_000, "unknown"),
    ] {
        assert!(!invalid.valid(), "invalid profile accepted: {invalid:?}");
    }
}

#[test]
fn failure_codes_accept_only_fixed_grammar() {
    let codes = |text: &str| capture_failure_codes(text.as_bytes());
    assert_eq!(
        codes("stage=windows-runtime\nhresult=0x887A0005\ndetail=x"),
        ("windows-runtime".into(), "0x887a0005".into())
    );
    assert_eq!(
        codes("result=x\r\nstage=capture-closed\r\n"),
        ("capture-closed".into(), String::new())
    );
    assert_eq!(
        codes("stage=C:\\private\nhresult=0x887a0005 private\n"),
        (String::new(), String::new())
    );
    assert_eq!(
        codes(&format!("stage={}", "a".repeat(65))),
        (String::new(), String::new())
    );
    assert_eq!(
        codes("stage=first\nstage=second"),
        ("first".into(), String::new())
    );
}

#[derive(Clone, Default)]
struct Capture(Arc<StdMutex<Vec<u8>>>);

impl io::Write for Capture {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| io::Error::other("poisoned"))?
            .extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

#[test]
fn capture_failure_debug_uses_only_fixed_fields() {
    use tracing_subscriber::layer::SubscriberExt;

    struct Check {
        name: &'static str,
        stderr: &'static str,
        stage: &'static str,
        hresult: &'static str,
        debug: bool,
        canceled: bool,
        success: bool,
        logged: bool,
    }
    let check = |name, stderr, stage, hresult, debug, canceled, success, logged| Check {
        name,
        stderr,
        stage,
        hresult,
        debug,
        canceled,
        success,
        logged,
    };
    let oversized = Box::leak(format!("stage={}", "a".repeat(65)).into_boxed_str());
    for check in [
        check(
            "closed",
            "result=window-capture-failed\r\nstage=capture-closed\r\ndetail=private window title\r\n",
            "capture-closed",
            "",
            true,
            false,
            false,
            true,
        ),
        check(
            "device",
            "stage=windows-runtime\nhresult=0x887A0005\ndetail=C:\\private\\capture.exe",
            "windows-runtime",
            "0x887a0005",
            true,
            false,
            false,
            true,
        ),
        check(
            "malformed",
            "stage=C:\\private\\capture.exe\nhresult=0x887a0005 private\n",
            "",
            "",
            true,
            false,
            false,
            true,
        ),
        check(
            "oversized-stage",
            oversized,
            "",
            "",
            true,
            false,
            false,
            true,
        ),
        check(
            "private-detail",
            "detail=private window title\n",
            "",
            "",
            true,
            false,
            false,
            true,
        ),
        check(
            "disabled",
            "stage=capture-closed\n",
            "",
            "",
            false,
            false,
            false,
            false,
        ),
        check(
            "clean",
            "stage=capture-closed\n",
            "",
            "",
            true,
            false,
            true,
            false,
        ),
        check(
            "canceled",
            "detail=private cancellation\n",
            "",
            "",
            true,
            true,
            false,
            false,
        ),
        check(
            "failure-before-cancel",
            "stage=capture-closed\n",
            "capture-closed",
            "",
            true,
            true,
            false,
            true,
        ),
    ] {
        let output = Capture::default();
        let level = if check.debug {
            tracing::Level::DEBUG
        } else {
            tracing::Level::INFO
        };
        let subscriber = tracing_subscriber::registry()
            .with(piik_diagnostics::RedactingLayer::new(output.clone()))
            .with(tracing_subscriber::filter::LevelFilter::from_level(level));
        tracing::subscriber::with_default(subscriber, || {
            log_capture_failure(!check.success, 2, check.stderr.as_bytes(), check.canceled);
        });
        let text = String::from_utf8(output.0.lock().expect("capture").clone()).expect("UTF-8");
        if !check.logged {
            assert!(
                text.is_empty(),
                "{}: unexpected diagnostic: {text}",
                check.name
            );
            continue;
        }
        let record: serde_json::Map<String, serde_json::Value> =
            serde_json::from_str(&text).expect("JSON record");
        let expected = serde_json::json!({
            "level": "DEBUG", "msg": "piik-client", "event": "capture-process-failed",
            "exitCode": 2, "stage": check.stage, "hresult": check.hresult, "canceled": check.canceled,
        });
        let mut fields = record.clone();
        fields.remove("time");
        assert_eq!(
            serde_json::Value::Object(fields),
            expected,
            "{}: diagnostic changed fields",
            check.name
        );
        assert!(
            !text.contains("private"),
            "{}: diagnostic leaked detail: {text}",
            check.name
        );
    }
}
