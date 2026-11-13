//! Capture generation lifecycle: starting and verifying a replacement,
//! committing it under the share lock, and the readiness observation loop.

use std::sync::Arc;

use piik_media_edge::VideoSource;
use piik_native_capture::VideoOptions;
use piik_native_control::media::{MediaError, MediaEventKind, MediaResult, QualityProfile};
use tokio_util::sync::CancellationToken;

use super::{Inner, START_TIMEOUT, State, canceled, unavailable};
use crate::bridge;
use crate::capture::{CaptureState, SharedStream};
/// One profile observation of a capture being prepared.
#[derive(Default)]
struct Observed {
    state: CaptureState,
    error: Option<(MediaError, &'static str)>,
    input: bool,
}

pub(super) const RECOVERABLE_HRESULTS: [&str; 4] = [
    "0x887a0005", // DXGI device removed
    "0x887a0006", // device hung
    "0x887a0007", // device reset
    "0x887a0020", // driver internal error
];

impl Inner {
    /// Starts and verifies a replacement capture. Installed capture always
    /// lives with the share: cancelling a permission request (`ctx`) must not
    /// kill the process it already started.
    pub(crate) async fn prepare_video(
        &self,
        ctx: &CancellationToken,
        mut options: VideoOptions,
        interactive: bool,
        allow_quiet: bool,
    ) -> MediaResult<(SharedStream, CaptureState)> {
        if ctx.is_cancelled() {
            return Err(canceled());
        }
        options.output_groups = self
            .edge_capacity
            .min(piik_native_capture::MAX_OUTPUT_GROUPS);
        let replacement = match self.capture.video(&self.ctx, options.clone()).await {
            Ok(stream) => stream,
            Err(error) => {
                crate::capture::log_profile_failure(
                    &options.profile,
                    "process-start",
                    &error.to_string(),
                );
                return Err(MediaError::failed("native capture could not start"));
            }
        };
        let state = match wait_for_capture_profile(
            ctx,
            &replacement,
            &options.profile,
            &options.codec,
            interactive,
            allow_quiet,
        )
        .await
        {
            Ok(state) => state,
            Err(error) => return Err(bridge::join_close(error, replacement.close().await)),
        };
        if options.remote_input && !state.remote_input {
            let _ = replacement.close().await;
            return Err(MediaError::failed("desktop did not grant remote input"));
        }
        Ok((replacement, state))
    }

    pub(crate) async fn commit_capture(
        &self,
        options: VideoOptions,
        profile: QualityProfile,
        replacement: Option<(SharedStream, CaptureState)>,
        replacement_audio: Option<SharedStream>,
        replace_audio: bool,
    ) -> MediaResult<()> {
        let committed = {
            let mut state = self.lock();
            if state.closed || self.ctx.is_cancelled() {
                Err((replacement, replacement_audio))
            } else {
                Ok(self.commit_locked(
                    &mut state,
                    options,
                    profile,
                    replacement,
                    replacement_audio,
                    replace_audio,
                ))
            }
        };
        let (previous, previous_audio, applied, installed, replacement_audio) = match committed {
            Ok(committed) => committed,
            Err((replacement, audio)) => {
                if let Some((replacement, _)) = replacement {
                    let _ = replacement.close().await;
                }
                if let Some(audio) = audio {
                    let _ = audio.close().await;
                }
                return Err(unavailable());
            }
        };
        if replace_audio && let Some(mixer) = self.mixer() {
            mixer.set_source(replacement_audio);
        }
        if replace_audio && let Some(previous) = previous_audio {
            let _ = previous.close().await;
        }
        let (Some(replacement), Some(applied)) = (installed, applied) else {
            return Ok(());
        };
        let _ = replacement.request_key_frame(-1).await;
        let _ = previous.close().await;
        tokio::select! {
            () = applied.cancelled() => {}
            () = self.ctx.cancelled() => return Err(canceled()),
        }
        if self.ctx.is_cancelled() {
            return Err(canceled());
        }
        Ok(())
    }

    #[allow(clippy::type_complexity)]
    fn commit_locked(
        &self,
        state: &mut State,
        mut options: VideoOptions,
        profile: QualityProfile,
        replacement: Option<(SharedStream, CaptureState)>,
        replacement_audio: Option<SharedStream>,
        replace_audio: bool,
    ) -> (
        SharedStream,
        Option<SharedStream>,
        Option<CancellationToken>,
        Option<SharedStream>,
        Option<SharedStream>,
    ) {
        let previous = state.stream.clone();
        let previous_audio = state.audio_stream.clone();
        let mut applied = None;
        let mut installed = None;
        // A source-audio change retains the current capture and encoder.
        if let Some((replacement, capture_state)) = replacement {
            capture_state.apply_backend(&mut options);
            state.stream = replacement.clone();
            let token = CancellationToken::new();
            state.capture_applied = Some(token.clone());
            state.capture_unavailable = capture_state.unavailable_layers;
            state.video_options = options;
            state.profile = profile;
            self.capture_changed.notify_one();
            applied = Some(token);
            installed = Some(replacement);
        }
        if replace_audio {
            state.audio_stream = replacement_audio.clone();
            self.audio_changed.notify_one();
        }
        (
            previous,
            previous_audio,
            applied,
            installed,
            replacement_audio,
        )
    }

    pub(super) async fn prepare_capture_recovery(&self, failed: SharedStream) -> MediaResult<()> {
        let _update = self.update.lock().await;
        let (options, profile) = {
            let state = self.lock();
            if state.closed || self.ctx.is_cancelled() {
                return Err(unavailable());
            }
            if !Arc::ptr_eq(&state.stream, &failed) {
                return Ok(());
            }
            (state.video_options.clone(), state.profile.clone())
        };
        let prepared = self
            .prepare_video(&self.ctx, options.clone(), false, false)
            .await?;
        self.commit_capture(options, profile, Some(prepared), None, false)
            .await
    }

    /// Only the video reader installs source metadata, after leaving the old
    /// input. The update waits for this exact stream's installation.
    pub(super) async fn install_capture(&self, next: &SharedStream) -> MediaResult<()> {
        let (unavailable, source) = {
            let state = self.lock();
            (state.capture_unavailable.clone(), state.source.clone())
        };
        let Some(source) = source else {
            return Err(MediaError::failed(
                "native capture input arrived before its state",
            ));
        };
        source.begin_generation().await;
        configure_capture_outputs(&source, next.outputs()).await?;
        // Preparation consumed these events; apply them to the new generation
        // before acknowledging it, as the live reader does.
        for layer in unavailable {
            self.disable_capture_layer(next, layer).await?;
        }
        {
            let mut state = self.lock();
            if Arc::ptr_eq(&state.stream, next)
                && let Some(applied) = state.capture_applied.take()
            {
                applied.cancel();
                state.capture_unavailable.clear();
            }
        }
        self.emit(
            MediaEventKind::CaptureState {
                state: "active".into(),
            },
            None,
        );
        Ok(())
    }
}

pub(crate) async fn configure_capture_outputs(
    source: &VideoSource,
    outputs: &[piik_native_capture::OutputProfile],
) -> MediaResult<()> {
    let media = bridge::media_error;
    for (layer, output) in outputs.iter().enumerate().take(2) {
        source
            .set_format(layer, output.width, output.height)
            .await
            .map_err(media)?;
    }
    let ceilings = outputs.iter().map(|output| output.bitrate).collect();
    source.configure_outputs(ceilings).await.map_err(media)?;
    for (layer, output) in outputs.iter().enumerate().skip(2) {
        source
            .set_format(layer, output.width, output.height)
            .await
            .map_err(media)?;
    }
    source.set_lowest_layer_rate_controlled(true).await;
    Ok(())
}

/// A new group receives its budget before activation admits a frame.
/// Startup retains the original until its first active status has arrived.
pub(crate) async fn apply_output_plan(
    stream: &SharedStream,
    plan: &piik_media_edge::OutputPlan,
    active_seen: bool,
) -> Result<(), piik_native_capture::Error> {
    for (layer, bitrate) in plan.bitrates.iter().enumerate() {
        if *bitrate > 0 {
            stream.set_output_bitrate(layer, *bitrate).await?;
        }
    }
    for layer in &plan.recovery_layers {
        stream
            .request_key_frame(i32::try_from(*layer).unwrap_or(i32::MAX))
            .await?;
    }
    if active_seen {
        for (layer, active) in plan.active.iter().enumerate() {
            stream.set_output_active(layer, *active).await?;
        }
    }
    Ok(())
}

fn profile_failure(
    profile: &piik_native_capture::VideoProfile,
    stage: &'static str,
    error: MediaError,
) -> MediaError {
    crate::capture::log_profile_failure(profile, stage, &error.to_string());
    error
}

/// Reads the candidate until it is active.
pub(crate) async fn wait_for_capture_profile(
    ctx: &CancellationToken,
    stream: &SharedStream,
    profile: &piik_native_capture::VideoProfile,
    codec: &str,
    interactive: bool,
    allow_quiet: bool,
) -> MediaResult<CaptureState> {
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Observed>(1);
    {
        let stream = stream.clone();
        let codec = codec.to_owned();
        tokio::spawn(async move { observe_profile(stream, codec, tx).await });
    }
    let timed = !interactive;
    let mut deadline = timed.then(|| tokio::time::Instant::now() + START_TIMEOUT);
    let (mut input_seen, mut starting_seen) = (false, false);
    loop {
        let current = deadline;
        let timeout = async move {
            match current {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            value = rx.recv() => {
                let Some(value) = value else {
                    return Err(profile_failure(profile, "process-ended",
                        MediaError::failed("native capture profile did not become ready")));
                };
                if let Some((error, stage)) = value.error {
                    return Err(profile_failure(profile, stage, error));
                }
                if value.state.state == "active" {
                    return Ok(value.state);
                }
                if allow_quiet && timed {
                    if value.input && !input_seen {
                        if !starting_seen {
                            return Err(profile_failure(profile, "status-invalid", MediaError::failed(
                                "native capture input arrived before its starting state")));
                        }
                        input_seen = true;
                        deadline = Some(tokio::time::Instant::now() + START_TIMEOUT);
                    } else if value.state.state == "starting" && !input_seen {
                        starting_seen = true;
                        deadline = None;
                    }
                }
            }
            () = timeout => {
                return Err(profile_failure(profile, "wait-timeout",
                    MediaError::failed("native capture profile timed out")));
            }
            () = ctx.cancelled() => {
                return Err(profile_failure(profile, "share-stopped",
                    MediaError::failed("native share stopped")));
            }
        }
    }
}

async fn observe_profile(
    stream: SharedStream,
    codec: String,
    tx: tokio::sync::mpsc::Sender<Observed>,
) {
    let fail = |error: &'static str, stage: &'static str| Observed {
        error: Some((MediaError::failed(error), stage)),
        ..Observed::default()
    };
    let outputs = stream.outputs().to_vec();
    let original = crate::capture::original_slot(outputs.len());
    let mut starting = CaptureState::default();
    let mut unavailable: Vec<usize> = Vec::new();
    let mut input_seen = false;
    loop {
        let frame = match stream.read().await {
            Ok(frame) => frame,
            Err(error) => {
                let error = if matches!(error, piik_native_capture::Error::SourceCanceled) {
                    MediaError::SourceCanceled
                } else {
                    MediaError::failed("native capture profile did not become ready")
                };
                let _ = tx
                    .send(Observed {
                        error: Some((error, "process-ended")),
                        ..Observed::default()
                    })
                    .await;
                return;
            }
        };
        use piik_native_capture::FrameKind;
        let layer = usize::from(frame.layer);
        if frame.kind == FrameKind::LayerUnavailable {
            if starting.state != "starting" || layer >= outputs.len() || layer == original {
                let _ = tx
                    .send(fail(
                        "native capture profile has an invalid or unavailable original output",
                        "output-unavailable",
                    ))
                    .await;
                return;
            }
            if !unavailable.contains(&layer) {
                unavailable.push(layer);
            }
            continue;
        }
        if frame.kind == FrameKind::Begin && !input_seen {
            input_seen = true;
            let observed = Observed {
                input: true,
                ..Observed::default()
            };
            if tx.send(observed).await.is_err() {
                return;
            }
        }
        if frame.kind != FrameKind::Status {
            continue;
        }
        let mut state = match crate::capture::decode_capture_state(&frame.data) {
            Ok(state) => state,
            Err(error) => {
                let _ = tx.send(fail(error, "status-invalid")).await;
                return;
            }
        };
        tracing::debug!(event = "capture-profile-observed", state = %state.state,
            expectedCodec = %codec, actualCodec = %state.codec, actualWidth = state.width,
            actualHeight = state.height, actualFPS = state.fps, "piik-client");
        if state.outputs != outputs {
            let _ = tx
                .send(fail(
                    "native capture outputs were not applied",
                    "outputs-mismatch",
                ))
                .await;
            return;
        }
        if state.codec != codec {
            let _ = tx
                .send(fail(
                    "native capture codec was not applied",
                    "profile-mismatch",
                ))
                .await;
            return;
        }
        if state.state == "active" {
            state.adapter_index = starting.adapter_index;
            state.encoder_index = starting.encoder_index;
            state.unavailable_layers = unavailable;
            let _ = tx
                .send(Observed {
                    state,
                    ..Observed::default()
                })
                .await;
            return;
        }
        starting = state.clone();
        if tx
            .send(Observed {
                state,
                ..Observed::default()
            })
            .await
            .is_err()
        {
            return;
        }
    }
}
