//! Decrypted input messages and the target geometry they apply to.

use serde::{Deserialize, Serialize};

use crate::gamepad::GamepadReport;

/// A source rectangle in physical desktop pixels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Bounds {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

fn is_zero_f64(value: &f64) -> bool {
    *value == 0.0
}

fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

fn is_zero_i32(value: &i32) -> bool {
    *value == 0
}

fn is_zero_u8(value: &u8) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// One decrypted Viewer message. Unknown fields and trailing data reject the
/// whole message.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct InputEvent {
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub grant: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub intent: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub password: String,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub x: f64,
    #[serde(default, skip_serializing_if = "is_zero_f64")]
    pub y: f64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub button: i64,
    #[serde(default, skip_serializing_if = "is_false")]
    pub down: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub delta: i64,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub dx: i32,
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub dy: i32,
    /// Which of the Viewer's controllers this report belongs to. The Browser's
    /// gamepad index is the address; the report itself stays a pure state.
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    pub pad: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gamepad: Option<GamepadReport>,
}

impl InputEvent {
    pub fn of(kind: &str) -> Self {
        Self {
            kind: kind.to_owned(),
            ..Self::default()
        }
    }

    pub fn key(code: &str, down: bool) -> Self {
        Self {
            code: code.to_owned(),
            down,
            ..Self::of("key")
        }
    }

    pub fn button(button: i64, down: bool) -> Self {
        Self {
            button,
            down,
            ..Self::of("button")
        }
    }

    pub fn text(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            ..Self::of("text")
        }
    }
}
