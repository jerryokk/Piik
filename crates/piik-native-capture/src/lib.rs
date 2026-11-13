//! Supervision of the Windows capture helper (`piik-capture.exe`, capture
//! protocol v7): capability probe, source listing, preview and the framed
//! capture stream lifecycle. The helper itself is reused unchanged.

mod command;
mod probe;
mod protocol;
mod sources;
mod stream;

#[cfg(test)]
mod tests;

use std::process::ExitStatus;

pub use command::CommandError;
pub use probe::{Adapter, Capabilities, Encoder, Summary, discover, packaged_executable};
pub use protocol::{
    Frame, FrameKind, MAX_CODED_VIDEO_DIMENSION, MAX_CODED_VIDEO_PIXELS, MAX_VIDEO_DIMENSION,
    MAX_VIDEO_FRAMERATE, MAX_VIDEO_MACROBLOCKS_PER_SECOND, MAX_VIDEO_PIXELS, read_frame,
    supported_h264_profile_level_id, valid_coded_video_size, valid_video_format, valid_video_size,
    write_frame,
};
pub use sources::{
    CaptureTarget, Microphone, list_microphones, list_sources, preview_source, valid_device_id,
};
pub use stream::{
    EncodedVideoOptions, MAX_OUTPUT_GROUPS, MAX_VIDEO_BITRATE, OutputProfile, Stream, VideoOptions,
    VideoProfile, screen_share_outputs, start_audio, start_encoded_video, start_microphone,
    start_system_audio, start_video,
};

/// Validated helper failure codes only, never stderr detail or identity.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Failure {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stage: String,
    #[serde(rename = "hresult", skip_serializing_if = "String::is_empty")]
    pub hresult: String,
    #[serde(rename = "exitCode")]
    pub exit_code: i32,
}

/// How the helper process ended when it did not exit successfully.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExitCause {
    #[error("{0}")]
    Status(ExitStatus),
    #[error("wait for native capture process: {0}")]
    Wait(String),
}

impl ExitCause {
    /// The process exit code, or -1 when none is available.
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Status(status) => status.code().unwrap_or(-1),
            Self::Wait(_) => -1,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A contract violation or bounded failure with a fixed message.
    #[error("{0}")]
    Invalid(&'static str),
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
    /// The helper output ended cleanly between envelopes.
    #[error("native capture stream ended")]
    Eof,
    /// The stream's input was retired.
    #[error("native capture input is closed")]
    Closed,
    /// The owner's cancellation ended the operation.
    #[error("native capture operation was canceled")]
    Canceled,
    /// The user dismissed the system source-selection dialog. Reported by the
    /// host session from the helper's status.
    #[error("capture source selection canceled")]
    SourceCanceled,
    #[error("native capture probe failed: {0}")]
    ProbeFailed(#[source] CommandError),
    /// The helper exited unsuccessfully with validated failure codes.
    #[error("native capture failed (stage={}, hresult={}): {cause}", details.stage, details.hresult)]
    Process {
        details: Failure,
        #[source]
        cause: ExitCause,
    },
}

impl Error {
    pub(crate) fn io(context: &'static str, source: std::io::Error) -> Self {
        Self::Io { context, source }
    }

    /// Structured failure codes, present only for a typed process failure.
    pub fn failure_details(&self) -> Option<&Failure> {
        match self {
            Self::Process { details, .. } => Some(details),
            _ => None,
        }
    }
}
