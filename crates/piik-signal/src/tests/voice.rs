//! Room voice and voice policy tests, plus the helpers `voice_owner.rs`
//! shares.

use std::sync::Arc;
use std::time::Duration;

use piik_protocol::{
    DEFAULT_ROUTE_POLICY, RoomVoiceState, RoutePolicy, RouteStatus, ServerMessage,
    ViewerRoutePreference, decode_server_message,
};
use serde_json::{Value, json};

use super::fake_media::FakeMedia;
use super::harness::*;
use super::wire::*;
use crate::router::SfuFallback;
use piik_sfu::runtime::{Admission, AdmissionOptions, PeerConnectionState, VoiceMediaEvent};

const SHORT: Duration = Duration::from_millis(30);

pub(super) fn voice_command(client: &TestClient, command: Value) {
    client.send_json(json!({"type": "room-voice-command", "command": command}));
}

pub(super) fn decode_voice_state(raw: &str) -> RoomVoiceState {
    match decode_server_message(raw.as_bytes()).expect("room-voice-state") {
        ServerMessage::RoomVoiceState(state) => *state,
        other => panic!("unexpected {other:?}"),
    }
}

#[track_caller]
pub(super) fn next_voice_state(
    client: &TestClient,
    matches: impl Fn(&RoomVoiceState) -> bool,
) -> RoomVoiceState {
    loop {
        let state = decode_voice_state(&client.next("room-voice-state").raw);
        if matches(&state) {
            return state;
        }
    }
}

pub(super) fn any_state(_: &RoomVoiceState) -> bool {
    true
}

fn member_deafened(state: &RoomVoiceState, peer_id: &str) -> Option<Option<bool>> {
    state
        .members
        .iter()
        .find(|member| member.peer_id == peer_id)
        .map(|member| member.deafened)
}

pub(super) fn single_connection(state: &RoomVoiceState, pending: bool) -> bool {
    state.connections.len() == 1 && state.connections[0].pending == pending
}

pub(super) fn report_connected(clients: &[&TestClient], connection_id: &str) {
    for client in clients {
        client.send_json(json!({"type": "room-voice-transport", "connectionId": connection_id, "state": "connected"}));
    }
}

pub(super) fn start_sharing(host: &TestClient, generation: &str, policy: RoutePolicy) {
    host.send_json(
        json!({"type": "start-sharing", "shareGeneration": generation,
        "routePolicy": policy_json(policy), "sharingPaused": false}),
    );
}

pub(super) fn route_ready(viewer: &TestClient, revision: i64) {
    viewer.send_json(json!({"type": "route-ready", "phase": "prepare", "revision": revision}));
}

pub(super) fn sfu_with(
    media: Arc<dyn piik_sfu::runtime::Runtime>,
    ingress: usize,
    egress: usize,
) -> SfuFallback {
    SfuFallback {
        media,
        admission: Admission::new(AdmissionOptions {
            ingress_capacity: ingress,
            egress_capacity: egress,
        }),
        prepare_timeout_ms: 0,
    }
}

/// `joinedVoiceOwnerRoom`: a Host and a Viewer joined to open room voice.
pub(super) fn joined_voice_owner_room() -> (
    Harness,
    TestClient,
    TestClient,
    Authenticated,
    Authenticated,
) {
    let h = Harness::start(HarnessOptions {
        viewer_disconnect_grace_ms: 30_000,
        ..Default::default()
    });
    let (host, viewer) = (h.open_client(), h.open_client());
    let host_id = authenticate(
        &host,
        &h.room,
        "host",
        "owner_host_123456",
        -1,
        "",
        Presence::default(),
    );
    let viewer_id = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "owner_viewer_123456",
        1,
        "",
        Presence::default(),
    );
    voice_command(&host, json!({"action": "join"}));
    next_voice_state(&host, |state| state.enabled);
    voice_command(&viewer, json!({"action": "join"}));
    let pending = next_voice_state(&viewer, |state| single_connection(state, true));
    report_connected(&[&host, &viewer], &pending.connections[0].connection_id);
    next_voice_state(&viewer, |state| single_connection(state, false));
    (h, host, viewer, host_id, viewer_id)
}

// ---------------------------------------------------------------------------
// Room voice
// ---------------------------------------------------------------------------

#[test]
fn voice_listening_state_reaches_every_participant_without_changing_media() {
    let (h, host, viewer, host_id, viewer_id) = joined_voice_owner_room();
    voice_command(&host, json!({"action": "publish"}));
    let before = next_voice_state(&host, |state| state.publications.len() == 1);
    voice_command(&host, json!({"action": "deafen", "deafened": true}));
    let after = next_voice_state(&host, |state| {
        member_deafened(state, &host_id.peer_id()) == Some(Some(true))
    });
    assert!(
        before.publications == after.publications && before.connections == after.connections,
        "deafen changed media publication or connections"
    );
    next_voice_state(&viewer, |state| {
        state.revision >= after.revision
            && member_deafened(state, &host_id.peer_id()) == Some(Some(true))
    });
    voice_command(&viewer, json!({"action": "deafen", "deafened": true}));
    next_voice_state(&host, |state| {
        state.revision > after.revision
            && member_deafened(state, &viewer_id.peer_id()) == Some(Some(true))
    });
    h.close_client(&host);
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "owner_host_123456",
        -1,
        "",
        Presence::default(),
    );
    // Reconnecting rebinds the Host member without restoring its publication.
    next_voice_state(&host, |state| {
        state
            .members
            .iter()
            .any(|member| member.peer_id == host_id.peer_id() && member.publication_id.is_none())
    });
}

#[test]
fn voice_only_sfu_prepares_without_video_publication() {
    let h = Harness::start(HarnessOptions {
        sfu_only: true,
        endpoint_media_copy_capacity: 1,
        sfu: Some(sfu_with(FakeMedia::with_voice(), 1, 2)),
        ..Default::default()
    });
    let (host, viewer) = (h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "voice_sfu_host_123",
        -1,
        "",
        Presence::default(),
    );
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "voice_sfu_viewer_123",
        1,
        "",
        Presence::default(),
    );
    voice_command(&host, json!({"action": "join"}));
    next_voice_state(&host, |state| state.enabled);
    voice_command(&viewer, json!({"action": "join"}));
    let state = next_voice_state(&viewer, |state| !state.connections.is_empty());
    assert!(
        state.connections.len() == 1
            && state.connections[0].transport == "sfu"
            && state.connections[0].pending,
        "invalid SFU preparation: {:?}",
        state.connections
    );
    for client in [&host, &viewer] {
        let own = next_voice_state(client, |state| !state.connections.is_empty());
        report_connected(&[client], &own.connections[0].connection_id);
    }
    let room = h.room.room_id.clone();
    let mut events: Vec<VoiceMediaEvent> = Vec::new();
    assert!(
        wait_for(DEFAULT_WAIT, || {
            h.locked(|core| {
                events.clear();
                let mut ready = true;
                for edge in core.voices[&room].connections.values() {
                    ready = ready && edge.connected(&edge.child);
                    events.push(VoiceMediaEvent {
                        room_id: room.clone(),
                        peer_id: edge.child.clone(),
                        session_id: edge.child_session.clone(),
                        connection_id: edge.id.clone(),
                        state: PeerConnectionState::Connected,
                        ..Default::default()
                    });
                }
                ready && events.len() == 2
            })
        }),
        "clients did not report both voice transports"
    );
    // The transport may leave Connected while its callback waits for the lock.
    // One stale facet must not combine with the other facet to commit the route.
    let deliver = |mut event: VoiceMediaEvent, current: bool| {
        event.is_current_state = Some(Arc::new(move || current));
        h.locked(|core| core.on_voice_media_event(event));
    };
    deliver(events[0].clone(), false);
    deliver(events[1].clone(), true);
    assert!(
        h.locked(|core| core.controller(&room).and_then(|c| c.operation()).is_some()),
        "stale Connected callback committed a voice route"
    );
    deliver(events[0].clone(), true);
    assert!(
        h.locked(|core| core.controller(&room).and_then(|c| c.operation()).is_none()),
        "current Connected callback did not commit voice route"
    );
}

#[test]
fn room_voice_bitrate_is_host_owned_for_every_participant() {
    let h = Harness::start(HarnessOptions::default());
    let (host, viewer, second) = (h.open_client(), h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "bitrate_host_123456",
        -1,
        "",
        Presence::default(),
    );
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "bitrate_viewer_123456",
        1,
        "",
        Presence::default(),
    );
    authenticate(
        &second,
        &h.room,
        "viewer",
        "bitrate_second_123456",
        1,
        "",
        Presence::default(),
    );
    // Opening the channel broadcasts the current bitrate to every participant.
    voice_command(&host, json!({"action": "join"}));
    for client in [&host, &viewer, &second] {
        next_voice_state(client, |state| {
            state.bitrate == Some(64_000) && state.enabled
        });
    }
    voice_command(&host, json!({"action": "bitrate", "bitrate": 96_000}));
    let state = next_voice_state(&viewer, |state| state.bitrate == Some(96_000));
    assert!(
        state.enabled && state.publications.is_empty() && state.connections.is_empty(),
        "quality setting changed media"
    );
    next_voice_state(&second, |next| next.bitrate == Some(96_000));
    voice_command(&viewer, json!({"action": "bitrate", "bitrate": 64_000}));
    viewer.next("room-voice-error");
    let room = h.room.room_id.clone();
    assert_eq!(
        h.locked(|core| core.voices[&room].bitrate),
        96_000,
        "Viewer changed room bitrate"
    );
}

#[test]
fn voice_only_route_failure_does_not_change_video_status() {
    let (h, _host, viewer, _, viewer_id) = joined_voice_owner_room();
    let room = h.room.room_id.clone();
    h.locked(|core| {
        // The wire type cannot pair a generic waiting status with
        // route-exhausted; the admission wait is the only waiting reason.
        for status in [
            RouteStatus::WaitingSfuAdmission,
            RouteStatus::FailedRouteExhausted,
        ] {
            core.send_viewer_route_status(&room, &viewer_id.peer_id(), 1, status);
        }
    });
    viewer.next("room-voice-error");
    assert!(
        viewer.try_next("route-status", SHORT).is_none(),
        "voice-only routing changed video status"
    );
}

#[test]
fn room_candidate_waits_for_both_picture_and_voice() {
    for picture_first in [true, false] {
        let h = Harness::start(HarnessOptions::default());
        let (host, viewer) = (h.open_client(), h.open_client());
        authenticate(
            &host,
            &h.room,
            "host",
            "composite_host_123456",
            -1,
            "",
            Presence::default(),
        );
        voice_command(&host, json!({"action": "join"}));
        start_sharing(&host, "composite_video_123456", DEFAULT_ROUTE_POLICY);
        host.next("sharing-started");
        authenticate(
            &viewer,
            &h.room,
            "viewer",
            "composite_viewer_123456",
            1,
            "",
            Presence::default(),
        );
        voice_command(&viewer, json!({"action": "join"}));
        let pending = next_voice_state(&viewer, |state| single_connection(state, true));
        let voice = pending.connections[0].clone();
        while next_prepared_route(&viewer).revision() != voice.revision {}
        let picture = || route_ready(&viewer, voice.revision);
        let audio = || report_connected(&[&host, &viewer], &voice.connection_id);
        let mut barriers = vec![&viewer];
        if picture_first {
            picture();
        } else {
            audio();
            barriers.push(&host);
        }
        for client in barriers {
            client.send_json(json!({"type": "signaling-challenge", "sequence": 1}));
            client.next("signaling-challenge-response");
        }
        let room = h.room.room_id.clone();
        h.locked(|core| {
            let operation = core.controller(&room).and_then(|c| c.operation());
            assert!(
                operation
                    .and_then(|op| op.current)
                    .is_some_and(|current| current.revision == voice.revision),
                "only one media facet committed the room route"
            );
        });
        if picture_first {
            audio();
        } else {
            picture();
        }
        next_active_route_revision(&viewer, voice.revision);
        next_voice_state(&viewer, |state| single_connection(state, false));
    }
}

#[test]
fn room_voice_uses_normal_room_authority_and_keeps_graph_across_video() {
    let h = Harness::start(HarnessOptions::default());
    let (host, viewer, old) = (h.open_client(), h.open_client(), h.open_client());
    let host_id = authenticate(
        &host,
        &h.room,
        "host",
        "voice_host_123456",
        -1,
        "",
        Presence::default(),
    );
    let viewer_id = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "voice_viewer_123456",
        1,
        "",
        Presence::default(),
    );
    authenticate(
        &old,
        &h.room,
        "viewer",
        "voice_old_page_123456",
        1,
        "",
        Presence::default(),
    );
    voice_command(&viewer, json!({"action": "join"}));
    viewer.next("room-voice-error");
    voice_command(&viewer, json!({"action": "publish"}));
    viewer.next("room-voice-error");
    voice_command(&host, json!({"action": "join"}));
    next_voice_state(&host, |state| state.enabled);
    voice_command(&viewer, json!({"action": "join"}));
    let pending = next_voice_state(&viewer, |state| single_connection(state, true));
    let voice_id = pending.connections[0].connection_id.clone();
    report_connected(&[&host, &viewer], &voice_id);
    let active = next_voice_state(&viewer, |state| single_connection(state, false));
    assert_eq!(
        active.connections[0].connection_id, voice_id,
        "voice commit replaced the physical transport"
    );
    voice_command(&host, json!({"action": "publish"}));
    let publication = next_voice_state(&viewer, |state| {
        state.publications.len() == 1 && state.connections[0].receive.len() == 1
    });
    let pub_id = publication.publications[0].id.clone();
    assert_eq!(
        publication.publications[0].peer_id,
        host_id.peer_id(),
        "publication sender was not authenticated"
    );
    voice_command(&viewer, json!({"action": "publish"}));
    next_voice_state(&host, |state| {
        state.publications.len() == 2 && state.connections[0].receive.len() == 1
    });
    let viewer_pub = next_voice_state(&viewer, |state| state.publications.len() == 2);
    for publication in &viewer_pub.publications {
        if publication.peer_id == viewer_id.peer_id() {
            voice_command(
                &viewer,
                json!({"action": "unpublish", "publicationId": publication.id}),
            );
        }
    }
    next_voice_state(&viewer, |state| {
        state.publications.len() == 1 && state.publications[0].id == pub_id
    });
    // Every authenticated participant receives voice state, whether or not
    // it joined a transport.
    next_voice_state(&old, |state| state.publications.len() == 1);

    let room = h.room.room_id.clone();
    let edge_of = |peer: &str| {
        h.locked(|core| {
            core.snapshot(&room)
                .and_then(|s| s.upstream_by_viewer.get(&peer.to_owned()).cloned())
        })
        .expect("viewer edge")
    };
    let logical = h.locked(|core| {
        let snapshot = core.snapshot(&room).expect("snapshot");
        snapshot
            .upstream_by_viewer
            .iter()
            .next()
            .expect("edge")
            .1
            .connection_id
            .clone()
    });
    start_sharing(&host, "voice_video_generation_1", DEFAULT_ROUTE_POLICY);
    host.next("sharing-started");
    let prepare = next_prepared_route(&viewer);
    assert_ne!(
        prepare.connection_id(),
        logical,
        "video attachment reused room transport identity"
    );
    route_ready(&viewer, prepare.revision());
    next_active_route_revision(&viewer, prepare.revision());
    let edge = edge_of(&viewer_id.peer_id());
    assert!(
        edge.video_ready
            && edge.connection_id == logical
            && edge.video_connection_id == prepare.connection_id(),
        "video did not attach to retained room edge"
    );
    host.send_json(json!({"type": "stop-sharing", "shareGeneration": "voice_video_generation_1"}));
    viewer.next("sharing-stopped");
    let state = next_voice_state(&viewer, |state| {
        state.publications.len() == 1 && single_connection(state, false)
    });
    assert!(
        state.connections[0].connection_id == voice_id && state.publications[0].id == pub_id,
        "video stop retired room voice"
    );
    let edge = edge_of(&viewer_id.peer_id());
    assert!(
        !edge.video_ready && edge.connection_id == logical && edge.usable,
        "video stop corrupted room graph"
    );
    // A late stop for a former source cannot retire a replacement microphone.
    voice_command(
        &host,
        json!({"action": "unpublish", "publicationId": pub_id}),
    );
    next_voice_state(&viewer, |state| state.publications.is_empty());
    voice_command(&host, json!({"action": "publish"}));
    let new_state = next_voice_state(&viewer, |state| state.publications.len() == 1);
    voice_command(
        &host,
        json!({"action": "unpublish", "publicationId": pub_id}),
    );
    let last = next_voice_state(&viewer, |state| {
        state.publications.len() == 1 && state.publications[0].id == new_state.publications[0].id
    });
    assert_ne!(
        last.publications[0].id, pub_id,
        "publication identity was reused"
    );
    voice_command(&viewer, json!({"action": "leave"}));
    next_voice_state(&viewer, |state| {
        state
            .members
            .iter()
            .any(|member| member.peer_id == viewer_id.peer_id() && !member.joined)
    });
    viewer.send_json(json!({"type": "signaling-challenge", "sequence": 42}));
    viewer.next("signaling-challenge-response");
}

// ---------------------------------------------------------------------------
// Voice policy
// ---------------------------------------------------------------------------

#[test]
fn room_voice_policy_follows_new_share_on_start_and_host_authentication() {
    for transition in ["start", "authenticate"] {
        let h = Harness::start(HarnessOptions {
            sfu: Some(sfu_with(FakeMedia::new(), 1, 2)),
            ..Default::default()
        });
        let (host, viewer) = (h.open_client(), h.open_client());
        authenticate(
            &host,
            &h.room,
            "host",
            "policy_owner_host_123456",
            -1,
            "",
            Presence::default(),
        );
        let viewer_id = authenticate(
            &viewer,
            &h.room,
            "viewer",
            "policy_owner_viewer_123456",
            0,
            "",
            Presence {
                route_preference: Some(ViewerRoutePreference::Sfu),
                ..Default::default()
            },
        );
        voice_command(&host, json!({"action": "join"}));
        next_voice_state(&host, |state| {
            state.members.iter().any(|member| member.joined)
        });
        let room = h.room.room_id.clone();
        let previous = h.locked(|core| {
            let runtime = core.room_rt(&room).expect("room runtime");
            let controller = runtime.controller.as_ref().expect("controller");
            assert!(
                controller.allows_viewer_route_preference(ViewerRoutePreference::Sfu),
                "{transition}: voice room did not begin with available SFU"
            );
            runtime.controller_id
        });
        let policy = RoutePolicy {
            peer_only: true,
            topology_optimization: false,
            nat_prediction: false,
        };
        let _replacement;
        if transition == "start" {
            start_sharing(&host, "policy_share_123456", policy);
            host.next("sharing-started");
        } else {
            let replacement = h.open_client();
            authenticate(
                &replacement,
                &h.room,
                "host",
                "policy_owner_host_123456",
                -1,
                "policy_share_123456",
                Presence {
                    route_policy: Some(policy),
                    ..Default::default()
                },
            );
            _replacement = replacement;
        }
        h.locked(|core| {
            let runtime = core.room_rt(&room).expect("room runtime");
            let controller = runtime.controller.as_ref().expect("controller");
            assert!(
                runtime.controller_id == previous
                    && !controller.allows_viewer_route_preference(ViewerRoutePreference::Sfu),
                "{transition}: new share failed to update policy on retained room controller"
            );
            let settings = runtime
                .viewer_settings
                .get(&viewer_id.peer_id())
                .expect("viewer settings");
            assert_eq!(
                settings.preference,
                ViewerRoutePreference::Auto,
                "{transition}: new peer-only policy retained incompatible Viewer SFU preference"
            );
        });
        loop {
            let message =
                decode_server_message(viewer.next("viewer-route-preference").raw.as_bytes())
                    .expect("decode");
            if matches!(
                message,
                ServerMessage::ViewerRoutePreference {
                    preference: ViewerRoutePreference::Auto,
                    ..
                }
            ) {
                break;
            }
        }
        assert_eq!(
            next_prepared_route(&viewer).transport(),
            "direct",
            "peer-only publication prepared SFU video"
        );
    }
}
