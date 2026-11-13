//! One-shot helper invocations and stderr forwarding.

use std::ffi::OsStr;
use std::io::Write as _;
use std::path::Path;
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use piik_diagnostics::LineWriter;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, ChildStderr, Command};
use tokio::task::JoinHandle;

pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[derive(Debug, thiserror::Error)]
pub enum CommandError {
    #[error("start native capture process: {0}")]
    Start(#[source] std::io::Error),
    #[error("read native capture process output: {0}")]
    Read(#[source] std::io::Error),
    #[error("native capture process output exceeded its bound")]
    OutputExceeded,
    #[error("native capture process timed out")]
    Timeout,
    #[error("native capture process exited: {0}")]
    Exit(ExitStatus),
}

/// A helper command with no console window that dies with its owner.
pub(crate) fn helper_command<I, S>(executable: &Path, arguments: I) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut command = Command::new(executable);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    command.creation_flags(CREATE_NO_WINDOW);
    command
}

/// Keeps the newest `limit` bytes so terminal failure codes survive a long
/// stream of encoder statistics.
#[derive(Debug)]
pub(crate) struct TailBuffer {
    bytes: Vec<u8>,
    limit: usize,
}

impl TailBuffer {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
        }
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        if data.len() >= self.limit {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&data[data.len() - self.limit..]);
            return;
        }
        let overflow = (self.bytes.len() + data.len()).saturating_sub(self.limit);
        self.bytes.drain(..overflow);
        self.bytes.extend_from_slice(data);
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

/// Reads until EOF, failing once more than `limit` bytes arrive.
pub(crate) async fn read_bounded<R: AsyncRead + Unpin>(
    input: &mut R,
    limit: usize,
) -> Result<Vec<u8>, CommandError> {
    let mut output = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let count = input.read(&mut chunk).await.map_err(CommandError::Read)?;
        if count == 0 {
            return Ok(output);
        }
        if output.len() + count > limit {
            return Err(CommandError::OutputExceeded);
        }
        output.extend_from_slice(&chunk[..count]);
    }
}

/// Forwards helper stderr as bounded dependency records, optionally retaining
/// a bounded tail for failure codes. Never stops reading, so the child cannot
/// block on a full diagnostic pipe.
pub(crate) fn forward_stderr(
    stderr: ChildStderr,
    scope: String,
    tail_limit: usize,
) -> JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut lines = LineWriter::new(scope);
        let mut tail = TailBuffer::new(tail_limit);
        let mut chunk = [0u8; 4096];
        while let Ok(count) = stderr.read(&mut chunk).await {
            if count == 0 {
                break;
            }
            tail.push(&chunk[..count]);
            let _ = lines.write_all(&chunk[..count]);
        }
        lines.close();
        tail.into_bytes()
    })
}

/// Runs a short helper query within [`PROBE_TIMEOUT`], returning its bounded stdout.
pub(crate) async fn run_bounded(
    executable: &Path,
    arguments: &[&str],
    stdout_limit: usize,
    stderr_scope: Option<&str>,
) -> Result<Vec<u8>, CommandError> {
    let mut command = helper_command(executable, arguments);
    command.stdout(Stdio::piped());
    command.stderr(if stderr_scope.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let mut child = command.spawn().map_err(CommandError::Start)?;
    let stderr = match (stderr_scope, child.stderr.take()) {
        (Some(scope), Some(stderr)) => Some(forward_stderr(stderr, scope.to_owned(), 0)),
        _ => None,
    };
    let result = tokio::time::timeout(PROBE_TIMEOUT, collect(&mut child, stdout_limit))
        .await
        .unwrap_or(Err(CommandError::Timeout));
    if result.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
    if let Some(stderr) = stderr {
        let _ = stderr.await;
    }
    result
}

async fn collect(child: &mut Child, limit: usize) -> Result<Vec<u8>, CommandError> {
    let output = match child.stdout.as_mut() {
        Some(stdout) => read_bounded(stdout, limit).await?,
        None => Vec::new(),
    };
    let status = child.wait().await.map_err(CommandError::Read)?;
    if !status.success() {
        return Err(CommandError::Exit(status));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn diagnostic_stderr_overflow_does_not_stop_the_child_reader() {
        let input = b"stage=capture-closed\ndetail=more evidence\n";
        let mut tail = TailBuffer::new(4);
        for chunk in input.chunks(7) {
            tail.push(chunk);
        }
        assert_eq!(
            tail.into_bytes(),
            b"nce\n",
            "diagnostic stderr lost its bounded tail"
        );
        let mut whole = TailBuffer::new(4);
        whole.push(input);
        assert_eq!(whole.into_bytes(), b"nce\n");
        let mut reader = &input[..];
        assert!(
            matches!(
                read_bounded(&mut reader, 4).await,
                Err(CommandError::OutputExceeded)
            ),
            "strict probe output no longer rejects overflow"
        );
    }
}
