//! Bounded-route exhaustion and diagnostics.

use super::*;

fn no_candidate_exhaustion() {
    let h = RouterHarness::new(options(1));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let first = h.connect_viewer(&created, "only-slot");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first);
    });
    let first_prepare = h.wait_prepared(&first.session_id);
    let blocked = h.connect_viewer(&created, "no-candidate");
    h.locked(|core| {
        do_ready(core, &first, first_prepare.revision);
        do_complete(core, &blocked);
    });
    h.wait_failed(&blocked.session_id);
}

#[test]
fn router_reports_bounded_route_exhaustion_without_raw_error_text() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "route-exhausted");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    h.wait_prepared_transport(&viewer.session_id, "direct");
    h.advance(150);
    h.wait_prepared_transport(&viewer.session_id, "sfu");
    h.advance(150);
    h.wait_failed(&viewer.session_id);
    for message in h.messages(&viewer.session_id) {
        if let ServerMessage::Error { message, .. } = message {
            assert!(
                !message.contains("media route"),
                "raw error text must not reach the viewer"
            );
        }
    }
}

#[test]
fn router_reports_typed_exhaustion_when_exact_candidate_fails() {
    let h = RouterHarness::new(options(1));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "candidate-failed");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.route_failed(
        &viewer,
        prepared.revision,
        "prepare",
        &prepared.candidate.connection_id,
    );
    h.wait_failed(&viewer.session_id);
}

#[test]
fn router_reports_typed_exhaustion_when_reconciliation_has_no_candidate() {
    no_candidate_exhaustion();
}

#[test]
fn router_emits_typed_exhaustion_when_reconciliation_has_no_candidate() {
    no_candidate_exhaustion();
}

#[test]
fn router_orders_exact_prepare_commits_only_child_proof_and_rolls_back_above_p() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let first = h.connect_viewer(&created, "first");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first);
    });
    let first_prepare = h.wait_prepared(&first.session_id);
    assert!(
        first_prepare.candidate.child_peer_id == first.peer_id
            && first_prepare.transport() == "direct"
    );
    h.locked(|core| {
        let host_prepare = last_prepare(&sent(core, &host.session_id), "").expect("host prepare");
        assert_eq!(host_prepare.candidate, first_prepare.candidate, "the host must receive the same candidate");
        do_ready(core, &host, first_prepare.revision);
        assert!(
            !sent(core, &first.session_id).iter().any(|message| matches!(
                message,
                ServerMessage::RouteUpdateActive { revision, .. } if *revision > first_prepare.revision
            )),
            "host proof must not commit the candidate"
        );
        do_ready(core, &first, first_prepare.revision);
    });
    h.wait_for("the active route at the prepare revision", || {
        h.messages(&first.session_id).iter().any(|message| matches!(
            message,
            ServerMessage::RouteUpdateActive { revision, .. } if *revision == first_prepare.revision
        ))
    });
    let second = h.connect_viewer(&created, "second");
    h.complete(&second);
    let second_prepare = h.wait_prepared(&second.session_id);
    h.route_failed(
        &second,
        second_prepare.revision,
        "prepare",
        &second_prepare.candidate.connection_id,
    );
    h.wait_for("the rollback broadcast", || {
        h.active_after(&second.session_id, second_prepare.revision)
            .is_some()
    });
}

#[test]
fn router_emits_typed_exhaustion_and_clears_diagnostics_on_departure_and_room_deletion() {
    let h = RouterHarness::new(options(1));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "diagnostic");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.locked(|core| {
        assert_eq!(
            core.route_diagnostic_snapshot(&room).children.len(),
            1,
            "one diagnostic child expected"
        );
        do_failed(
            core,
            &viewer,
            prepared.revision,
            "prepare",
            Some(&prepared.candidate.connection_id),
        );
    });
    h.wait_failed(&viewer.session_id);
    let replacement = h.connect_viewer(&created, "diagnostic-replacement");
    h.locked(|core| {
        let empty = |core: &Core| {
            let snapshot = core.route_diagnostic_snapshot(&room);
            snapshot.children.is_empty() && snapshot.operation.is_none()
        };
        core.remove_viewer(&room, &viewer.peer_id);
        assert!(empty(core), "departure must clear the diagnostics");
        do_complete(core, &replacement);
        assert_eq!(
            core.route_diagnostic_snapshot(&room).children.len(),
            1,
            "the replacement must appear in the diagnostics"
        );
        core.stop_room(&room);
        assert!(empty(core), "stop_room must clear the diagnostics");
        do_complete(core, &host);
        assert!(
            !core.route_diagnostic_snapshot(&room).children.is_empty(),
            "a restarted share must rebuild the diagnostics"
        );
        core.delete_room(&room);
        assert!(empty(core), "delete_room must clear the diagnostics");
    });
}

#[test]
fn router_preserves_connected_viewer_capacity_across_sharing_generations() {
    let h = RouterHarness::new(options(1));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let root = h.connect_viewer(&created, "retained-capacity-root");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &root);
    });
    let initial_root = h.wait_prepared(&root.session_id);
    h.locked(|core| {
        do_ready(core, &root, initial_root.revision);
        do_relay(core, &root, 1);
        core.stop_room(&created.room_id);
        clear_sent(core, &root.session_id);
        do_complete(core, &host);
    });
    let restarted_root = h.wait_prepared(&root.session_id);
    let child = h.connect_viewer(&created, "retained-capacity-child");
    h.locked(|core| {
        do_ready(core, &root, restarted_root.revision);
        do_complete(core, &child);
    });
    let child_prepare = h.wait_prepared(&child.session_id);
    assert_eq!(
        child_prepare.assignment.upstream,
        peer(&root.peer_id),
        "child upstream must be the retained root"
    );
}

// TS declared this scenario twice under two names; both are kept.
#[test]
fn router_debug_flag_parses_like_node_debug() {
    for (value, expected) in [
        ("", false),
        ("route", true),
        ("http,route", true),
        (" route ", true),
        ("router", false),
    ] {
        assert_eq!(
            route_debug_flag(value),
            expected,
            "route_debug_flag({value:?})"
        );
        assert_eq!(
            route_debug_enabled(Some(value)),
            expected,
            "route_debug_enabled with {value:?}"
        );
    }
    assert!(!route_debug_enabled(None));
    tracing::subscriber::with_default(super::super::DebugSubscriber, || {
        assert!(
            route_debug_enabled(None),
            "the active diagnostic logger did not enable route events"
        );
    });
}
