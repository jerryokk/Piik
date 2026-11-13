//! Signaling server tests over real WebSocket clients.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use piik_protocol::{
    CodeEntryPolicy, DEFAULT_ROUTE_POLICY, MAX_VIEWERS_PER_ROOM_LIMIT, RoomAccessUpdateRequest,
    RoomAccessUpdateResponse, RoutePolicy, VIEWER_QUALITY_EVIDENCE_INTERVAL_MS,
    decode_server_message,
};
use serde_json::{Value, json};

use super::fake_media::{Barrier, FakeMedia};
use super::harness::*;
use super::router_harness::{RouterHarness, RouterOptions, base_options};
use super::wire::*;
use crate::router::SfuFallback;
use crate::server::Server;
use piik_sfu::runtime::{Admission, AdmissionOptions};

#[path = "server/server_grace.rs"]
mod server_grace;
#[path = "server/server_grants.rs"]
mod server_grants;
#[path = "server/server_lifecycle.rs"]
mod server_lifecycle;
#[path = "server/server_pause.rs"]
mod server_pause;
#[path = "server/server_presence.rs"]
mod server_presence;
#[path = "server/server_quality.rs"]
mod server_quality;
#[path = "server/server_roomsession.rs"]
mod server_roomsession;
#[path = "server/server_routing.rs"]
mod server_routing;
#[path = "server/server_sharing.rs"]
mod server_sharing;

/// Covers scrypt derivation in unoptimized test builds.
pub(crate) const PASSWORD_AUTHENTICATION_TIMEOUT_MS: u64 = 10_000;
pub(crate) const PASSWORD_WAIT: Duration = Duration::from_secs(10);

pub(crate) fn presence() -> Presence {
    Presence::default()
}

pub(crate) fn fake_sfu() -> SfuFallback {
    SfuFallback {
        media: FakeMedia::new(),
        admission: Admission::new(AdmissionOptions {
            ingress_capacity: 2,
            egress_capacity: 20,
        }),
        prepare_timeout_ms: 0,
    }
}

fn panicking_store() -> piik_room::Store {
    piik_room::Store::new(piik_room::Options {
        max_rooms: 1,
        max_viewers_per_room: 1,
        random: Some(Arc::new(|_| panic!("test random source"))),
        ..Default::default()
    })
    .expect("store")
}

pub(crate) fn tempdir() -> std::path::PathBuf {
    let path =
        std::env::temp_dir().join(format!("piik-signal-test-{}", crate::runtime::opaque_id()));
    std::fs::create_dir_all(&path).expect("temp dir");
    path
}

fn chat(client: &TestClient, request_id: &str, text: &str) {
    client.send_json(json!({
        "type": "send-room-interaction",
        "requestId": request_id,
        "payload": {"kind": "chat", "text": text},
    }));
}

fn wall_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| i64::try_from(elapsed.as_millis()).unwrap_or(0))
}

pub(crate) fn send_password_authenticate(
    client: &TestClient,
    room: &piik_room::CreatedRoom,
    client_id: &str,
    password: &str,
) {
    client.send_json(json!({
        "type": "authenticate",
        "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": room.room_id,
        "role": "viewer",
        "clientId": client_id,
        "viewerPassword": password,
    }));
}

pub(crate) fn with_name(name: &str) -> Presence {
    Presence {
        display_name: name.into(),
        ..Default::default()
    }
}

fn diagnostic_quality(raw: &str) -> Value {
    let value: Value = serde_json::from_str(raw).expect("snapshot JSON");
    value
        .pointer("/snapshot/children")
        .and_then(Value::as_array)
        .and_then(|children| {
            children
                .iter()
                .find_map(|child| child.get("quality").filter(|q| q.is_object()).cloned())
        })
        .unwrap_or(Value::Null)
}

pub(crate) fn viewer_grant_of(invite_url: &str) -> String {
    let parsed = url::Url::parse(invite_url).expect("invite URL");
    let fragment = parsed.fragment().unwrap_or_default();
    url::form_urlencoded::parse(fragment.as_bytes())
        .find(|(key, _)| key == "v")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_default()
}

pub(crate) fn rotated_invite(response: &RoomAccessUpdateResponse) -> Option<String> {
    match response {
        RoomAccessUpdateResponse::ViewerGrantUpdated { invite_url, .. } => invite_url.clone(),
        _ => None,
    }
}

pub(crate) fn send_code_only_authenticate(
    client: &TestClient,
    room_id: &str,
    client_id: &str,
    extra: Value,
) {
    let mut message = json!({
        "type": "authenticate",
        "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": room_id,
        "role": "viewer",
        "clientId": client_id,
    });
    if let (Some(target), Some(extra)) = (message.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            target.insert(key.clone(), value.clone());
        }
    }
    client.send_json(message);
}

fn code_only() -> Presence {
    Presence {
        code_only: true,
        ..Default::default()
    }
}

const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

pub(crate) fn set_sharing_paused(client: &TestClient, share_generation: &str, paused: bool) {
    client.send_json(json!({"type": "set-sharing-paused", "shareGeneration": share_generation, "paused": paused}));
}

fn paused(value: bool) -> Presence {
    Presence {
        sharing_paused: Some(value),
        ..Default::default()
    }
}

fn quality(settings: Value) -> Presence {
    Presence {
        quality_settings: Some(settings),
        ..Default::default()
    }
}

fn grace(ms: u64) -> HarnessOptions {
    HarnessOptions {
        viewer_disconnect_grace_ms: ms,
        ..Default::default()
    }
}
