//! Router signaling authorization and committed-edge identity.

use super::*;

#[test]
fn router_publishes_connection_identity_only_for_explicit_commit() {
    for committed in [false, true] {
        let h = RouterHarness::new(options(2));
        let convergence = prepare_quality_convergence(&h, false);
        let got = h.locked(|core| {
            if committed {
                do_ready_approved(
                    core,
                    &convergence.first,
                    convergence.quality_prepare.revision,
                );
            } else {
                do_ready(
                    core,
                    &convergence.first,
                    convergence.quality_prepare.revision,
                );
            }
            do_edge(
                core,
                &convergence.created.room_id,
                &convergence.first.peer_id,
            )
            .map(|edge| edge.connection_id)
            .unwrap_or_default()
        });
        let want = if committed {
            convergence.quality_prepare.candidate.connection_id.clone()
        } else {
            convergence.first_edge.connection_id.clone()
        };
        assert_eq!(got, want, "committed={committed}");
    }
}

#[test]
fn router_logs_only_aggregate_candidate_origin_for_route_signals() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 2,
        debug: true,
        ..Default::default()
    });
    h.locked(|core| {
        core.debug_peer_signal(
            &PeerSignalInput {
                room_id: "1234".to_owned(),
                source_peer_id: "source_12345678".to_owned(),
                target_peer_id: "target_12345678".to_owned(),
                signal_kind: "candidate",
                ..Default::default()
            },
            Some(CandidateSignalOrigin::Predicted),
            SignalAuthorization::Probe,
        );
    });
    assert!(h.debug.called_with(
        "1234",
        "peer-signal",
        &[
            ("candidateOrigin", json!("predicted")),
            ("signalKind", json!("candidate")),
            ("authorization", json!("probe")),
        ]
    ));
    for (_, _, details) in h.debug.records.lock().expect("records").iter() {
        for (_, value) in details {
            if let Some(text) = value.as_str() {
                assert!(
                    !text.contains("source_12345678") && !text.contains("target_12345678"),
                    "raw peer id leaked into debug output"
                );
            }
        }
    }
}

/// A real committed edge (its crate-private fields cannot be built here).
#[test]
fn router_maps_exact_host_active_failure_to_committed_direct_edge() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "host-edge-failure");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.locked(|core| {
        do_ready(core, &viewer, prepared.revision);
        let active = do_must_edge(core, &created.room_id, &viewer.peer_id);
        do_failed(
            core,
            &host,
            active.revision,
            "active",
            Some(&active.connection_id),
        );
    });
    h.wait_failed(&viewer.session_id);
}

#[test]
fn router_rebinds_replacement_host_and_clears_offline_viewer_media_ownership() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let previous_host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "host-rebind-offline-viewer");
    h.locked(|core| {
        do_complete(core, &previous_host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.locked(|core| {
        do_ready(core, &viewer, prepared.revision);
        do_relay(core, &viewer, 1);
    });
    h.wait_for("the viewer connection identity", || {
        h.active_edge(&created.room_id, &viewer.peer_id).is_some()
    });
    h.locked(|core| {
        do_disconnect(core, &viewer);
        do_disconnect(core, &previous_host);
    });
    let resumed_host = h.connect_host(
        &created,
        None,
        "replacement-host-client",
        "replacement-host-session",
    );
    assert_ne!(
        resumed_host.peer_id, previous_host.peer_id,
        "a replacement client must receive a new host peer id"
    );
    h.locked(|core| {
        do_complete(core, &resumed_host);
        assert!(
            do_edge(core, &created.room_id, &viewer.peer_id).is_none(),
            "the offline viewer must lose its media ownership"
        );
    });
    let reconnected = h.connect_viewer(&created, "host-rebind-offline-viewer");
    assert_eq!(
        reconnected.peer_id, viewer.peer_id,
        "the same client must keep its viewer peer id"
    );
    h.locked(|core| {
        core.connect_participant(&reconnected);
        assert!(
            core.route_diagnostic_snapshot(&created.room_id)
                .children
                .iter()
                .any(|child| child.effective_capacity == 1),
            "the reconnected viewer must keep its advertised capacity"
        );
    });
}

#[test]
fn router_accepts_active_failure_for_direct_connection_adopted_during_rebuild() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "rebuilt-edge-failure");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.locked(|core| {
        do_ready(core, &viewer, prepared.revision);
        let active = do_must_edge(core, &room, &viewer.peer_id);
        let rebuilt = "rebuilt_connection_12345678";
        let authorization =
            core.peer_signal_authorization(&offer_input(&room, &host, &viewer, rebuilt));
        assert_eq!(
            authorization,
            SignalAuthorization::Allowed,
            "adoption authorization"
        );
        let adopted = do_must_edge(core, &room, &viewer.peer_id);
        assert_eq!(
            (adopted.revision, adopted.connection_id.as_str()),
            (active.revision, rebuilt)
        );
        do_failed(
            core,
            &viewer,
            active.revision,
            "active",
            Some(&active.connection_id),
        );
        let edge = core
            .snapshot(&room)
            .and_then(|snapshot| snapshot.upstream_by_viewer.get(&viewer.peer_id).cloned())
            .expect("edge");
        assert!(
            edge.usable && edge.connection_id == rebuilt,
            "retired-connection failure affected its replacement"
        );
        do_failed(core, &viewer, active.revision, "active", Some(rebuilt));
    });
    h.wait_failed(&viewer.session_id);
}

#[test]
fn router_keeps_committed_edge_signaling_authorized_during_another_peer_candidate() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    h.complete(&host);
    let roots = [
        h.connect_viewer(&created, "stable-root-a"),
        h.connect_viewer(&created, "stable-root-b"),
    ];
    // The ready + capacity of one root share the segment with the next root's complete.
    let mut pending: Option<(RouteParticipant, i64)> = None;
    for root in &roots {
        h.locked(|core| {
            if let Some((previous, revision)) = &pending {
                do_ready(core, previous, *revision);
                do_relay(core, previous, 1);
            }
            do_complete(core, root);
        });
        let prepared = h.wait_prepared(&root.session_id);
        pending = Some((root.clone(), prepared.revision));
    }
    let candidate_child = h.connect_viewer(&created, "candidate-child");
    h.locked(|core| {
        let (previous, revision) = pending.as_ref().expect("pending root");
        do_ready(core, previous, *revision);
        do_relay(core, previous, 1);
        do_complete(core, &candidate_child);
    });
    let prepared = h.wait_prepared_matching(
        &candidate_child.session_id,
        "a peer candidate",
        |prepared| {
            matches!(
                prepared.assignment.upstream,
                MediaRouteUpstream::Peer { .. }
            )
        },
    );
    let parent_id = match &prepared.assignment.upstream {
        MediaRouteUpstream::Peer { peer_id } => peer_id.clone(),
        _ => unreachable!(),
    };
    let (candidate_parent, unrelated_root) = if roots[0].peer_id == parent_id {
        (&roots[0], &roots[1])
    } else {
        (&roots[1], &roots[0])
    };
    h.locked(|core| {
        let active_parent_edge = do_must_edge(core, &room, &candidate_parent.peer_id);
        let committed = core.peer_signal_authorization(&offer_input(
            &room,
            &host,
            candidate_parent,
            &active_parent_edge.connection_id,
        ));
        assert_eq!(
            committed,
            SignalAuthorization::Allowed,
            "committed edge authorization"
        );
        let candidate = core.peer_signal_authorization(&offer_input(
            &room,
            candidate_parent,
            &candidate_child,
            &prepared.candidate.connection_id,
        ));
        assert_eq!(
            candidate,
            SignalAuthorization::Probe,
            "candidate authorization"
        );
        let unrelated = core.peer_signal_authorization(&offer_input(
            &room,
            unrelated_root,
            &candidate_child,
            &prepared.candidate.connection_id,
        ));
        assert_eq!(
            unrelated,
            SignalAuthorization::Unassigned,
            "unrelated authorization"
        );
    });
}
