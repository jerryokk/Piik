//! The capture streams one share installs and the helper's video state
//! contract.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use piik_native_capture::{
    CaptureTarget, Error, Frame, OutputProfile, Stream, VideoOptions, VideoProfile,
};
use serde::{Deserialize, Serialize};

/// One installed capture generation. Desktop capture is a supervised helper
/// process; tests supply their own streams. Identity is the `Arc` allocation.
pub trait CaptureStream: Send + Sync {
    /// Reads the next envelope. Callers never drop an in-flight read; they
    /// retire the stream instead, which ends the read.
    fn read(&self) -> BoxFuture<'_, Result<Frame, Error>>;
    fn close(&self) -> BoxFuture<'_, Result<(), Error>>;
    fn outputs(&self) -> &[OutputProfile];
    fn request_key_frame(&self, layer: i32) -> BoxFuture<'_, Result<(), Error>>;
    fn set_output_active(&self, layer: usize, active: bool) -> BoxFuture<'_, Result<(), Error>>;
    fn set_output_bitrate(&self, layer: usize, bitrate: u32) -> BoxFuture<'_, Result<(), Error>>;
}

pub type SharedStream = Arc<dyn CaptureStream>;

pub fn same(left: Option<&SharedStream>, right: Option<&SharedStream>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => Arc::ptr_eq(left, right),
        (None, None) => true,
        _ => false,
    }
}

impl CaptureStream for Stream {
    fn read(&self) -> BoxFuture<'_, Result<Frame, Error>> {
        Box::pin(Stream::read(self))
    }
    fn close(&self) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(Stream::close(self))
    }
    fn outputs(&self) -> &[OutputProfile] {
        Stream::outputs(self)
    }
    fn request_key_frame(&self, layer: i32) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(Stream::request_key_frame(self, layer))
    }
    fn set_output_active(&self, layer: usize, active: bool) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(Stream::set_output_active(self, layer, active))
    }
    fn set_output_bitrate(&self, layer: usize, bitrate: u32) -> BoxFuture<'_, Result<(), Error>> {
        Box::pin(Stream::set_output_bitrate(self, layer, bitrate))
    }
}

/// Starts the capture inputs of one share. `ctx` owns the started stream:
/// cancelling it retires the stream.
pub trait CaptureProvider: Send + Sync {
    fn video<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        options: VideoOptions,
    ) -> BoxFuture<'a, Result<SharedStream, Error>>;
    fn audio<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        target: CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) -> BoxFuture<'a, Result<SharedStream, Error>>;
    fn microphone<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        device: String,
    ) -> BoxFuture<'a, Result<SharedStream, Error>>;
}

/// The packaged `piik-capture.exe` helper.
#[derive(Debug, Clone)]
pub struct ProcessCapture {
    pub executable: PathBuf,
}

impl CaptureProvider for ProcessCapture {
    fn video<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        options: VideoOptions,
    ) -> BoxFuture<'a, Result<SharedStream, Error>> {
        Box::pin(async move {
            let stream = piik_native_capture::start_video(ctx, &self.executable, &options).await?;
            Ok(Arc::new(stream) as SharedStream)
        })
    }

    fn audio<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        target: CaptureTarget,
        exclude: Option<CaptureTarget>,
    ) -> BoxFuture<'a, Result<SharedStream, Error>> {
        Box::pin(async move {
            let executable = &self.executable;
            let stream = match exclude {
                Some(exclude) => {
                    piik_native_capture::start_audio(ctx, executable, &exclude, true).await?
                }
                None if matches!(target.kind.as_str(), "display" | "picker") => {
                    piik_native_capture::start_system_audio(ctx, executable).await?
                }
                None => piik_native_capture::start_audio(ctx, executable, &target, false).await?,
            };
            Ok(Arc::new(stream) as SharedStream)
        })
    }

    fn microphone<'a>(
        &'a self,
        ctx: &'a tokio_util::sync::CancellationToken,
        device: String,
    ) -> BoxFuture<'a, Result<SharedStream, Error>> {
        Box::pin(async move {
            let stream =
                piik_native_capture::start_microphone(ctx, &self.executable, &device).await?;
            Ok(Arc::new(stream) as SharedStream)
        })
    }
}

/// The helper's video status.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CaptureState {
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub hardware_only: bool,
    #[serde(default)]
    pub codec: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_index: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adapter_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub adapter_identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoder_index: Option<u32>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub encoder_name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub encoder_identity: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub profile_level_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub width: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub height: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fps: u32,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote_input: bool,
    #[serde(default)]
    pub outputs: Vec<OutputProfile>,
    /// Lower outputs the helper reported unavailable while starting.
    #[serde(skip)]
    pub unavailable_layers: Vec<usize>,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

/// Decodes and validates the helper's video status payload.
pub fn decode_capture_state(payload: &[u8]) -> Result<CaptureState, &'static str> {
    let invalid = "native capture state is invalid";
    let state: CaptureState = serde_json::from_slice(payload).map_err(|_| invalid)?;
    if !matches!(state.state.as_str(), "starting" | "active")
        || !matches!(state.codec.as_str(), "h264" | "vp8")
        || state.hardware_only != (state.codec == "h264")
        || !(1..=6).contains(&state.outputs.len())
    {
        return Err(invalid);
    }
    if !state.outputs.iter().all(OutputProfile::valid) {
        return Err("native capture output profile is invalid");
    }
    if state.state == "starting"
        && (state.adapter_index.is_none()
            || (state.codec == "h264" && state.encoder_index.is_none())
            || state.adapter_name.is_empty()
            || state.adapter_identity.is_empty()
            || state.encoder_name.is_empty()
            || state.encoder_identity.is_empty())
    {
        return Err("native capture starting state is incomplete");
    }
    let original = state.outputs[original_slot(state.outputs.len())];
    // Output profiles are ceilings; aspect-preserving capture may be smaller.
    if state.state == "active"
        && ((state.codec == "h264"
            && !piik_native_capture::supported_h264_profile_level_id(&state.profile_level_id))
            || (state.codec == "vp8" && !state.profile_level_id.is_empty())
            || !piik_native_capture::valid_video_size(state.width, state.height)
            || state.width > original.width
            || state.height > original.height
            || state.fps != original.framerate)
    {
        return Err("native capture active state is incomplete");
    }
    Ok(state)
}

/// The original output's slot: 1 when lower outputs exist.
pub fn original_slot(outputs: usize) -> usize {
    outputs.saturating_sub(1).min(1)
}

impl CaptureState {
    /// Selected hardware is a capture fact, not the page's enumeration
    /// preference. Keep it for quality and source replacement.
    pub fn apply_backend(&self, options: &mut VideoOptions) {
        if let Some(index) = self.adapter_index {
            options.adapter_index = index;
        }
        if let Some(index) = self.encoder_index {
            options.encoder_index = index;
        }
    }
}

/// The profile's video part, for capture failure logging.
pub fn log_profile_failure(profile: &VideoProfile, stage: &str, error: &str) {
    tracing::debug!(
        event = "capture-profile-rejected",
        stage,
        width = profile.width,
        height = profile.height,
        fps = profile.framerate,
        bitrate = profile.bitrate,
        error = %piik_diagnostics::safe_text(error),
        "piik-client"
    );
}
