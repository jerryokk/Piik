//! Platform source listing, preview and microphone listing.

use std::path::PathBuf;

use futures_util::future::BoxFuture;
use piik_native_capture::{CaptureTarget, Error, Microphone};

/// The source operations used by the control session.
pub trait SourceProvider: Send + Sync + 'static {
    fn list_sources(&self) -> BoxFuture<'_, Result<Vec<CaptureTarget>, Error>>;
    fn preview_source<'a>(
        &'a self,
        target: &'a CaptureTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, Error>>;
    fn list_microphones(&self) -> BoxFuture<'_, Result<Vec<Microphone>, Error>>;
}

/// The desktop capture helper executable.
#[derive(Debug, Clone)]
pub struct ProcessSources {
    pub executable: PathBuf,
}

impl SourceProvider for ProcessSources {
    fn list_sources(&self) -> BoxFuture<'_, Result<Vec<CaptureTarget>, Error>> {
        Box::pin(piik_native_capture::list_sources(&self.executable))
    }

    fn preview_source<'a>(
        &'a self,
        target: &'a CaptureTarget,
    ) -> BoxFuture<'a, Result<Vec<u8>, Error>> {
        Box::pin(piik_native_capture::preview_source(
            &self.executable,
            target,
        ))
    }

    fn list_microphones(&self) -> BoxFuture<'_, Result<Vec<Microphone>, Error>> {
        Box::pin(piik_native_capture::list_microphones(&self.executable))
    }
}
