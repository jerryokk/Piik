//! Replays the cross-language wire samples. `tests/protocol-fixture.test.ts`
//! replays the same file against the zod schemas.

use piik_protocol::*;
use serde_json::{Value, json};

fn fixture() -> Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/wire-samples.json"
    );
    let raw = std::fs::read(path).expect("read fixture");
    serde_json::from_slice(&raw).expect("parse fixture")
}

fn samples<'a>(fixture: &'a Value, key: &str) -> &'a [Value] {
    let samples = fixture[key].as_array().expect("sample array");
    assert!(!samples.is_empty(), "fixture carries no {key}");
    samples
}

fn text<'a>(sample: &'a Value, key: &str) -> &'a str {
    sample[key].as_str().expect("sample string")
}

#[test]
fn fixture_constants() {
    let expected = json!({
        "signalingProtocol": SIGNALING_PROTOCOL,
        "maxViewersPerRoomLimit": MAX_VIEWERS_PER_ROOM_LIMIT,
        "maxParticipantsPerRoomLimit": MAX_PARTICIPANTS_PER_ROOM_LIMIT,
        "maxSignalBytes": MAX_SIGNAL_BYTES,
        "roomCodeLength": ROOM_CODE_LENGTH,
        "maxMediaRouteRevision": MAX_MEDIA_ROUTE_REVISION,
        "maxIceServerUrls": MAX_ICE_SERVER_URLS,
        "maxNatPredictionAuxiliaryStunUrls": MAX_NAT_PREDICTION_AUXILIARY_STUN_URLS,
        "maxViewerQualityEvidenceBytes": MAX_VIEWER_QUALITY_EVIDENCE_BYTES,
        "viewerQualityEvidenceIntervalMs": VIEWER_QUALITY_EVIDENCE_INTERVAL_MS,
        "viewerQualityEvidenceExpiryMs": VIEWER_QUALITY_EVIDENCE_EXPIRY_MS,
        "persistentNativeEdgeDegradedWindows": PERSISTENT_NATIVE_EDGE_DEGRADED_WINDOWS,
        "maxDisplayNameCodePoints": MAX_DISPLAY_NAME_CODE_POINTS,
        "maxChatCodePoints": MAX_CHAT_CODE_POINTS,
        "interactionIntervalMs": INTERACTION_INTERVAL_MS,
        "defaultViewerDisplayName": DEFAULT_VIEWER_DISPLAY_NAME,
        "defaultHostDisplayNamePrefix": DEFAULT_HOST_DISPLAY_NAME_PREFIX,
        "minViewerPasswordLength": MIN_VIEWER_PASSWORD_LENGTH,
        "maxViewerPasswordLength": MAX_VIEWER_PASSWORD_LENGTH,
        "defaultEndpointMediaCopyCapacity": DEFAULT_ENDPOINT_MEDIA_COPY_CAPACITY,
        "maxEndpointMediaCopyCapacity": MAX_ENDPOINT_MEDIA_COPY_CAPACITY,
        "signalCloseCodeServiceRestart": SignalCloseCode::ServiceRestart.code(),
        "signalCloseCodeSessionReplaced": SignalCloseCode::SessionReplaced.code(),
        "signalCloseCodeClientReconnect": SignalCloseCode::ClientReconnect.code(),
        "signalCloseCodeAuthenticationFailed": SignalCloseCode::AuthenticationFailed.code(),
        "signalCloseCodeViewerAccessRevoked": SignalCloseCode::ViewerAccessRevoked.code(),
        "defaultQualitySettings": DEFAULT_QUALITY_SETTINGS.to_wire(),
        "defaultRoutePolicy": DEFAULT_ROUTE_POLICY.to_wire(),
    });
    assert_eq!(fixture()["constants"], expected);
}

#[test]
fn fixture_client_samples() {
    let fixture = fixture();
    for sample in samples(&fixture, "clientMessages") {
        let (name, data) = (text(sample, "name"), text(sample, "json").as_bytes());
        let result = match text(sample, "schema") {
            "client" => decode_client_message(data).map(drop),
            "createRoomRequest" => decode_create_room_request(data).map(drop),
            "replaceRoomRequest" => decode_replace_room_request(data).map(drop),
            "roomAccessUpdateRequest" => decode_room_access_update_request(data).map(drop),
            other => panic!("unknown fixture schema {other:?}"),
        };
        let valid = sample["valid"].as_bool().expect("valid flag");
        match result {
            Err(error) if valid => panic!("{name}: expected the sample to decode, got {error}"),
            Ok(()) if !valid => panic!("{name}: expected the sample to be rejected"),
            _ => {}
        }
    }
}

/// A valid client sample also survives a client encode round trip, which
/// covers the client encoders.
#[test]
fn fixture_valid_client_samples_round_trip() {
    let fixture = fixture();
    for sample in samples(&fixture, "clientMessages") {
        if text(sample, "schema") != "client" || sample["valid"] != json!(true) {
            continue;
        }
        let (name, data) = (text(sample, "name"), text(sample, "json"));
        let message = decode_client_message(data.as_bytes()).expect(name);
        let encoded: Value = serde_json::from_slice(&encode_client_message(&message)).unwrap();
        let mut want: Value = serde_json::from_str(data).unwrap();
        // A host authenticate without routePolicy takes the default, which
        // the encoder then emits.
        if let (Value::Object(want), Some(policy)) = (&mut want, encoded.get("routePolicy")) {
            want.entry("routePolicy").or_insert_with(|| policy.clone());
        }
        let reencoded = decode_client_message(&encode_client_message(&message)).expect(name);
        assert_eq!(
            reencoded, message,
            "{name}: decode(encode) changed the message"
        );
        assert_eq!(encoded, want, "{name}: round trip changed the message");
    }
}

#[test]
fn fixture_server_samples_round_trip() {
    let fixture = fixture();
    for sample in samples(&fixture, "serverMessages") {
        let (name, data) = (text(sample, "name"), text(sample, "json"));
        let message = decode_server_message(data.as_bytes())
            .unwrap_or_else(|error| panic!("{name}: decode: {error}"));
        let encoded = encode_server_message(&message);
        let got: Value = serde_json::from_slice(&encoded).unwrap();
        let want: Value = serde_json::from_str(data).unwrap();
        assert_eq!(
            got,
            want,
            "{name}: round trip changed the message\n got: {}\nwant: {data}",
            String::from_utf8_lossy(&encoded)
        );
    }
}
