//! Gamepad grants, devices and lazy creation.

use super::*;

#[test]
fn co_play_gamepad_grant_isolation_and_retirement() {
    let pads = RecordedPads::default();
    let fixture = Fixture::with_gamepads(pads.clone());
    let (session, players) = co_play_session(&fixture, 3);
    let (a, b, keyboard) = (&players[0], &players[1], &players[2]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    assert!(
        players.iter().all(|c| !fixture.grant(c).is_empty()) && pads.devices().is_empty(),
        "one permission did not grant input without allocating gamepads"
    );
    let report_a = GamepadReport {
        buttons: 0x1000,
        left_x: 12000,
        ..GamepadReport::default()
    };
    let report_b = GamepadReport {
        buttons: 0x2000,
        right_trigger: 190,
        ..GamepadReport::default()
    };
    let pad = |report: GamepadReport| InputEvent {
        gamepad: Some(report),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, InputEvent::of("gamepad-reset"));
    assert!(pads.devices().is_empty(), "reset allocated a gamepad");
    fixture.send(a, pad(report_a));
    fixture.send(b, pad(report_b));
    let (first, second) = (pads.devices()[0].clone(), pads.devices()[1].clone());
    assert!(
        first.state() == report_a && second.state() == report_b,
        "gamepad reports crossed grant ownership"
    );
    assert!(
        session
            .state()
            .requests
            .iter()
            .all(|view| !view.endpoint_id.is_empty()),
        "Host presentation lost the authenticated endpoint"
    );
    fixture.send(a, InputEvent::key("KeyA", true));
    fixture.sink.next();
    fixture.send(a, InputEvent::of("gamepad-reset"));
    assert!(
        first.state() == GamepadReport::default()
            && second.state() == report_b
            && fixture.conn(a, |c| c.keys.contains("KeyA")),
        "gamepad reset changed keys or another participant"
    );
    fixture.send(a, pad(report_a));
    fixture.send(a, InputEvent::of("reset"));
    assert!(
        first.state() == GamepadReport::default() && fixture.conn(a, |c| c.keys.is_empty()),
        "whole input reset retained held input"
    );
    fixture.sink.next();
    fixture.sink.set_focused(false);
    fixture.refresh();
    assert!(
        players.iter().all(|c| fixture.conn(c, |c| c.suspended))
            && second.state() == GamepadReport::default()
            && !first.closed()
            && !second.closed(),
        "focus loss did not neutralize all input"
    );
    fixture.send(b, pad(report_b));
    assert_eq!(
        second.state(),
        GamepadReport::default(),
        "unfocused source accepted gamepad input"
    );
    fixture.sink.set_focused(true);
    fixture.send(b, pad(report_b));
    assert!(
        fixture.pad(a) && fixture.pad(b) && pads.devices().len() == 2,
        "focus recovery replaced a player's virtual device"
    );
    fixture.send(keyboard, InputEvent::key("KeyB", true));
    fixture.sink.next();
    let (edge, epoch) = fixture.conn(a, |c| (c.edge_id.clone(), c.epoch.clone()));
    fixture.manager.shared().receive_packet(
        session.id(),
        &edge,
        Packet {
            id: a.1.clone(),
            epoch,
            closed: true,
            ..Packet::default()
        },
        None,
    );
    assert!(
        first.closed()
            && fixture.grant(a).is_empty()
            && !fixture.grant(b).is_empty()
            && fixture.conn(keyboard, |c| c.keys.contains("KeyB")),
        "one route loss affected another participant"
    );
    second.fail_updates();
    fixture.send(b, pad(report_b));
    assert!(
        second.closed()
            && !fixture.pad(b)
            && !fixture.grant(b).is_empty()
            && fixture.pad_refused(b)
            && fixture.conn(keyboard, |c| c.keys.contains("KeyB")),
        "gamepad failure changed healthy input authorization"
    );
    fixture.send(b, pad(report_b));
    assert_eq!(pads.devices().len(), 2, "failed gamepad retried allocation");
    fixture.send(b, InputEvent::key("KeyC", true));
    fixture.sink.next();
    assert!(
        fixture.conn(b, |c| c.keys.contains("KeyC")),
        "gamepad failure disabled keyboard input"
    );
    fixture
        .inner(|inner| inner.connections.get_mut(a).unwrap().edge_id = "replacement-edge".into());
    fixture.send(a, InputEvent::of("request"));
    fixture.send(a, pad(report_a));
    let third = pads.devices()[2].clone();
    session.set_target(display("2"));
    assert!(
        third.closed()
            && third.state() == GamepadReport::default()
            && fixture.grant(a).is_empty()
            && fixture.grant(keyboard).is_empty()
            && fixture.owner().is_none(),
        "source replacement retained input"
    );
    let data = serde_json::to_string(&session.state()).unwrap();
    assert!(
        !data.contains("gamepad") && !data.contains("device"),
        "baseline state gained extension fields"
    );
    fixture.manager.close();
    assert!(pads.closed(), "Manager did not close gamepad backend");
}

#[test]
fn gamepad_stop_releases_only_its_device_and_slot() {
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let pad = InputEvent {
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, InputEvent::key("KeyA", true));
    fixture.send(a, pad.clone());
    let first = pads.devices()[0].clone();
    fixture.send(a, InputEvent::of("gamepad-reset"));
    assert!(
        !first.closed() && first.state() == GamepadReport::default(),
        "focus reset retired the game-assigned device"
    );
    fixture.send(a, InputEvent::of("gamepad-stop"));
    assert!(
        first.closed()
            && !fixture.pad(a)
            && !fixture.grant(a).is_empty()
            && fixture.conn(a, |c| c.keys.contains("KeyA"))
            && !fixture.grant(b).is_empty(),
        "device stop retained the device or retired keyboard authorization"
    );
    fixture.send(b, pad.clone());
    assert!(
        fixture.pad(b) && !fixture.pad_refused(b),
        "device stop did not free the backend slot for another participant"
    );
    fixture.send(b, InputEvent::of("gamepad-stop"));
    fixture.send(a, pad);
    let devices = pads.devices();
    assert!(
        fixture.pad(a) && devices.len() == 3 && !devices[2].same(&first) && !fixture.pad_refused(a),
        "reconnected device did not reuse its existing grant"
    );
}

#[test]
fn co_play_gamepad_admission_and_failure() {
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (session, players) = co_play_session(&fixture, 3);
    set_policy(&session, "ask", "").unwrap();
    let (a, b, third) = (&players[0], &players[1], &players[2]);
    let request_id = |key: &Key| fixture.conn(key, |c| c.request_id.clone());
    fixture.send(a, InputEvent::of("request"));
    assert!(
        fixture.conn(a, |c| c.requested) && !fixture.pad(a),
        "pending permission allocated a pad"
    );
    pads.set_open_error(Some("no driver slot"));
    session.decide(&request_id(a), true).unwrap();
    let pad = InputEvent {
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, pad.clone());
    assert!(
        !fixture.grant(a).is_empty() && fixture.pad_refused(a) && !fixture.pad(a),
        "pad allocation failure revoked ordinary permission"
    );
    pads.set_open_error(None);
    fixture.send(a, pad.clone());
    assert!(
        pads.devices().is_empty(),
        "failed grant retried gamepad allocation"
    );
    let intent = fixture.conn(a, |c| c.intent.clone());
    fixture.send(
        a,
        InputEvent {
            intent,
            ..InputEvent::of("release")
        },
    );
    fixture.send(a, InputEvent::of("request"));
    session.decide(&request_id(a), true).unwrap();
    fixture.send(a, pad.clone());
    assert!(
        fixture.pad(a) && !fixture.pad_refused(a),
        "fresh grant did not reset gamepad failure"
    );
    fixture.send(b, InputEvent::of("request"));
    session.decide(&request_id(b), true).unwrap();
    fixture.send(b, pad.clone());
    assert!(
        !fixture.grant(b).is_empty() && fixture.pad_refused(b) && pads.devices().len() == 1,
        "capacity failure revoked keyboard authorization"
    );
    fixture.send(b, InputEvent::key("KeyA", true));
    fixture.sink.next();
    fixture.release();
    fixture.send(third, InputEvent::of("request"));
    session.decide(&request_id(third), true).unwrap();
    fixture.send(third, pad);
    assert!(
        fixture.pad(third) && !fixture.grant(third).is_empty(),
        "a freed gamepad slot was not reusable by the next granted controller"
    );
    fixture.manager.revoke();
    assert!(
        pads.devices()
            .iter()
            .all(|pad| pad.closed() && pad.state() == GamepadReport::default()),
        "panic stop retained gamepad input"
    );
}

fn push(fixture: &Fixture, key: &Key) {
    let event = InputEvent {
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.inner(|inner| inner.receive_gamepad(key, &event));
}

#[test]
fn only_one_controller_is_enumerated_at_a_time() {
    // Two controllers report in the same frame while one slot is free. The
    // driver picks the free XInput index itself, so two concurrent plugs can
    // both claim it and one target silently never appears. A report that
    // arrives during enumeration is dropped; full reports repeat at 60 Hz.
    let (backend, started, finish) = blocked();
    let fixture = Fixture::with_gamepads(backend.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    let report = |index: u8| InputEvent {
        pad: index,
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.inner(|inner| inner.receive_gamepad(a, &report(0)));
    started
        .recv_timeout(Duration::from_secs(1))
        .expect("the first creation did not start");
    fixture.inner(|inner| inner.receive_gamepad(a, &report(1)));
    assert_eq!(
        backend.opens.load(Ordering::SeqCst),
        1,
        "a second controller was enumerated while the first was still opening"
    );
    finish.send(()).unwrap();
    fixture.wait_gamepad_idle();
    // The dropped report returns with the next full report.
    fixture.inner(|inner| inner.receive_gamepad(a, &report(1)));
    finish.send(()).unwrap();
    fixture.wait_gamepad_idle();
    assert_eq!(
        fixture.pads(a),
        2,
        "the controller dropped during enumeration never got a device"
    );
}

#[test]
fn a_reset_reaches_every_controller_even_when_another_index_was_refused() {
    // Index 0 was refused, index 1 holds the only device. Leaving the page
    // resets the Viewer, not one controller, so index 1 must still go neutral.
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    for index in [1u8, 0u8] {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(GamepadReport {
                    buttons: 1,
                    left_x: 20_000,
                    ..GamepadReport::default()
                }),
                ..InputEvent::of("gamepad")
            },
        );
    }
    assert!(
        fixture.pad(a) && fixture.pad_refused(a),
        "setup did not leave one working and one refused controller"
    );
    let working = pads.devices()[0].clone();
    assert_ne!(working.state(), GamepadReport::default());
    fixture.send(a, InputEvent::of("gamepad-reset"));
    assert_eq!(
        working.state(),
        GamepadReport::default(),
        "a refusal on index 0 swallowed the Viewer's reset"
    );
}

#[test]
fn gamepad_creation_does_not_block_current_input_or_revocation() {
    let (backend, started, finish) = blocked();
    let fixture = Fixture::with_gamepads(backend.clone());
    let (session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let existing = RecordedPad::default();
    fixture.inner(|inner| {
        inner
            .connections
            .get_mut(a)
            .unwrap()
            .pads
            .insert(0, Box::new(PadDevice(existing.clone())))
    });
    push(&fixture, b);
    started
        .recv_timeout(Duration::from_secs(1))
        .expect("creation did not start");
    for _ in 0..32 {
        push(&fixture, b);
    }
    // Current pad, keyboard and control-stop all progress during enumeration.
    push(&fixture, a);
    fixture.send(a, InputEvent::key("KeyA", true));
    fixture.manager.revoke();
    assert!(
        existing.closed()
            && existing.state() == GamepadReport::default()
            && fixture.grant(a).is_empty()
            && fixture.grant(b).is_empty(),
        "control-stop retained current input"
    );
    fixture.send(b, InputEvent::of("request"));
    let id = fixture.conn(b, |c| c.request_id.clone());
    session.decide(&id, true).unwrap();
    push(&fixture, b);
    assert_eq!(
        backend.opens.load(Ordering::SeqCst),
        1,
        "full reports or a replacement grant started parallel creation"
    );
    finish.send(()).unwrap();
    fixture.wait_gamepad_idle();
    assert!(
        backend.pad.closed()
            && !fixture.pad(b)
            && !fixture.grant(b).is_empty()
            && !fixture.pad_refused(b),
        "stale creation attached to or changed a replacement grant"
    );
}

#[test]
fn gamepad_stop_discards_pending_creation_without_retiring_grant() {
    let (backend, started, finish) = blocked();
    let fixture = Fixture::with_gamepads(backend.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let c = &players[0];
    fixture.send(c, InputEvent::of("request"));
    let grant = fixture.grant(c);
    push(&fixture, c);
    started
        .recv_timeout(Duration::from_secs(1))
        .expect("device enumeration did not start");
    fixture.send(c, InputEvent::of("gamepad-stop"));
    finish.send(()).unwrap();
    fixture.wait_gamepad_idle();
    assert!(
        backend.pad.closed()
            && !fixture.pad(c)
            && fixture.grant(c) == grant
            && !fixture.pad_refused(c),
        "late enumeration revived the stopped device or changed its keyboard grant"
    );
    // A subsequent full report can create a fresh device under the same grant.
    let pads = RecordedPads::default();
    fixture.inner(|inner| inner.gamepads = Some(Arc::new(pads.clone())));
    let report = GamepadReport {
        buttons: 1,
        ..GamepadReport::default()
    };
    fixture.send(
        c,
        InputEvent {
            gamepad: Some(report),
            ..InputEvent::of("gamepad")
        },
    );
    assert!(
        fixture.pad(c) && pads.devices()[0].state() == report && fixture.grant(c) == grant,
        "reconnection could not create a fresh device"
    );
}

/// The last reply this Viewer received, decrypted. Replies are sealed per
/// endpoint and carry the refusal state, so the Viewer sees exactly what the
/// Browser would.
fn last_reply(channel: &FakeChannel, endpoint: &str, secret: &str) -> serde_json::Value {
    let cipher = InputCipher::new(secret).unwrap();
    let last = channel
        .take()
        .into_iter()
        .rfind(|packet| packet.id == endpoint && !packet.data.is_empty())
        .expect("the Viewer received no reply");
    let plain = cipher
        .open_with(crate::cipher::FROM_HOST, &last.data, &last.epoch)
        .expect("reply decrypts");
    serde_json::from_slice(&plain).unwrap()
}

#[test]
fn a_refused_controller_keeps_its_reason_in_the_last_reply() {
    // The Host answers a refusal with its reason, then tells the share the new
    // slot count. A slot update carrying no reason would overwrite the only
    // explanation the Viewer gets, leaving a controller that silently does
    // nothing.
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    let channel = FakeChannel::open();
    let _lose = session.attach(&b.1, channel.clone());
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let pad = InputEvent {
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, pad.clone());
    fixture.send(b, pad);
    assert!(fixture.pad_refused(b), "the second player was not refused");
    let body = last_reply(&channel, &b.1, &"02".repeat(32));
    assert_eq!(
        body.get("reason").and_then(|reason| reason.as_str()),
        Some("gamepad"),
        "the last reply dropped the refusal reason"
    );
}

#[test]
fn a_partial_refusal_names_only_the_missing_controller() {
    // Two slots, three controllers: the first two work, the third is refused.
    // The reply must name that index, or the Viewer cannot tell which
    // controller to stop and would either spin on it or drop the working ones.
    let pads = RecordedPads::with_capacity(2);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    let channel = FakeChannel::open();
    let _lose = session.attach(&a.1, channel.clone());
    fixture.send(a, InputEvent::of("request"));
    for index in 0..3u8 {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(GamepadReport {
                    buttons: 1 << index,
                    ..GamepadReport::default()
                }),
                ..InputEvent::of("gamepad")
            },
        );
    }
    assert_eq!(fixture.pads(a), 2, "the two free slots were not both used");
    let body = last_reply(&channel, &a.1, &"01".repeat(32));
    assert_eq!(
        body.get("gamepadFailed"),
        Some(&serde_json::json!([2])),
        "the reply did not name the refused index alone"
    );
    // Unplugging the refused controller ends its refusal, so the index can be
    // reused without the Viewer having to retry.
    fixture.send(
        a,
        InputEvent {
            pad: 2,
            ..InputEvent::of("gamepad-stop")
        },
    );
    assert!(
        !fixture.pad_refused(a) && fixture.pads(a) == 2,
        "the refused index kept its refusal after the controller left"
    );
}

#[test]
fn a_refused_gamepad_can_retry_once_a_slot_frees() {
    // One slot, two players: the second is refused, and the refusal must not be
    // permanent. The capability stays advertised so the Viewer can ask again.
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let report = GamepadReport {
        buttons: 1,
        ..GamepadReport::default()
    };
    let pad = InputEvent {
        gamepad: Some(report),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, pad.clone());
    assert!(fixture.pad(a), "the first player did not get the only slot");
    fixture.send(b, pad.clone());
    assert!(
        !fixture.pad(b) && fixture.pad_refused(b),
        "the second player took a slot that was already in use"
    );
    // A refusal is about room, not about this build: the option must survive.
    assert!(fixture.pad_refused(b), "the refusal removed the retry path");
    // The first player releases the slot; the second asks again.
    fixture.send(a, InputEvent::of("gamepad-stop"));
    fixture.send(b, InputEvent::of("gamepad-retry"));
    fixture.send(b, pad.clone());
    fixture.wait_gamepad_idle();
    assert!(
        fixture.pad(b) && pads.devices().iter().filter(|pad| !pad.closed()).count() == 1,
        "the retry after a freed slot did not allocate exactly one device"
    );
}

#[test]
fn a_refused_gamepad_ignores_reports_until_it_retries() {
    // Ordinary reports from a refused connection must not spin up attempts.
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 2);
    let (a, b) = (&players[0], &players[1]);
    for c in &players {
        fixture.send(c, InputEvent::of("request"));
    }
    let pad = InputEvent {
        gamepad: Some(GamepadReport {
            buttons: 1,
            ..GamepadReport::default()
        }),
        ..InputEvent::of("gamepad")
    };
    fixture.send(a, pad.clone());
    fixture.send(b, pad.clone());
    let attempts = pads.devices().len();
    fixture.send(b, pad.clone());
    fixture.send(b, pad);
    assert_eq!(
        pads.devices().len(),
        attempts,
        "a refused connection kept retrying on its own"
    );
}

#[test]
fn one_viewer_can_drive_several_controllers() {
    // A Viewer plugging in two pads owns two virtual devices, each reporting
    // its own state. They are addressed by the Browser's gamepad index.
    let pads = RecordedPads::default();
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    let first = GamepadReport {
        buttons: 1,
        left_x: 9000,
        ..GamepadReport::default()
    };
    let second = GamepadReport {
        buttons: 2,
        right_trigger: 200,
        ..GamepadReport::default()
    };
    for (index, report) in [(0u8, first), (1u8, second)] {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(report),
                ..InputEvent::of("gamepad")
            },
        );
    }
    assert_eq!(
        fixture.pads(a),
        2,
        "a second controller did not get its own device"
    );
    let devices = pads.devices();
    assert!(
        devices[0].state() == first && devices[1].state() == second,
        "controller reports crossed each other"
    );
}

#[test]
fn each_controller_retires_on_its_own_stop() {
    // Stopping one controller must leave the other running: a Viewer putting
    // down one pad is not ending remote play.
    let pads = RecordedPads::default();
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    for index in [0u8, 1u8] {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(GamepadReport {
                    buttons: 1 << index,
                    ..GamepadReport::default()
                }),
                ..InputEvent::of("gamepad")
            },
        );
    }
    let (first, second) = (pads.devices()[0].clone(), pads.devices()[1].clone());
    fixture.send(
        a,
        InputEvent {
            pad: 0,
            ..InputEvent::of("gamepad-stop")
        },
    );
    assert!(
        first.closed() && !second.closed() && fixture.pads(a) == 1,
        "stopping one controller retired the other"
    );
    // The retired index can be brought back.
    fixture.send(
        a,
        InputEvent {
            pad: 0,
            gamepad: Some(GamepadReport {
                buttons: 4,
                ..GamepadReport::default()
            }),
            ..InputEvent::of("gamepad")
        },
    );
    assert_eq!(
        fixture.pads(a),
        2,
        "a restarted controller was not re-created"
    );
}

#[test]
fn a_reset_neutralizes_every_controller_a_viewer_owns() {
    // Leaving the page is one event for one Viewer, so every controller it
    // owns must go neutral. Addressing a single index would leave the others
    // holding their last stick position in the game.
    let pads = RecordedPads::default();
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    for index in [0u8, 1u8] {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(GamepadReport {
                    buttons: 1 << index,
                    left_x: 20_000,
                    ..GamepadReport::default()
                }),
                ..InputEvent::of("gamepad")
            },
        );
    }
    let devices = pads.devices();
    assert!(
        devices.len() == 2
            && devices
                .iter()
                .all(|pad| pad.state() != GamepadReport::default()),
        "the test did not put both controllers into an active state"
    );
    // The Browser's reset carries no index: it is about the Viewer, not a pad.
    fixture.send(a, InputEvent::of("gamepad-reset"));
    assert!(
        devices
            .iter()
            .all(|pad| pad.state() == GamepadReport::default() && !pad.closed()),
        "a reset left a controller holding input or retired it"
    );
    assert_eq!(
        fixture.pads(a),
        2,
        "a reset dropped the controller slots it must keep"
    );
}

#[test]
fn a_retry_covers_every_refused_controller() {
    // One slot, a Viewer asking for two controllers: the second is refused.
    // Retrying clears the refusal so the next report can try again.
    let pads = RecordedPads::with_capacity(1);
    let fixture = Fixture::with_gamepads(pads.clone());
    let (_session, players) = co_play_session(&fixture, 1);
    let a = &players[0];
    fixture.send(a, InputEvent::of("request"));
    for index in [0u8, 1u8] {
        fixture.send(
            a,
            InputEvent {
                pad: index,
                gamepad: Some(GamepadReport {
                    buttons: 1,
                    ..GamepadReport::default()
                }),
                ..InputEvent::of("gamepad")
            },
        );
    }
    assert!(
        fixture.pads(a) == 1 && fixture.pad_refused(a),
        "the second controller did not record a refusal"
    );
    let grant = fixture.grant(a);
    fixture.send(
        a,
        InputEvent {
            pad: 1,
            ..InputEvent::of("gamepad-retry")
        },
    );
    // Retrying is a controller decision, not a control decision: the reply must
    // not tell the Viewer its grant is gone, or the UI would drop the pad it
    // was still allowed to use.
    assert!(
        !fixture.pad_refused(a) && fixture.grant(a) == grant,
        "retry left the refusal in place or retired the grant"
    );
}
