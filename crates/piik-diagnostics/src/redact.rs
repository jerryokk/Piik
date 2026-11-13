//! Secret policy shared by application and dependency records.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn pattern(source: &str) -> Regex {
    Regex::new(source).expect("static redaction pattern")
}

static PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)")
});
static BEARER: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)\b(Bearer|Basic)\s+[A-Za-z0-9._~+/=-]+"));
static SECRET_PAIR: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r#"(?i)((?:[a-z_-]*(?:password|passwd|pwd|ufrag|credential|token|secret|authorization|cookie)|a=ice-(?:pwd|ufrag))["']?\s*[:=]\s*["']?)([^\s,"';}]+)"#,
    )
});
static URL_CREDENTIALS: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)(https?|wss?)://[^\s/@]+:[^\s/@]+@"));
static INVITE_FRAGMENT: LazyLock<Regex> =
    LazyLock::new(|| pattern(r#"(?i)(https?://[^\s#"']+)#[^\s"']*"#));
static DIAGNOSTIC_ID: LazyLock<Regex> = LazyLock::new(|| pattern(r"^id-[0-9a-f]{16}$"));
static ICE_USERNAME_MISMATCH: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)(username[^\r\n]*?expected\()[0-9a-f]+(\)\s+actual\()[0-9a-f]+(\))")
});
static ICE_UFRAG_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)(\bufrag\s+)[A-Za-z0-9+/=_-]+"));
static ICE_UNLABELLED_CREDENTIALS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)((?:get(?: all)? TCP connections by ufrag|get port of UDPAddr from ListenUDPInPortRange):)[^\r\n]*",
    )
});
static REMOTE_TRACK_DUMP: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)got new track:[^\r\n]*"));

/// Preserves technical causes and system codes, not authentication material.
/// It is deliberately not a promise to anonymize system paths or network addresses.
pub fn safe_text(text: &str) -> String {
    let text = PRIVATE_KEY.replace_all(text, "[private key redacted]");
    let text = REMOTE_TRACK_DUMP.replace_all(
        &text,
        "got new track: [opaque object omitted; see RTP statistics]",
    );
    // ICE stacks also report usernames as hex and as unlabelled candidate extensions.
    let text = ICE_USERNAME_MISMATCH.replace_all(&text, "${1}[redacted]${2}[redacted]${3}");
    let text = ICE_UNLABELLED_CREDENTIALS.replace_all(&text, "${1} [redacted ICE credentials]");
    let text = ICE_UFRAG_EXTENSION.replace_all(&text, "${1}[redacted]");
    let text = BEARER.replace_all(&text, "${1} [redacted]");
    let text = SECRET_PAIR.replace_all(&text, "${1}[redacted]");
    let text = URL_CREDENTIALS.replace_all(&text, "${1}://[redacted]@");
    INVITE_FRAGMENT
        .replace_all(&text, "${1}#[redacted]")
        .into_owned()
}

/// Permits matching one opaque media identity across separately exported reports.
pub fn id(value: &str) -> String {
    if value.is_empty() || DIAGNOSTIC_ID.is_match(value) {
        return value.to_owned();
    }
    let sum = Sha256::digest(value.as_bytes());
    format!("id-{}", hex::encode(&sum[..8]))
}

fn normalized(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '_' && *c != '-')
        .collect::<String>()
        .to_lowercase()
}

pub(crate) fn private_field(key: &str) -> bool {
    let key = normalized(key);
    key.contains("password")
        || key.contains("token")
        || key.contains("secret")
        || key.contains("privatekey")
        || matches!(
            key.as_str(),
            "authorization" | "cookie" | "setcookie" | "sdp" | "credential"
        )
        || key.ends_with("pwd")
        || key.ends_with("ufrag")
}

pub(crate) fn identity_field(key: &str) -> bool {
    matches!(
        normalized(key).as_str(),
        "peerid"
            | "shareid"
            | "connectionid"
            | "trackid"
            | "streamid"
            | "subid"
            | "publicationgeneration"
            | "sid"
            | "trackidentifier"
            | "rtpstatsid"
            | "rtcpeerid"
    )
}

/// Applies the field policy to one value: private keys are replaced, identity
/// strings are hashed, other strings are filtered, and containers recurse.
pub fn safe_value(key: &str, value: Value) -> Value {
    if private_field(key) {
        return Value::String("[redacted]".to_owned());
    }
    match value {
        Value::String(text) if identity_field(key) => Value::String(id(&text)),
        Value::String(text) => Value::String(safe_text(&text)),
        Value::Object(fields) => Value::Object(
            fields
                .into_iter()
                .map(|(name, field)| {
                    let field = safe_value(&name, field);
                    (name, field)
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(|item| safe_value("", item)).collect())
        }
        other => other,
    }
}
