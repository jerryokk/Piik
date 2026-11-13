use crate::protocol::*;

#[test]
fn decode_request_accepts_only_the_current_shape() {
    for payload in [
        r#"{"version":10,"id":"request_hello","type":"hello"}"#,
        r#"{"version":10,"id":"request_ping","type":"ping"}"#,
    ] {
        assert!(decode_request(payload.as_bytes()).is_ok(), "{payload}");
    }
    for payload in [
        r#"{"version":3,"id":"request_ping","type":"ping"}"#,
        r#"{"version":10,"id":"short","type":"ping"}"#,
        r#"{"version":10,"id":"request_ping","type":"ping","extra":true}"#,
        r#"{"version":10,"id":"request_hello","type":"hello","nonce":"obsolete"}"#,
        r#"{"version":10,"id":"request_ping","type":"ping"} trailing"#,
    ] {
        assert!(
            decode_request(payload.as_bytes()).is_err(),
            "accepted {payload}"
        );
    }
}

#[test]
fn decode_request_uses_the_shared_identifier_boundary() {
    let identifier = format!("request_{}", "a".repeat(248));
    assert_eq!(identifier.len(), 256);
    let payload = format!(r#"{{"version":10,"id":"{identifier}","type":"ping"}}"#);
    assert!(
        decode_request(payload.as_bytes()).is_ok(),
        "maximum identifier was rejected"
    );
    let too_long = format!(r#"{{"version":10,"id":"{identifier}a","type":"ping"}}"#);
    assert!(decode_request(too_long.as_bytes()).is_err());
}

#[test]
fn validate_messages_require_the_expected_phase() {
    let hello = ControlMessage::new("request_hello", "hello");
    let ping = ControlMessage::new("request_ping", "ping");
    assert!(validate_hello(&hello).is_ok());
    assert!(validate_ping(&ping).is_ok());
    assert!(
        validate_ping(&hello).is_err(),
        "validatePing accepted hello"
    );
    assert!(
        validate_hello(&ping).is_err(),
        "validateHello accepted ping"
    );
}

#[test]
fn envelope_allows_an_extension_to_own_its_strict_shape() {
    let payload = br#"{"version":10,"id":"request_extension","type":"extension","value":1}"#;
    let message = decode_envelope(payload).unwrap();
    assert_eq!(message.kind, "extension");
    assert!(
        decode_request(payload).is_err(),
        "base decoder accepted extension fields"
    );
    assert!(decode_envelope(br#"{"version":10,"id":"request_empty","type":""}"#).is_err());
}

#[test]
fn response_bound_fits_one_source_preview() {
    let mut preview = serde_json::json!({
        "type": "source-preview",
        "mime": "image/bmp",
        "data": "A".repeat((54 + 320 * 180 * 3) * 4 / 3),
    });
    assert!(
        encode_message(&preview).is_ok(),
        "320x180 preview was rejected"
    );
    preview["data"] = "A".repeat(MAX_CONTROL_MESSAGE_BYTES).into();
    assert!(encode_message(&preview).is_err());
}

#[test]
fn normalize_port_range_uses_the_bounded_defaults() {
    assert_eq!(
        normalize_port_range(0, 0),
        (DEFAULT_PORT_START, DEFAULT_PORT_END)
    );
    assert_eq!(normalize_port_range(0, 40000), (40000, 40000));
    assert_eq!(normalize_port_range(40000, 1), (40000, 40000));
}
