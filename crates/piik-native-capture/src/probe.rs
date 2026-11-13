//! Capability probe (`--probe`).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::Error;
use crate::command::{CommandError, run_bounded};

const PROBE_PROTOCOL: i64 = 7;
const MAX_PROBE_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_ADAPTERS: usize = 64;
const MAX_ENCODERS: usize = 64;
const MAX_IDENTITY_BYTES: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Encoder {
    pub index: u32,
    pub name: String,
    pub identity: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Adapter {
    pub index: u32,
    pub name: String,
    pub identity: String,
    #[serde(default)]
    pub hardware_h264: Vec<Encoder>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub protocol: i64,
    #[serde(default)]
    pub platform: String,
    #[serde(default)]
    pub platform_build: String,
    #[serde(default)]
    pub video_capture: bool,
    #[serde(default)]
    pub remote_input: bool,
    #[serde(default)]
    pub capture_border_control: bool,
    #[serde(default)]
    pub microphone: bool,
    #[serde(default)]
    pub process_audio: bool,
    #[serde(default)]
    pub process_audio_exclusion: bool,
    #[serde(default)]
    pub system_audio: bool,
    #[serde(default, rename = "softwareVP8")]
    pub software_vp8: bool,
    #[serde(default)]
    pub adapters: Vec<Adapter>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    pub video: bool,
    pub capture_border_control: bool,
    pub microphone: bool,
    pub process_audio: bool,
    pub process_audio_exclusion: bool,
    pub system_audio: bool,
    pub hardware_h264: bool,
    pub software_vp8: bool,
}

impl Capabilities {
    pub fn summary(&self) -> Summary {
        Summary {
            video: self.video_capture,
            capture_border_control: self.video_capture && self.capture_border_control,
            microphone: self.microphone,
            process_audio: self.process_audio,
            // Exclusion needs the replaceable mixed microphone source.
            process_audio_exclusion: self.process_audio_exclusion && self.microphone,
            system_audio: self.system_audio,
            hardware_h264: self
                .adapters
                .iter()
                .any(|adapter| !adapter.hardware_h264.is_empty()),
            software_vp8: self.software_vp8,
        }
    }
}

impl Summary {
    pub fn audio_for(&self, kind: &str) -> bool {
        match kind {
            "window" => self.process_audio,
            "display" | "picker" => self.system_audio,
            _ => false,
        }
    }
}

fn valid_identity(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_IDENTITY_BYTES
}

pub(crate) fn decode_probe(payload: &[u8]) -> Result<Capabilities, Error> {
    // serde_json rejects trailing non-whitespace input itself.
    let capabilities: Capabilities = serde_json::from_slice(payload).map_err(|error| {
        if error.to_string().contains("trailing characters") {
            Error::Invalid("native capture probe returned trailing data")
        } else {
            Error::Invalid("native capture probe returned invalid JSON")
        }
    })?;
    if capabilities.protocol != PROBE_PROTOCOL
        || capabilities.platform != "windows"
        || !valid_identity(&capabilities.platform_build)
        || capabilities.adapters.len() > MAX_ADAPTERS
    {
        return Err(Error::Invalid(
            "native capture probe returned an invalid contract",
        ));
    }
    let mut adapters = HashSet::new();
    for adapter in &capabilities.adapters {
        if !valid_identity(&adapter.name)
            || !valid_identity(&adapter.identity)
            || adapter.hardware_h264.len() > MAX_ENCODERS
        {
            return Err(Error::Invalid(
                "native capture probe returned an invalid adapter",
            ));
        }
        if !adapters.insert(adapter.index) {
            return Err(Error::Invalid(
                "native capture probe returned duplicate adapters",
            ));
        }
        let mut encoders = HashSet::new();
        for encoder in &adapter.hardware_h264 {
            if !valid_identity(&encoder.name) || !valid_identity(&encoder.identity) {
                return Err(Error::Invalid(
                    "native capture probe returned an invalid encoder",
                ));
            }
            if !encoders.insert(encoder.index) {
                return Err(Error::Invalid(
                    "native capture probe returned duplicate encoders",
                ));
            }
        }
    }
    Ok(capabilities)
}

/// Probes the helper. A blank path or missing helper reports no capability.
pub async fn discover(executable: &Path) -> Result<Capabilities, Error> {
    if executable.as_os_str().to_string_lossy().trim().is_empty() {
        return Ok(Capabilities::default());
    }
    match tokio::fs::metadata(executable).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Capabilities::default());
        }
        Err(error) => return Err(Error::io("inspect native capture process", error)),
    }
    let output = run_bounded(
        executable,
        &["--probe"],
        MAX_PROBE_OUTPUT_BYTES,
        Some("native-capture-probe"),
    )
    .await
    .map_err(|error| {
        tracing::debug!(
            target: "piik_native_capture",
            event = "capture-probe-failed",
            error = %piik_diagnostics::safe_text(&error.to_string()),
            "piik-client"
        );
        match error {
            CommandError::Timeout => Error::Invalid("native capture probe timed out"),
            other => Error::ProbeFailed(other),
        }
    })?;
    decode_probe(&output)
}

/// `runtime\native\piik-capture.exe` beside the running App.
pub fn packaged_executable() -> Option<PathBuf> {
    let executable = std::env::current_exe().ok()?;
    Some(
        executable
            .parent()?
            .join("runtime")
            .join("native")
            .join("piik-capture.exe"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_PROBE: &str = r#"{"protocol":7,"platform":"windows","platformBuild":"26200","videoCapture":true,"processAudio":true,"systemAudio":true,"softwareVP8":true,"adapters":[{"index":0,"name":"GPU","identity":"0x0:0x1","hardwareH264":[{"index":0,"name":"H264","identity":"{encoder}"}]}]}"#;

    #[test]
    fn decode_probe_separates_capture_audio_and_hardware_encode() {
        let summary = decode_probe(VALID_PROBE.as_bytes())
            .expect("probe")
            .summary();
        assert!(
            summary.video
                && summary.process_audio
                && summary.system_audio
                && summary.hardware_h264
                && summary.software_vp8,
            "summary = {summary:?}"
        );
    }

    #[test]
    fn decode_probe_rejects_unknown_trailing_and_duplicate_data() {
        for payload in [
            VALID_PROBE.replacen(r#""protocol":7"#, r#""protocol":7,"extra":true"#, 1),
            VALID_PROBE.replacen(r#""protocol":7"#, r#""protocol":6"#, 1),
            format!("{VALID_PROBE}{{}}"),
            VALID_PROBE.replacen(
                r#""adapters":["#,
                r#""adapters":[{"index":0,"name":"GPU 2","identity":"0x0:0x2","hardwareH264":[]},"#,
                1,
            ),
        ] {
            assert!(
                decode_probe(payload.as_bytes()).is_err(),
                "invalid probe was accepted: {payload}"
            );
        }
    }

    #[test]
    fn audio_exclusion_capability_requires_explicit_support_and_mixing() {
        let capabilities = decode_probe(VALID_PROBE.as_bytes()).expect("probe");
        assert!(
            !capabilities.summary().process_audio_exclusion,
            "old probe advertised audio exclusion"
        );
        let payload = VALID_PROBE.replacen(
            r#""processAudio":true"#,
            r#""processAudio":true,"processAudioExclusion":true,"microphone":true"#,
            1,
        );
        let mut capabilities = decode_probe(payload.as_bytes()).expect("probe");
        assert!(
            capabilities.summary().process_audio_exclusion,
            "explicit audio exclusion capability was lost"
        );
        capabilities.microphone = false;
        assert!(
            !capabilities.summary().process_audio_exclusion,
            "audio exclusion requires the mixed source"
        );
    }

    #[test]
    fn summary_does_not_conflate_video_audio_and_encoder() {
        let summary = decode_probe(
            br#"{"protocol":7,"platform":"windows","platformBuild":"19045","videoCapture":true,"processAudio":false,"systemAudio":true,"adapters":[{"index":0,"name":"GPU","identity":"0x0:0x1","hardwareH264":[]}]}"#,
        )
        .expect("probe")
        .summary();
        assert!(
            summary.video
                && !summary.process_audio
                && summary.system_audio
                && !summary.hardware_h264
                && !summary.capture_border_control,
            "summary = {summary:?}"
        );
    }

    #[test]
    fn capture_border_capability_requires_capture_and_explicit_support() {
        for supported in [false, true] {
            let payload = if supported {
                VALID_PROBE.replacen(
                    r#""videoCapture":true"#,
                    r#""videoCapture":true,"captureBorderControl":true"#,
                    1,
                )
            } else {
                VALID_PROBE.to_owned()
            };
            let mut capabilities = decode_probe(payload.as_bytes()).expect("probe");
            assert_eq!(capabilities.summary().capture_border_control, supported);
            capabilities.video_capture = false;
            assert!(
                !capabilities.summary().capture_border_control,
                "border control without video capture"
            );
        }
    }

    #[test]
    fn capture_target_audio_scope_follows_target_kind() {
        let audio = Summary {
            process_audio: true,
            system_audio: true,
            ..Summary::default()
        };
        let window_without_process = Summary {
            system_audio: true,
            ..Summary::default()
        };
        assert!(
            audio.audio_for("window") && audio.audio_for("display") && audio.audio_for("picker")
        );
        assert!(!window_without_process.audio_for("window") && !audio.audio_for("other"));
    }

    #[tokio::test]
    async fn missing_or_blank_helper_reports_no_capability() {
        assert_eq!(
            discover(Path::new(" ")).await.expect("blank"),
            Capabilities::default()
        );
        let missing = std::env::temp_dir().join("piik-missing-capture-helper.exe");
        assert_eq!(
            discover(&missing).await.expect("missing"),
            Capabilities::default()
        );
    }
}
