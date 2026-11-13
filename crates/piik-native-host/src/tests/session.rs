//! Session lifecycle and publication tests.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::future::BoxFuture;
use piik_media_edge::PcState;
use piik_native_capture::{CaptureTarget, FrameKind, VideoOptions, VideoProfile};
use piik_native_control::media::{MediaEvent, MediaEventKind, QualityProfile};
use tokio_util::sync::CancellationToken;

use super::capture_state::state_payload;
use super::{Fixture, events, next_event, options, recovery_profile, start, video_options};
use crate::capture::{CaptureProvider, ProcessCapture, SharedStream};
use crate::session::{apply_output_plan, wait_for_capture_profile};

const STARTING: &str = r#"{"state":"starting","codec":"vp8","adapterIndex":0,"adapterName":"Fixture","adapterIdentity":"0:0","encoderName":"VP8","encoderIdentity":"vp8"}"#;
const ACTIVE: &str = r#"{"state":"active","codec":"vp8","width":1280,"height":720,"fps":30}"#;

fn backend(fixture: &Fixture) {
    fixture.set("PIIK_CAPTURE_STARTING", &state_payload(STARTING));
    fixture.set("PIIK_CAPTURE_ACTIVE", &state_payload(ACTIVE));
}

fn backend_options(groups: usize) -> VideoOptions {
    VideoOptions {
        target: super::display(),
        codec: "vp8".into(),
        profile: VideoProfile {
            width: 1280,
            height: 720,
            framerate: 30,
            bitrate: 3_000_000,
            preference: "balanced".into(),
        },
        output_groups: groups,
        ..VideoOptions::default()
    }
}

#[tokio::test]
async fn start_validates_identity_capacity_and_profile() {
    let fixture = Fixture::new("media");
    let parent = CancellationToken::new();
    let invalid = |change: fn(&mut crate::Options)| {
        let (sink, _rx) = events();
        let mut options = options(&fixture, sink);
        change(&mut options);
        options
    };
    for options in [
        invalid(|options| options.share_id.clear()),
        invalid(|options| options.share_id = "x".repeat(257)),
        invalid(|options| options.edge_capacity = 0),
        invalid(|options| options.edge_capacity = 21),
        invalid(|options| options.profile.audio_bitrate = 1),
        invalid(|options| options.video.profile.width = 640),
    ] {
        assert!(crate::Session::start(&parent, options).await.is_err());
    }
}

#[tokio::test]
async fn capture_startup_returns_native_failure_codes_without_private_detail() {
    let fixture = Fixture::new("media");
    fixture.set("PIIK_VIDEO_FAIL", "1");
    let (sink, _rx) = events();
    let parent = CancellationToken::new();
    let error = crate::Session::start(&parent, options(&fixture, sink))
        .await
        .expect_err("failed capture started");
    let failure = error
        .failure_details()
        .expect("startup lost its typed codes");
    assert_eq!(
        (
            failure.stage.as_str(),
            failure.hresult.as_str(),
            failure.exit_code
        ),
        ("mft-set-output-type", "0xc00d36b4", 2)
    );
    assert!(
        !error.to_string().contains("private"),
        "startup leaked detail: {error}"
    );
}

#[tokio::test]
async fn capture_failure_preserves_process_cause() {
    // The helper fails right after its active status.
    for mode in ["read", "control"] {
        let fixture = Fixture::new("backend");
        backend(&fixture);
        fixture.set("PIIK_CAPTURE_LIVE_FAILURE", mode);
        let (sink, _rx) = events();
        let mut options = options(&fixture, sink);
        options.video = backend_options(0);
        options.profile.video = options.video.profile.clone();
        options.edge_capacity = 4;
        // The device-loss code is recoverable; its replacement cannot start.
        options.capture = Arc::new(FailingReplacement {
            inner: fixture.capture(),
            starts: AtomicUsize::new(0),
        });
        let parent = CancellationToken::new();
        let ended = match crate::Session::start(&parent, options).await {
            Ok(session) => tokio::time::timeout(Duration::from_secs(5), session.done())
                .await
                .expect("live failure did not end the share")
                .expect("live failure looked like an owner close"),
            Err(error) => Err(error),
        };
        let error = ended.expect_err("live failure ended cleanly");
        let failure = error
            .failure_details()
            .expect("failure lost its typed helper codes");
        assert_eq!(
            (
                failure.stage.as_str(),
                failure.hresult.as_str(),
                failure.exit_code
            ),
            ("windows-runtime", "0x887a0005", 2),
            "{mode}"
        );
        assert!(!error.to_string().contains("private"), "{mode}: {error}");
    }
}

/// Starts the first video only; every later start fails.
struct FailingReplacement {
    inner: Arc<ProcessCapture>,
    starts: AtomicUsize,
}

impl CaptureProvider for FailingReplacement {
    fn video<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        options: VideoOptions,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        if self.starts.fetch_add(1, Ordering::SeqCst) > 0 {
            return Box::pin(async {
                Err(piik_native_capture::Error::Invalid(
                    "replacement could not start",
                ))
            });
        }
        self.inner.video(ctx, options)
    }
    fn audio<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        target: CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        self.inner.audio(ctx, target, exclude)
    }
    fn microphone<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        device: String,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        self.inner.microphone(ctx, device)
    }
}

#[tokio::test]
async fn output_plan_preserves_original_startup_and_independent_activation() {
    let fixture = Fixture::new("echo");
    let ctx = CancellationToken::new();
    let mut options = video_options();
    options.profile.framerate = 15;
    options.output_groups = 2;
    let stream = ProcessCapture {
        executable: fixture.executable.clone(),
    }
    .video(&ctx, options)
    .await
    .unwrap();
    let mut plan = piik_media_edge::OutputPlan {
        active: vec![false, true, true, false],
        bitrates: vec![0, 0, 250_000, 0],
        recovery_layers: Vec::new(),
    };
    apply_output_plan(&stream, &plan, false).await.unwrap();
    plan.recovery_layers = vec![2];
    apply_output_plan(&stream, &plan, true).await.unwrap();
    plan.active.fill(false);
    plan.recovery_layers.clear();
    apply_output_plan(&stream, &plan, true).await.unwrap();
    for command in ["B 2 250000", "K 2", "A 0 0", "A 2 1", "A 1 0", "A 2 0"] {
        let frame = stream.read().await.unwrap();
        assert_eq!(frame.kind, FrameKind::Control);
        assert_eq!(String::from_utf8_lossy(&frame.data), command);
    }
    let _ = stream.close().await;
}

#[tokio::test]
async fn capture_profile_retains_selected_backend_and_fitted_size() {
    let starting = r#"{"state":"starting","hardwareOnly":true,"codec":"h264","adapterIndex":1,"adapterName":"GPU","adapterIdentity":"0:1","encoderIndex":2,"encoderName":"H264","encoderIdentity":"encoder"}"#;
    let mut active: serde_json::Value = serde_json::from_str(&state_payload(
        r#"{"state":"active","hardwareOnly":true,"codec":"h264","profileLevelId":"42c01f","width":1280,"height":720,"fps":30}"#,
    ))
    .unwrap();
    active["width"] = 640.into();
    active["height"] = 480.into();
    let profile = backend_options(4).profile;
    for failed in ["", "0", "1", "5", "6"] {
        let fixture = Fixture::new("backend");
        fixture.set("PIIK_CAPTURE_STARTING", &state_payload(starting));
        fixture.set("PIIK_CAPTURE_ACTIVE", &active.to_string());
        fixture.set("PIIK_CAPTURE_UNAVAILABLE", failed);
        let ctx = CancellationToken::new();
        let mut options = backend_options(4);
        options.codec = "h264".into();
        let stream = ProcessCapture {
            executable: fixture.executable.clone(),
        }
        .video(&ctx, options)
        .await
        .unwrap();
        let state = wait_for_capture_profile(&ctx, &stream, &profile, "h264", false, false).await;
        let _ = stream.close().await;
        if failed == "1" || failed == "6" {
            assert!(
                state.is_err(),
                "original or invalid output failure was accepted"
            );
            continue;
        }
        let state = state.unwrap();
        assert_eq!(
            (state.adapter_index, state.encoder_index),
            (Some(1), Some(2))
        );
        assert_eq!((state.width, state.height), (640, 480));
        let expected: Vec<usize> = failed.parse().into_iter().collect();
        assert_eq!(state.unavailable_layers, expected, "unavailable-{failed}");
    }
}

#[tokio::test]
async fn prepared_capture_outlives_its_permission_operation() {
    let fixture = Fixture::new("backend");
    backend(&fixture);
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.video = backend_options(0);
    options.profile.video = options.video.profile.clone();
    // The fixture reports four independent groups.
    options.edge_capacity = 4;
    let (session, parent) = start(options).await;
    let operation = session.inner.ctx.child_token();
    let (stream, _) = session
        .inner
        .prepare_video(&operation, backend_options(0), true, false)
        .await
        .unwrap();
    operation.cancel();
    let probe = stream.read();
    let ended = tokio::time::timeout(Duration::from_millis(250), probe).await;
    assert!(
        ended.is_err(),
        "finishing permission preparation stopped its picture"
    );
    parent.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(2), session.done())
        .await
        .expect("share outlived its owner");
    assert!(
        stream.read().await.is_err(),
        "capture outlived its owning share"
    );
}

/// Delays the Nth video start until released.
struct GatedCapture {
    inner: ProcessCapture,
    gate: usize,
    starts: AtomicUsize,
    entered: CancellationToken,
    release: CancellationToken,
    finished: CancellationToken,
}

impl CaptureProvider for GatedCapture {
    fn video<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        options: VideoOptions,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        Box::pin(async move {
            if self.starts.fetch_add(1, Ordering::SeqCst) + 1 == self.gate {
                self.entered.cancel();
                let _finished = self.finished.clone().drop_guard();
                tokio::select! {
                    () = self.release.cancelled() => {}
                    () = ctx.cancelled() => return Err(piik_native_capture::Error::Canceled),
                }
            }
            self.inner.video(ctx, options).await
        })
    }
    fn audio<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        target: CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        self.inner.audio(ctx, target, exclude)
    }
    fn microphone<'a>(
        &'a self,
        ctx: &'a CancellationToken,
        device: String,
    ) -> BoxFuture<'a, Result<SharedStream, piik_native_capture::Error>> {
        self.inner.microphone(ctx, device)
    }
}

fn capture_active(event: &MediaEvent) -> Option<()> {
    matches!(&event.kind, MediaEventKind::CaptureState { state } if state == "active").then_some(())
}

#[tokio::test]
async fn device_loss_recovers_with_retained_media() {
    for mode in ["read", "control"] {
        let fixture = Fixture::new("media");
        let failure = fixture.path("device-failure");
        fixture.set("PIIK_VIDEO_DEVICE_FAILURE", &failure);
        if mode == "control" {
            fixture.set("PIIK_VIDEO_DEVICE_CONTROL", "1");
        }
        let (sink, mut rx) = events();
        let mut options = options(&fixture, sink);
        options.audio_enabled = true;
        let (session, _parent) = start(options).await;
        next_event(&mut rx, Duration::from_secs(5), capture_active)
            .await
            .expect("initial capture did not become active");
        let (stream, audio, source, audio_source, selected) = {
            let state = session.inner.lock();
            (
                state.stream.clone(),
                state.audio_stream.clone(),
                state.source.clone(),
                state.audio_source.clone(),
                state.video_options.clone(),
            )
        };
        std::fs::write(&failure, b"").unwrap();
        next_event(&mut rx, Duration::from_secs(5), capture_active)
            .await
            .expect("replacement was not installed");
        {
            let state = session.inner.lock();
            assert!(
                !Arc::ptr_eq(&state.stream, &stream),
                "{mode}: capture not replaced"
            );
            assert!(super::super::capture::same(
                state.audio_stream.as_ref(),
                audio.as_ref()
            ));
            assert_eq!(state.source, source, "{mode}: recovery replaced the source");
            assert_eq!(state.audio_source, audio_source);
            assert_eq!(
                state.video_options, selected,
                "{mode}: recovery changed capture options"
            );
        }
        assert!(
            !session.inner.ctx.is_cancelled(),
            "{mode}: device loss ended the share"
        );
        session.close_session().await.unwrap();
    }
}

#[tokio::test]
async fn device_loss_recovery_yields_to_capture_operations() {
    for mode in ["profile", "stop"] {
        let fixture = Fixture::new("media");
        let failure = fixture.path("device-failure");
        fixture.set("PIIK_VIDEO_DEVICE_FAILURE", &failure);
        let capture = Arc::new(GatedCapture {
            inner: ProcessCapture {
                executable: fixture.executable.clone(),
            },
            gate: 2,
            starts: AtomicUsize::new(0),
            entered: CancellationToken::new(),
            release: CancellationToken::new(),
            finished: CancellationToken::new(),
        });
        let (sink, mut rx) = events();
        let mut options = options(&fixture, sink);
        options.capture = capture.clone();
        let (session, _parent) = start(options).await;
        next_event(&mut rx, Duration::from_secs(5), capture_active).await;
        let mut next = QualityProfile {
            video: recovery_profile(),
            audio_bitrate: 64_000,
        };
        next.video.width = 640;
        next.video.height = 360;
        let completed = if mode == "profile" {
            let session = session.clone();
            let next = next.clone();
            let task = tokio::spawn(async move { session.update_profile(next).await });
            tokio::time::timeout(Duration::from_secs(5), capture.entered.cancelled())
                .await
                .expect("profile preparation did not start");
            std::fs::write(&failure, b"").unwrap();
            // Recovery waits behind the update; let the update proceed.
            tokio::time::sleep(Duration::from_millis(200)).await;
            capture.release.cancel();
            task
        } else {
            std::fs::write(&failure, b"").unwrap();
            tokio::time::timeout(Duration::from_secs(5), capture.entered.cancelled())
                .await
                .expect("recovery preparation did not start");
            let session = session.clone();
            tokio::spawn(async move { session.close_session().await })
        };
        tokio::time::timeout(Duration::from_secs(5), completed)
            .await
            .expect("capture operation deadlocked with its reader")
            .unwrap()
            .unwrap();
        assert!(
            capture.finished.is_cancelled(),
            "operation returned before preparation retired"
        );
        if mode == "profile" {
            let state = session.inner.lock();
            assert_eq!(state.profile, next, "recovery overwrote the user's profile");
            assert_eq!(state.video_options.profile, next.video);
            drop(state);
            assert!(
                !session.inner.ctx.is_cancelled(),
                "profile update ended the share"
            );
        }
        session.close_session().await.unwrap();
    }
}

#[tokio::test]
async fn close_releases_update_lock_before_waiting_for_capture_retirement() {
    let fixture = Fixture::new("media");
    let gate = fixture.path("video");
    fixture.set("PIIK_VIDEO_START_GATE", &gate);
    std::fs::write(format!("{gate}.release"), b"").unwrap();
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    options.microphone_mixing = true;
    let (session, _parent) = start(options).await;
    let closing = {
        let session = session.clone();
        tokio::spawn(async move { session.close_session().await })
    };
    tokio::time::timeout(Duration::from_secs(5), closing)
        .await
        .expect("close did not finish")
        .unwrap()
        .unwrap();
    assert!(
        session.inner.update.try_lock().is_ok(),
        "close retained the update lock"
    );
    assert!(
        session.done().await.is_none(),
        "owner close reported share-ended"
    );
}

#[tokio::test]
async fn mixed_source_audio_waits_for_active_video_without_blocking_start() {
    let fixture = Fixture::new("media");
    let gate = fixture.path("video");
    fixture.set("PIIK_VIDEO_START_GATE", &gate);
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    options.microphone_mixing = true;
    let (session, _parent) = start(options).await;
    assert!(session.has_audio(), "mixed share lost its stable output");
    assert_eq!(
        session.source_audio(),
        Some(false),
        "source audio preceded active video"
    );
    assert!(!std::path::Path::new(&format!("{gate}.early-audio")).exists());
    std::fs::write(format!("{gate}.release"), b"").unwrap();
    super::eventually("source audio", || session.source_audio() == Some(true)).await;
    let identity = std::fs::read_to_string(format!("{gate}.audio")).unwrap();
    assert_eq!(identity, "0:0", "source audio lost its display target");
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn pending_source_audio_retires_with_video_failure() {
    let fixture = Fixture::new("media");
    let gate = fixture.path("video");
    fixture.set("PIIK_VIDEO_START_GATE", &gate);
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    options.microphone_mixing = true;
    let (session, _parent) = start(options).await;
    std::fs::write(format!("{gate}.fail"), b"").unwrap();
    let ended = tokio::time::timeout(Duration::from_secs(5), session.done())
        .await
        .expect("video failure did not end the share")
        .expect("video failure looked like an owner close");
    assert!(ended.is_err());
    assert!(
        !std::path::Path::new(&format!("{gate}.audio")).exists(),
        "audio started after video failed"
    );
}

#[tokio::test]
async fn fixed_source_audio_waits_for_active_video_and_can_cancel() {
    let fixture = Fixture::new("media");
    let gate = fixture.path("video");
    fixture.set("PIIK_VIDEO_START_GATE", &gate);
    let (sink, _rx) = events();
    let mut options = options(&fixture, sink);
    options.audio_enabled = true;
    let parent = CancellationToken::new();
    let starting = {
        let parent = parent.clone();
        tokio::spawn(async move { crate::Session::start(&parent, options).await })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !std::path::Path::new(&format!("{gate}.early-audio")).exists(),
        "audio preceded video"
    );
    parent.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), starting)
        .await
        .expect("cancelled start did not return")
        .unwrap();
    assert!(result.is_err(), "cancelled start produced a share");
}

#[tokio::test]
async fn edge_replacement_fences_native_control() {
    let fixture = Fixture::new("media");
    let (sink, mut rx) = events();
    let (session, _parent) = start(options(&fixture, sink)).await;
    const ID: &str = "connection_12345678";
    session.prepare_edge(ID, Vec::new(), false).await.unwrap();
    let old = session.inner.lock().edges[ID].edge.clone();
    old.close().await;
    let mut observed = Vec::new();
    let closed = next_event(&mut rx, Duration::from_secs(3), |event| match &event.kind {
        MediaEventKind::EdgeState { state, .. } if state == PcState::Closed.as_str() => {
            Some(event.current.as_ref().is_some_and(|current| current()))
        }
        _ => None,
    })
    .await
    .expect("edge did not emit its terminal state");
    assert!(
        closed,
        "current transport failure lost its delivery authority"
    );
    while let Ok(event) = rx.try_recv() {
        observed.push(event);
    }
    session.close_edge(ID).await;
    session.prepare_edge(ID, Vec::new(), false).await.unwrap();
    for event in &observed {
        if let Some(current) = &event.current {
            assert!(
                !current(),
                "same-ID replacement accepted a retired edge event"
            );
        }
    }
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn candidate_for_retired_edge_is_ignored() {
    let fixture = Fixture::new("media");
    let (sink, _rx) = events();
    let (session, _parent) = start(options(&fixture, sink)).await;
    session.add_candidate("retired-edge", None).await.unwrap();
    session.close_session().await.unwrap();
    session.add_candidate("retired-edge", None).await.unwrap();
}

#[tokio::test]
async fn publication_generation_fences_native_control() {
    let fixture = Fixture::new("media");
    let (sink, mut rx) = events();
    let mut options = options(&fixture, sink);
    options.edge_capacity = 3;
    let (session, _parent) = start(options).await;
    let (generation, connection) = ("generation_12345678", "connection_12345678");
    let (_, media) = session
        .prepare_publication(generation, connection, Vec::new())
        .await
        .unwrap();
    assert_eq!(
        media.codec, "vp8",
        "publication did not reuse capture metadata"
    );
    assert!(!media.layers.is_empty());
    assert!(session.publication("stale_generation", connection).is_err());
    session
        .close_publication("stale_generation", connection)
        .await;
    assert!(
        session.publication(generation, connection).is_ok(),
        "stale close retired current"
    );
    session
        .prepare_publication("candidate_generation", "candidate_connection", Vec::new())
        .await
        .expect("candidate displaced the current publication");
    assert!(
        session
            .prepare_publication("third_generation", "third_connection", Vec::new())
            .await
            .is_err(),
        "third publication was admitted"
    );
    session
        .close_publication("candidate_generation", "candidate_connection")
        .await;
    let old = session.publication(generation, connection).unwrap();
    old.close().await;
    assert_eq!(
        session.publication_state(generation, connection),
        Some(PcState::Closed)
    );
    let closed = next_event(&mut rx, Duration::from_secs(3), |event| match &event.kind {
        MediaEventKind::PublicationState {
            state,
            publication_generation,
            ..
        } if state == PcState::Closed.as_str() && publication_generation == generation => event
            .current
            .as_ref()
            .map(|current| (current(), event.current.is_some())),
        _ => None,
    })
    .await
    .expect("publication did not emit its terminal state");
    assert!(
        closed.0,
        "current transport failure lost its delivery authority"
    );
    session.close_publication(generation, connection).await;
    assert!(session.publication(generation, connection).is_err());
    session
        .prepare_publication(generation, connection, Vec::new())
        .await
        .unwrap();
    let replacement = session.publication(generation, connection).unwrap();
    assert!(
        !Arc::ptr_eq(&replacement, &old),
        "same-key replacement reused the retired publication"
    );
    session.close_session().await.unwrap();
}

#[tokio::test]
async fn update_profile_replaces_capture_and_waits_for_installation() {
    let fixture = Fixture::new("media");
    let (sink, mut rx) = events();
    let (session, _parent) = start(options(&fixture, sink)).await;
    next_event(&mut rx, Duration::from_secs(5), capture_active).await;
    let previous = session.inner.lock().stream.clone();
    let mut next = session.inner.lock().profile.clone();
    next.video.width = 854;
    next.video.height = 480;
    session.update_profile(next.clone()).await.unwrap();
    {
        let state = session.inner.lock();
        assert!(
            !Arc::ptr_eq(&state.stream, &previous),
            "profile did not replace the capture"
        );
        assert!(
            state.capture_applied.is_none(),
            "update returned before installation"
        );
        assert_eq!(state.profile, next);
    }
    let mut invalid = next;
    invalid.audio_bitrate = 1;
    assert!(session.update_profile(invalid).await.is_err());
    session.close_session().await.unwrap();
    assert!(
        session
            .update_profile(QualityProfile::default())
            .await
            .is_err()
    );
}
