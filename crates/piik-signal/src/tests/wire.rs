//! Protocol helpers over [`TestClient`]: authenticate, route updates,
//! presence and evidence messages.

use std::time::{Duration, Instant};

use piik_protocol::{RoutePolicy, SIGNALING_PROTOCOL, ViewerRoutePreference};
use piik_room::CreatedRoom;
use serde_json::{Map, Value, json};

use super::harness::TestClient;

/// The parsed `authenticated` message.
#[derive(Debug, Clone)]
pub(crate) struct Authenticated {
    pub raw: String,
    pub value: Value,
}

impl Authenticated {
    fn field(&self, key: &str) -> &Value {
        self.value.get(key).unwrap_or(&Value::Null)
    }
    pub(crate) fn peer_id(&self) -> String {
        self.field("peerId").as_str().unwrap_or_default().to_owned()
    }
    pub(crate) fn host_online(&self) -> bool {
        self.field("hostOnline").as_bool().unwrap_or(false)
    }
    pub(crate) fn host_paused(&self) -> Option<bool> {
        self.field("hostPaused").as_bool()
    }
    pub(crate) fn share_generation(&self) -> Option<String> {
        self.field("shareGeneration").as_str().map(str::to_owned)
    }
    pub(crate) fn route_revision(&self) -> i64 {
        self.field("routeRevision").as_i64().unwrap_or(0)
    }
    pub(crate) fn upstream_kind(&self) -> String {
        self.value
            .pointer("/routeAssignment/upstream/kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn route_policy(&self) -> RoutePolicy {
        let policy = self.field("routePolicy");
        let flag = |key: &str| policy.get(key).and_then(Value::as_bool).unwrap_or(false);
        RoutePolicy {
            peer_only: flag("peerOnly"),
            topology_optimization: flag("topologyOptimization"),
            nat_prediction: flag("natPrediction"),
        }
    }
    pub(crate) fn role(&self) -> String {
        self.field("role").as_str().unwrap_or_default().to_owned()
    }
    pub(crate) fn viewer_authorization_generation(&self) -> String {
        self.field("viewerAuthorizationGeneration")
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn connection_id(&self) -> Option<String> {
        self.field("connectionId").as_str().map(str::to_owned)
    }
    pub(crate) fn ice_config(&self) -> String {
        self.field("iceConfig").to_string()
    }
    pub(crate) fn quality_settings(&self) -> Value {
        self.field("qualitySettings").clone()
    }
}

pub(crate) fn policy_json(policy: RoutePolicy) -> Value {
    json!({
        "peerOnly": policy.peer_only,
        "topologyOptimization": policy.topology_optimization,
        "natPrediction": policy.nat_prediction,
    })
}

#[derive(Default, Clone)]
pub(crate) struct Presence {
    pub route_preference: Option<ViewerRoutePreference>,
    pub display_name: String,
    pub viewer_password: String,
    pub code_only: bool,
    pub sharing_paused: Option<bool>,
    pub quality_settings: Option<Value>,
    pub route_policy: Option<RoutePolicy>,
}

/// The authenticate message `authenticate` sends.
pub(crate) fn authenticate_message(
    room: &CreatedRoom,
    role: &str,
    client_id: &str,
    share_generation: &str,
    presence: &Presence,
) -> Value {
    let mut message = Map::new();
    message.insert("type".into(), json!("authenticate"));
    message.insert("protocol".into(), json!(SIGNALING_PROTOCOL));
    message.insert("roomId".into(), json!(room.room_id));
    message.insert("role".into(), json!(role));
    message.insert("clientId".into(), json!(client_id));
    if role == "host" {
        message.insert("token".into(), json!(room.host_token));
        if !share_generation.is_empty() {
            message.insert("shareGeneration".into(), json!(share_generation));
        }
        if let Some(paused) = presence.sharing_paused {
            message.insert("sharingPaused".into(), json!(paused));
        }
        if let Some(settings) = &presence.quality_settings {
            message.insert("qualitySettings".into(), settings.clone());
        }
        if let Some(policy) = presence.route_policy {
            message.insert("routePolicy".into(), policy_json(policy));
        }
        if !presence.display_name.is_empty() {
            message.insert("displayName".into(), json!(presence.display_name));
        }
    } else {
        if !presence.viewer_password.is_empty() {
            message.insert("viewerPassword".into(), json!(presence.viewer_password));
        } else if !presence.code_only && !room.viewer_grant.is_empty() {
            message.insert("viewerGrant".into(), json!(room.viewer_grant));
        }
        if !presence.display_name.is_empty() {
            message.insert("displayName".into(), json!(presence.display_name));
        }
        if let Some(preference) = presence.route_preference {
            message.insert("routePreference".into(), json!(preference.as_str()));
        }
    }
    Value::Object(message)
}

/// `authenticate()`; a negative `relay_capacity` sends no relay-capacity.
#[track_caller]
pub(crate) fn authenticate(
    client: &TestClient,
    room: &CreatedRoom,
    role: &str,
    client_id: &str,
    relay_capacity: i64,
    share_generation: &str,
    presence: Presence,
) -> Authenticated {
    client.send_json(authenticate_message(
        room,
        role,
        client_id,
        share_generation,
        &presence,
    ));
    let message = client.next("authenticated");
    // Room data starts with the authenticated session.
    client.next("room-interactions-ready");
    let authenticated = Authenticated {
        value: message.json(),
        raw: message.raw,
    };
    if role == "viewer" && relay_capacity >= 0 {
        client.send_json(json!({"type": "relay-capacity", "downstreamEdges": relay_capacity}));
    }
    authenticated
}

/// A parsed `route-update`.
#[derive(Debug, Clone)]
pub(crate) struct RouteUpdate {
    pub raw: String,
    pub value: Value,
}

impl RouteUpdate {
    pub(crate) fn revision(&self) -> i64 {
        self.value["revision"].as_i64().unwrap_or(0)
    }
    pub(crate) fn phase(&self) -> &str {
        self.value["phase"].as_str().unwrap_or_default()
    }
    pub(crate) fn connection_id(&self) -> String {
        self.value
            .pointer("/candidate/connectionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn transport(&self) -> String {
        self.value
            .pointer("/candidate/transport")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn candidate(&self) -> Value {
        self.value["candidate"].clone()
    }
    pub(crate) fn upstream(&self) -> Value {
        self.value
            .pointer("/assignment/upstream")
            .cloned()
            .unwrap_or(Value::Null)
    }
    pub(crate) fn upstream_kind(&self) -> String {
        self.upstream()["kind"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn upstream_peer(&self) -> String {
        self.upstream()["peerId"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
}

fn route_update(client: &TestClient) -> RouteUpdate {
    let message = client.next("route-update");
    RouteUpdate {
        value: message.json(),
        raw: message.raw,
    }
}

#[track_caller]
pub(crate) fn next_prepared_route(client: &TestClient) -> RouteUpdate {
    loop {
        let update = route_update(client);
        if update.phase() == "prepare" {
            return update;
        }
    }
}

#[track_caller]
pub(crate) fn next_active_route_revision(client: &TestClient, revision: i64) -> RouteUpdate {
    loop {
        let update = route_update(client);
        if update.phase() == "active" && update.revision() == revision {
            return update;
        }
    }
}

#[track_caller]
pub(crate) fn next_active_route_after(client: &TestClient, revision: i64) -> RouteUpdate {
    loop {
        let update = route_update(client);
        if update.phase() == "active" && update.revision() > revision {
            return update;
        }
    }
}

pub(crate) fn send_ready(client: &TestClient, prepared: &RouteUpdate) {
    client.send_json(json!({
        "type": "route-transport-connected",
        "revision": prepared.revision(),
        "connectionId": prepared.connection_id(),
    }));
    client.send_json(
        json!({"type": "route-ready", "revision": prepared.revision(), "phase": "prepare"}),
    );
}

#[track_caller]
pub(crate) fn commit_prepared_route(client: &TestClient) -> RouteUpdate {
    let prepared = next_prepared_route(client);
    send_ready(client, &prepared);
    next_active_route_revision(client, prepared.revision())
}

/// One `viewer-presence` entry.
#[derive(Debug, Clone)]
pub(crate) struct PresenceEntry {
    pub value: Value,
}

impl PresenceEntry {
    pub(crate) fn role(&self) -> &str {
        self.value["role"].as_str().unwrap_or_default()
    }
    pub(crate) fn peer_id(&self) -> String {
        self.value["peerId"].as_str().unwrap_or_default().to_owned()
    }
    pub(crate) fn display_name(&self) -> String {
        self.value["displayName"]
            .as_str()
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn upstream_kind(&self) -> &str {
        self.value
            .pointer("/upstream/kind")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
    pub(crate) fn upstream_peer(&self) -> String {
        self.value
            .pointer("/upstream/peerId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    }
    pub(crate) fn media_ready(&self) -> bool {
        self.value["mediaReady"].as_bool().unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PresenceMessage {
    pub raw: String,
    pub entries: Vec<PresenceEntry>,
}

impl PresenceMessage {
    pub(crate) fn viewers(&self) -> Vec<PresenceEntry> {
        self.entries
            .iter()
            .filter(|entry| entry.role() == "viewer")
            .cloned()
            .collect()
    }
    pub(crate) fn host(&self) -> Option<PresenceEntry> {
        self.entries
            .iter()
            .find(|entry| entry.role() == "host")
            .cloned()
    }
    pub(crate) fn viewers_json(&self) -> String {
        Value::Array(
            self.viewers()
                .into_iter()
                .map(|entry| entry.value)
                .collect(),
        )
        .to_string()
    }
}

pub(crate) fn parse_presence(raw: &str) -> PresenceMessage {
    let value: Value = serde_json::from_str(raw).expect("presence JSON");
    let entries = value["viewers"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|value| PresenceEntry {
                    value: value.clone(),
                })
                .collect()
        })
        .unwrap_or_default();
    PresenceMessage {
        raw: raw.to_owned(),
        entries,
    }
}

pub(crate) fn display_names(entries: &[PresenceEntry]) -> Vec<String> {
    entries.iter().map(PresenceEntry::display_name).collect()
}

pub(crate) fn peer_ids(entries: &[PresenceEntry]) -> Vec<String> {
    entries.iter().map(PresenceEntry::peer_id).collect()
}

#[track_caller]
pub(crate) fn next_viewer_presence_matching(
    client: &TestClient,
    predicate: impl Fn(&PresenceMessage) -> bool,
) -> PresenceMessage {
    for _ in 0..20 {
        let message = parse_presence(&client.next("viewer-presence").raw);
        if predicate(&message) {
            return message;
        }
    }
    panic!("Viewer presence did not reach the expected state");
}

#[track_caller]
pub(crate) fn expect_no_viewer_presence_matching(
    client: &TestClient,
    predicate: impl Fn(&PresenceMessage) -> bool,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
        let Some(message) = client.try_next("viewer-presence", remaining) else {
            return;
        };
        assert!(
            !predicate(&parse_presence(&message.raw)),
            "Viewer presence reached a forbidden state: {}",
            message.raw
        );
    }
}

pub(crate) fn viewer_quality_evidence(
    connection_id: &str,
    route_revision: i64,
    sequence: i64,
    presentation_epoch: i64,
) -> Value {
    json!({
        "type": "viewer-quality-evidence",
        "guard": {
            "connectionId": connection_id,
            "routeRevision": route_revision,
            "presentationEpoch": presentation_epoch,
        },
        "sequence": sequence,
        "windowMs": 2_000,
        "metrics": {
            "natTraversalPath": "ordinary",
            "width": 1_920,
            "height": 1_080,
            "framesPerSecond": 60,
            "bitrateKbps": 7_500,
            "packetsReceivedDelta": 1_500,
            "packetsLostDelta": 2,
            "rttMs": 18,
            "jitterMs": 3.5,
            "framesDecodedDelta": 120,
            "framesDroppedDelta": 1,
            "decodeMsPerFrame": 2.4,
            "freezeCountDelta": 0,
            "freezeDurationMsDelta": 0,
            "pauseCountDelta": 0,
            "pauseDurationMsDelta": 0,
            "codec": "video/VP8",
            "codecProfile": null,
            "codecParameters": null,
            "audioBitrateKbps": 192,
            "audioPacketLossPercent": 0.2,
            "audioJitterMs": 2.5,
            "audioVideoPlayoutDeltaMs": -12.5,
            "videoJitterBufferDelayMs": 24,
            "audioJitterBufferDelayMs": 18,
            "audioConcealedSamplesPercent": 1,
            "audioConcealmentEventsDelta": 3,
            "audioCodec": "audio/opus",
        },
    })
}

pub(crate) fn signal_description(
    target_peer_id: &str,
    connection_id: &str,
    description_type: &str,
) -> Value {
    json!({
        "type": "signal",
        "targetPeerId": target_peer_id,
        "payload": {
            "kind": "description",
            "connectionId": connection_id,
            "description": {"type": description_type, "sdp": "v=0\r\n"},
        },
    })
}

pub(crate) fn signal_candidate(target_peer_id: &str, connection_id: &str) -> Value {
    json!({
        "type": "signal",
        "targetPeerId": target_peer_id,
        "payload": {"kind": "candidate", "connectionId": connection_id, "candidate": null},
    })
}

/// A settable test clock.
pub(crate) struct TestClock(std::sync::atomic::AtomicI64);

impl TestClock {
    pub(crate) fn new(start: i64) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self(std::sync::atomic::AtomicI64::new(start)))
    }
    pub(crate) fn now(&self) -> i64 {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub(crate) fn advance(&self, delta_ms: i64) {
        self.0
            .fetch_add(delta_ms, std::sync::atomic::Ordering::SeqCst);
    }
    pub(crate) fn clock(self: &std::sync::Arc<Self>) -> crate::runtime::Clock {
        let clock = self.clone();
        std::sync::Arc::new(move || clock.now())
    }
}

pub(crate) fn utc_millis(year: i32, month: u32, day: u32, hour: u32) -> i64 {
    // Days from civil (Howard Hinnant), proleptic Gregorian, UTC.
    let (year, month) = if month <= 2 {
        (year - 1, month + 9)
    } else {
        (year, month - 3)
    };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100
        + i32::try_from(day_of_year).expect("day");
    let days = i64::from(era) * 146_097 + i64::from(day_of_era) - 719_468;
    (days * 24 + i64::from(hour)) * 3_600_000
}
