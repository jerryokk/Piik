//! Audio and microphone helper startup, including the active-state handshake.

use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use super::{Stream, StreamSettings, start_stream};
use crate::Error;
use crate::command::PROBE_TIMEOUT;
use crate::protocol::{Frame, FrameKind};
use crate::sources::{CaptureTarget, valid_device_id, zero_when_empty};

const MICROPHONE_STARTUP_TIMEOUT: Duration = Duration::from_secs(60);

/// Window or display audio; `exclude` captures everything except the window.
pub async fn start_audio(
    parent: &CancellationToken,
    executable: &Path,
    target: &CaptureTarget,
    exclude: bool,
) -> Result<Stream, Error> {
    if !target.valid() || (exclude && target.kind != "window") {
        return Err(Error::Invalid("native audio target is invalid"));
    }
    let kind = if exclude {
        "exclude"
    } else {
        target.kind.as_str()
    };
    let pid = target.pid.to_string();
    let arguments = [
        "--capture-audio",
        kind,
        &pid,
        zero_when_empty(&target.creation_time),
    ];
    start_audio_stream(parent, executable, &arguments, PROBE_TIMEOUT).await
}

pub async fn start_system_audio(
    parent: &CancellationToken,
    executable: &Path,
) -> Result<Stream, Error> {
    let arguments = ["--capture-audio", "display", "0", "0"];
    start_audio_stream(parent, executable, &arguments, PROBE_TIMEOUT).await
}

/// The empty device identity selects the system default. Startup may wait on
/// the OS permission prompt, so it has a longer bound.
pub async fn start_microphone(
    parent: &CancellationToken,
    executable: &Path,
    device_id: &str,
) -> Result<Stream, Error> {
    if !valid_device_id(device_id) {
        return Err(Error::Invalid("microphone device is invalid"));
    }
    let mut arguments = vec!["--capture-microphone"];
    if !device_id.is_empty() {
        arguments.extend(["--device", device_id]);
    }
    start_audio_stream(parent, executable, &arguments, MICROPHONE_STARTUP_TIMEOUT).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AudioCaptureState {
    state: String,
    audio: bool,
}

pub(crate) fn validate_audio_ready_frame(frame: &Frame) -> Result<(), Error> {
    let state = (frame.kind == FrameKind::Status)
        .then(|| serde_json::from_slice::<AudioCaptureState>(&frame.data).ok())
        .flatten();
    match state {
        Some(state) if state.state == "active" && state.audio => Ok(()),
        _ => Err(Error::Invalid(
            "native audio capture returned an invalid ready state",
        )),
    }
}

async fn start_audio_stream(
    parent: &CancellationToken,
    executable: &Path,
    arguments: &[&str],
    timeout: Duration,
) -> Result<Stream, Error> {
    start_audio_stream_with(parent, executable, arguments, &[], timeout).await
}

/// Starts an audio helper and waits for its explicit active state.
pub(crate) async fn start_audio_stream_with(
    parent: &CancellationToken,
    executable: &Path,
    arguments: &[&str],
    environment: &[(&str, String)],
    timeout: Duration,
) -> Result<Stream, Error> {
    let arguments: Vec<String> = arguments.iter().map(|value| (*value).to_owned()).collect();
    let stream = start_stream(
        parent,
        executable,
        &arguments,
        environment,
        StreamSettings::default(),
    )
    .await?;
    let ready = tokio::select! {
        frame = stream.read() => match frame {
            Ok(frame) => validate_audio_ready_frame(&frame),
            Err(_) => Err(Error::Invalid("native audio capture did not become ready")),
        },
        () = tokio::time::sleep(timeout) => Err(Error::Invalid("native audio capture timed out during startup")),
        () = parent.cancelled() => Err(Error::Canceled),
    };
    match ready {
        Ok(()) => Ok(stream),
        Err(error) => {
            let _ = stream.close().await;
            Err(error)
        }
    }
}
