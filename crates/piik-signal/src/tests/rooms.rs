//! Room presence tests: audio and co-play presence, interactions, remote
//! input, retention and departure.

use std::time::Duration;

use piik_protocol::{
    CodeEntryPolicy, DEFAULT_ROUTE_POLICY, INTERACTION_INTERVAL_MS, InteractionPayload,
    RoomAccessUpdateRequest, SendRoomInteractionMessage, ServerMessage, decode_server_message,
};
use serde_json::json;

use super::harness::*;
use super::server::{presence, with_name};
use super::wire::*;

const SHORT: Duration = Duration::from_millis(30);

pub(crate) fn room_error_code(error: &piik_room::Error) -> Option<piik_room::ErrorCode> {
    error.code()
}

// ---------------------------------------------------------------------------
// audio presence
// ---------------------------------------------------------------------------

#[test]
fn audio_presence_follows_generation_and_reconnect_for_every_viewer() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let generation = "audio_share_generation_12345678";
    authenticate(
        &host,
        &h.room,
        "host",
        "audio_host_client",
        1,
        generation,
        presence(),
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "audio_viewer_client",
        1,
        "",
        presence(),
    );
    let second = h.open_client();
    authenticate(
        &second,
        &h.room,
        "viewer",
        "audio_second_client",
        1,
        "",
        presence(),
    );
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":null}"#);
    expect_match(&second.next("audio-presence").raw, r#"{"available":null}"#);
    let set = |client: &TestClient, share: &str, available: bool| {
        client.send_json(
            json!({"type": "set-audio-presence", "shareGeneration": share, "available": available}),
        );
    };
    set(&host, generation, false);
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":false}"#);
    expect_match(&second.next("audio-presence").raw, r#"{"available":false}"#);
    set(&host, generation, true);
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":true}"#);
    expect_match(&second.next("audio-presence").raw, r#"{"available":true}"#);
    set(&viewer, generation, false);
    set(&host, "replaced_audio_generation", false);
    assert!(
        viewer.try_next("audio-presence", SHORT).is_none(),
        "non-Host or stale generation changed availability"
    );
    h.close_client(&viewer);
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "audio_viewer_client",
        1,
        "",
        presence(),
    );
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":true}"#);
    h.close_client(&host);
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "audio_host_client",
        1,
        generation,
        presence(),
    );
    assert_eq!(
        h.locked(|core| core.share(&h.room.room_id).audio_available),
        Some(true),
        "same-generation Host reconnect lost input availability"
    );
    set(&host, generation, false);
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":false}"#);
    host.send_json(json!({"type": "stop-sharing", "shareGeneration": generation}));
    // stop_sharing clears the room fact before notifying publication retirement.
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":null}"#);
    viewer.next("sharing-stopped");
    host.send_json(json!({"type": "start-sharing", "shareGeneration": "next_audio_generation", "routePolicy": policy_json(DEFAULT_ROUTE_POLICY)}));
    host.next("sharing-started");
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":null}"#);
    set(&host, "next_audio_generation", true);
    expect_match(&viewer.next("audio-presence").raw, r#"{"available":true}"#);
}

// ---------------------------------------------------------------------------
// co-play presence
// ---------------------------------------------------------------------------

fn co_play_participants(raw: &str) -> Vec<String> {
    match decode_server_message(raw.as_bytes()).expect("co-play-presence") {
        ServerMessage::CoPlayPresence { participants } => {
            participants.into_iter().map(|p| p.peer_id).collect()
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn co_play_presence_uses_current_input_authority() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    let generation = "coplay_share_generation_12345678";
    authenticate(
        &host,
        &h.room,
        "host",
        "coplay_host_client",
        1,
        generation,
        presence(),
    );
    let (viewer, observer) = (h.open_client(), h.open_client());
    let actor = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "coplay_viewer_client",
        1,
        "",
        presence(),
    );
    authenticate(
        &observer,
        &h.room,
        "viewer",
        "coplay_observer_client",
        1,
        "",
        presence(),
    );
    let endpoint = "1".repeat(32);
    viewer.send_json(
        json!({"type": "subscribe-remote-input", "endpointId": endpoint, "secret": "2".repeat(64)}),
    );
    // Drop the authenticated and input-registration projections.
    for _ in 0..2 {
        viewer.next("co-play-presence");
        observer.next("co-play-presence");
    }
    let set = |client: &TestClient, share: &str, id: &str| {
        client.send_json(json!({"type": "set-co-play-presence", "shareGeneration": share, "grants": [{"endpointId": id}]}));
    };
    set(&host, generation, &endpoint);
    assert_eq!(
        co_play_participants(&viewer.next("co-play-presence").raw),
        vec![actor.peer_id()],
        "presence did not resolve the authenticated endpoint's room identity"
    );
    assert_eq!(
        co_play_participants(&observer.next("co-play-presence").raw).len(),
        1,
        "another Viewer missed the current participants"
    );
    set(&viewer, generation, &endpoint);
    set(&host, "stale_coplay_generation", &endpoint);
    assert!(
        viewer.try_next("co-play-presence", SHORT).is_none(),
        "non-Host or stale source changed collaboration"
    );
    h.close_client(&viewer);
    expect_match(
        &observer.next("co-play-presence").raw,
        r#"{"participants":[]}"#,
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "coplay_viewer_client",
        1,
        "",
        presence(),
    );
    viewer.send_json(
        json!({"type": "subscribe-remote-input", "endpointId": endpoint, "secret": "5".repeat(64)}),
    );
    for _ in 0..2 {
        expect_match(
            &viewer.next("co-play-presence").raw,
            r#"{"participants":[]}"#,
        );
    }
    h.close_client(&viewer);
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "coplay_viewer_client",
        1,
        "",
        presence(),
    );
    viewer.send_json(json!({"type": "subscribe-remote-input", "endpointId": "3".repeat(32), "secret": "4".repeat(64)}));
    for _ in 0..2 {
        expect_match(
            &viewer.next("co-play-presence").raw,
            r#"{"participants":[]}"#,
        );
    }
    set(&host, generation, &endpoint);
    expect_match(
        &viewer.next("co-play-presence").raw,
        r#"{"participants":[]}"#,
    );
    set(&host, generation, &"3".repeat(32));
    assert_eq!(
        co_play_participants(&viewer.next("co-play-presence").raw).len(),
        1,
        "new endpoint could not be presented"
    );
    host.send_json(json!({"type": "stop-sharing", "shareGeneration": generation}));
    expect_match(
        &viewer.next("co-play-presence").raw,
        r#"{"participants":[]}"#,
    );
    viewer.next("sharing-stopped");
    host.send_json(json!({"type": "start-sharing", "shareGeneration": "next_coplay_generation", "routePolicy": policy_json(DEFAULT_ROUTE_POLICY)}));
    host.next("sharing-started");
    expect_match(
        &viewer.next("co-play-presence").raw,
        r#"{"participants":[]}"#,
    );
}

// ---------------------------------------------------------------------------
// room interactions
// ---------------------------------------------------------------------------

fn interaction(client: &TestClient, request_id: &str, payload: serde_json::Value) {
    client.send_json(
        json!({"type": "send-room-interaction", "requestId": request_id, "payload": payload}),
    );
}

#[test]
fn room_interactions_stay_inside_authorized_membership() {
    let clock = TestClock::new(100_000);
    let h = Harness::start(HarnessOptions {
        max_viewers_per_room: 4,
        now: Some(clock.clock()),
        ..Default::default()
    });
    let (host, viewer, other_in_room) = (h.open_client(), h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "host_interaction",
        -1,
        "",
        with_name("房主"),
    );
    let actor = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer_interaction",
        -1,
        "",
        with_name("小明"),
    );
    authenticate(
        &other_in_room,
        &h.room,
        "viewer",
        "other_room_member",
        -1,
        "",
        with_name("同在"),
    );
    let other_room = h.create_room(CodeEntryPolicy::Open, "");
    let outsider = h.open_client();
    let other = authenticate(
        &outsider,
        &other_room,
        "viewer",
        "outsider_interaction",
        -1,
        "",
        with_name("Elsewhere"),
    );
    interaction(
        &viewer,
        "request_message_1",
        json!({"kind": "chat", "text": "<b>你好 👋</b>"}),
    );
    let host_event = host.next("room-interaction");
    let viewer_event = viewer.next("room-interaction");
    assert_eq!(
        host_event.raw, viewer_event.raw,
        "recipients received different events"
    );
    assert_eq!(
        other_in_room.next("room-interaction").raw,
        host_event.raw,
        "a room member missed the interaction"
    );
    let ServerMessage::RoomInteraction(event) =
        decode_server_message(host_event.raw.as_bytes()).expect("decode")
    else {
        panic!("not a room interaction");
    };
    assert_eq!(
        event.occurred_at,
        clock.now(),
        "event time was not stamped at server acceptance"
    );
    assert!(
        event.sender.peer_id == actor.peer_id() && event.sender.display_name.as_str() == "小明",
        "sender identity was not derived from authentication"
    );
    assert!(
        outsider.try_next("room-interaction", SHORT).is_none(),
        "interaction escaped its room boundary"
    );
    interaction(
        &viewer,
        "request_message_2",
        json!({"kind": "reaction", "reaction": "wave"}),
    );
    expect_match(
        &viewer.next("room-interaction-rejected").raw,
        r#"{"reason":"rate-limited"}"#,
    );
    clock.advance(INTERACTION_INTERVAL_MS);
    interaction(
        &viewer,
        "request_message_3",
        json!({"kind": "reaction", "reaction": "tomato", "targetPeerId": other.peer_id()}),
    );
    expect_match(
        &viewer.next("room-interaction-rejected").raw,
        r#"{"reason":"target-unavailable"}"#,
    );
    // Room data and its rejections never terminate signaling or require media.
    viewer.send_json(json!({"type": "signaling-challenge", "sequence": 123}));
    viewer.next("signaling-challenge-response");
}

#[test]
fn unknown_reaction_commands_are_invalid_messages() {
    let h = Harness::start(HarnessOptions::default());
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "unknown_reaction",
        -1,
        "",
        with_name("Friend"),
    );
    interaction(
        &viewer,
        "request_message_1",
        json!({"kind": "reaction", "reaction": "future-effect"}),
    );
    expect_match(&viewer.next("error").raw, r#"{"code":"INVALID_MESSAGE"}"#);
}

#[test]
fn viewers_can_interact_after_host_disconnects() {
    let clock = TestClock::new(100_000);
    let h = Harness::start(HarnessOptions {
        now: Some(clock.clock()),
        ..Default::default()
    });
    let (host, first, second) = (h.open_client(), h.open_client(), h.open_client());
    let host_actor = authenticate(
        &host,
        &h.room,
        "host",
        "leaving_host",
        -1,
        "",
        Presence {
            display_name: "Host".into(),
            ..Default::default()
        },
    );
    authenticate(
        &first,
        &h.room,
        "viewer",
        "remaining_first",
        -1,
        "",
        with_name("First"),
    );
    let second_actor = authenticate(
        &second,
        &h.room,
        "viewer",
        "remaining_second",
        -1,
        "",
        with_name("Second"),
    );
    h.close_client(&host);
    for payload in [
        json!({"kind": "chat", "text": "still here"}),
        json!({"kind": "reaction", "reaction": "wave"}),
        json!({"kind": "reaction", "reaction": "tomato", "targetPeerId": second_actor.peer_id()}),
    ] {
        interaction(&first, "after_host_left", payload);
        assert_eq!(
            first.next("room-interaction").raw,
            second.next("room-interaction").raw,
            "remaining viewers did not receive the same interaction"
        );
        clock.advance(INTERACTION_INTERVAL_MS);
    }
    interaction(
        &first,
        "departed_target",
        json!({"kind": "reaction", "reaction": "tomato", "targetPeerId": host_actor.peer_id()}),
    );
    expect_match(
        &first.next("room-interaction-rejected").raw,
        r#"{"reason":"target-unavailable"}"#,
    );
}

#[test]
fn room_interaction_backpressure_does_not_retire_media_signaling() {
    let h = Harness::start(HarnessOptions::default());
    let (host, viewer) = (h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "data_host_1234",
        -1,
        "",
        with_name("Host"),
    );
    let actor = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "slow_viewer_1234",
        -1,
        "",
        with_name("Viewer"),
    );
    h.locked(|core| {
        let peer = core
            .store
            .get_connected_viewer(&h.room.room_id, &actor.peer_id())
            .expect("missing viewer");
        let slow = core.session_key(&peer.session_id).expect("slow session");
        // Inject a stalled writer under the same lock as a real broadcast.
        let previous = core.session(slow).expect("session").queued_bytes;
        core.session_mut(slow).expect("session").queued_bytes = 20 * 1024;
        let host_peer = core
            .store
            .get_connected_host(&h.room.room_id)
            .expect("host");
        let host_key = core
            .session_key(&host_peer.session_id)
            .expect("host session");
        let host_auth = core.auth_of(host_key).cloned().expect("host auth");
        core.handle_room_interaction(
            host_key,
            &host_auth,
            SendRoomInteractionMessage {
                request_id: "busy_request_1234".into(),
                payload: InteractionPayload::Chat {
                    text: "hello".into(),
                },
            },
        );
        let session = core.session_mut(slow).expect("session");
        assert!(
            !session.terminated && !session.close_queued,
            "room data closed the signaling session"
        );
        assert_eq!(
            session.queued_bytes,
            20 * 1024,
            "room data entered a stalled signaling queue"
        );
        session.queued_bytes = previous;
    });
    host.next("room-interaction");
    assert!(
        viewer.try_next("room-interaction", SHORT).is_none(),
        "stalled recipient got room data"
    );
    viewer.send_json(json!({"type": "signaling-challenge", "sequence": 456}));
    viewer.next("signaling-challenge-response");
}

// ---------------------------------------------------------------------------
// remote input
// ---------------------------------------------------------------------------

fn remote_input_peers(raw: &str) -> Vec<(String, String)> {
    match decode_server_message(raw.as_bytes()).expect("remote-input-peers") {
        ServerMessage::RemoteInputPeers { peers } => peers
            .into_iter()
            .map(|peer| (peer.peer_id, peer.secret))
            .collect(),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn input_keys_belong_to_authenticated_room_endpoints() {
    let h = Harness::start(HarnessOptions::default());
    let (host, viewer, other) = (h.open_client(), h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "host_input_test",
        -1,
        "",
        presence(),
    );
    let actor = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "viewer_input_test",
        -1,
        "",
        presence(),
    );
    authenticate(
        &other,
        &h.room,
        "viewer",
        "other_input_test",
        -1,
        "",
        presence(),
    );
    let secret = "2".repeat(64);
    let message =
        json!({"type": "subscribe-remote-input", "endpointId": "1".repeat(32), "secret": secret});
    viewer.send_json(message.clone());
    assert!(
        host.try_next("remote-input-peers", SHORT).is_none(),
        "unopted Host received a new event"
    );
    host.send_json(json!({"type": "subscribe-remote-input"}));
    assert_eq!(
        remote_input_peers(&host.next("remote-input-peers").raw),
        vec![(actor.peer_id(), secret.clone())],
        "key is not bound to authenticated Viewer"
    );
    other.send_json(message);
    assert!(
        other.try_next("remote-input-peers", SHORT).is_none(),
        "Viewer received another participant's keys"
    );
    assert!(
        host.try_next("remote-input-peers", SHORT).is_none(),
        "second Viewer replaced an occupied endpoint"
    );
    h.close_client(&viewer);
    assert!(
        remote_input_peers(&host.next("remote-input-peers").raw).is_empty(),
        "departure retained input authority"
    );
}

#[test]
fn invitation_revocation_removes_only_grant_admitted_input_keys() {
    for request in [
        RoomAccessUpdateRequest::RotateViewerGrant,
        RoomAccessUpdateRequest::RevokeViewerGrant,
    ] {
        let h = Harness::start(HarnessOptions::default());
        let (host, invited, code_viewer) = (h.open_client(), h.open_client(), h.open_client());
        authenticate(
            &host,
            &h.room,
            "host",
            "host_input_revoke",
            -1,
            "",
            presence(),
        );
        authenticate(
            &invited,
            &h.room,
            "viewer",
            "invited_input_revoke",
            -1,
            "",
            presence(),
        );
        let remaining = authenticate(
            &code_viewer,
            &h.room,
            "viewer",
            "code_input_revoke",
            -1,
            "",
            Presence {
                code_only: true,
                ..Default::default()
            },
        );
        host.send_json(json!({"type": "subscribe-remote-input"}));
        host.next("remote-input-peers");
        invited.send_json(json!({"type": "subscribe-remote-input", "endpointId": "1".repeat(32), "secret": "2".repeat(64)}));
        host.next("remote-input-peers");
        code_viewer.send_json(json!({"type": "subscribe-remote-input", "endpointId": "3".repeat(32), "secret": "4".repeat(64)}));
        host.next("remote-input-peers");
        h.update_room_access(request.clone());
        invited.next("viewer-grant-revoked");
        assert_eq!(
            remote_input_peers(&host.next("remote-input-peers").raw),
            vec![(remaining.peer_id(), "4".repeat(64))],
            "input keys did not follow current room authority ({request:?})"
        );
    }
}

#[test]
fn replaced_viewer_cannot_retain_input_keys_without_subscribing() {
    let h = Harness::start(HarnessOptions::default());
    let (host, old_viewer) = (h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "host_input_replace",
        -1,
        "",
        presence(),
    );
    authenticate(
        &old_viewer,
        &h.room,
        "viewer",
        "viewer_input_replace",
        -1,
        "",
        presence(),
    );
    host.send_json(json!({"type": "subscribe-remote-input"}));
    host.next("remote-input-peers");
    old_viewer.send_json(json!({"type": "subscribe-remote-input", "endpointId": "1".repeat(32), "secret": "2".repeat(64)}));
    host.next("remote-input-peers");
    let replacement = h.open_client();
    authenticate(
        &replacement,
        &h.room,
        "viewer",
        "viewer_input_replace",
        -1,
        "",
        presence(),
    );
    assert!(
        remote_input_peers(&host.next("remote-input-peers").raw).is_empty(),
        "replaced session retained input authority"
    );
}
