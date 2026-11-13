//! Supervises the packaged `cloudflared` Quick Tunnel for public-link rooms.
//!
//! [`Startup::start`] runs bounded startup attempts until the child reports
//! both its public origin and a registered edge connection. Only a pre-ready
//! child that exits is retried; all attempts share one deadline. A ready tunnel
//! survives startup cancellation so the owner can send room-closed and drain
//! its server first, then call [`Process::close`]. On Windows the child runs in
//! a kill-on-close Job Object, so it cannot outlive the App.

mod error;
mod job;
mod output;

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use piik_failure::Reason;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use error::Error;
use output::{Shared, Signal};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
const STARTUP_ATTEMPTS: u32 = 4;
const STARTUP_RETRY_DELAY: Duration = Duration::from_secs(1);
/// Bound for draining the child's output after it exits.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const TEMPORARY_CONFIG: &str = "metrics: 127.0.0.1:0\n";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The `cloudflared.exe` shipped beside the App executable.
pub fn packaged_executable() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    Some(
        executable
            .parent()?
            .join("runtime")
            .join("tunnel")
            .join("cloudflared.exe"),
    )
}

/// Startup parameters for one public tunnel.
#[derive(Debug, Clone)]
pub struct Startup {
    executable: PathBuf,
    local_origin: String,
    temporary_directory: Option<PathBuf>,
    timeout: Duration,
}

impl Startup {
    /// `local_origin` must be `http://<loopback IP>:<port>` with no path.
    pub fn new(executable: impl Into<PathBuf>, local_origin: impl Into<String>) -> Self {
        Self {
            executable: executable.into(),
            local_origin: local_origin.into(),
            temporary_directory: None,
            timeout: STARTUP_TIMEOUT,
        }
    }

    /// Directory for the per-attempt cloudflared config; the system temporary
    /// directory by default.
    pub fn temporary_directory(mut self, directory: impl Into<PathBuf>) -> Self {
        self.temporary_directory = Some(directory.into());
        self
    }

    /// Shared deadline for all startup attempts; 30 seconds by default.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Starts the tunnel. Canceling `stop` during startup retires the child and
    /// returns [`Error::is_canceled`]; it does not affect a ready tunnel.
    pub async fn start(&self, stop: &CancellationToken) -> Result<Process, Error> {
        if stop.is_cancelled() {
            return Err(Error::canceled());
        }
        validate_executable(&self.executable)
            .map_err(|error| error.with_reason(Reason::TunnelUnavailable))?;
        validate_local_origin(&self.local_origin)
            .map_err(|error| error.with_reason(Reason::TunnelStart))?;
        let window = Window {
            stop: stop.clone(),
            deadline: Instant::now() + self.timeout,
        };
        let mut attempt = 1;
        let mut error = loop {
            let error = match self.attempt(&window).await {
                Ok(process) => return Ok(process),
                Err(error) => error,
            };
            if window.error().is_some() || !error.startup_exited() || attempt == STARTUP_ATTEMPTS {
                break error;
            }
            // Only a retired, pre-ready child is retried. Ready tunnel recovery
            // remains cloudflared's job.
            tracing::debug!(
                event = "startup-retry",
                attempt = attempt + 1,
                total = STARTUP_ATTEMPTS,
                "public-tunnel"
            );
            tokio::select! {
                biased;
                _ = window.ended() => {}
                _ = tokio::time::sleep(STARTUP_RETRY_DELAY) => {}
            }
            // No child is live during the retry pause; stopping here is clean.
            if let Some(ended) = window.error() {
                break ended;
            }
            attempt += 1;
        };
        if let Some(ended) = window.error() {
            if !error.is_bare() {
                error = ended.join(error);
            }
            if !stop.is_cancelled() {
                error = Error::message(format!(
                    "public invitation service did not connect within {} seconds; check your Internet connection and reopen Piik to try again",
                    self.timeout.as_secs()
                ))
                .with_reason(Reason::TunnelTimeout)
                .join(error);
            }
        }
        let parent_cancellation = stop.is_cancelled() && error.is_canceled();
        if !parent_cancellation && error.reason().is_none() {
            let reason = if error.startup_exited() {
                Reason::TunnelExited
            } else {
                Reason::TunnelStart
            };
            error = error.with_reason(reason);
        }
        Err(error)
    }

    async fn attempt(&self, window: &Window) -> Result<Process, Error> {
        if let Some(error) = window.error() {
            return Err(error);
        }
        let config = write_temporary_config(self.temporary_directory.as_deref())?;
        let mut command = Command::new(&self.executable);
        command
            .arg("tunnel")
            .arg("--config")
            .arg(&*config)
            .args(["--no-autoupdate", "--loglevel", "info", "--output", "json"])
            // Quick Tunnels otherwise force QUIC. Let cloudflared try TCP after
            // the first failed edge attempt, within the bounded startup window.
            .args(["--protocol", "auto", "--max-edge-addr-retries", "0"])
            .arg("--url")
            .arg(&self.local_origin)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        command.creation_flags(CREATE_NO_WINDOW);
        let job =
            job::Job::new().map_err(|error| Error::io("public tunnel could not start", error))?;
        let mut child = command
            .spawn()
            .map_err(|error| Error::io("public tunnel could not start", error))?;
        job::contain(&job, &mut child)
            .map_err(|error| Error::io("public tunnel could not start", error))?;
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            let _ = child.start_kill();
            return Err(Error::message("public tunnel output is unavailable"));
        };

        let shared = Arc::new(Mutex::new(Shared::default()));
        let (signals, mut received) = mpsc::unbounded_channel();
        let readers = [
            tokio::spawn(output::read(
                stdout,
                false,
                Arc::clone(&shared),
                signals.clone(),
            )),
            tokio::spawn(output::read(stderr, true, Arc::clone(&shared), signals)),
        ];
        // The owner closes a ready tunnel explicitly after its ordered shutdown;
        // startup cancellation cannot kill that transport ahead of it.
        let kill = CancellationToken::new();
        let done = CancellationToken::new();
        let process = Process {
            origin: String::new(),
            kill: kill.clone(),
            done: done.clone(),
            shared: Arc::clone(&shared),
        };
        tokio::spawn(supervise(child, job, config, readers, kill, done, shared));

        let mut process = process;
        let mut origin = None;
        let mut registered = false;
        let mut output_open = true;
        while origin.is_none() || !registered {
            tokio::select! {
                signal = received.recv(), if output_open => match signal {
                    Some(Signal::Origin(found)) => { origin.get_or_insert(found); }
                    Some(Signal::Registered) => registered = true,
                    None => output_open = false,
                },
                _ = process.done.cancelled() => return Err(process.startup_exited()),
                ended = window.ended() => return Err(process.stop_startup(ended).await),
            }
        }
        if let Some(ended) = window.error() {
            return Err(process.stop_startup(ended).await);
        }
        if process.done.is_cancelled() {
            return Err(process.startup_exited());
        }
        process.origin = origin.unwrap_or_default();
        Ok(process)
    }
}

/// Waits for the child, classifies its exit before draining output, then
/// removes its config and marks the process retired.
async fn supervise(
    mut child: tokio::process::Child,
    job: job::Job,
    config: tempfile::TempPath,
    readers: [tokio::task::JoinHandle<()>; 2],
    kill: CancellationToken,
    done: CancellationToken,
    shared: Arc<Mutex<Shared>>,
) {
    let status = tokio::select! {
        status = child.wait() => status,
        _ = kill.cancelled() => {
            job.terminate();
            let _ = child.start_kill();
            child.wait().await
        }
    };
    // Classify the observed exit first: later owner cleanup cannot turn a
    // dependency failure into a successful cancellation.
    let failure = if kill.is_cancelled() {
        None
    } else {
        match status {
            Ok(status) if status.success() => None,
            Ok(status) => Some(output::ExitFailure {
                code: status.code(),
                text: match status.code() {
                    Some(code) => format!("exit status {code}"),
                    None => status.to_string(),
                },
            }),
            Err(error) => Some(output::ExitFailure {
                code: None,
                text: format!("public tunnel wait failed: {error}"),
            }),
        }
    };
    let failed = failure.is_some();
    lock(&shared).failure = failure;
    tracing::debug!(event = "child-exited", failed, "public-tunnel");
    // Join the readers before reporting: the owner may close its recorder next.
    let [stdout, stderr] = readers;
    let (stdout_abort, stderr_abort) = (stdout.abort_handle(), stderr.abort_handle());
    let joined = async {
        let _ = stdout.await;
        let _ = stderr.await;
    };
    if tokio::time::timeout(SHUTDOWN_TIMEOUT, joined)
        .await
        .is_err()
    {
        stdout_abort.abort();
        stderr_abort.abort();
    }
    {
        let mut shared = lock(&shared);
        let detail = shared.last_detail.clone();
        if let Some(failure) = shared.failure.as_mut()
            && !detail.is_empty()
        {
            failure.text = format!("{}\nlast public invitation detail: {detail}", failure.text);
        }
    }
    // Close the job (killing any descendant) and remove the config first.
    drop(job);
    drop(config);
    done.cancel();
}

fn lock(shared: &Mutex<Shared>) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One running tunnel child. Dropping it stops the child; prefer
/// [`Process::close`] to observe its result.
pub struct Process {
    origin: String,
    kill: CancellationToken,
    done: CancellationToken,
    shared: Arc<Mutex<Shared>>,
}

impl Process {
    /// `https://<name>.trycloudflare.com`, without a path.
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// Completes once the child has exited and its output has drained.
    pub async fn done(&self) {
        self.done.cancelled().await;
    }

    pub fn is_done(&self) -> bool {
        self.done.is_cancelled()
    }

    /// The child's unexpected exit, once retired. Owner-requested shutdown is
    /// not an error.
    pub fn err(&self) -> Option<Error> {
        if !self.done.is_cancelled() {
            return None;
        }
        lock(&self.shared).failure.clone().map(Error::exit)
    }

    /// Stops the child and waits until it is retired. Repeated calls return
    /// the same result.
    pub async fn close(&self) -> Result<(), Error> {
        self.kill.cancel();
        self.done.cancelled().await;
        self.err().map_or(Ok(()), Err)
    }

    fn startup_exited(&self) -> Error {
        self.kill.cancel();
        let exited = Error::exited_before_connecting();
        match self.err() {
            Some(cause) => exited.join(cause),
            None => exited,
        }
    }

    async fn stop_startup(&self, cause: Error) -> Error {
        let deadline = cause.is_deadline_exceeded();
        match self.close().await {
            Err(close) => cause.join(close),
            Ok(()) => {
                // Killing an unready child at the deadline is clean shutdown,
                // but its last dependency error still explains the failure.
                let detail = lock(&self.shared).last_detail.clone();
                if deadline && !detail.is_empty() {
                    cause.line(format!("last public invitation detail: {detail}"))
                } else {
                    cause
                }
            }
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        self.kill.cancel();
    }
}

impl std::fmt::Debug for Process {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Process")
            .field("origin", &self.origin)
            .field("done", &self.is_done())
            .finish_non_exhaustive()
    }
}

/// Parent cancellation and the shared startup deadline.
struct Window {
    stop: CancellationToken,
    deadline: Instant,
}

impl Window {
    fn error(&self) -> Option<Error> {
        if self.stop.is_cancelled() {
            Some(Error::canceled())
        } else if Instant::now() >= self.deadline {
            Some(Error::deadline_exceeded())
        } else {
            None
        }
    }

    async fn ended(&self) -> Error {
        tokio::select! {
            biased;
            _ = self.stop.cancelled() => Error::canceled(),
            _ = tokio::time::sleep_until(self.deadline) => Error::deadline_exceeded(),
        }
    }
}

fn write_temporary_config(directory: Option<&Path>) -> Result<tempfile::TempPath, Error> {
    use std::io::Write;

    let directory = directory.map_or_else(std::env::temp_dir, Path::to_path_buf);
    let mut file = tempfile::Builder::new()
        .prefix("piik-cloudflared-")
        .suffix(".yml")
        .tempfile_in(directory)
        .map_err(|error| Error::io("public tunnel temporary file could not be created", error))?;
    file.write_all(TEMPORARY_CONFIG.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| Error::io("public tunnel temporary file could not be written", error))?;
    // Closing the handle lets the child read it; the path is removed on drop.
    Ok(file.into_temp_path())
}

fn validate_executable(path: &Path) -> Result<(), Error> {
    let metadata = std::fs::metadata(path).map_err(|error| {
        Error::io(
            "packaged public tunnel is unavailable; extract the complete Piik archive again",
            error,
        )
    })?;
    if !metadata.is_file() {
        return Err(Error::message(
            "packaged public tunnel is not a file; extract the complete Piik archive again",
        ));
    }
    Ok(())
}

/// Accepts only `http://<loopback IP>:<port>` without credentials, path,
/// query or fragment.
fn validate_local_origin(raw: &str) -> Result<(), Error> {
    let invalid = || Error::message("public tunnel Local origin is invalid");
    let scheme_end = raw.find("://").ok_or_else(invalid)?;
    if !raw[..scheme_end].eq_ignore_ascii_case("http") {
        return Err(invalid());
    }
    let authority = &raw[scheme_end + 3..];
    if authority.contains(['/', '?', '#', '@']) {
        return Err(invalid());
    }
    let (host, port) = match authority.strip_prefix('[') {
        Some(rest) => {
            let (host, rest) = rest.split_once(']').ok_or_else(invalid)?;
            (host, rest.strip_prefix(':').ok_or_else(invalid)?)
        }
        None => authority.rsplit_once(':').ok_or_else(invalid)?,
    };
    let ip: std::net::IpAddr = host.parse().map_err(|_| invalid())?;
    if !ip.is_loopback() || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid());
    }
    match port.parse::<u32>() {
        Ok(1..=65_535) => Ok(()),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests;
