//! Shared calculations ported from `src/shared`.

use std::sync::LazyLock;

use regex::Regex;
use thiserror::Error;

use crate::codec::{Int, wire_enum};
use crate::constants::MAX_ENDPOINT_MEDIA_COPY_CAPACITY;
use crate::scalars::is_js_whitespace;

// ---------------------------------------------------------------------------
// src/shared/media-copy-accounting.ts
// ---------------------------------------------------------------------------

wire_enum! {
    /// `EndpointMediaCopyPhase`.
    pub enum EndpointMediaCopyPhase {
        Steady = "steady",
        Transition = "transition",
    }
}

/// The `assertEndpointMediaCopyCapacity` throw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("Endpoint media copy capacity must be 1, 2, or 3")]
pub struct EndpointMediaCopyCapacityError;

/// Returns the copy limit, failing where TypeScript throws.
pub fn endpoint_media_copy_limit(
    capacity: Int,
    phase: EndpointMediaCopyPhase,
) -> Result<Int, EndpointMediaCopyCapacityError> {
    assert_endpoint_media_copy_capacity(capacity)?;
    Ok(match phase {
        EndpointMediaCopyPhase::Steady => capacity,
        EndpointMediaCopyPhase::Transition => (capacity + 1).min(MAX_ENDPOINT_MEDIA_COPY_CAPACITY),
    })
}

/// Reports whether `count` copies fit; an unavailable capacity never fits.
pub fn endpoint_media_copy_count_fits(
    count: Int,
    capacity: Int,
    phase: EndpointMediaCopyPhase,
) -> bool {
    count >= 0 && endpoint_media_copy_limit(capacity, phase).is_ok_and(|limit| count <= limit)
}

/// An integer type makes the fractional part of `Number.isSafeInteger` implicit.
pub fn is_endpoint_media_copy_capacity(capacity: Int) -> bool {
    (1..=MAX_ENDPOINT_MEDIA_COPY_CAPACITY).contains(&capacity)
}

/// `assertEndpointMediaCopyCapacity`.
pub fn assert_endpoint_media_copy_capacity(
    capacity: Int,
) -> Result<(), EndpointMediaCopyCapacityError> {
    if is_endpoint_media_copy_capacity(capacity) {
        Ok(())
    } else {
        Err(EndpointMediaCopyCapacityError)
    }
}

// ---------------------------------------------------------------------------
// src/shared/nat-candidate.ts
// ---------------------------------------------------------------------------

wire_enum! {
    /// `CandidateSignalOrigin`.
    pub enum CandidateSignalOrigin {
        End = "end",
        Ordinary = "ordinary",
        Predicted = "predicted",
    }
}

/// The JavaScript `\s` class, spelled out because the regex crate's `\s` is
/// the Unicode White_Space property, which differs.
static PREDICTED_CANDIDATE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)^candidate:s[pm][0-9]+[\t\n\x0b\x0c\r \x{00a0}\x{1680}\x{2000}-\x{200a}",
        r"\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]"
    ))
    .expect("constant pattern compiles")
});

/// Treats an empty string as TypeScript null (both yield [`CandidateSignalOrigin::End`]).
pub fn candidate_signal_origin_of(candidate: &str) -> CandidateSignalOrigin {
    if candidate.trim_matches(is_js_whitespace).is_empty() {
        CandidateSignalOrigin::End
    } else if PREDICTED_CANDIDATE_PATTERN.is_match(candidate) {
        CandidateSignalOrigin::Predicted
    } else {
        CandidateSignalOrigin::Ordinary
    }
}

// ---------------------------------------------------------------------------
// src/shared/packet-loss.ts
// ---------------------------------------------------------------------------

/// `None` arguments are TypeScript null; `None` is returned where the shared
/// calculation returns null.
pub fn packet_loss_percent_from_deltas(
    packets_received_delta: Option<f64>,
    packets_lost_delta: Option<f64>,
) -> Option<f64> {
    let (received, lost) = (packets_received_delta?, packets_lost_delta?);
    if received < 0.0 || lost < 0.0 {
        return None;
    }
    let total = received + lost;
    (total > 0.0).then(|| (lost / total) * 100.0)
}

// ---------------------------------------------------------------------------
// src/shared/video-codec-evidence.ts
// ---------------------------------------------------------------------------

struct CodecParameterRule {
    profile_key: Option<&'static str>,
    parameter_keys: &'static [&'static str],
    validators: &'static [(&'static str, &'static str)],
}

const NUMBER_PATTERN: &str = r"^[1-9][0-9]{0,9}$";

fn codec_rule(codec: &str) -> Option<&'static CodecParameterRule> {
    static H264: CodecParameterRule = CodecParameterRule {
        profile_key: Some("profile-level-id"),
        parameter_keys: &["packetization-mode", "level-asymmetry-allowed"],
        validators: &[
            ("profile-level-id", r"^[0-9a-f]{6}$"),
            ("packetization-mode", r"^[0-2]$"),
            ("level-asymmetry-allowed", r"^[01]$"),
        ],
    };
    static VP9: CodecParameterRule = CodecParameterRule {
        profile_key: Some("profile-id"),
        parameter_keys: &["max-fr", "max-fs"],
        validators: &[
            ("profile-id", r"^[0-3]$"),
            ("max-fr", NUMBER_PATTERN),
            ("max-fs", NUMBER_PATTERN),
        ],
    };
    static VP8: CodecParameterRule = CodecParameterRule {
        profile_key: None,
        parameter_keys: &["max-fr", "max-fs"],
        validators: &[("max-fr", NUMBER_PATTERN), ("max-fs", NUMBER_PATTERN)],
    };
    static AV1: CodecParameterRule = CodecParameterRule {
        profile_key: Some("profile"),
        parameter_keys: &["level-idx", "tier"],
        validators: &[
            ("profile", r"^[0-2]$"),
            ("level-idx", r"^(?:[0-9]|[12][0-9]|3[01])$"),
            ("tier", r"^[01]$"),
        ],
    };
    match codec.to_lowercase().as_str() {
        "video/h264" => Some(&H264),
        "video/vp9" => Some(&VP9),
        "video/vp8" => Some(&VP8),
        "video/av1" => Some(&AV1),
        _ => None,
    }
}

fn validator_matches(source: &str, candidate: &str) -> bool {
    // The validators are tiny anchored patterns; compiling on demand keeps the
    // rule tables constant. Evidence arrives at most every two seconds.
    Regex::new(source).is_ok_and(|pattern| pattern.is_match(candidate))
}

/// `None` and the empty string both stand for TypeScript null; the schema
/// rejects an empty codec string before this refinement runs.
pub fn is_canonical_video_codec_evidence(
    codec: Option<&str>,
    codec_profile: Option<&str>,
    codec_parameters: Option<&str>,
) -> bool {
    let profile = codec_profile.unwrap_or_default();
    let parameters = codec_parameters.unwrap_or_default();
    let Some(rule) = codec.filter(|codec| !codec.is_empty()).and_then(codec_rule) else {
        return profile.is_empty() && parameters.is_empty();
    };
    let profile_keys: &[&str] = match &rule.profile_key {
        Some(key) => std::slice::from_ref(key),
        None => &[],
    };
    canonical_values_match(profile, profile_keys, rule.validators)
        && canonical_values_match(parameters, rule.parameter_keys, rule.validators)
}

fn canonical_values_match(
    encoded: &str,
    ordered_keys: &[&str],
    validators: &[(&str, &str)],
) -> bool {
    if encoded.is_empty() {
        return true;
    }
    let segments: Vec<&str> = encoded.split("; ").collect();
    if segments.len() > ordered_keys.len() {
        return false;
    }
    let mut previous: Option<usize> = None;
    for segment in segments {
        let (key, candidate) = match segment.find('=') {
            Some(separator) if separator >= 1 => (&segment[..separator], &segment[separator + 1..]),
            _ => ("", ""),
        };
        let Some(key_index) = ordered_keys.iter().position(|known| *known == key) else {
            return false;
        };
        let Some((_, source)) = validators.iter().find(|(name, _)| *name == key) else {
            return false;
        };
        if previous.is_some_and(|previous| key_index <= previous)
            || !validator_matches(source, candidate)
        {
            return false;
        }
        previous = Some(key_index);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_media_copy_limits() {
        for (capacity, steady, transition) in [(1, 1, 2), (2, 2, 3), (3, 3, 3)] {
            assert_eq!(
                endpoint_media_copy_limit(capacity, EndpointMediaCopyPhase::Steady),
                Ok(steady)
            );
            assert_eq!(
                endpoint_media_copy_limit(capacity, EndpointMediaCopyPhase::Transition),
                Ok(transition)
            );
            assert!(endpoint_media_copy_count_fits(
                transition,
                capacity,
                EndpointMediaCopyPhase::Transition
            ));
            assert!(!endpoint_media_copy_count_fits(
                transition + 1,
                capacity,
                EndpointMediaCopyPhase::Transition
            ));
        }
    }

    #[test]
    fn endpoint_media_copy_capacity_guards() {
        for capacity in [0, 4, -1] {
            assert!(assert_endpoint_media_copy_capacity(capacity).is_err());
            assert!(endpoint_media_copy_limit(capacity, EndpointMediaCopyPhase::Steady).is_err());
        }
        // An unavailable runtime capacity returns false instead of throwing.
        assert!(!endpoint_media_copy_count_fits(
            0,
            0,
            EndpointMediaCopyPhase::Steady
        ));
    }

    #[test]
    fn nat_candidate_diagnostics() {
        for (candidate, want) in [
            ("", CandidateSignalOrigin::End),
            (" ", CandidateSignalOrigin::End),
            (
                "candidate:sp2 1 udp 1 203.0.113.1 50000 typ srflx",
                CandidateSignalOrigin::Predicted,
            ),
            (
                "candidate:base 1 udp 1 203.0.113.1 50000 typ srflx",
                CandidateSignalOrigin::Ordinary,
            ),
        ] {
            assert_eq!(candidate_signal_origin_of(candidate), want, "{candidate:?}");
        }
    }

    #[test]
    fn packet_loss_percent() {
        assert_eq!(
            packet_loss_percent_from_deltas(Some(90.0), Some(10.0)),
            Some(10.0)
        );
        assert_eq!(packet_loss_percent_from_deltas(Some(0.0), Some(0.0)), None);
        assert_eq!(packet_loss_percent_from_deltas(None, Some(1.0)), None);
        assert_eq!(
            packet_loss_percent_from_deltas(Some(10.0), Some(-1.0)),
            None
        );
    }

    #[test]
    fn canonical_video_codec_evidence() {
        let samples: [(&str, &str, &str, bool); 12] = [
            (
                "video/H264",
                "profile-level-id=42e01f",
                "packetization-mode=1; level-asymmetry-allowed=1",
                true,
            ),
            ("video/VP8", "", "max-fr=60; max-fs=8160", true),
            ("video/VP9", "profile-id=2", "max-fs=8160", true),
            ("video/AV1", "profile=1", "level-idx=31; tier=0", true),
            ("video/unknown", "", "", true),
            ("", "", "", true),
            ("video/unknown", "profile=1", "", false),
            ("video/H264", "device-id=deadbeef", "", false),
            ("video/H264", "", "sprop-parameter-sets=deadbeef", false),
            // Order is part of the canonical form.
            (
                "video/H264",
                "",
                "level-asymmetry-allowed=1; packetization-mode=1",
                false,
            ),
            // A duplicate key exceeds the ordered key budget.
            ("video/VP8", "", "max-fr=60; max-fr=60", false),
            ("video/AV1", "profile=3", "", false),
        ];
        let some = |value: &'static str| (!value.is_empty()).then_some(value);
        for (codec, profile, parameters, want) in samples {
            assert_eq!(
                is_canonical_video_codec_evidence(some(codec), some(profile), some(parameters)),
                want,
                "{codec}|{profile}|{parameters}"
            );
        }
    }
}
