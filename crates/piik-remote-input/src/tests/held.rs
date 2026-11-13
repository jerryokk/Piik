use super::*;

#[test]
fn partial_text_only_retries_release() {
    let fixture = Fixture::new();
    fixture.sink.with(|r| r.reject_key_release = true);
    let (_session, keys) = co_play_session(&fixture, 1);
    let c = &keys[0];
    fixture.send(c, InputEvent::of("request"));
    let grant = fixture.grant(c);
    fixture.send(c, InputEvent::text("once\nnever"));
    assert!(
        fixture.grant(c) == grant
            && fixture.conn(c, |c| c.input_failed)
            && fixture.pending_key("Enter"),
        "text failure changed grant or lost release debt"
    );
    assert_eq!(
        fixture.sink.next().text,
        "once",
        "text prefix was not delivered"
    );
    let press = fixture.sink.next();
    assert!(
        press.code == "Enter" && press.down,
        "text did not execute newline press"
    );
    fixture.sink.with(|r| r.reject_key_release = false);
    let _ = fixture.retry_releases();
    assert_eq!(
        fixture.sink.events(),
        1,
        "text failure replayed a text fragment"
    );
    let release = fixture.sink.next();
    assert!(
        release.code == "Enter" && !release.down,
        "text recovery did not restrict itself to release"
    );
}

#[test]
fn rejected_input_keeps_grant_and_does_not_replay() {
    for shared in [false, true] {
        let fixture = Fixture::new();
        let (session, keys) = co_play_session(&fixture, 1);
        fixture.inner(|inner| inner.sessions.get_mut(&session.id()).unwrap().co_play = shared);
        let c = &keys[0];
        fixture.send(c, InputEvent::of("request"));
        let grant = fixture.grant(c);
        fixture.sink.set_apply_error(Some("temporarily rejected"));
        fixture.send(c, InputEvent::key("KeyA", true));
        assert!(
            !grant.is_empty()
                && fixture.grant(c) == grant
                && fixture.conn(c, |c| c.input_failed && c.keys.is_empty()),
            "failed press changed grant or became held (shared={shared})"
        );
        fixture.sink.set_apply_error(None);
        let _ = fixture.retry_releases();
        assert_eq!(fixture.sink.events(), 0, "failed press was replayed");
        fixture.send(c, InputEvent::key("KeyB", true));
        assert!(
            fixture.grant(c) == grant
                && fixture.conn(c, |c| !c.input_failed && c.keys.contains("KeyB")),
            "new input did not recover within grant"
        );
        fixture.sink.next();
        fixture.sink.set_apply_error(Some("release rejected"));
        fixture.send(c, InputEvent::key("KeyB", false));
        assert!(
            fixture.grant(c) == grant
                && fixture.pending_key("KeyB")
                && !fixture.conn(c, |c| c.keys.contains("KeyB")),
            "failed release lost cleanup debt or grant"
        );
        let error = fixture
            .apply_held(
                c,
                InputEvent {
                    x: 0.7,
                    y: 0.4,
                    ..InputEvent::of("move")
                },
            )
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("release rejected") && error.contains("key release retry"),
            "cleanup hid the rejected release: {error}"
        );
        assert_eq!(fixture.sink.events(), 0, "new action ran before cleanup");
        fixture.sink.set_reset_error(Some("reset blocked"));
        fixture.release_connection(c);
        assert!(
            fixture.grant(c).is_empty() && fixture.pending_key("KeyB"),
            "retirement lost pending release"
        );
        fixture.sink.set_apply_error(None);
        fixture.sink.set_reset_error(None);
        let _ = fixture.retry_releases();
        assert!(
            !fixture.pending_key("KeyB") && fixture.sink.events() == 1,
            "retired release did not recover"
        );
        let event = fixture.sink.next();
        assert!(
            event.kind == "key" && event.code == "KeyB" && !event.down,
            "cleanup replayed action: {event:?}"
        );
    }
}

#[test]
fn player_cleanup_failure_preserves_sibling_grant() {
    let fixture = Fixture::new();
    let (session, keys) = co_play_session(&fixture, 2);
    let (a, b) = (&keys[0], &keys[1]);
    for c in &keys {
        fixture.send(c, InputEvent::of("request"));
    }
    let grant = fixture.grant(b);
    fixture.send(a, InputEvent::button(0, true));
    fixture.sink.next();
    fixture.sink.set_apply_error(Some("release blocked"));
    fixture.send(a, InputEvent::of("reset"));
    assert!(
        !fixture.grant(a).is_empty()
            && fixture.grant(b) == grant
            && fixture.conn(a, |c| c.input_failed)
            && fixture.pending_button(0),
        "personal reset changed authority or lost release"
    );
    fixture.release_connection(a);
    assert!(
        fixture.grant(b) == grant && fixture.owner() == Some(session.id()),
        "disconnect cleanup revoked sibling"
    );
    let error = fixture
        .apply_held(
            b,
            InputEvent {
                x: 0.5,
                y: 0.5,
                ..InputEvent::of("move")
            },
        )
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("release blocked") && error.contains("button release retry"),
        "shared cleanup hid the rejected button release: {error}"
    );
    fixture.sink.set_apply_error(None);
    let _ = fixture.retry_releases();
    let event = fixture.sink.next();
    assert!(
        event.kind == "button" && !event.down,
        "cleanup replayed press"
    );
    fixture.send(b, InputEvent::key("KeyW", true));
    fixture.sink.next();
    fixture.sink.set_reset_error(Some("focus cleanup blocked"));
    fixture.sink.set_focused(false);
    fixture.refresh();
    assert!(
        fixture.grant(b) == grant
            && fixture.conn(b, |c| c.suspended)
            && fixture.pending_key("KeyW"),
        "focus cleanup lost grant or release debt"
    );
    let _ = fixture.retry_releases();
    let event = fixture.sink.next();
    assert!(
        event.code == "KeyW" && !event.down,
        "focus cleanup replayed press"
    );
    fixture.sink.set_reset_error(None);
    fixture.sink.set_focused(true);
    fixture.refresh();
    assert!(
        fixture.grant(b) == grant
            && !fixture.conn(b, |c| c.suspended)
            && fixture.sink.events() == 0,
        "focus return changed grant or replayed action"
    );
}

#[test]
fn committed_multiline_text() {
    use crate::text::{MAX_TEXT_BYTES, TextSink, apply_text};
    struct Sink(RecordedInput);
    impl TextSink for Sink {
        fn reset(&mut self) -> Result<(), Error> {
            InputSink::reset(&mut self.0)
        }
        fn apply(&mut self, target: Bounds, event: InputEvent) -> Result<(), Error> {
            InputSink::apply(&mut self.0, target, &event)
        }
    }
    let recorded = RecordedInput::default();
    let mut sink = Sink(recorded.clone());
    let long_line = "\\\"".repeat(MAX_TEXT_BYTES / 2);
    for text in [
        long_line.as_str(),
        "if (ready) {\r\n\tprint(\"中文 😀\");\n}",
    ] {
        apply_text(&mut sink, Bounds::default(), text).unwrap();
    }
    let want = [
        InputEvent::text(&long_line),
        InputEvent::text("if (ready) {"),
        InputEvent::key("Enter", true),
        InputEvent::key("Enter", false),
        InputEvent::key("Tab", true),
        InputEvent::key("Tab", false),
        InputEvent::text("print(\"中文 😀\");"),
        InputEvent::key("Enter", true),
        InputEvent::key("Enter", false),
        InputEvent::text("}"),
    ];
    assert_eq!(recorded.drain(), want);
    for text in ["", &"x".repeat(MAX_TEXT_BYTES + 1), "a\u{0}b", "a\u{7f}b"] {
        assert!(
            apply_text(&mut sink, Bounds::default(), text).is_err(),
            "invalid text accepted"
        );
    }
    assert!(
        recorded.events() == 0 && recorded.resets() == 2,
        "invalid text applied partial input or reset held keys"
    );
    // Invalid UTF-8 never decodes into an input message at all.
    assert!(
        serde_json::from_slice::<InputEvent>(b"{\"type\":\"text\",\"text\":\"\xff\"}").is_err()
    );
}
