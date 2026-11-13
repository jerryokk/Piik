//! App entry URLs: they select the App runtime without carrying authority in
//! the query string.

use std::collections::BTreeMap;

use piik_loopback::{DEFAULT_PORT_END, DEFAULT_PORT_START};

/// Query values: keys sorted on encode, values in arrival order.
type Values = BTreeMap<String, Vec<String>>;

/// Marks `raw` as an App page: `#piik-client=1`, plus `?piik-client-port`
/// when `control_port` is inside the loopback range. Unparseable input is
/// returned unchanged.
pub fn entry_url(raw: &str, control_port: u16) -> String {
    let Some(parts) = Parts::parse(raw) else {
        return raw.to_owned();
    };
    let Ok(mut fragment) = parse_query(parts.fragment) else {
        return raw.to_owned();
    };
    fragment.insert("piik-client".to_owned(), vec!["1".to_owned()]);
    // Malformed query pairs are dropped rather than fatal.
    let mut query = parse_query_lossy(parts.query.unwrap_or_default());
    if (DEFAULT_PORT_START..=DEFAULT_PORT_END).contains(&control_port) {
        query.insert(
            "piik-client-port".to_owned(),
            vec![control_port.to_string()],
        );
    }
    let query = encode_query(&query);
    let mut target = parts.base.to_owned();
    if !query.is_empty() {
        target.push('?');
        target.push_str(&query);
    }
    target.push('#');
    target.push_str(&encode_query(&fragment));
    target
}

/// Passes the local access choice only through the consumed Host fragment.
pub fn local_entry_url(raw: &str, password: &str, control_port: u16) -> String {
    let Some(parts) = Parts::parse(raw) else {
        return raw.to_owned();
    };
    let Ok(mut fragment) = parse_query(parts.fragment) else {
        return raw.to_owned();
    };
    fragment.remove("client-access");
    if !password.is_empty() {
        fragment.insert("client-access".to_owned(), vec![password.to_owned()]);
    }
    let mut target = parts.base.to_owned();
    if let Some(query) = parts.query {
        target.push('?');
        target.push_str(query);
    }
    target.push('#');
    target.push_str(&encode_query(&fragment));
    entry_url(&target, control_port)
}

/// `scheme://authority/path`, raw query and escaped fragment of one URL.
struct Parts<'a> {
    base: &'a str,
    query: Option<&'a str>,
    fragment: &'a str,
}

impl<'a> Parts<'a> {
    fn parse(raw: &'a str) -> Option<Self> {
        // Control characters are rejected; spaces are invalid in hosts.
        if raw.bytes().any(|byte| byte.is_ascii_control()) {
            return None;
        }
        let (rest, fragment) = raw.split_once('#').unwrap_or((raw, ""));
        let (base, query) = match rest.split_once('?') {
            Some((base, query)) => (base, Some(query)),
            None => (rest, None),
        };
        if let Some((_, authority)) = base.split_once("://") {
            let host = authority.split('/').next().unwrap_or_default();
            if host.contains(' ') {
                return None;
            }
        }
        Some(Self {
            base,
            query,
            fragment,
        })
    }
}

#[derive(Debug)]
struct InvalidQuery;

/// Strict query parsing: any malformed pair fails the whole value.
fn parse_query(raw: &str) -> Result<Values, InvalidQuery> {
    let mut values = Values::new();
    let mut valid = true;
    for pair in raw.split('&').filter(|pair| !pair.is_empty()) {
        if pair.contains(';') {
            valid = false;
            continue;
        }
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        match (unescape(key), unescape(value)) {
            (Some(key), Some(value)) => values.entry(key).or_default().push(value),
            _ => valid = false,
        }
    }
    if valid { Ok(values) } else { Err(InvalidQuery) }
}

fn parse_query_lossy(raw: &str) -> Values {
    let mut values = Values::new();
    for pair in raw
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.contains(';'))
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if let (Some(key), Some(value)) = (unescape(key), unescape(value)) {
            values.entry(key).or_default().push(value);
        }
    }
    values
}

/// Decodes a query component: `+` is a space and every `%` needs two hex digits.
fn unescape(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = bytes.get(index + 1..index + 3)?;
                let hex = std::str::from_utf8(hex).ok()?;
                output.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                output.push(b' ');
                index += 1;
            }
            byte => {
                output.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(output).ok()
}

/// Percent-encodes a query component, spaces as `+`.
fn escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                output.push(char::from(byte));
            }
            b' ' => output.push('+'),
            _ => output.push_str(&format!("%{byte:02X}")),
        }
    }
    output
}

/// Encodes query values: sorted keys, escaped pairs joined by `&`.
fn encode_query(values: &Values) -> String {
    let mut pairs = Vec::new();
    for (key, list) in values {
        for value in list {
            pairs.push(format!("{}={}", escape(key), escape(value)));
        }
    }
    pairs.join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query_value(target: &str, part: char, key: &str) -> Option<String> {
        let raw = match part {
            '?' => target.split('#').next()?.split_once('?')?.1,
            _ => target.split_once('#')?.1,
        };
        parse_query(raw).ok()?.get(key)?.first().cloned()
    }

    #[test]
    fn app_launch_url_marks_the_page_without_changing_origin() {
        let value = entry_url("https://share.example/", DEFAULT_PORT_START);
        assert_eq!(
            value,
            "https://share.example/?piik-client-port=39721#piik-client=1"
        );
        assert_eq!(
            entry_url("https://share.example/", 18990),
            "https://share.example/#piik-client=1",
            "a port outside the loopback range is not advertised"
        );
    }

    #[test]
    fn launch_url_preserves_local_access_inside_the_private_fragment() {
        let value = entry_url(
            "http://localhost:8787/#client-access=secret",
            DEFAULT_PORT_START,
        );
        assert_eq!(
            query_value(&value, '?', "piik-client-port").as_deref(),
            Some("39721")
        );
        assert_eq!(
            query_value(&value, '#', "client-access").as_deref(),
            Some("secret")
        );
        assert_eq!(
            query_value(&value, '#', "piik-client").as_deref(),
            Some("1")
        );
    }

    #[test]
    fn launch_url_encodes_and_clears_optional_local_access() {
        for password in ["x", "好", " ", "+", "%", "#&?=", " 中文 a+b&c?d=e% "] {
            let value = local_entry_url(
                "http://localhost:8787/?retained=query#retained=a%2Bb%25&client-access=old&piik-lang=zh&piik-mode=text&piik-theme=dark",
                password,
                DEFAULT_PORT_START,
            );
            for target in [value.clone(), entry_url(&value, DEFAULT_PORT_START)] {
                assert_eq!(
                    query_value(&target, '?', "retained").as_deref(),
                    Some("query")
                );
                assert_eq!(
                    query_value(&target, '?', "piik-client-port").as_deref(),
                    Some("39721")
                );
                // location.hash stays escaped; one decode must give the password.
                for (key, want) in [
                    ("client-access", password),
                    ("piik-client", "1"),
                    ("retained", "a+b%"),
                    ("piik-lang", "zh"),
                    ("piik-mode", "text"),
                    ("piik-theme", "dark"),
                ] {
                    assert_eq!(
                        query_value(&target, '#', key).as_deref(),
                        Some(want),
                        "{target}: {key}"
                    );
                }
            }
        }
        let open = local_entry_url(
            "http://localhost:8787/#client-access=old",
            "",
            DEFAULT_PORT_START,
        );
        assert_eq!(query_value(&open, '#', "client-access"), None);
        assert_eq!(query_value(&open, '#', "piik-client").as_deref(), Some("1"));
    }

    #[test]
    fn malformed_input_is_returned_unchanged() {
        for raw in ["http://a b/", "http://x/\u{7}", "http://x/#bad=%zz"] {
            assert_eq!(entry_url(raw, DEFAULT_PORT_START), raw);
        }
    }
}
