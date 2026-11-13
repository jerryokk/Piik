//! Native media discovery, the advertised `/health` capabilities and the
//! native control factory.
//!
//! Native Host media needs a probed capture helper; without one its
//! capability bits stay false, so pages keep Browser media, and its control
//! requests fail with `operation-failed`. Native Viewer media needs no helper
//! (a probed one only lets it derive relay layers).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use piik_loopback::{BoxFuture, CancellationToken, ControlFactory, NativeMediaCapabilities};
use piik_native_capture::Capabilities;
use piik_native_control::media::{
    HostMedia, HostMediaFactory, HostOptions, MediaError, MediaResult, ViewerMediaFactory,
};
use piik_native_control::{Config, ProcessSources};
use piik_protocol::QualityResolution;
use piik_remote_input::Manager;

/// The relay-children limit advertised for native Viewer media.
const MAX_RELAY_CHILDREN: i64 = 20;

/// The probed capture helper and the native Viewer media factory; Host
/// media is created per control factory with the App's input owner.
pub(super) struct NativeMedia {
    pub executable: PathBuf,
    pub capture: Capabilities,
    /// The probe failure, if the helper exists but could not be probed.
    pub discovery: Option<String>,
    viewer: Arc<dyn ViewerMediaFactory>,
}

/// Native Host media on the probed capture helper; it shares the App's
/// remote-input owner with native control.
fn host_factory(executable: &Path, input: Option<Arc<Manager>>) -> Arc<dyn HostMediaFactory> {
    Arc::new(piik_native_host::Factory::new(
        executable.to_path_buf(),
        input,
    ))
}

/// Native Viewer media needs no helper; a probed one adds its relay encoder.
fn viewer_factory(executable: &Path, capture: &Capabilities) -> Arc<dyn ViewerMediaFactory> {
    Arc::new(piik_native_viewer::Factory::new(
        executable.to_path_buf(),
        capture.clone(),
    ))
}

/// `--capture-process`, else `runtime\native\piik-capture.exe` beside the App.
pub(super) async fn discover(configured: Option<&Path>) -> NativeMedia {
    let executable = configured
        .filter(|path| !path.as_os_str().to_string_lossy().trim().is_empty())
        .map(Path::to_path_buf)
        .or_else(piik_native_capture::packaged_executable)
        .unwrap_or_default();
    let (capture, discovery) = match piik_native_capture::discover(&executable).await {
        Ok(capture) => (capture, None),
        Err(error) => (Capabilities::default(), Some(error.to_string())),
    };
    let viewer = viewer_factory(&executable, &capture);
    NativeMedia {
        executable,
        capture,
        discovery,
        viewer,
    }
}

impl NativeMedia {
    /// The advertised capabilities: Host media bits need a probed helper.
    pub fn capabilities(&self, input: Option<&Manager>) -> NativeMediaCapabilities {
        let mut capabilities = NativeMediaCapabilities {
            receiver_reuse: true,
            max_relay_children: MAX_RELAY_CHILDREN,
            // Endpoint and control features of this build; they do not depend
            // on a probed capture helper.
            share_volume: true,
            source_cancellation: true,
            capture_failure_details: true,
            ..NativeMediaCapabilities::default()
        };
        if let Some(input) = input {
            capabilities.remote_input = true;
            capabilities.remote_co_play = true;
            capabilities.remote_stop_shortcut = input.supports_stop_shortcut();
        }
        if !self.host_available() {
            return capabilities;
        }
        let summary = self.capture.summary();
        let dimension = i64::from(piik_native_capture::MAX_VIDEO_DIMENSION);
        capabilities.max_video_resolution = Some(QualityResolution {
            width: dimension,
            height: dimension,
        });
        capabilities.max_video_pixels = piik_native_capture::MAX_VIDEO_PIXELS as i64;
        capabilities.max_video_macroblocks_per_second =
            piik_native_capture::MAX_VIDEO_MACROBLOCKS_PER_SECOND as i64;
        capabilities.max_video_framerate = i64::from(piik_native_capture::MAX_VIDEO_FRAMERATE);
        capabilities.max_video_bitrate = i64::from(piik_native_capture::MAX_VIDEO_BITRATE);
        capabilities.max_audio_bitrate = i64::from(piik_native_audio::MAX_SEND_BITRATE);
        capabilities.video = summary.video;
        capabilities.capture_border_control = summary.capture_border_control;
        capabilities.microphone = summary.microphone;
        capabilities.process_audio = summary.process_audio;
        capabilities.process_audio_exclusion = summary.process_audio_exclusion;
        capabilities.system_audio = summary.system_audio;
        capabilities.hardware_h264 = summary.hardware_h264;
        capabilities.software_vp8 = summary.software_vp8;
        capabilities
    }

    /// Native Host media starts only on a probed capture helper.
    fn host_available(&self) -> bool {
        self.discovery.is_none()
    }

    /// One native control session per accepted loopback connection.
    pub fn control_factory(&self, input: Option<Arc<Manager>>) -> ControlFactory {
        Config {
            capabilities: self.capture.clone(),
            port_mapping: true,
            remote_input: input.clone(),
            sources: Arc::new(ProcessSources {
                executable: self.executable.clone(),
            }),
            host: if self.host_available() {
                host_factory(&self.executable, input)
            } else {
                Arc::new(Unavailable) as Arc<dyn HostMediaFactory>
            },
            viewer: self.viewer.clone(),
        }
        .factory()
    }
}

/// The App's remote input owner; its absence only disables remote control.
pub(super) fn remote_input() -> Option<Arc<Manager>> {
    match Manager::new() {
        Ok(manager) => Some(Arc::new(manager)),
        Err(error) => {
            tracing::warn!(reason = %error, "Remote control is unavailable");
            None
        }
    }
}

/// A media role without its platform helper.
struct Unavailable;

const UNAVAILABLE: &str = "native media is unavailable";

impl HostMediaFactory for Unavailable {
    fn start(
        &self,
        _cancel: CancellationToken,
        _options: HostOptions,
    ) -> BoxFuture<'_, MediaResult<Arc<dyn HostMedia>>> {
        Box::pin(async { Err(MediaError::failed(UNAVAILABLE)) })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::discover;

    #[tokio::test]
    async fn missing_capture_keeps_viewer_media_available() {
        let native = discover(Some(Path::new("missing-capture-process"))).await;
        let capabilities = native.capabilities(None);
        assert!(
            !capabilities.video && !capabilities.hardware_h264,
            "{capabilities:?}"
        );
        assert!(capabilities.receiver_reuse && capabilities.max_relay_children == 20);
        // The Viewer control stays available without a helper.
        let _control = native.control_factory(None);
    }

    #[tokio::test]
    async fn failed_capture_probe_retains_its_cause() {
        // A directory exists but cannot run as a capture helper.
        let directory = tempfile::tempdir().unwrap();
        let native = discover(Some(directory.path())).await;
        assert!(native.discovery.is_some());
        assert_eq!(native.executable, directory.path());
        assert!(!native.capabilities(None).video);
    }
}
