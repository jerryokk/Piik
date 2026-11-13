//! Reads cloudflared output: readiness signals, redacted dependency detail and
//! debug records.

use std::sync::{Arc, LazyLock, Mutex, PoisonError};

use piik_diagnostics::safe_text;
use regex::Regex;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader};
use tokio::sync::mpsc;

pub(crate) use crate::error::ExitFailure;

const MAX_LOG_LINE_BYTES: usize = 64 * 1024;
const REGISTERED: &str = "Registered tunnel connection";

static QUICK_ORIGIN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"https://[a-z0-9](?:[a-z0-9-]*[a-z0-9])?\.trycloudflare\.com")
        .expect("static Quick Tunnel origin pattern")
});

/// State shared between the output readers, the supervisor and the owner.
#[derive(Default)]
pub(crate) struct Shared {
    /// The latest redacted error detail, kept for the user-facing failure.
    pub last_detail: String,
    /// Recorded when the child exits before its owner asked it to stop.
    pub failure: Option<ExitFailure>,
}

pub(crate) enum Signal {
    Origin(String),
    Registered,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Entry {
    level: String,
    message: String,
    error: String,
}

/// Reads one output stream line by line until it closes. A line longer than
/// the bound ends reading.
pub(crate) async fn read(
    stream: impl AsyncRead + Unpin,
    stderr: bool,
    shared: Arc<Mutex<Shared>>,
    signals: mpsc::UnboundedSender<Signal>,
) {
    let mut reader = BufReader::new(stream);
    let mut buffer = Vec::new();
    loop {
        buffer.clear();
        let limit = (MAX_LOG_LINE_BYTES + 1) as u64;
        match (&mut reader)
            .take(limit)
            .read_until(b'\n', &mut buffer)
            .await
        {
            Ok(0) => return,
            Ok(_) => {}
            Err(error) => {
                tracing::debug!(
                    event = "output-read-failed",
                    error = &error as &dyn std::error::Error,
                    "public-tunnel"
                );
                return;
            }
        }
        if buffer.last() == Some(&b'\n') {
            buffer.pop();
            if buffer.last() == Some(&b'\r') {
                buffer.pop();
            }
        } else if buffer.len() > MAX_LOG_LINE_BYTES {
            tracing::debug!(
                event = "output-read-failed",
                cause = "token too long",
                "public-tunnel"
            );
            return;
        }
        let line = String::from_utf8_lossy(&buffer);
        handle_line(&line, stderr, &shared, &signals);
    }
}

fn handle_line(
    line: &str,
    stderr: bool,
    shared: &Mutex<Shared>,
    signals: &mpsc::UnboundedSender<Signal>,
) {
    let record_detail = |text: &str| {
        // Redact before any UI truncation.
        let text = safe_text(text.trim());
        if !text.is_empty() {
            shared
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last_detail = text;
        }
    };
    match serde_json::from_str::<Entry>(line) {
        // cloudflared's CLI also writes plain stderr errors, including Quick
        // Tunnel allocation failures before a transport exists.
        Err(_) => {
            if stderr {
                record_detail(line);
            }
            tracing::debug!(event = "dependency-output", output = %safe_text(line), "public-tunnel");
        }
        Ok(entry) if matches!(entry.level.as_str(), "error" | "warn" | "fatal") => {
            if entry.level != "warn" {
                record_detail(&format!("{}\n{}", entry.message, entry.error));
            }
            tracing::debug!(
                event = "dependency-output",
                level = %entry.level,
                output = %safe_text(&entry.message),
                cause = %safe_text(&entry.error),
                "public-tunnel"
            );
        }
        Ok(_) => {}
    }
    if let Some(origin) = origin_from_log_line(line) {
        let _ = signals.send(Signal::Origin(origin));
    }
    if connection_registered(line) {
        let _ = signals.send(Signal::Registered);
    }
}

fn log_message(line: &str) -> String {
    match serde_json::from_str::<Entry>(line) {
        Ok(entry) if !entry.message.is_empty() => entry.message,
        _ => line.to_owned(),
    }
}

/// The Quick Tunnel origin announced in one log line, lower-cased.
pub(crate) fn origin_from_log_line(line: &str) -> Option<String> {
    let message = log_message(line).to_lowercase();
    QUICK_ORIGIN
        .find(&message)
        .map(|origin| origin.as_str().to_owned())
}

pub(crate) fn connection_registered(line: &str) -> bool {
    log_message(line).contains(REGISTERED)
}
