//! Capture output and video profiles and their argument validation.

use super::{MAX_VIDEO_BITRATE, MIN_OUTPUT_BITRATE};
use crate::Error;
use crate::protocol::{MAX_OUTPUTS, valid_video_format};
use crate::sources::CaptureTarget;
use serde::Deserialize;

/// One codec output, not a selectable product preset.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, Deserialize)]
pub struct OutputProfile {
    pub width: u32,
    pub height: u32,
    #[serde(rename = "fps")]
    pub framerate: u32,
    pub bitrate: u32,
}

impl OutputProfile {
    pub fn valid(&self) -> bool {
        valid_video_format(self.width, self.height, self.framerate)
            && (MIN_OUTPUT_BITRATE..=MAX_VIDEO_BITRATE).contains(&self.bitrate)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoProfile {
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
    pub bitrate: u32,
    pub preference: String,
}

pub(super) fn valid_video_preference(preference: &str) -> bool {
    matches!(
        preference,
        "maintain-resolution" | "balanced" | "maintain-framerate"
    )
}

impl VideoProfile {
    /// The source ceiling must also yield a valid half-size output. Preset
    /// labels belong to the UI, not the capture process's numeric boundary.
    pub fn valid(&self) -> bool {
        valid_video_format(self.width, self.height, self.framerate)
            && self.width >= 4
            && self.height >= 4
            && (100_000..=MAX_VIDEO_BITRATE).contains(&self.bitrate)
            && valid_video_preference(&self.preference)
    }
}

/// Follows LiveKit 2.22.1's screen-share simulcast construction, with even
/// derived dimensions for NV12. The source profile has already passed `valid`.
pub fn screen_share_outputs(profile: &VideoProfile) -> Vec<OutputProfile> {
    vec![
        OutputProfile {
            width: profile.width / 4 * 2,
            height: profile.height / 4 * 2,
            framerate: profile.framerate,
            bitrate: profile.bitrate.min(150_000.max(profile.bitrate / 4)),
        },
        OutputProfile {
            width: profile.width,
            height: profile.height,
            framerate: profile.framerate,
            bitrate: profile.bitrate,
        },
    ]
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct VideoOptions {
    pub remote_input: bool,
    pub target: CaptureTarget,
    /// `auto`, `h264` or `vp8`.
    pub codec: String,
    pub adapter_index: u32,
    pub encoder_index: u32,
    pub profile: VideoProfile,
    /// Additional derived low-layer groups, at most [`MAX_OUTPUT_GROUPS`].
    pub output_groups: usize,
    pub show_capture_border: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EncodedVideoOptions {
    /// `h264` or `vp8`.
    pub codec: String,
    pub adapter_index: u32,
    pub encoder_index: u32,
    pub preference: String,
    pub outputs: Vec<OutputProfile>,
}

pub(super) fn append_output_arguments(
    arguments: &mut Vec<String>,
    outputs: &[OutputProfile],
) -> Result<(), Error> {
    if outputs.is_empty() || outputs.len() > MAX_OUTPUTS {
        return Err(Error::Invalid("native output count is invalid"));
    }
    if !outputs.iter().all(OutputProfile::valid) {
        return Err(Error::Invalid("native output profile is invalid"));
    }
    for output in outputs {
        arguments.push("--output".into());
        for value in [
            output.width,
            output.height,
            output.framerate,
            output.bitrate,
        ] {
            arguments.push(value.to_string());
        }
    }
    Ok(())
}
