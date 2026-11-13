//! Route debug event tests.

use std::sync::{Arc, Mutex};

use piik_protocol::{
    MAX_VIEWERS_PER_ROOM_LIMIT, ServerMessage, decode_server_message, encode_server_message,
};
use serde_json::Value;

use super::*;
use crate::*;

#[test]
fn diagnostic_readers_accept_current_room_producer() {
    let mut routes = controller(1, opts());
    add_viewer(&mut routes, A, 20, None);
    routes.set_relay_volunteer(A, &session_of(A), true, None);
    routes.hydrate_edge(A, peer_edge(HOST, "host_a"));
    for peer in [B, C, D, E] {
        add_viewer(&mut routes, peer, 0, None);
        routes.hydrate_edge(peer, peer_edge(A, &format!("{peer}_edge")));
    }
    routes.set_video_publication("new_video", false, 1);
    // Replacement preserves the room edges and prepares one video attachment.
    routes.set_video_publication("replacement_video", false, 2);
    routes.reconcile(3);
    let encoded = encode_server_message(&ServerMessage::RouteDiagnosticSnapshot(
        routes.route_diagnostic_snapshot(3),
    ));
    decode_server_message(&encoded).expect("current producer accepted");
    for field in ["effectiveCapacity", "childCount"] {
        let mut invalid: Value = serde_json::from_slice(&encoded).unwrap();
        invalid["snapshot"]["children"][0][field] =
            Value::from(MAX_VIEWERS_PER_ROOM_LIMIT as i64 + 1);
        let raw = serde_json::to_vec(&invalid).unwrap();
        assert!(
            decode_server_message(&raw).is_err(),
            "diagnostic {field} exceeded the room resource bound"
        );
    }
    // The same production output is replayed by the TypeScript wire fixture.
    let raw = std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/fixtures/wire-samples.json"
    ))
    .unwrap();
    let fixture: Value = serde_json::from_slice(&raw).unwrap();
    let sample = fixture["serverMessages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|sample| sample["name"] == "room-relay-video-attachment-diagnostic")
        .expect("current room diagnostic is in the cross-language fixture");
    assert_eq!(
        sample["json"].as_str().unwrap().as_bytes(),
        encoded.as_slice(),
        "fixture drifted from the room producer"
    );
}

#[test]
fn controller_debug_requires_injected_sink_and_room() {
    let seen: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let sink_seen = seen.clone();
    let sink: DebugSink = Arc::new(move |kind, fields| {
        sink_seen
            .lock()
            .unwrap()
            .push((kind.to_owned(), fields[0].1.clone()));
    });
    let build = |room: &str, sink: Option<DebugSink>| {
        Controller::new(Options {
            host_peer_id: HOST.to_owned(),
            debug_room_id: room.to_owned(),
            debug_log: sink,
            endpoint_media_copy_capacity: 2,
            operation_timeout_ms: 10_000,
            ..Options::default()
        })
    };
    seen.lock().unwrap().clear();
    let enabled = build("1234", Some(sink.clone()));
    assert_eq!(
        *seen.lock().unwrap(),
        [("piik-route".to_owned(), "controller-created".to_owned())]
    );
    enabled.emit_debug("operation-started", vec![("child", "viewer-1".to_owned())]);
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(seen.lock().unwrap()[1].1, "operation-started");
    build("1234", None).emit_debug("no-sink", Vec::new());
    build("", Some(sink)).emit_debug("no-room", Vec::new());
    assert_eq!(
        seen.lock().unwrap().len(),
        2,
        "disabled controller emitted events"
    );
}
