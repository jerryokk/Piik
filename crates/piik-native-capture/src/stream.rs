//! Capture process supervision: startup, framed reads, control writes and one
//! bounded retirement path shared by explicit close and owner cancellation.

mod audio;
mod options;

use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, OnceCell, watch};
use tokio_util::sync::CancellationToken;

use crate::command::{forward_stderr, helper_command};
use crate::protocol::{Frame, MAX_OUTPUTS, read_frame, write_frame};
use crate::sources::zero_when_empty;
use crate::{Error, ExitCause, Failure};
use options::{append_output_arguments, valid_video_preference};

pub use audio::{start_audio, start_microphone, start_system_audio};
#[cfg(test)]
pub(crate) use audio::{start_audio_stream_with, validate_audio_ready_frame};
pub use options::{
    EncodedVideoOptions, OutputProfile, VideoOptions, VideoProfile, screen_share_outputs,
};

pub(crate) const CAPTURE_STOP_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_PROBE_ERROR_BYTES: usize = 4 * 1024;
const MIN_OUTPUT_BITRATE: u32 = 1_000; // Codec rate APIs use whole kbps.
pub const MAX_OUTPUT_GROUPS: usize = MAX_OUTPUTS - 2;
/// H.264 Level 5.2 Constrained Baseline video bitrate.
pub const MAX_VIDEO_BITRATE: u32 = 240_000_000;
const LOG_TARGET: &str = "piik_native_capture";

pub async fn start_video(
    parent: &CancellationToken,
    executable: &Path,
    options: &VideoOptions,
) -> Result<Stream, Error> {
    if !options.target.valid()
        || !options.profile.valid()
        || !matches!(options.codec.as_str(), "auto" | "h264" | "vp8")
        || options.output_groups > MAX_OUTPUT_GROUPS
    {
        return Err(Error::Invalid("native video target is invalid"));
    }
    let mut environment = Vec::new();
    if options.remote_input {
        environment.push(("PIIK_CAPTURE_REMOTE_INPUT", "1".to_owned()));
    }
    let target = &options.target;
    let profile = &options.profile;
    let mut arguments: Vec<String> = vec![
        "--capture-video".into(),
        target.kind.clone(),
        target.source_id.clone(),
        target.pid.to_string(),
        zero_when_empty(&target.creation_time).into(),
        "--adapter-index".into(),
        options.adapter_index.to_string(),
        "--mft-index".into(),
        options.encoder_index.to_string(),
        "--width".into(),
        profile.width.to_string(),
        "--height".into(),
        profile.height.to_string(),
        "--fps".into(),
        profile.framerate.to_string(),
        "--bitrate".into(),
        profile.bitrate.to_string(),
        "--preference".into(),
        profile.preference.clone(),
        "--codec".into(),
        options.codec.clone(),
        "--protocol-v7".into(),
    ];
    let mut outputs = screen_share_outputs(profile);
    outputs.extend(std::iter::repeat_n(outputs[0], options.output_groups));
    append_output_arguments(&mut arguments, &outputs)?;
    if options.show_capture_border {
        arguments.push("--show-capture-border".into());
    }
    let mut active = [false; MAX_OUTPUTS];
    active[..2].fill(true);
    let settings = StreamSettings {
        outputs,
        remote_input: options.remote_input,
        active,
    };
    start_stream(parent, executable, &arguments, &environment, settings).await
}

pub async fn start_encoded_video(
    parent: &CancellationToken,
    executable: &Path,
    options: &EncodedVideoOptions,
) -> Result<Stream, Error> {
    if !matches!(options.codec.as_str(), "h264" | "vp8")
        || !valid_video_preference(&options.preference)
    {
        return Err(Error::Invalid("native encoded video options are invalid"));
    }
    let mut arguments: Vec<String> = vec![
        "--encoded-video".into(),
        "--codec".into(),
        options.codec.clone(),
        "--adapter-index".into(),
        options.adapter_index.to_string(),
        "--mft-index".into(),
        options.encoder_index.to_string(),
        "--preference".into(),
        options.preference.clone(),
        "--protocol-v7".into(),
    ];
    append_output_arguments(&mut arguments, &options.outputs)?;
    let settings = StreamSettings {
        outputs: options.outputs.clone(),
        ..StreamSettings::default()
    };
    start_stream(parent, executable, &arguments, &[], settings).await
}

#[derive(Debug, Clone, Default)]
pub(crate) struct StreamSettings {
    pub(crate) outputs: Vec<OutputProfile>,
    pub(crate) remote_input: bool,
    pub(crate) active: [bool; MAX_OUTPUTS],
}

/// How the helper ended, published once before `done` resolves.
#[derive(Debug, Clone)]
pub(crate) struct Outcome {
    exit: Result<(), ExitCause>,
    failure: Option<Failure>,
}

type Reader = Box<dyn AsyncRead + Send + Unpin>;
type Writer = Box<dyn AsyncWrite + Send + Unpin>;

struct Control {
    key: Option<Writer>,
    closed: bool,
    active: [bool; MAX_OUTPUTS],
    bitrates: [u32; MAX_OUTPUTS],
}

struct Inner {
    remote_input: bool,
    outputs: Vec<OutputProfile>,
    // Cancelling retires the process: the supervisor kills it.
    cancel: CancellationToken,
    input: Arc<Mutex<Reader>>,
    control: Mutex<Control>,
    done: watch::Receiver<Option<Outcome>>,
    closed: OnceCell<()>,
    read_ended: AtomicBool,
    capture_id: String,
}

impl Drop for Inner {
    /// A stream dropped without `close` must not leak a running helper.
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// One running helper process and its framed stdout/stdin pipes. Cheap to
/// clone; every clone refers to the same process.
#[derive(Clone)]
pub struct Stream {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Stream {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Stream")
            .field("capture", &self.inner.capture_id)
            .field("outputs", &self.inner.outputs)
            .finish_non_exhaustive()
    }
}

fn base36(mut value: u128) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut output = Vec::new();
    loop {
        output.push(DIGITS[(value % 36) as usize]);
        value /= 36;
        if value == 0 {
            break;
        }
    }
    output.reverse();
    String::from_utf8(output).unwrap_or_default()
}

fn valid_stage(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
}

fn valid_hresult(value: &str) -> bool {
    value.len() == 10
        && value.starts_with("0x")
        && value.as_bytes()[2..].iter().all(u8::is_ascii_hexdigit)
}

/// Extracts the first valid `stage=` and `hresult=` codes from helper stderr.
pub(crate) fn capture_failure_codes(stderr: &[u8]) -> (String, String) {
    let (mut stage, mut hresult) = (String::new(), String::new());
    for line in String::from_utf8_lossy(stderr).split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        if key == "stage" && stage.is_empty() && valid_stage(value) {
            stage = value.to_owned();
        } else if key == "hresult" && hresult.is_empty() && valid_hresult(value) {
            hresult = value.to_ascii_lowercase();
        }
    }
    (stage, hresult)
}

/// A real child failure may race owner cancellation. Keep its fixed codes,
/// while ordinary cancellation without a reported failure stays silent.
pub(crate) fn log_capture_failure(failed: bool, exit_code: i32, stderr: &[u8], canceled: bool) {
    if !failed || !tracing::enabled!(target: LOG_TARGET, tracing::Level::DEBUG) {
        return;
    }
    let (stage, hresult) = capture_failure_codes(stderr);
    if canceled && stage.is_empty() && hresult.is_empty() {
        return;
    }
    tracing::debug!(
        target: LOG_TARGET,
        event = "capture-process-failed",
        exitCode = exit_code,
        stage = %stage,
        hresult = %hresult,
        canceled = canceled,
        "piik-client"
    );
}

fn launch_mode(arguments: &[String]) -> &str {
    match arguments.first().map(String::as_str) {
        Some(mode @ ("--capture-video" | "--capture-audio" | "--encoded-video")) => &mode[2..],
        _ => "capture",
    }
}

pub(crate) async fn start_stream(
    parent: &CancellationToken,
    executable: &Path,
    arguments: &[String],
    environment: &[(&str, String)],
    settings: StreamSettings,
) -> Result<Stream, Error> {
    if executable.as_os_str().is_empty() {
        return Err(Error::Invalid("native capture process is unavailable"));
    }
    if parent.is_cancelled() {
        return Err(Error::Canceled);
    }
    let started = Instant::now();
    // A diagnostic-only process label correlates stderr while old and new captures overlap.
    let capture_id = base36(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    let mut command = helper_command(executable, arguments);
    command.envs(
        environment
            .iter()
            .map(|(key, value)| (*key, value.as_str())),
    );
    if tracing::enabled!(target: LOG_TARGET, tracing::Level::DEBUG) {
        command.env("PIIK_CAPTURE_DEBUG", "1");
    }
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| {
        tracing::debug!(target: LOG_TARGET, capture = %capture_id, event = "capture-process-start-failed",
            error = %piik_diagnostics::safe_text(&error.to_string()), "piik-client");
        Error::io("start native capture process", error)
    })?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.start_kill();
        return Err(Error::Invalid(
            "native capture process pipes are unavailable",
        ));
    };
    let pid = child.id().unwrap_or_default();
    let mode = launch_mode(arguments).to_owned();
    tracing::debug!(target: LOG_TARGET, capture = %capture_id, pid, event = "capture-process-started", mode = %mode, "piik-client");
    let stderr = forward_stderr(
        stderr,
        format!("native-capture/{capture_id}"),
        MAX_PROBE_ERROR_BYTES,
    );
    let cancel = CancellationToken::new();
    let (done_tx, done) = watch::channel(None);
    let stream = Stream::from_parts(
        Box::new(stdout),
        Box::new(stdin),
        cancel.clone(),
        done,
        settings,
        capture_id.clone(),
    );
    let supervisor_cancel = cancel;
    tokio::spawn(async move {
        let status = tokio::select! {
            status = child.wait() => status,
            () = supervisor_cancel.cancelled() => {
                let _ = child.start_kill();
                child.wait().await
            }
        };
        let tail = stderr.await.unwrap_or_default();
        let (stage, hresult) = capture_failure_codes(&tail);
        let canceled = supervisor_cancel.is_cancelled();
        let exit = match status {
            Ok(status) if status.success() => Ok(()),
            Ok(status) => Err(ExitCause::Status(status)),
            Err(error) => Err(ExitCause::Wait(error.to_string())),
        };
        let exit_code = exit.as_ref().err().map_or(0, ExitCause::exit_code);
        // Preserve validated codes and the exit cause, never the helper's raw detail.
        let failure = match &exit {
            Err(_) if !stage.is_empty() || !hresult.is_empty() || !canceled => Some(Failure {
                stage,
                hresult,
                exit_code,
            }),
            _ => None,
        };
        log_capture_failure(exit.is_err(), exit_code, &tail, canceled);
        tracing::debug!(target: LOG_TARGET, capture = %capture_id, pid, event = "capture-process-ended", mode = %mode,
            durationMs = started.elapsed().as_millis() as u64, exitCode = exit_code, canceled, "piik-client");
        let _ = done_tx.send(Some(Outcome { exit, failure }));
    });
    // Owner cancellation requests the same bounded Q/close sequence as an
    // explicit stop. Killing first can leave platform capture state behind.
    let weak = Arc::downgrade(&stream.inner);
    let mut done = stream.inner.done.clone();
    let parent = parent.clone();
    tokio::spawn(async move {
        let canceled = tokio::select! {
            () = parent.cancelled() => true,
            // The watch guard is dropped inside this arm, before any later await.
            _ = done.wait_for(Option::is_some) => false,
        };
        if canceled {
            close_weak(weak).await;
        }
    });
    Ok(stream)
}

async fn close_weak(inner: Weak<Inner>) {
    if let Some(inner) = inner.upgrade() {
        let _ = Stream { inner }.close().await;
    }
}

impl Stream {
    fn from_parts(
        input: Reader,
        key: Writer,
        cancel: CancellationToken,
        done: watch::Receiver<Option<Outcome>>,
        settings: StreamSettings,
        capture_id: String,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                remote_input: settings.remote_input,
                outputs: settings.outputs,
                cancel,
                input: Arc::new(Mutex::new(input)),
                control: Mutex::new(Control {
                    key: Some(key),
                    closed: false,
                    active: settings.active,
                    bitrates: [0; MAX_OUTPUTS],
                }),
                done,
                closed: OnceCell::new(),
                read_ended: AtomicBool::new(false),
                capture_id,
            }),
        }
    }

    /// Reads the next helper envelope. Reads are serialized; dropping an
    /// in-flight read desynchronizes the stream, so callers retire it instead.
    pub async fn read(&self) -> Result<Frame, Error> {
        let result = read_frame(&mut **self.inner.input.lock().await).await;
        if let Err(error) = &result
            && !self.inner.read_ended.swap(true, Ordering::Relaxed)
        {
            tracing::debug!(target: LOG_TARGET, capture = %self.inner.capture_id, event = "capture-stream-ended",
                eof = matches!(error, Error::Eof), canceled = self.inner.cancel.is_cancelled(),
                error = %piik_diagnostics::safe_text(&error.to_string()), "piik-client");
        }
        result
    }

    /// Sends one encoded access unit to an encoded-video helper. It shares the
    /// control lock; capture status and PCM remain producer outputs only.
    pub async fn write_frame(&self, frame: &Frame) -> Result<(), Error> {
        if !frame.kind.is_video() {
            return Err(Error::Invalid("native encoded input is not video"));
        }
        let mut control = self.inner.control.lock().await;
        write_control_frame(&mut control, frame).await
    }

    pub fn outputs(&self) -> &[OutputProfile] {
        &self.inner.outputs
    }

    /// Requests recovery for one output, or every output with `-1`.
    pub async fn request_key_frame(&self, layer: i32) -> Result<(), Error> {
        let outputs = self.inner.outputs.len();
        if outputs == 0 || layer < -1 || usize::try_from(layer).is_ok_and(|layer| layer >= outputs)
        {
            return Err(Error::Invalid("native recovery output is invalid"));
        }
        self.write_control(&format!("K {layer}")).await
    }

    pub async fn set_output_active(&self, layer: usize, active: bool) -> Result<(), Error> {
        if layer >= self.inner.outputs.len() {
            return Err(Error::Invalid("native active output is invalid"));
        }
        let mut control = self.inner.control.lock().await;
        if control.closed {
            return Err(Error::Closed);
        }
        if control.active[layer] == active {
            return Ok(());
        }
        write_control_frame(
            &mut control,
            &Frame::control(&format!("A {layer} {}", u8::from(active))),
        )
        .await?;
        control.active[layer] = active;
        Ok(())
    }

    pub async fn set_output_bitrate(&self, layer: usize, bitrate: u32) -> Result<(), Error> {
        let Some(output) = self.inner.outputs.get(layer) else {
            return Err(Error::Invalid("native output bitrate is invalid"));
        };
        if !(MIN_OUTPUT_BITRATE..=output.bitrate).contains(&bitrate) {
            return Err(Error::Invalid("native output bitrate is invalid"));
        }
        let mut control = self.inner.control.lock().await;
        if control.closed {
            return Err(Error::Closed);
        }
        if control.bitrates[layer] == bitrate {
            return Ok(());
        }
        write_control_frame(
            &mut control,
            &Frame::control(&format!("B {layer} {bitrate}")),
        )
        .await?;
        control.bitrates[layer] = bitrate;
        Ok(())
    }

    async fn write_control(&self, command: &str) -> Result<(), Error> {
        let mut control = self.inner.control.lock().await;
        write_control_frame(&mut control, &Frame::control(command)).await
    }

    /// Remote input uses the selected capture's inherited control pipe; it never
    /// opens a separate OS session, listener or network route. Availability does
    /// not wait behind a blocked media-control write.
    pub fn remote_input(&self) -> bool {
        self.inner.remote_input && !self.inner.cancel.is_cancelled()
    }

    /// A stalled helper must not hold the input grant indefinitely: the same
    /// capture is retired if even a small control command cannot make progress.
    pub async fn send_remote_input(&self, command: &str) -> Result<(), Error> {
        let cancel = self.inner.cancel.clone();
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep(CAPTURE_STOP_TIMEOUT).await;
            cancel.cancel();
        });
        let result = if self.remote_input() {
            self.write_control(&format!("I {command}")).await
        } else {
            Err(Error::Invalid("capture input is unavailable"))
        };
        watchdog.abort();
        if result.is_err() {
            self.inner.cancel.cancel();
        }
        result
    }

    /// Resolves once the helper process has been retired, with its exit cause.
    pub async fn done(&self) -> Result<(), ExitCause> {
        let mut done = self.inner.done.clone();
        match done.wait_for(Option::is_some).await {
            Ok(outcome) => outcome
                .as_ref()
                .map_or(Ok(()), |outcome| outcome.exit.clone()),
            Err(_) => Ok(()),
        }
    }

    /// Requests a graceful stop (`Q`), then retires the process within the stop
    /// budget. Returns the retained helper failure, if any; repeated and
    /// concurrent calls share the first retirement.
    pub async fn close(&self) -> Result<(), Error> {
        self.inner.closed.get_or_init(|| self.retire()).await;
        let outcome = self.inner.done.borrow().clone();
        match outcome.and_then(|outcome| outcome.failure.zip(outcome.exit.err())) {
            Some((details, cause)) => Err(Error::Process { details, cause }),
            None => Ok(()),
        }
    }

    async fn retire(&self) {
        let inner = &self.inner;
        // Start the stop budget before waiting for a possibly blocked control write.
        let budget = inner.cancel.clone();
        let watchdog = tokio::spawn(async move {
            tokio::time::sleep(CAPTURE_STOP_TIMEOUT).await;
            budget.cancel();
        });
        // A retired consumer must not leave the producer blocked on stdout before
        // it can read Q. Any in-flight frame finishes before the rest is discarded.
        let input = inner.input.clone();
        let drained = tokio::spawn(async move {
            let mut input = input.lock().await;
            let _ = tokio::io::copy(&mut **input, &mut tokio::io::sink()).await;
        });
        {
            let mut control = inner.control.lock().await;
            control.closed = true;
            if let Some(mut key) = control.key.take() {
                let _ = write_frame(&mut key, &Frame::control("Q")).await;
                let _ = key.flush().await;
                let _ = key.shutdown().await;
            }
        }
        let _ = self.done().await;
        inner.cancel.cancel();
        let _ = drained.await;
        watchdog.abort();
    }
}

#[cfg(test)]
impl Stream {
    /// An in-memory stream whose retirement completes when `done` is sent.
    pub(crate) fn for_test(
        input: impl AsyncRead + Send + Unpin + 'static,
        key: impl AsyncWrite + Send + Unpin + 'static,
        outputs: Vec<OutputProfile>,
    ) -> (Self, watch::Sender<Option<Outcome>>) {
        let (done_tx, done) = watch::channel(None);
        let settings = StreamSettings {
            outputs,
            ..StreamSettings::default()
        };
        let stream = Self::from_parts(
            Box::new(input),
            Box::new(key),
            CancellationToken::new(),
            done,
            settings,
            "test".into(),
        );
        (stream, done_tx)
    }
}

#[cfg(test)]
mod tests;

async fn write_control_frame(control: &mut Control, frame: &Frame) -> Result<(), Error> {
    if control.closed {
        return Err(Error::Closed);
    }
    let key = control.key.as_mut().ok_or(Error::Closed)?;
    write_frame(key, frame).await?;
    // Surface pipe failure for this envelope rather than the next one.
    key.flush()
        .await
        .map_err(|error| Error::io("write native capture frame", error))
}
