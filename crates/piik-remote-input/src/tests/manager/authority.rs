//! Host authorization policy, prepared changes and target eligibility.

use super::*;

#[test]
fn source_authorization_policy() {
    let fixture = Fixture::new();
    let session = fixture.session();
    let peers = [
        peer("first-viewer", 'a', 'b'),
        peer("second-viewer", 'c', 'd'),
    ];
    session.set_peers(&peers).unwrap();
    let (first, second) = (key_of(&session, &peers[0]), key_of(&session, &peers[1]));
    for key in [&first, &second] {
        bind(&fixture, key, "root-edge", &"e".repeat(32));
    }
    let requested = |key: &Key| fixture.conn(key, |c| c.requested);
    request(&fixture, &session, &first, "");
    assert!(
        session.state().mode == "off" && !requested(&first) && fixture.owner().is_none(),
        "ordinary sharing accepted a control request before opt-in"
    );
    set_policy(&session, "ask", "").unwrap();
    request(&fixture, &session, &first, "");
    let old_request = fixture.conn(&first, |c| c.request_id.clone());
    assert!(
        requested(&first) && fixture.owner().is_none(),
        "Ask each time did not require Host approval"
    );
    set_policy(&session, "room", "").unwrap();
    assert!(
        fixture.owner().is_none()
            && !requested(&first)
            && session.decide(&old_request, true).is_err(),
        "policy change inherited an old request"
    );
    request(&fixture, &session, &first, "");
    assert!(
        !fixture.grant(&first).is_empty(),
        "explicit room policy did not grant control"
    );
    request(&fixture, &session, &second, "");
    assert!(
        !fixture.grant(&first).is_empty()
            && fixture.grant(&second).is_empty()
            && !requested(&second),
        "automatic request displaced an active controller"
    );
    fixture.manager.revoke();
    assert!(
        fixture.owner().is_none() && session.state().mode == "ask",
        "local stop did not disable automatic access"
    );
    request(&fixture, &session, &second, "");
    assert!(
        requested(&second) && fixture.owner().is_none(),
        "local stop allowed automatic re-entry"
    );
    assert!(
        set_policy(&session, "password", "").is_err(),
        "password mode accepted an empty credential"
    );
    set_policy(&session, "password", "shared-secret").unwrap();
    request(&fixture, &session, &first, "wrong");
    assert!(
        fixture.owner().is_none() && !requested(&first),
        "incorrect password acquired control"
    );
    request(&fixture, &session, &second, "shared-secret");
    assert!(
        fixture.owner().is_none(),
        "changing endpoints bypassed password attempt pacing"
    );
    fixture.inner(|inner| {
        inner
            .sessions
            .get_mut(&session.id())
            .unwrap()
            .password_retry = None
    });
    request(&fixture, &session, &first, "shared-secret");
    assert!(
        !fixture.grant(&first).is_empty(),
        "correct password did not authorize control"
    );
    let id = fixture.conn(&first, |c| c.request_id.clone());
    session.decide(&id, false).unwrap();
    assert!(
        fixture.owner().is_none() && session.state().mode == "ask",
        "Host stop left automatic access enabled"
    );
    set_policy(&session, "room", "").unwrap();
    request(&fixture, &session, &first, "");
    fixture.inner(|inner| inner.revoke_edge(session.id(), "root-edge"));
    assert!(fixture.owner().is_none(), "route loss retained input");
    request(&fixture, &session, &first, "");
    let old_grant = fixture.grant(&first);
    assert!(
        !old_grant.is_empty(),
        "room policy could not authorize a fresh request"
    );
    session.pause(true);
    set_policy(&session, "off", "").unwrap();
    session.pause(false);
    fixture.receive(&first, &key_down(&old_grant));
    request(&fixture, &session, &second, "");
    assert!(
        fixture.owner().is_none() && !requested(&second) && fixture.sink.events() == 0,
        "disabled remote control retained input or accepted a new request"
    );
    set_policy(&session, "room", "").unwrap();
    request(&fixture, &session, &first, "");
    session.set_target(display("2"));
    assert!(
        fixture.owner().is_none() && session.state().mode == "off",
        "source replacement inherited permission"
    );
    request(&fixture, &session, &second, "");
    assert!(
        !requested(&second) && fixture.owner().is_none(),
        "replacement source accepted input without opt-in"
    );
    set_policy(&session, "room", "").unwrap();
    request(&fixture, &session, &first, "");
    let revoked = fixture.grant(&first);
    session.set_peers(&peers[1..]).unwrap();
    assert!(
        fixture.owner().is_none() && fixture.inner(|inner| !inner.connections.contains_key(&first)),
        "room membership removal retained the active input grant"
    );
    // The removed endpoint has no binding: its old grant and key are dead.
    fixture.receive(&first, &key_down(&revoked));
    assert!(
        fixture.owner().is_none() && fixture.sink.events() == 0,
        "removed endpoint regained input with its old grant or key"
    );
    request(&fixture, &session, &second, "");
    assert!(
        !fixture.grant(&second).is_empty(),
        "membership removal interrupted the remaining authenticated endpoint"
    );
}

#[test]
fn route_replacement_requires_approval_and_ignores_old_closure() {
    let fixture = Fixture::new();
    let session = fixture.session();
    set_policy(&session, "ask", "").unwrap();
    let viewer = peer("viewer-route-test", 'a', 'b');
    session.set_peers(std::slice::from_ref(&viewer)).unwrap();
    let key = key_of(&session, &viewer);
    let cipher = InputCipher::new(&viewer.secret).unwrap();
    let mut sequence = 0;
    let mut request = |epoch: &str| {
        sequence += 1;
        let event = InputEvent {
            intent: epoch.into(),
            ..InputEvent::of("request")
        };
        let packet = viewer_packet(&cipher, sequence, epoch, &viewer.endpoint_id, &event);
        fixture
            .manager
            .shared()
            .receive_packet(session.id(), "same-root-relay", packet, None);
        let views = session.state().requests;
        assert!(
            views.len() == 1 && views[0].state == "pending",
            "new channel did not require fresh approval"
        );
        session.decide(&views[0].id, true).unwrap();
        fixture.grant(&key)
    };
    let (old_epoch, new_epoch) = ("c".repeat(32), "d".repeat(32));
    let old_grant = request(&old_epoch);
    let new_grant = request(&new_epoch);
    assert!(
        old_grant != new_grant && fixture.sink.resets() == 1,
        "replacement retained old input"
    );
    let closed = |epoch: &str| Packet {
        id: viewer.endpoint_id.clone(),
        epoch: epoch.into(),
        closed: true,
        ..Packet::default()
    };
    fixture.manager.shared().receive_packet(
        session.id(),
        "same-root-relay",
        closed(&old_epoch),
        None,
    );
    assert!(
        fixture.owner().is_some() && fixture.grant(&key) == new_grant,
        "old descendant closure retired the new grant"
    );
    fixture.manager.shared().receive_packet(
        session.id(),
        "same-root-relay",
        closed(&new_epoch),
        None,
    );
    assert!(
        fixture.owner().is_none() && fixture.sink.resets() == 2,
        "current route loss did not release input"
    );
}

#[test]
fn os_permission_cannot_authorize_a_replacement_source() {
    // Closing or replacing the share stays possible while an OS dialog is
    // open between preparation and commit.
    let fixture = Fixture::new();
    let session = fixture.session();
    let change = session
        .begin_policy(&CancellationToken::new(), "room", "", false)
        .unwrap();
    session.set_target(display("2"));
    assert!(
        change.apply().is_err() && session.state().mode == "off",
        "late OS permission enabled input on a replacement source"
    );
    let change = session
        .begin_policy(&CancellationToken::new(), "room", "", false)
        .unwrap();
    set_policy(&session, "off", "").unwrap();
    assert!(
        change.apply().is_err() && session.state().mode == "off",
        "late OS permission undid Host revocation"
    );
}

#[test]
fn prepared_policy_follows_the_committed_source_and_host_intent() {
    for action in [
        "complete",
        "off-before-work",
        "off-during-prepare",
        "pause-and-resume",
        "replace-source",
    ] {
        let fixture = Fixture::new();
        let session = fixture.session();
        let change = session
            .begin_policy(&CancellationToken::new(), "room", "", false)
            .unwrap();
        if action == "off-before-work" {
            set_policy(&session, "off", "").unwrap();
        }
        // Capture replacement retires input while preserving only the same
        // current Host intent through its own two source bindings.
        change.set_target(CaptureTarget::default());
        match action {
            "off-during-prepare" => set_policy(&session, "off", "").unwrap(),
            "pause-and-resume" => {
                session.pause(true);
                session.pause(false);
            }
            "replace-source" => session.set_target(display("2")),
            _ => {}
        }
        let installed = display("3");
        change.set_target(installed.clone());
        let cancelled = action != "complete";
        assert_eq!(
            change.token().is_cancelled(),
            cancelled,
            "{action}: cancellation did not follow the Host intent"
        );
        assert_eq!(
            change.apply().is_err(),
            cancelled,
            "{action}: late preparation changed a retired permission"
        );
        let expected = if cancelled { "off" } else { "room" };
        assert!(
            session.state().mode == expected
                && fixture.inner(|inner| inner.sessions[&session.id()].target == installed),
            "{action}: policy or actual source binding diverged after preparation"
        );
        set_policy(&session, "ask", "").unwrap_or_else(|error| panic!("{action}: {error}"));
    }
}

#[test]
fn picker_and_unknown_targets_are_unavailable() {
    let fixture = Fixture::new();
    let session = fixture.session();
    for kind in ["picker", "camera", ""] {
        session.set_target(CaptureTarget {
            kind: kind.into(),
            source_id: "5".into(),
            ..CaptureTarget::default()
        });
        assert!(
            set_policy(&session, "ask", "").is_err(),
            "{kind:?} target accepted input"
        );
    }
    session.set_target(window("7"));
    set_policy(&session, "ask", "").unwrap();
    assert!(set_policy(&session, "bogus", "").is_err());
    assert!(set_policy(&session, "room", "unexpected").is_err());
    assert!(set_policy(&session, "password", &"p".repeat(513)).is_err());
}

/// A grant raises the shared window once, so a Viewer asking to control a
/// background window can use it without the Host bringing it forward by hand.
#[test]
fn granting_control_asks_the_source_to_come_forward() {
    let fixture = Fixture::new();
    let session = fixture.session();
    let peers = [peer("viewer-one", 'a', 'b')];
    session.set_peers(&peers).unwrap();
    let key = key_of(&session, &peers[0]);
    bind(&fixture, &key, "root-edge", &"e".repeat(32));
    let raises = || fixture.sink.with(|r| r.raises);

    set_policy(&session, "ask", "").unwrap();
    request(&fixture, &session, &key, "");
    let request_id = fixture.conn(&key, |c| c.request_id.clone());
    assert_eq!(raises(), 0, "requesting must not raise the source");
    session.decide(&request_id, true).unwrap();
    assert!(!fixture.grant(&key).is_empty(), "control was not granted");
    assert_eq!(raises(), 1, "a grant must ask the source to come forward");

    fixture.manager.revoke();
    assert_eq!(raises(), 1, "releasing control must not raise the source");
}
