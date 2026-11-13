//! Input failure and stage diagnostics.

use super::*;

#[test]
fn input_failure_diagnostics_preserve_execution_authority() {
    for stage in ["target", "apply"] {
        let logs = Arc::new(Mutex::new(Vec::<u8>::new()));
        let writer = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .json()
            .flatten_event(true)
            .with_max_level(tracing::Level::DEBUG)
            .with_writer(move || LogWriter(writer.clone()))
            .finish();
        let fixture = Fixture::new();
        let (_session, keys) = co_play_session(&fixture, 1);
        let c = &keys[0];
        fixture.send(c, InputEvent::of("request"));
        let grant = fixture.grant(c);
        assert!(!grant.is_empty(), "control was not granted");
        fixture.send(c, InputEvent::key("KeyA", true));
        let (edge, epoch, intent) = fixture.conn(c, |c| {
            (c.edge_id.clone(), c.epoch.clone(), c.intent.clone())
        });
        if stage == "target" {
            fixture
                .sink
                .with(|r| r.target_error = Some(Error::input("platform input failed")));
        } else {
            fixture.sink.set_apply_error(Some("platform input failed"));
        }
        tracing::subscriber::with_default(subscriber, || {
            fixture.send(c, InputEvent::text("private committed text"));
        });
        if stage == "target" {
            assert!(
                fixture.owner().is_none()
                    && fixture.grant(c).is_empty()
                    && fixture.conn(c, |c| c.keys.is_empty()),
                "invalid target retained authority"
            );
        } else {
            assert!(
                fixture.owner().is_some()
                    && fixture.grant(c) == grant
                    && fixture.conn(c, |c| c.input_failed),
                "execution failure retired authorization or hid failure"
            );
        }
        let text = String::from_utf8(lock(&logs).clone()).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(text.trim()).expect("expected one failure record");
        assert_eq!(record["event"], format!("{stage}-failed"));
        assert_eq!(record["connectionId"], piik_diagnostics::id(&edge));
        assert_eq!(record["inputGeneration"], piik_diagnostics::id(&epoch));
        if stage == "apply" {
            assert!(record["operation"] == "text" && record["sourceKind"] == "display");
            assert!(
                record["targetReady"].is_boolean(),
                "failure lost its target observation"
            );
        }
        for private in [
            grant.as_str(),
            &intent,
            &edge,
            &epoch,
            "KeyA",
            "private committed text",
        ] {
            assert!(
                !text.contains(private),
                "failure diagnostic recorded private input or authority"
            );
        }
    }
}

struct LogWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for LogWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        lock(&self.0).extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn input_diagnostics_distinguish_stages_without_input_values() {
    let fixture = Fixture::with_gamepads(RecordedPads::default());
    let (session, keys) = co_play_session(&fixture, 1);
    let c = &keys[0];
    let shared = fixture.manager.shared().clone();
    let edge = fixture.conn(c, |c| c.edge_id.clone());
    let packet = |plain: &[u8]| {
        shared.receive_packet(session.id(), &edge, encrypted(&fixture, c, plain), None)
    };
    let input = |mut event: InputEvent| {
        if event.grant.is_empty() {
            event.grant = fixture.grant(c);
        }
        packet(&serde_json::to_vec(&event).unwrap());
    };
    let before = fixture.manager.diagnostics();
    let epoch = fixture.conn(c, |c| c.epoch.clone());
    shared.receive_packet(
        session.id(),
        &edge,
        Packet {
            id: c.1.clone(),
            epoch,
            data: "not-base64".into(),
            ..Packet::default()
        },
        None,
    );
    packet(b"not-json");
    input(InputEvent {
        intent: "f".repeat(32),
        ..InputEvent::of("request")
    });
    let mv = |x, y| InputEvent {
        x,
        y,
        ..InputEvent::of("move")
    };
    input(InputEvent {
        grant: "wrong-grant".into(),
        ..mv(0.37, 0.29)
    });
    fixture.sink.set_focused(false);
    input(mv(0.37, 0.29));
    let suspended = fixture.manager.diagnostics();
    assert!(
        suspended.active_grants == 0
            && suspended.suspended_grants == 1
            && fixture.sink.events() == 0
            && suspended.counters.mouse
                == crate::DiagnosticFamilyCounters {
                    parsed: 2,
                    authorized: 1,
                    ..Default::default()
                },
        "quiet target suspension lost its stage: {suspended:?}"
    );
    fixture.sink.set_focused(true);
    input(mv(0.37, 0.29));
    fixture.sink.set_apply_error(Some("test sink rejection"));
    input(InputEvent::button(0, true));
    fixture.sink.set_apply_error(None);
    input(InputEvent::button(0, true));
    input(InputEvent::button(0, true)); // Held deduplication is not another API acceptance.
    input(InputEvent::key("KeyZ", true));
    let accepted = fixture.manager.diagnostics();
    let delta = |after: u64, before: u64| after - before;
    assert_eq!(
        delta(
            accepted.counters.received_packets,
            before.counters.received_packets
        ),
        10
    );
    assert_eq!(
        delta(
            accepted.counters.decrypted_packets,
            before.counters.decrypted_packets
        ),
        9
    );
    assert_eq!(
        delta(
            accepted.counters.parsed_events,
            before.counters.parsed_events
        ),
        8
    );
    assert_eq!(
        accepted.counters.mouse,
        crate::DiagnosticFamilyCounters {
            parsed: 6,
            authorized: 5,
            target_ready: 4,
            api_attempts: 3,
            api_accepted: 2,
            api_rejected: 1
        }
    );
    assert_eq!(
        accepted.counters.keyboard,
        crate::DiagnosticFamilyCounters {
            parsed: 1,
            authorized: 1,
            target_ready: 1,
            api_attempts: 1,
            api_accepted: 1,
            api_rejected: 0
        }
    );
    assert!(accepted.active_grants == 1 && accepted.suspended_grants == 0);
    fixture.sink.set_apply_error(Some("test release rejection"));
    input(InputEvent::key("KeyZ", false));
    input(InputEvent::button(0, false));
    let pending = fixture.manager.diagnostics();
    assert!(
        pending.pending_key_releases == 1 && pending.pending_button_releases == 1,
        "release counts missing: {pending:?}"
    );
    // Export the bounded report context. The snapshot must not retry the
    // rejected cleanup or expose held input, credentials or connection IDs.
    let (events, grant) = (fixture.sink.events(), fixture.grant(c));
    let directory = tempfile::tempdir().unwrap();
    let recorder = piik_diagnostics::Recorder::open(directory.path(), "client", "test").unwrap();
    recorder.context("remoteInput", fixture.manager.diagnostics());
    let path = recorder.export().unwrap();
    let mut archive = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut content = String::new();
    std::io::Read::read_to_string(&mut archive.by_name("context.json").unwrap(), &mut content)
        .unwrap();
    let context: serde_json::Value = serde_json::from_str(&content).unwrap();
    let exported: crate::DiagnosticSnapshot =
        serde_json::from_value(context["remoteInput"].clone()).unwrap();
    assert_eq!(
        exported, pending,
        "ZIP omitted the current input counts: {content}"
    );
    let (secret, peer_id) = fixture.conn(c, |c| (c.secret.clone(), c.peer_id.clone()));
    for private in [
        grant.as_str(),
        &secret,
        &c.1,
        &peer_id,
        "KeyZ",
        "\"x\":",
        "\"y\":",
        "\"text\":",
        "\"code\":",
    ] {
        assert!(
            !content.contains(private),
            "diagnostic context contains input values or identity: {private:?}"
        );
    }
    recorder.close().unwrap();
    assert!(
        fixture.manager.diagnostics() == pending
            && fixture.sink.events() == events
            && fixture.grant(c) == grant,
        "observing/exporting diagnostics changed input or cleanup state"
    );
    fixture.sink.set_apply_error(None);
    fixture.retry_releases().unwrap();
}
