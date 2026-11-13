//! Co-play input sharing, text and session retirement.

use super::*;

#[test]
fn co_play_shared_keyboard_and_mouse() {
    let fixture = Fixture::new();
    let (session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    assert!(
        !fixture.grant(a).is_empty()
            && !fixture.grant(b).is_empty()
            && fixture.owner() == Some(session.id())
            && session.state().requests.len() == 2,
        "co-play did not admit both players"
    );
    for c in &players {
        fixture.send(c, InputEvent::key("KeyA", true));
        fixture.send(c, InputEvent::button(0, true));
        fixture.send(c, InputEvent::button(0, true));
    }
    assert_eq!(
        fixture.sink.drain().len(),
        2,
        "shared held input was injected twice"
    );
    for c in &players {
        fixture.send(c, InputEvent::key("KeyA", true));
        let events = fixture.sink.drain();
        assert!(
            events.len() == 1
                && events[0].kind == "key"
                && events[0].code == "KeyA"
                && events[0].down,
            "an existing holder's keyboard repeat was suppressed"
        );
    }
    fixture.send(a, InputEvent::key("KeyA", false));
    fixture.send(a, InputEvent::button(0, false));
    fixture.send(b, InputEvent::text("hello"));
    assert!(
        fixture.sink.events() == 0 && fixture.sink.resets() == 0,
        "a player released another player's input or reset shared input through text"
    );
    for c in &players {
        fixture.send(
            c,
            InputEvent {
                x: 0.5,
                y: 0.5,
                ..InputEvent::of("move")
            },
        );
    }
    fixture.send(
        b,
        InputEvent {
            dx: 10,
            dy: -4,
            ..InputEvent::of("relative")
        },
    );
    fixture.send(
        a,
        InputEvent {
            delta: 120,
            ..InputEvent::of("wheel")
        },
    );
    assert_eq!(
        fixture.sink.drain().len(),
        4,
        "authorized players cannot share pointer movement and scrolling"
    );
    fixture.send(a, InputEvent::key("KeyW", true));
    fixture.send(a, InputEvent::button(1, true));
    fixture.sink.drain();
    let id = fixture.conn(a, |c| c.request_id.clone());
    session.decide(&id, false).unwrap();
    let events = fixture.sink.drain();
    assert_eq!(
        events.len(),
        2,
        "personal revocation did not release only distinct held input"
    );
    for event in events {
        assert!(
            !event.down
                && ((event.kind == "key" && event.code == "KeyW")
                    || (event.kind == "button" && event.button == 1)),
            "personal revocation changed a sibling's input"
        );
    }
    assert!(
        !fixture.grant(b).is_empty()
            && fixture.conn(b, |c| c.keys.contains("KeyA") && c.buttons.contains(&0))
            && fixture.sink.resets() == 0
            && session.state().mode == "room",
        "revocation disrupted another player"
    );
    fixture.send(b, InputEvent::of("reset"));
    let events = fixture.sink.drain();
    assert_eq!(
        events.len(),
        2,
        "final holder reset missed a key or mouse button"
    );
    assert!(
        events.iter().all(|event| !event.down),
        "reset pressed input"
    );
    fixture.send(b, InputEvent::key("KeyB", true));
    fixture.sink.next();
    fixture.manager.revoke();
    assert!(
        fixture.grant(b).is_empty()
            && fixture.owner().is_none()
            && fixture.sink.resets() == 1
            && session.state().mode == "ask",
        "Host stop left a player active"
    );
}

#[test]
fn simultaneous_control_preserves_authorization_and_resets_for_new_source() {
    let fixture = Fixture::with_gamepads(RecordedPads::default());
    let (session, players) = co_play_session(&fixture, 2);
    set_policy(&session, "password", "source-password").unwrap();
    session.set_simultaneous(false).unwrap();
    let clear_retry = || {
        fixture.inner(|inner| {
            inner
                .sessions
                .get_mut(&session.id())
                .unwrap()
                .password_retry = None
        })
    };
    let password_request = InputEvent {
        password: "source-password".into(),
        ..InputEvent::of("request")
    };
    for c in &players {
        clear_retry();
        fixture.send(c, password_request.clone());
    }
    assert!(
        !fixture.grant(&players[0]).is_empty() && fixture.grant(&players[1]).is_empty(),
        "single control admitted multiple participants"
    );
    let state = session.state();
    assert!(
        state.co_play
            && state.gamepad_driver.is_some()
            && !state.requests[0].endpoint_id.is_empty(),
        "single control hid source capabilities or participation"
    );
    session.set_simultaneous(true).unwrap();
    assert!(
        fixture.grant(&players[0]).is_empty()
            && session.state().requests.is_empty()
            && session.state().mode == "password",
        "setting change retained grants or changed authorization"
    );
    for c in &players {
        clear_retry();
        fixture.send(c, password_request.clone());
    }
    assert!(
        !fixture.grant(&players[0]).is_empty() && !fixture.grant(&players[1]).is_empty(),
        "setting change lost password or failed to admit both participants"
    );
    set_policy(&session, "ask", "").unwrap();
    assert_eq!(
        session.state().simultaneous,
        Some(true),
        "ordinary authorization update reset source concurrency"
    );
    session.set_target(display("9"));
    set_policy_co_play(&session, "room", "", true).unwrap();
    let state = session.state();
    assert!(
        state.simultaneous == Some(false) && state.co_play,
        "new source inherited concurrency or lost negotiated support"
    );
    set_policy(&session, "ask", "").unwrap();
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let request_id = |key: &Key| fixture.conn(key, |c| c.request_id.clone());
    session.decide(&request_id(&players[0]), true).unwrap();
    fixture.send(&players[0], InputEvent::key("KeyA", true));
    fixture.sink.next();
    session.decide(&request_id(&players[1]), true).unwrap();
    assert!(
        fixture.grant(&players[0]).is_empty()
            && !fixture.grant(&players[1]).is_empty()
            && fixture.owner() == Some(session.id())
            && fixture.conn(&players[0], |c| c.keys.is_empty()),
        "explicit Host approval did not hand over exclusive input"
    );
    let (other, others) = co_play_session(&fixture, 1);
    set_policy(&other, "ask", "").unwrap();
    other.set_simultaneous(false).unwrap();
    fixture.send(&others[0], InputEvent::of("request"));
    assert!(
        other.decide(&request_id(&others[0]), true).is_err()
            && !fixture.grant(&players[1]).is_empty()
            && fixture.owner() == Some(session.id()),
        "explicit approval displaced another source"
    );
}

#[test]
fn co_play_text_follows_keyboard_grants() {
    let fixture = Fixture::with_gamepads(RecordedPads::default());
    let (session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    fixture.send(a, InputEvent::of("request"));
    fixture.send(
        a,
        InputEvent {
            gamepad: Some(GamepadReport {
                buttons: 1,
                ..GamepadReport::default()
            }),
            ..InputEvent::of("gamepad")
        },
    );
    fixture.send(a, InputEvent::key("ControlLeft", true));
    fixture.sink.next();
    fixture.send(a, InputEvent::text("hello"));
    assert!(
        fixture.sink.next().text == "hello"
            && fixture.conn(a, |c| c.keys.is_empty())
            && fixture.sink.resets() == 1
            && fixture.pad(a),
        "single participant text did not reset its own held keyboard input"
    );
    fixture.send(b, InputEvent::of("request"));
    fixture.send(b, InputEvent::key("KeyW", true));
    fixture.sink.next();
    fixture.send(a, InputEvent::text("blocked"));
    assert!(
        fixture.sink.events() == 0
            && fixture.sink.resets() == 1
            && fixture.conn(b, |c| c.keys.contains("KeyW")),
        "text interrupted another keyboard player"
    );
    let id = fixture.conn(b, |c| c.request_id.clone());
    session.decide(&id, false).unwrap();
    fixture.sink.next();
    fixture.send(a, InputEvent::text("restored"));
    assert!(
        fixture.sink.next().text == "restored" && !fixture.grant(a).is_empty() && fixture.pad(a),
        "text did not resume after the other keyboard grant ended"
    );
}

#[test]
fn co_play_retirement_and_admission() {
    let fixture = Fixture::new();
    let (session, players) = co_play_session(&fixture, 3);
    let (a, b, third) = (&players[0], &players[1], &players[2]);
    for c in [a, b, third] {
        fixture.send(c, InputEvent::of("request"));
    }
    assert!(
        players.iter().all(|c| !fixture.grant(c).is_empty()),
        "simultaneous co-play did not admit every Viewer"
    );
    let intent = fixture.conn(third, |c| c.intent.clone());
    fixture.send(
        third,
        InputEvent {
            intent,
            ..InputEvent::of("release")
        },
    );
    // A released participant re-enters the same shared session.
    fixture.send(third, InputEvent::of("request"));
    assert!(
        !fixture.grant(a).is_empty()
            && !fixture.grant(b).is_empty()
            && !fixture.grant(third).is_empty(),
        "a returning participant displaced or was refused by the shared session"
    );
    let (other, others) = co_play_session(&fixture, 1);
    fixture.send(&others[0], InputEvent::of("request"));
    let a_request = fixture.conn(a, |c| c.request_id.clone());
    assert!(
        fixture.grant(&others[0]).is_empty() && other.decide(&a_request, false).is_err(),
        "another native session acquired or changed the active desktop source"
    );
    for c in [a, b] {
        fixture.send(c, InputEvent::key("KeyA", true));
    }
    fixture.sink.next();
    let edge = fixture.conn(a, |c| c.edge_id.clone());
    fixture.inner(|inner| inner.revoke_edge(session.id(), &edge));
    assert!(
        fixture.grant(a).is_empty()
            && !fixture.grant(b).is_empty()
            && fixture.sink.events() == 0
            && fixture.owner() == Some(session.id()),
        "one route loss released another player's held key"
    );
    fixture.sink.set_focused(false);
    fixture.refresh();
    fixture.refresh();
    assert!(
        fixture.conn(b, |c| c.suspended && c.keys.is_empty()) && fixture.sink.resets() == 1,
        "shared-input source did not suspend once"
    );
    fixture.sink.set_focused(true);
    fixture.refresh();
    assert!(
        !fixture.conn(b, |c| c.suspended) && fixture.sink.events() == 0,
        "focus recovery replayed held keys"
    );
    fixture.send(b, InputEvent::key("KeyB", true));
    fixture.sink.next();
    let old_grant = fixture.grant(b);
    session.pause(true);
    assert!(
        fixture.grant(b).is_empty() && fixture.sink.resets() == 2,
        "pause retained co-play input"
    );
    session.pause(false);
    fixture.send(
        b,
        InputEvent {
            grant: old_grant,
            ..InputEvent::key("KeyC", true)
        },
    );
    assert_eq!(fixture.sink.events(), 0, "resume restored a stale grant");
    fixture.send(b, InputEvent::of("request"));
    session.set_target(display("2"));
    let state = session.state();
    assert!(
        fixture.grant(b).is_empty()
            && fixture.owner().is_none()
            && !state.co_play
            && state.mode == "off",
        "source replacement inherited co-play authorization"
    );
    let data = serde_json::to_string(&state).unwrap();
    assert!(
        !data.contains("coPlay") && !data.contains("pointer"),
        "default Host state added fields to the published baseline"
    );
    assert_eq!(data, r#"{"platform":"windows","mode":"off","requests":[]}"#);
}
