//! Source listing (`--list`), previews (`--preview`) and microphones.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;

use serde::de::{SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};

use crate::Error;
use crate::command::run_bounded;

const MAX_SOURCES: usize = 1024;
pub(crate) const MAX_SOURCE_TITLE_BYTES: usize = 4096;
/// Leaves room for the control envelope within one 256 KiB control message.
pub(crate) const MAX_SOURCE_LIST_BYTES: usize = 192 * 1024;
/// A complete helper list may be larger than the returned prefix; JSON
/// escaping can use six bytes for each title byte.
pub(crate) const MAX_SOURCE_OUTPUT_BYTES: usize =
    MAX_SOURCES * (MAX_SOURCE_TITLE_BYTES * 6 + 256) + 2;
const MAX_PREVIEW_BYTES: usize = 192 * 1024;
const MAX_MICROPHONE_OUTPUT_BYTES: usize = 64 * 1024;
const MAX_MICROPHONES: usize = 64;
const MAX_DEVICE_TEXT_BYTES: usize = 512;

/// One capturable window, display or system-picker entry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CaptureTarget {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub source_id: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub pid: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub creation_time: String,
    #[serde(default)]
    pub title: String,
}

fn is_zero(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn positive_decimal(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 20
        && !value.starts_with('0')
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u64>().is_ok()
}

/// The helper's argument form for an absent creation time.
pub(crate) fn zero_when_empty(value: &str) -> &str {
    if value.is_empty() { "0" } else { value }
}

impl CaptureTarget {
    pub fn valid(&self) -> bool {
        if !positive_decimal(&self.source_id)
            || self.title.is_empty()
            || self.title.len() > MAX_SOURCE_TITLE_BYTES
        {
            return false;
        }
        match self.kind.as_str() {
            "window" => self.pid > 0 && positive_decimal(&self.creation_time),
            "display" | "picker" => self.pid == 0 && self.creation_time.is_empty(),
            _ => false,
        }
    }
}

/// Decodes the helper's list one entry at a time and returns the prefix that
/// fits [`MAX_SOURCE_LIST_BYTES`] when serialized. Every entry, including those
/// beyond the prefix, must be valid.
struct SourceList(Vec<CaptureTarget>);

impl<'de> Deserialize<'de> for SourceList {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ListVisitor;

        impl<'de> Visitor<'de> for ListVisitor {
            type Value = SourceList;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a capture source array")
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<SourceList, A::Error> {
                use serde::de::Error as _;
                let mut targets = Vec::new();
                let mut size = 2; // Array delimiters; later entries also need a comma.
                let mut full = false;
                let mut count = 0;
                while let Some(target) = sequence.next_element::<CaptureTarget>()? {
                    count += 1;
                    if count > MAX_SOURCES || !target.valid() {
                        return Err(A::Error::custom("invalid capture source"));
                    }
                    if full {
                        continue;
                    }
                    let encoded = serde_json::to_vec(&target).map_err(A::Error::custom)?.len();
                    if size + encoded + 1 > MAX_SOURCE_LIST_BYTES {
                        full = true;
                        continue;
                    }
                    targets.push(target);
                    size += encoded + 1;
                }
                Ok(SourceList(targets))
            }
        }

        deserializer.deserialize_seq(ListVisitor)
    }
}

pub(crate) fn decode_sources(payload: &[u8]) -> Result<Vec<CaptureTarget>, Error> {
    let invalid = Error::Invalid("native capture source list is invalid");
    if payload.len() > MAX_SOURCE_OUTPUT_BYTES {
        return Err(invalid);
    }
    serde_json::from_slice::<SourceList>(payload)
        .map(|list| list.0)
        .map_err(|_| invalid)
}

/// Lists capturable sources within the probe timeout.
pub async fn list_sources(executable: &Path) -> Result<Vec<CaptureTarget>, Error> {
    // One past the bound distinguishes an oversized list from a complete one.
    let output = run_bounded(
        executable,
        &["--list"],
        MAX_SOURCE_OUTPUT_BYTES + 1,
        Some("native-source-list"),
    )
    .await
    .map_err(|error| {
        tracing::debug!(
            target: "piik_native_capture",
            event = "capture-source-list-failed",
            error = %piik_diagnostics::safe_text(&error.to_string()),
            "piik-client"
        );
        match error {
            crate::CommandError::OutputExceeded => {
                Error::Invalid("native capture source list is invalid")
            }
            _ => Error::Invalid("native capture source list is unavailable"),
        }
    })?;
    decode_sources(&output)
}

/// Returns one bounded BMP preview of `target`.
pub async fn preview_source(executable: &Path, target: &CaptureTarget) -> Result<Vec<u8>, Error> {
    if !target.valid() {
        return Err(Error::Invalid("native preview target is invalid"));
    }
    let pid = target.pid.to_string();
    let arguments = [
        "--preview",
        target.kind.as_str(),
        target.source_id.as_str(),
        pid.as_str(),
        zero_when_empty(&target.creation_time),
    ];
    let preview = run_bounded(
        executable,
        &arguments,
        MAX_PREVIEW_BYTES,
        Some("native-source-preview"),
    )
    .await
    .map_err(|error| {
        tracing::debug!(
            target: "piik_native_capture",
            event = "capture-source-preview-failed",
            error = %piik_diagnostics::safe_text(&error.to_string()),
            "piik-client"
        );
        Error::Invalid("native capture preview is unavailable")
    })?;
    if preview.len() < 54 || !preview.starts_with(b"BM") {
        return Err(Error::Invalid("native capture preview is invalid"));
    }
    Ok(preview)
}

/// Device identities remain local to the authenticated App control connection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Microphone {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
}

fn valid_device_text(value: &str) -> bool {
    value.len() <= MAX_DEVICE_TEXT_BYTES && !value.contains('\0')
}

/// The empty identity selects the system default microphone.
pub fn valid_device_id(id: &str) -> bool {
    valid_device_text(id)
}

pub(crate) fn decode_microphones(payload: &[u8]) -> Result<Vec<Microphone>, Error> {
    let devices: Option<Vec<Microphone>> = serde_json::from_slice(payload).ok().flatten();
    let devices = devices
        .filter(|devices| devices.len() <= MAX_MICROPHONES)
        .ok_or(Error::Invalid("native microphone list is invalid"))?;
    let mut seen = HashSet::with_capacity(devices.len());
    for device in &devices {
        if device.id.is_empty()
            || !valid_device_id(&device.id)
            || !seen.insert(device.id.as_str())
            || device.label.is_empty()
            || !valid_device_text(&device.label)
        {
            return Err(Error::Invalid("native microphone identity is invalid"));
        }
    }
    Ok(devices)
}

pub async fn list_microphones(executable: &Path) -> Result<Vec<Microphone>, Error> {
    let output = run_bounded(
        executable,
        &["--list-microphones"],
        MAX_MICROPHONE_OUTPUT_BYTES,
        None,
    )
    .await
    .map_err(|_| Error::Invalid("native microphone list is unavailable"))?;
    decode_microphones(&output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(index: usize, title: &str) -> CaptureTarget {
        CaptureTarget {
            kind: "window".into(),
            source_id: (index + 1).to_string(),
            pid: 1,
            creation_time: "1".into(),
            title: title.into(),
        }
    }

    #[test]
    fn source_list_bounds_follow_serialized_output() {
        // serde_json escapes quotes as two bytes; it does not HTML-escape.
        for (name, title, count, truncated) in [
            ("empty", "Window".to_owned(), 0, false),
            ("long-localized-titles", "界".repeat(512), 64, false),
            ("many-windows", "Window".to_owned(), MAX_SOURCES, false),
            ("escaped-titles", "\"".repeat(512), 200, true),
            (
                "full-list",
                "a".repeat(MAX_SOURCE_TITLE_BYTES),
                MAX_SOURCES,
                true,
            ),
        ] {
            let targets: Vec<_> = (0..count).map(|index| window(index, &title)).collect();
            let input = serde_json::to_vec(&targets).expect("encode list");
            let got = decode_sources(&input).unwrap_or_else(|error| panic!("{name}: {error}"));
            if truncated {
                assert!(
                    !got.is_empty() && got.len() < targets.len(),
                    "{name}: {} of {}",
                    got.len(),
                    targets.len()
                );
            } else {
                assert_eq!(got.len(), targets.len(), "{name}");
            }
            assert_eq!(
                got[..],
                targets[..got.len()],
                "{name}: bounded list changed a source"
            );
            let output = serde_json::to_vec(&got).expect("encode response");
            assert!(
                output.len() <= MAX_SOURCE_LIST_BYTES,
                "{name}: response is {} bytes",
                output.len()
            );
        }
    }

    #[test]
    fn source_list_rejects_malformed_output_even_after_the_returned_prefix() {
        let valid = r#"{"kind":"display","sourceId":"1","title":"Display"}"#;
        let full = format!(
            r#"{{"kind":"display","sourceId":"1","title":"{}"}}"#,
            "a".repeat(MAX_SOURCE_TITLE_BYTES)
        );
        for payload in [
            "null".to_owned(),
            "{}".to_owned(),
            "[".to_owned(),
            format!("[{valid}"),
            format!("[{valid}] []"),
            r#"[{"kind":"display","sourceId":"1","title":"Display","extra":true}]"#.to_owned(),
            format!("[{}{valid}]", format!("{valid},").repeat(MAX_SOURCES)),
            format!(
                r#"[{}{{"kind":"wrong","sourceId":"1","title":"Window"}}]"#,
                format!("{full},").repeat(100)
            ),
            format!("[]{}", " ".repeat(MAX_SOURCE_OUTPUT_BYTES)),
        ] {
            assert!(
                decode_sources(payload.as_bytes()).is_err(),
                "invalid helper output was accepted"
            );
        }
    }

    #[test]
    fn source_list_rejects_private_protocol_drift() {
        let payload = r#"[{"kind":"window","sourceId":"1","pid":2,"creationTime":"3","title":"Game","extra":true}]"#;
        assert!(
            serde_json::from_str::<Vec<CaptureTarget>>(payload).is_err(),
            "unknown source field accepted"
        );
    }

    #[test]
    fn capture_target_identity_is_bounded_by_kind() {
        let window = CaptureTarget {
            kind: "window".into(),
            source_id: "123".into(),
            pid: 42,
            creation_time: "456".into(),
            title: "Game".into(),
        };
        let display = CaptureTarget {
            kind: "display".into(),
            source_id: "789".into(),
            title: "Display 1".into(),
            ..CaptureTarget::default()
        };
        let picker = CaptureTarget {
            kind: "picker".into(),
            source_id: "1".into(),
            title: "System picker".into(),
            ..CaptureTarget::default()
        };
        assert!(
            window.valid() && display.valid() && picker.valid(),
            "valid capture targets were rejected"
        );
        for invalid in [
            CaptureTarget {
                creation_time: String::new(),
                ..window.clone()
            },
            CaptureTarget {
                pid: 42,
                ..display.clone()
            },
            CaptureTarget {
                creation_time: "456".into(),
                ..display.clone()
            },
            CaptureTarget {
                pid: 42,
                ..picker.clone()
            },
            CaptureTarget {
                kind: "other".into(),
                ..display.clone()
            },
            CaptureTarget {
                source_id: "0123".into(),
                ..display.clone()
            },
            CaptureTarget {
                source_id: "99999999999999999999".into(),
                ..display.clone()
            },
            CaptureTarget {
                title: String::new(),
                ..display.clone()
            },
        ] {
            assert!(
                !invalid.valid(),
                "invalid capture target was accepted: {invalid:?}"
            );
        }
    }

    #[test]
    fn microphones_are_strict_unique_and_bounded() {
        let devices =
            decode_microphones(br#"[{"id":"{a}","label":"Mic"},{"id":"{b}","label":"Headset"}]"#)
                .expect("list");
        assert_eq!(devices.len(), 2);
        assert!(decode_microphones(b"[]").expect("empty list").is_empty());
        let many = format!(
            "[{}]",
            (0..65)
                .map(|index| format!(r#"{{"id":"{index}","label":"Mic"}}"#))
                .collect::<Vec<_>>()
                .join(",")
        );
        for payload in [
            "null".to_owned(),
            r#"[{"id":"a","label":"Mic","extra":1}]"#.to_owned(),
            r#"[{"id":"a","label":"Mic"},{"id":"a","label":"Other"}]"#.to_owned(),
            r#"[{"id":"","label":"Mic"}]"#.to_owned(),
            r#"[{"id":"a","label":""}]"#.to_owned(),
            r#"[{"id":"a\u0000","label":"Mic"}]"#.to_owned(),
            many,
        ] {
            assert!(
                decode_microphones(payload.as_bytes()).is_err(),
                "invalid microphones accepted: {payload}"
            );
        }
        assert!(valid_device_id("") && !valid_device_id(&"a".repeat(513)));
    }
}
