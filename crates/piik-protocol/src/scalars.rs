//! Scalar validators shared by the wire types and the server packages.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use crate::codec::{FromWire, Result, ToWire, invalid, type_error};
use crate::constants::{
    MAX_DISPLAY_NAME_CODE_POINTS, MAX_VIEWER_PASSWORD_LENGTH, MIN_VIEWER_PASSWORD_LENGTH,
    ROOM_CODE_LENGTH,
};

/// Compiles one constant pattern.
macro_rules! pattern {
    ($name:ident, $source:expr) => {
        pub(crate) static $name: ::std::sync::LazyLock<::regex::Regex> =
            ::std::sync::LazyLock::new(|| {
                ::regex::Regex::new($source).expect("constant pattern compiles")
            });
    };
}

/// Counts UTF-16 code units, which is what zod's string `.min/.max/.length`
/// and `String.prototype.length` measure: not bytes, not code points.
pub fn utf16_length(value: &str) -> usize {
    value.chars().map(char::len_utf16).sum()
}

/// Matches the JavaScript regular-expression `\s` class, which is also what
/// `String.prototype.trim` and `Number(string)` strip.
pub fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\t' | '\n' | '\u{0b}' | '\u{0c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

pattern!(OPAQUE_ID_PATTERN, r"^[A-Za-z0-9_-]+$");
pattern!(ROOM_CODE_PATTERN, r"^[1-9][0-9]*$");
pattern!(VIEWER_GRANT_PATTERN, r"^[A-Za-z0-9_-]{21}[AQgw]$");
pattern!(VIEWER_PASSWORD_PATTERN, r"^[\x21-\x7e]+$");

/// `FORBIDDEN_DISPLAY_NAME_CHARACTERS` from `src/shared/protocol.ts`.
pub(crate) static FORBIDDEN_DISPLAY_NAME_CHARACTERS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"[\p{Cc}\p{Zl}\p{Zp}\x{061c}\x{200b}\x{200e}\x{200f}\x{202a}-\x{202e}",
        r"\x{2060}\x{2066}-\x{2069}\x{feff}]"
    ))
    .expect("constant pattern compiles")
});

/// Mirrors `opaqueIdSchema`: 8..128 code units of `[A-Za-z0-9_-]`.
pub fn valid_opaque_id(value: &str) -> bool {
    (8..=128).contains(&utf16_length(value)) && OPAQUE_ID_PATTERN.is_match(value)
}

/// Mirrors `tokenSchema`: 32..128 code units of `[A-Za-z0-9_-]`.
pub fn valid_token(value: &str) -> bool {
    (32..=128).contains(&utf16_length(value)) && OPAQUE_ID_PATTERN.is_match(value)
}

/// Mirrors `roomCodeSchema`.
pub fn valid_room_code(value: &str) -> bool {
    utf16_length(value) == ROOM_CODE_LENGTH && ROOM_CODE_PATTERN.is_match(value)
}

/// Mirrors `viewerGrantSchema`.
pub fn valid_viewer_grant(value: &str) -> bool {
    utf16_length(value) == 22 && VIEWER_GRANT_PATTERN.is_match(value)
}

/// Mirrors `viewerPasswordSchema`.
pub fn valid_viewer_password(value: &str) -> bool {
    (MIN_VIEWER_PASSWORD_LENGTH..=MAX_VIEWER_PASSWORD_LENGTH).contains(&utf16_length(value))
        && VIEWER_PASSWORD_PATTERN.is_match(value)
}

/// `\p{Zs}`. Every other character trimmed by `String.prototype.trim` (Cc,
/// Zl, Zp, FEFF) is already forbidden, so trimming Zs alone is equivalent.
fn is_unicode_space_separator(character: char) -> bool {
    matches!(
        character,
        ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
    )
}

/// `.replace(/\p{Zs}+/gu, " ")`.
fn collapse_unicode_spaces(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_space = false;
    for character in value.chars() {
        if is_unicode_space_separator(character) {
            pending_space = true;
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(character);
    }
    if pending_space {
        out.push(' ');
    }
    out
}

/// Returns `None` where the shared TypeScript normalizer returns null. A Rust
/// string cannot carry an unpaired surrogate, so that rejection is implicit.
pub fn normalize_display_name(value: &str) -> Option<String> {
    if FORBIDDEN_DISPLAY_NAME_CHARACTERS.is_match(value) {
        return None;
    }
    let composed: String = value.nfc().collect();
    let normalized = collapse_unicode_spaces(composed.trim_matches(is_unicode_space_separator));
    let count = normalized.chars().count();
    (1..=MAX_DISPLAY_NAME_CODE_POINTS)
        .contains(&count)
        .then_some(normalized)
}

/// Mirrors `displayNameSchema`'s bounds and canonical refine.
pub fn valid_display_name(value: &str) -> bool {
    (1..=MAX_DISPLAY_NAME_CODE_POINTS * 4).contains(&utf16_length(value))
        && normalize_display_name(value).as_deref() == Some(value)
}

/// A display name that `displayNameSchema` accepts: 1..96 UTF-16 code units
/// that the normalizer leaves unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DisplayName(String);

impl DisplayName {
    /// Accepts only an already canonical display name.
    pub fn new(value: impl Into<String>) -> Option<Self> {
        let value = value.into();
        valid_display_name(&value).then_some(Self(value))
    }

    /// Normalizes raw user input into a canonical display name.
    pub fn normalize(value: &str) -> Option<Self> {
        normalize_display_name(value).map(Self)
    }

    /// The canonical text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DisplayName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl AsRef<str> for DisplayName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromWire for DisplayName {
    /// A JSON null decodes as the empty string and therefore fails.
    /// An unpaired surrogate escape is already rejected by the JSON parser.
    fn from_wire(value: &Value) -> Result<Self> {
        let text = match value {
            Value::Null => String::new(),
            Value::String(text) => text.clone(),
            other => return Err(type_error("a string", other)),
        };
        DisplayName::new(text).ok_or_else(|| invalid("display name must be canonical"))
    }
}

impl ToWire for DisplayName {
    fn to_wire(&self) -> Value {
        Value::String(self.0.clone())
    }
}

/// Ports `isValidStunUrl` from `src/shared/protocol.ts`. The TypeScript feeds
/// `http://<authority>` to the WHATWG URL parser; the `url` crate implements
/// that standard, so the browser answers apply directly.
pub fn valid_stun_url(value: &str) -> bool {
    let Some(separator) = value.find(':') else {
        return false;
    };
    if separator == 0 || !value[..separator].eq_ignore_ascii_case("stun") {
        return false;
    }
    let authority = &value[separator + 1..];
    if authority.is_empty()
        || authority.ends_with(':')
        || authority
            .chars()
            .any(|c| matches!(c, '\\' | '/' | '?' | '#') || is_js_whitespace(c))
    {
        return false;
    }
    let Ok(parsed) = url::Url::parse(&format!("http://{authority}")) else {
        return false;
    };
    parsed.host_str().is_some_and(|host| !host.is_empty())
        && parsed.username().is_empty()
        && parsed.password().is_none_or(str::is_empty)
        && parsed.path() == "/"
        && parsed.query().is_none_or(str::is_empty)
        && parsed.fragment().is_none_or(str::is_empty)
        && parsed.port().is_none_or(|port| port > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::decode;

    #[test]
    fn utf16_len() {
        // zod's string bounds count UTF-16 code units, so an astral code point
        // counts as two.
        for (value, want) in [
            ("", 0),
            ("abc", 3),
            ("名", 1),
            ("\u{1f469}", 2),
            ("\u{1f469}\u{200d}\u{1f4bb}", 5),
        ] {
            assert_eq!(utf16_length(value), want, "{value:?}");
        }
    }

    #[test]
    fn normalize_display_names() {
        let at_limit = "名".repeat(MAX_DISPLAY_NAME_CODE_POINTS);
        let past_limit = "名".repeat(MAX_DISPLAY_NAME_CODE_POINTS + 1);
        let samples: [(&str, &str, Option<&str>); 9] = [
            (
                "composes and collapses",
                "  Cafe\u{301} 朋友  ",
                Some("Café 朋友"),
            ),
            (
                "keeps a zero width joiner sequence",
                "玩家 \u{1f469}\u{200d}\u{1f4bb}",
                Some("玩家 \u{1f469}\u{200d}\u{1f4bb}"),
            ),
            ("accepts the code point limit", &at_limit, Some(&at_limit)),
            ("rejects one code point past the limit", &past_limit, None),
            ("rejects a control character", "a\nb", None),
            ("rejects a bidi override", "a\u{202e}b", None),
            ("rejects a byte order mark", "a\u{feff}b", None),
            ("rejects a zero width space", "a\u{200b}b", None),
            ("rejects an all-space name", "   ", None),
        ];
        for (name, value, want) in samples {
            assert_eq!(normalize_display_name(value).as_deref(), want, "{name}");
        }
    }

    #[test]
    fn valid_display_name_requires_canonical_form() {
        assert!(!valid_display_name(" Café "));
        assert!(valid_display_name("Café 朋友"));
        assert!(!valid_display_name(""));
    }

    #[test]
    fn display_name_rejects_surrogate_escapes() {
        assert!(decode::<DisplayName>(br#""a\ud800b""#).is_err());
        assert!(decode::<DisplayName>(br#""a\udc00b""#).is_err());
        assert!(decode::<DisplayName>("\"a😀b\"".as_bytes()).is_ok());
        assert!(decode::<DisplayName>(b"null").is_err());
    }

    #[test]
    fn scalar_validators() {
        let a128 = "a".repeat(128);
        let a129 = "a".repeat(129);
        let a32 = "a".repeat(32);
        let a31 = "a".repeat(31);
        let grant = "b".repeat(21) + "g";
        let grant_short = "b".repeat(21);
        let grant_long = "b".repeat(23);
        let grant_padded = "b".repeat(21) + "=";
        let grant_tail = "b".repeat(21) + "b";
        let long_password = "x".repeat(MAX_VIEWER_PASSWORD_LENGTH + 1);
        type Validator = fn(&str) -> bool;
        let samples: [(&str, Validator, &str, bool); 23] = [
            ("opaque id at floor", valid_opaque_id, "abcdefgh", true),
            ("opaque id below floor", valid_opaque_id, "abcdefg", false),
            (
                "opaque id with punctuation",
                valid_opaque_id,
                "abcdefg.",
                false,
            ),
            ("opaque id at ceiling", valid_opaque_id, &a128, true),
            ("opaque id past ceiling", valid_opaque_id, &a129, false),
            ("token at floor", valid_token, &a32, true),
            ("token below floor", valid_token, &a31, false),
            ("token past ceiling", valid_token, &a129, false),
            ("room code", valid_room_code, "1234", true),
            (
                "room code with leading zero",
                valid_room_code,
                "0123",
                false,
            ),
            ("room code too long", valid_room_code, "12345", false),
            ("room code too short", valid_room_code, "123", false),
            ("viewer grant", valid_viewer_grant, &grant, true),
            (
                "viewer grant short",
                valid_viewer_grant,
                &grant_short,
                false,
            ),
            ("viewer grant long", valid_viewer_grant, &grant_long, false),
            (
                "viewer grant padded",
                valid_viewer_grant,
                &grant_padded,
                false,
            ),
            (
                "viewer grant off alphabet tail",
                valid_viewer_grant,
                &grant_tail,
                false,
            ),
            ("viewer password minimal", valid_viewer_password, "x", true),
            (
                "viewer password simple",
                valid_viewer_password,
                "simple-password",
                true,
            ),
            ("viewer password empty", valid_viewer_password, "", false),
            (
                "viewer password with space",
                valid_viewer_password,
                "contains space",
                false,
            ),
            (
                "viewer password with newline",
                valid_viewer_password,
                "line\nbreak",
                false,
            ),
            (
                "viewer password past ceiling",
                valid_viewer_password,
                &long_password,
                false,
            ),
        ];
        for (name, valid, value, want) in samples {
            assert_eq!(valid(value), want, "{name}");
        }
    }

    /// The WHATWG answers `isValidStunUrl` produces in the browser.
    #[test]
    fn stun_urls() {
        for (value, want) in [
            ("stun:stun.test:3478", true),
            ("stun:share.test:5349", true),
            ("STUN:[2001:db8::1]:3478", true),
            ("stun:stun.test", true),
            ("stun:stun.test/path", false),
            ("stun:stun.test:80/", false),
            ("stun:stun.test:3478/", false),
            ("stun:stun.test?transport=udp", false),
            ("stun:stun.test#fragment", false),
            ("stun:", false),
            ("stun:stun.test:", false),
            ("stun:stun.test:0", false),
            ("stun:user@stun.test:3478", false),
            ("turn:turn.test:3478", false),
            ("stuns:stun.test:5349", false),
            ("stun:relay.example.test:3478?transport=udp", false),
            ("stun:stun.test:65535", true),
            ("stun:stun.test:65536", false),
            ("stun:stun.test:99999", false),
            ("stun:a<b:3478", false),
            ("stun:a>b:3478", false),
            ("stun:a]b:3478", false),
            ("stun:a|b:3478", false),
            ("stun:a^b:3478", false),
            ("stun:999.999.999.999", false),
            ("stun:1.2.3.4.5", false),
            ("stun:09.1", false),
            ("stun:1.2.3.256", false),
            ("stun:1.2.3.4", true),
            ("stun:1.2.3.4.", true),
            ("stun:0300.0250.0.1", true),
            ("stun:0x7f.1", true),
            ("stun:123", true),
            ("stun:[1.2.3.4]", false),
            ("stun:[notanip]:3478", false),
            ("stun:[2001:db8::1", false),
            ("stun:[::1]", true),
            ("stun:@stun.test:3478", true),
            ("stun::@stun.test:3478", true),
            ("stun::pass@stun.test:3478", false),
            ("stun:user:@stun.test:3478", false),
            ("stun::3478", false),
            ("stun:stun.test:+3478", false),
            ("stun:stun.test:34a78", false),
            ("stun:stun test:3478", false),
            ("stun:stun.test\\path", false),
            ("stun:a_b.test", true),
            ("stun:stun.test:03478", true),
        ] {
            assert_eq!(valid_stun_url(value), want, "{value}");
        }
    }
}
