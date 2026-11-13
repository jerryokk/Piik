//! Codec tables and the SDP text policy str0m does not express.
//!
//! - Offer side: [`codec_config`] declares exactly one entry per H.264
//!   profile (str0m 0.25 panics in `accept_answer` on duplicates).
//! - Answer side: [`select_offer`] narrows a remote offer to the one video
//!   payload Piik forwards, plus its RTX, before str0m parses it. str0m alone
//!   would pick the last of several tied H.264 entries.
//! - Opus: str0m drops `maxaveragebitrate` and never adopts a remote
//!   `stereo=1`, so [`set_opus_fmtp`] rewrites outbound text and
//!   [`remote_opus`] reads the remote preference.

use std::collections::HashSet;

use str0m::RtcConfig;
use str0m::format::{Codec, FormatParams};
use str0m::media::Frequency;

/// Constrained Baseline at Level 5.2: the shared capture and receive ceiling,
/// in the browser-standard spelling.
pub const H264_PROFILE_LEVEL_ID: u32 = 0x42e034;
pub const H264_PT: u8 = 102;
pub const VP8_PT: u8 = 96;
pub const OPUS_PT: u8 = 111;
/// `minptime=10;useinbandfec=1`, the Opus parameters every Piik side declares.
pub const OPUS_FMTP: &str = "minptime=10;useinbandfec=1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    H264,
    Vp8,
}

impl VideoCodec {
    /// The lowercase codec name used in Piik signaling and metadata.
    pub const fn name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Vp8 => "vp8",
        }
    }

    pub fn from_name(name: &str) -> Option<Self> {
        if name.eq_ignore_ascii_case("h264") {
            Some(Self::H264)
        } else if name.eq_ignore_ascii_case("vp8") {
            Some(Self::Vp8)
        } else {
            None
        }
    }
}

/// Builds an RTP-mode config with exactly Piik's codecs: each listed video
/// codec with its RTX at `pt + 1` (RFC 4588 keeps late repair outside the
/// primary SRTP replay window), and Opus at PT 111 when `audio` is set.
pub fn codec_config(video: &[VideoCodec], audio: bool) -> RtcConfig {
    let mut config = RtcConfig::new().set_rtp_mode(true).clear_codecs();
    let codecs = config.codec_config();
    let mut added = HashSet::new();
    for codec in video {
        if !added.insert(*codec) {
            continue;
        }
        match codec {
            VideoCodec::H264 => codecs.add_h264(
                H264_PT.into(),
                Some((H264_PT + 1).into()),
                true,
                H264_PROFILE_LEVEL_ID,
            ),
            VideoCodec::Vp8 => codecs.add_config(
                VP8_PT.into(),
                Some((VP8_PT + 1).into()),
                Codec::Vp8,
                Frequency::NINETY_KHZ,
                None,
                FormatParams::default(),
            ),
        }
    }
    if audio {
        codecs.add_config(
            OPUS_PT.into(),
            None,
            Codec::Opus,
            Frequency::FORTY_EIGHT_KHZ,
            Some(2),
            FormatParams {
                min_p_time: Some(10),
                use_inband_fec: Some(true),
                ..Default::default()
            },
        );
    }
    config
}

/// Packetization mode 1 and a Constrained
/// Baseline profile within the receive contract. An absent profile does not
/// promise Constrained Baseline (RFC 6184).
pub fn forwardable_h264(fmtp: &str) -> bool {
    let mut profile = "";
    let mut packetization = "";
    for (key, value) in fmtp_params(fmtp) {
        if key.eq_ignore_ascii_case("profile-level-id") {
            profile = value;
        } else if key.eq_ignore_ascii_case("packetization-mode") {
            packetization = value;
        }
    }
    packetization == "1" && piik_encoded::h264::supported_profile_level_id(profile)
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SdpError {
    #[error("SDP is invalid")]
    Invalid,
    #[error("offer has no supported video")]
    NoSupportedVideo,
}

/// What [`select_offer`] kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Selection {
    pub video: Option<(VideoCodec, u8)>,
    pub audio: Option<u8>,
}

/// Narrows every active video section to its first payload, in offer order,
/// that `allowed` contains (H.264 only when [`forwardable_h264`]), plus the
/// RTX that repairs it; and every active audio section to its first Opus
/// payload. A zero-port section that is not `bundle-only` is left alone.
pub fn select_offer(offer: &str, allowed: &[VideoCodec]) -> Result<(String, Selection), SdpError> {
    let mut sdp = Sdp::parse(offer)?;
    let mut selection = Selection::default();
    for section in &mut sdp.sections {
        if !section.active() {
            continue;
        }
        match section.kind() {
            "video" => {
                let chosen = section.formats().into_iter().find_map(|pt| {
                    let (name, _) = section.rtpmap(pt)?;
                    let codec = VideoCodec::from_name(name).filter(|c| allowed.contains(c))?;
                    let usable =
                        codec != VideoCodec::H264 || section.fmtp(pt).is_some_and(forwardable_h264);
                    usable.then_some((codec, pt))
                });
                let Some((codec, pt)) = chosen else {
                    return Err(SdpError::NoSupportedVideo);
                };
                let mut keep = vec![pt];
                keep.extend(section.rtx_for(pt));
                section.retain_formats(&keep);
                selection.video.get_or_insert((codec, pt));
            }
            "audio" => {
                let opus = section.formats().into_iter().find(|pt| {
                    section
                        .rtpmap(*pt)
                        .is_some_and(|(name, _)| name.eq_ignore_ascii_case("opus"))
                });
                if let Some(pt) = opus {
                    section.retain_formats(&[pt]);
                    selection.audio.get_or_insert(pt);
                }
            }
            _ => {}
        }
    }
    Ok((sdp.to_string(), selection))
}

/// Sets `params` on the Opus fmtp of every audio section, replacing a key
/// that is already present and adding the fmtp line when it is missing.
/// For example `[("stereo", "1"), ("maxaveragebitrate", "510000")]`.
pub fn set_opus_fmtp(sdp: &str, params: &[(&str, &str)]) -> Result<String, SdpError> {
    let mut parsed = Sdp::parse(sdp)?;
    for section in &mut parsed.sections {
        if section.kind() != "audio" {
            continue;
        }
        let opus: Vec<u8> = section
            .formats()
            .into_iter()
            .filter(|pt| {
                section
                    .rtpmap(*pt)
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("opus"))
            })
            .collect();
        for pt in opus {
            let mut merged: Vec<(String, String)> = section
                .fmtp(pt)
                .map(|fmtp| {
                    fmtp_params(fmtp)
                        .map(|(k, v)| (k.to_owned(), v.to_owned()))
                        .collect()
                })
                .unwrap_or_default();
            for (key, value) in params {
                match merged.iter_mut().find(|(k, _)| k.eq_ignore_ascii_case(key)) {
                    Some(entry) => entry.1 = (*value).to_owned(),
                    None => merged.push(((*key).to_owned(), (*value).to_owned())),
                }
            }
            let joined = merged
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(";");
            section.set_fmtp(pt, &joined);
        }
    }
    Ok(parsed.to_string())
}

/// The remote Opus receive preference that str0m does not keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RemoteOpus {
    pub pt: u8,
    pub stereo: bool,
    /// The smallest valid (6000..=510000, RFC 7587) `maxaveragebitrate`.
    pub max_average_bitrate: Option<u32>,
}

/// Reads the first Opus payload of the first active audio section.
pub fn remote_opus(sdp: &str) -> Result<Option<RemoteOpus>, SdpError> {
    let parsed = Sdp::parse(sdp)?;
    for section in parsed
        .sections
        .iter()
        .filter(|s| s.kind() == "audio" && s.active())
    {
        for pt in section.formats() {
            if !section
                .rtpmap(pt)
                .is_some_and(|(name, _)| name.eq_ignore_ascii_case("opus"))
            {
                continue;
            }
            let mut opus = RemoteOpus {
                pt,
                ..Default::default()
            };
            for (key, value) in section.fmtp(pt).into_iter().flat_map(fmtp_params) {
                if key.eq_ignore_ascii_case("stereo") {
                    opus.stereo = value == "1";
                } else if key.eq_ignore_ascii_case("maxaveragebitrate")
                    && let Ok(limit) = value.parse::<u32>()
                    && (6_000..=510_000).contains(&limit)
                {
                    opus.max_average_bitrate = Some(
                        opus.max_average_bitrate
                            .map_or(limit, |known| known.min(limit)),
                    );
                }
            }
            return Ok(Some(opus));
        }
    }
    Ok(None)
}

fn fmtp_params(fmtp: &str) -> impl Iterator<Item = (&str, &str)> {
    fmtp.split(';').filter_map(|parameter| {
        let (key, value) = parameter.split_once('=').unwrap_or((parameter, ""));
        let key = key.trim();
        (!key.is_empty()).then(|| (key, value.trim()))
    })
}

struct Sdp {
    session: Vec<String>,
    sections: Vec<Section>,
}

struct Section {
    lines: Vec<String>,
}

impl Sdp {
    fn parse(text: &str) -> Result<Self, SdpError> {
        let mut session = Vec::new();
        let mut sections: Vec<Section> = Vec::new();
        for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
            if line.is_empty() {
                continue;
            }
            if line.starts_with("m=") {
                if line.split_whitespace().count() < 3 {
                    return Err(SdpError::Invalid);
                }
                sections.push(Section {
                    lines: vec![line.to_owned()],
                });
            } else if let Some(section) = sections.last_mut() {
                section.lines.push(line.to_owned());
            } else {
                session.push(line.to_owned());
            }
        }
        if !session.first().is_some_and(|l| l.starts_with("v=")) {
            return Err(SdpError::Invalid);
        }
        Ok(Self { session, sections })
    }
}

impl std::fmt::Display for Sdp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for line in self
            .session
            .iter()
            .chain(self.sections.iter().flat_map(|s| s.lines.iter()))
        {
            write!(f, "{line}\r\n")?;
        }
        Ok(())
    }
}

impl Section {
    fn m_fields(&self) -> Vec<&str> {
        self.lines[0][2..].split_whitespace().collect()
    }

    fn kind(&self) -> &str {
        self.lines[0][2..].split_whitespace().next().unwrap_or("")
    }

    fn attribute(&self, name: &str) -> bool {
        self.lines.iter().any(|l| {
            l.strip_prefix("a=")
                .is_some_and(|a| a == name || a.starts_with(&format!("{name}:")))
        })
    }

    /// A zero port rejects the section unless it is `bundle-only` (RFC 9143).
    fn active(&self) -> bool {
        let port = self.m_fields().get(1).copied().unwrap_or("0");
        port.split('/').next() != Some("0") || self.attribute("bundle-only")
    }

    fn formats(&self) -> Vec<u8> {
        self.m_fields()
            .iter()
            .skip(3)
            .filter_map(|f| f.parse().ok())
            .collect()
    }

    fn pt_attribute(&self, prefix: &str, pt: u8) -> Option<&str> {
        let prefix = format!("a={prefix}:{pt} ");
        self.lines
            .iter()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
    }

    /// `(encoding name, rest)` of `a=rtpmap:<pt> <name>/<clock>[/<channels>]`.
    fn rtpmap(&self, pt: u8) -> Option<(&str, &str)> {
        let value = self.pt_attribute("rtpmap", pt)?.trim();
        Some(value.split_once('/').unwrap_or((value, "")))
    }

    fn fmtp(&self, pt: u8) -> Option<&str> {
        self.pt_attribute("fmtp", pt).map(str::trim)
    }

    /// RTX payloads whose `apt` names `pt`.
    fn rtx_for(&self, pt: u8) -> Vec<u8> {
        self.formats()
            .into_iter()
            .filter(|rtx| {
                self.rtpmap(*rtx)
                    .is_some_and(|(name, _)| name.eq_ignore_ascii_case("rtx"))
                    && self.fmtp(*rtx).is_some_and(|fmtp| {
                        fmtp_params(fmtp)
                            .any(|(k, v)| k.eq_ignore_ascii_case("apt") && v == pt.to_string())
                    })
            })
            .collect()
    }

    /// Keeps only `keep` in the m-line (in their original order) and removes
    /// the per-payload attributes of every other format.
    fn retain_formats(&mut self, keep: &[u8]) {
        let fields: Vec<String> = self.m_fields().iter().map(|f| (*f).to_owned()).collect();
        let dropped: HashSet<String> = fields[3..]
            .iter()
            .filter(|f| !f.parse::<u8>().is_ok_and(|pt| keep.contains(&pt)))
            .cloned()
            .collect();
        let kept: Vec<&str> = fields[..3]
            .iter()
            .chain(fields[3..].iter().filter(|f| !dropped.contains(*f)))
            .map(String::as_str)
            .collect();
        self.lines[0] = format!("m={}", kept.join(" "));
        self.lines.retain(|line| {
            ["a=rtpmap:", "a=fmtp:", "a=rtcp-fb:"].iter().all(|prefix| {
                line.strip_prefix(prefix)
                    .and_then(|rest| rest.split([' ', '\t']).next())
                    .is_none_or(|pt| !dropped.contains(pt))
            })
        });
    }

    fn set_fmtp(&mut self, pt: u8, params: &str) {
        let prefix = format!("a=fmtp:{pt} ");
        let line = format!("{prefix}{params}");
        if let Some(existing) = self.lines.iter_mut().find(|l| l.starts_with(&prefix)) {
            *existing = line;
            return;
        }
        let rtpmap = format!("a=rtpmap:{pt} ");
        let at = self
            .lines
            .iter()
            .position(|l| l.starts_with(&rtpmap))
            .map_or(self.lines.len(), |i| i + 1);
        self.lines.insert(at, line);
    }
}

#[cfg(test)]
mod tests;
