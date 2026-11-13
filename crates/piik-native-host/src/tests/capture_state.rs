//! Capture-state contract tests.

use piik_native_capture::{VideoProfile, screen_share_outputs};
use serde_json::{Map, Value};

use crate::capture::{CaptureState, decode_capture_state};

/// The payload with six outputs derived from its size.
fn payload(json: &str) -> Vec<u8> {
    let mut fields: Map<String, Value> = serde_json::from_str(json).unwrap();
    let read = |key: &str, fallback: u32| {
        fields
            .get(key)
            .and_then(Value::as_u64)
            .map_or(fallback, |value| value as u32)
    };
    let profile = VideoProfile {
        width: read("width", 1280),
        height: read("height", 720),
        framerate: read("fps", 30),
        bitrate: 3_000_000,
        preference: String::new(),
    };
    let mut slots = screen_share_outputs(&profile);
    for _ in 0..4 {
        slots.push(slots[0]);
    }
    fields.insert("outputs".into(), serde_json::to_value(slots).unwrap());
    serde_json::to_vec(&fields).unwrap()
}

pub(crate) fn state_payload(json: &str) -> String {
    String::from_utf8(payload(json)).unwrap()
}

#[test]
fn capture_state_keeps_starting_and_active_contracts_distinct() {
    let starting = decode_capture_state(&payload(
        r#"{"state":"starting","hardwareOnly":true,"codec":"h264","adapterIndex":0,"adapterName":"GPU","adapterIdentity":"0:1","encoderIndex":0,"encoderName":"H264","encoderIdentity":"encoder"}"#,
    ))
    .unwrap();
    assert!(starting.state == "starting" && starting.adapter_index.is_some());
    let active = decode_capture_state(&payload(
        r#"{"state":"active","hardwareOnly":true,"codec":"h264","profileLevelId":"42c01f","width":1280,"height":720,"fps":30}"#,
    ))
    .unwrap();
    assert!(active.state == "active" && active.profile_level_id == "42c01f");
    for json in [
        r#"{"state":"starting","hardwareOnly":false,"codec":"vp8","adapterIndex":0,"adapterName":"GPU","adapterIdentity":"0:1","encoderName":"libvpx VP8","encoderIdentity":"libvpx/v1.17.0"}"#,
        r#"{"state":"active","hardwareOnly":false,"codec":"vp8","width":1280,"height":720,"fps":30}"#,
        r#"{"state":"active","hardwareOnly":true,"codec":"h264","profileLevelId":"42c01e","width":854,"height":480,"fps":15}"#,
        r#"{"state":"active","hardwareOnly":true,"codec":"h264","profileLevelId":"42c033","width":2560,"height":1440,"fps":60}"#,
    ] {
        assert!(decode_capture_state(&payload(json)).is_ok(), "{json}");
    }
}

#[test]
fn capture_state_rejects_unknown_or_unattributed_state() {
    for json in [
        r#"{"state":"active","hardwareOnly":true,"codec":"vp8","width":1280,"height":720,"fps":30}"#,
        r#"{"state":"active","hardwareOnly":false,"codec":"vp8","profileLevelId":"42c01f","width":1280,"height":720,"fps":30}"#,
        r#"{"state":"active","hardwareOnly":false}"#,
        r#"{"state":"ready","hardwareOnly":true}"#,
        r#"{"state":"active","hardwareOnly":true,"codec":"h264","extra":true}"#,
    ] {
        assert!(decode_capture_state(&payload(json)).is_err(), "{json}");
    }
}

#[test]
fn capture_state_validates_h264_tools_rather_than_constraint_spelling() {
    let state = |profile: &str| {
        payload(&format!(
            r#"{{"state":"active","hardwareOnly":true,"codec":"h264","profileLevelId":"{profile}","width":1280,"height":720,"fps":30}}"#
        ))
    };
    for profile in [
        "42c01f", "42e01f", "42E033", "4d801f", "58c02a", "42c00a", "42c016", "42c034",
    ] {
        let decoded = decode_capture_state(&state(profile)).unwrap();
        assert_eq!(decoded.profile_level_id, profile);
    }
    for profile in [
        "42001f", "4d001f", "64001f", "42e11f", "42e01", "42e01fg", "42e0gg", "42e000", "42e035",
        "42e03c",
    ] {
        assert!(decode_capture_state(&state(profile)).is_err(), "{profile}");
    }
}

#[test]
fn capture_state_uses_original_slot_and_bounds_physical_groups() {
    let state = decode_capture_state(&payload(
        r#"{"state":"active","hardwareOnly":false,"codec":"vp8","width":1280,"height":720,"fps":30}"#,
    ))
    .unwrap();
    assert!(state.outputs.len() == 6 && state.outputs[5] == state.outputs[0]);
    let mut oversized = state.clone();
    oversized.width = state.outputs[1].width + 2;
    let mut too_many = state.clone();
    too_many.outputs.push(too_many.outputs[0]);
    for invalid in [oversized, too_many] {
        let encoded = serde_json::to_vec(&invalid).unwrap();
        assert!(decode_capture_state(&encoded).is_err());
    }
}

#[test]
fn capture_state_applies_selected_backend() {
    let state = CaptureState {
        adapter_index: Some(1),
        encoder_index: Some(2),
        ..CaptureState::default()
    };
    let mut options = piik_native_capture::VideoOptions::default();
    state.apply_backend(&mut options);
    assert!(options.adapter_index == 1 && options.encoder_index == 2);
}
