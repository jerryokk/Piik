//! Opt-in App diagnostics: one rotating `client.log` through piik-diagnostics,
//! in this directory order: `--log-dir`, `PIIK_LOG_DIR`, `<exe dir>/logs`,
//! then `%LOCALAPPDATA%/Piik/logs` when the install directory is read-only.
//! The shell owns the slot; the runtime can enable it later from the launcher.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result};
use piik_diagnostics::{Error, Recorder};
use serde::Serialize;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::build_info;

/// The process-wide recorder, opened at most once.
#[derive(Clone, Default)]
pub struct Diagnostics {
    log_dir: Option<PathBuf>,
    recorder: Arc<Mutex<Option<Recorder>>>,
}

impl Diagnostics {
    pub fn new(log_dir: Option<PathBuf>) -> Self {
        Self {
            log_dir,
            recorder: Arc::default(),
        }
    }

    /// Opens the recorder and makes it the `tracing` sink; repeated calls
    /// keep the first recorder.
    pub fn enable(&self) -> Result<()> {
        let mut slot = self.lock();
        if slot.is_none() {
            *slot = Some(enable(self.log_dir.as_deref())?);
        }
        Ok(())
    }

    pub fn enabled(&self) -> bool {
        self.lock().is_some()
    }

    pub fn context(&self, name: &str, value: impl Serialize) {
        if let Some(recorder) = self.lock().as_ref() {
            recorder.context(name, value);
        }
    }

    pub fn binary(&self, name: &str, path: &Path) {
        if let Some(recorder) = self.lock().as_ref() {
            recorder.binary(name, path);
        }
    }

    /// Exports a report after a failed run, then closes the log.
    pub fn finish(&self, failed: bool) {
        if let Some(recorder) = self.lock().take() {
            tracing::debug!(event = "stopped", failed, "piik-client");
            if failed {
                let _ = recorder.export();
            }
            let _ = recorder.close();
        }
    }

    fn lock(&self) -> MutexGuard<'_, Option<Recorder>> {
        self.recorder.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn enable(log_dir: Option<&Path>) -> Result<Recorder> {
    let recorder = open(log_dir).context("Piik App diagnostics are unavailable")?;
    recorder.context(
        "build",
        serde_json::json!({ "version": build_info::RELEASE, "revision": build_info::REVISION }),
    );
    tracing_subscriber::registry()
        .with(recorder.layer())
        .try_init()
        .context("Piik App diagnostics are unavailable")?;
    tracing::debug!(event = "diagnostics-enabled", "piik-client");
    Ok(recorder)
}

fn open(log_dir: Option<&Path>) -> Result<Recorder, Error> {
    let explicit = log_dir.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("PIIK_LOG_DIR")
            .map(PathBuf::from)
            .filter(|directory| !directory.as_os_str().is_empty())
    });
    if let Some(directory) = explicit {
        return Recorder::open(directory, "client", build_info::REVISION);
    }
    let executable = std::env::current_exe().map_err(io_error)?;
    let beside = executable
        .parent()
        .map(|directory| directory.join("logs"))
        .unwrap_or_else(|| PathBuf::from("logs"));
    match Recorder::open(beside, "client", build_info::REVISION) {
        Err(Error::Io { source, .. }) if source.kind() == io::ErrorKind::PermissionDenied => {
            let cache = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| Error::Invalid("local application data is unavailable".into()))?;
            Recorder::open(
                cache.join("Piik").join("logs"),
                "client",
                build_info::REVISION,
            )
        }
        result => result,
    }
}

fn io_error(source: io::Error) -> Error {
    Error::Io {
        context: "resolve App executable",
        source: source.into(),
    }
}
