//! `Number(string)` coercion, which the TypeScript integer settings inherited.

use std::sync::LazyLock;

use piik_protocol::is_js_whitespace;
use regex::Regex;

/// The StrDecimalLiteral grammar `Number(string)` accepts.
static DECIMAL_NUMBER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)(?:[eE][+-]?[0-9]+)?$")
        .expect("constant pattern compiles")
});

/// Ports `Number(string)`. `None` is where JavaScript yields NaN; overflowing
/// literals are rejected later by the safe-integer bound either way.
pub(crate) fn js_number(value: &str) -> Option<f64> {
    let text = value.trim_matches(is_js_whitespace);
    if text.is_empty() {
        return Some(0.0); // Number("") and Number(" ") are 0.
    }
    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[0] == b'0' {
        let base = match bytes[1] {
            b'x' | b'X' => 16,
            b'o' | b'O' => 8,
            b'b' | b'B' => 2,
            _ => 0,
        };
        if base != 0 {
            let digits = &text[2..];
            // from_str_radix accepts a leading sign; Number("0x+1") is NaN.
            if digits.starts_with(['+', '-']) {
                return None;
            }
            return u64::from_str_radix(digits, base).ok().map(|n| n as f64);
        }
    }
    if !DECIMAL_NUMBER.is_match(text) {
        return None;
    }
    text.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::js_number;

    #[test]
    fn coerces_like_javascript() {
        for (input, want) in [
            ("", Some(0.0)),
            (" 9123 ", Some(9123.0)),
            ("0x1F5B", Some(8027.0)),
            ("0o17", Some(15.0)),
            ("0b101", Some(5.0)),
            ("1.", Some(1.0)),
            (".5", Some(0.5)),
            ("1e3", Some(1000.0)),
            ("-1", Some(-1.0)),
            ("0x+1", None),
            ("0x", None),
            ("eight", None),
            ("NaN", None),
            ("Infinity", None),
            ("1_000", None),
        ] {
            assert_eq!(js_number(input), want, "{input:?}");
        }
    }
}
