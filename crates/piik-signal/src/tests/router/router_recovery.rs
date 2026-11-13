//! Route recovery, rebuild and retained resources.

use super::*;

#[test]
fn router_creates_nat_retries_through_real_prepare_messages() {
    for with_sfu in [false, true] {
        let h = RouterHarness::new(RouterOptions {
            capacity: 2,
            with_sfu,
            ..Default::default()
        });
        let created = h.create_room();
        let policy = RoutePolicy {
            peer_only: true,
            topology_optimization: false,
            nat_prediction: true,
        };
        let host = h.connect_host(&created, Some(policy), "", "");
        let viewer = h.connect_viewer(&created, "nat-retry");
        h.locked(|core| {
            do_complete(core, &host);
            do_complete(core, &viewer);
        });
        let mut connection_ids = HashSet::new();
        let mut stale_failure: Option<(i64, String)> = None;
        for current in 1..=4 {
            let prepared =
                h.wait_prepared_matching(&viewer.session_id, "connection attempt", |prepared| {
                    prepared.candidate.connection_attempt
                        == Some(ConnectionAttempt { current, total: 4 })
                });
            connection_ids.insert(prepared.candidate.connection_id.clone());
            h.locked(|core| {
                let host_prepare =
                    last_prepare(&sent(core, &host.session_id), "").expect("host prepare");
                assert_eq!(
                    host_prepare.candidate, prepared.candidate,
                    "the host must see the same candidate"
                );
                if let Some((revision, connection)) = &stale_failure {
                    do_failed(core, &viewer, *revision, "prepare", Some(connection));
                }
                let again = last_prepare(&sent(core, &viewer.session_id), "").expect("prepare");
                assert_eq!(
                    again.candidate.connection_id, prepared.candidate.connection_id,
                    "a stale failure must not disturb the current candidate"
                );
                if current == 4 {
                    do_ready(core, &viewer, prepared.revision);
                    assert_eq!(
                        do_must_edge(core, &created.room_id, &viewer.peer_id).connection_id,
                        prepared.candidate.connection_id
                    );
                } else {
                    let failure = (prepared.revision, prepared.candidate.connection_id.clone());
                    do_failed(core, &viewer, failure.0, "prepare", Some(&failure.1));
                    do_failed(core, &host, failure.0, "prepare", Some(&failure.1));
                    stale_failure = Some(failure);
                }
            });
        }
        assert_eq!(connection_ids.len(), 4, "distinct connection ids");
        for message in h.all_messages() {
            assert!(
                !matches!(message, ServerMessage::SfuConfig { .. }),
                "peer-only media must not receive an sfu-config"
            );
            assert!(
                !matches!(
                    message,
                    ServerMessage::RouteStatus {
                        status: RouteStatus::FailedRouteExhausted,
                        ..
                    }
                ),
                "NAT retries must not report failure"
            );
        }
    }
}

#[test]
fn router_notifies_presence_owner_after_committed_graph_changes() {
    let h = RouterHarness::new(sfu_options(1));
    let created = h.create_room();
    h.establish_sfu_room(&created);
    h.wait_for("the last routesChanged call", || {
        h.routes_changed().last() == Some(&created.room_id)
    });
}

#[test]
fn router_keeps_exact_transport_connected_direct_candidate_past_boundary() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "transport-connected");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let direct = h.wait_prepared_transport(&viewer.session_id, "direct");
    h.locked(|core| {
        core.handle_route_transport_connected(
            &viewer,
            direct.revision,
            &direct.candidate.connection_id,
        )
    });
    h.advance(150);
    assert_eq!(
        h.prepared_for(&viewer.session_id),
        Some(direct.clone()),
        "prepare after the boundary"
    );
    h.route_ready(&viewer, direct.revision);
    h.wait_for("the active direct edge", || {
        h.active_after(&viewer.session_id, direct.revision - 1)
            .is_some_and(|(_, assignment)| {
                matches!(assignment.upstream, MediaRouteUpstream::Peer { .. })
            })
    });
}

#[test]
fn router_accepts_exact_parent_progress_without_granting_readiness() {
    for relay_parent in [false, true] {
        let h = RouterHarness::new(timed_sfu(2, 300));
        let created = h.create_room();
        let room = created.room_id.clone();
        let host = h.connect_host(&created, None, "", "");
        h.locked(|core| {
            do_complete(core, &host);
        });
        let mut parents = vec![host.clone()];
        if relay_parent {
            for name in ["root-a", "root-b"] {
                let root = h.connect_viewer(&created, name);
                h.locked(|core| {
                    do_complete(core, &root);
                });
                let prepared = h.wait_prepared_transport(&root.session_id, "direct");
                h.route_ready(&root, prepared.revision);
                h.locked(|core| do_relay(core, &root, 2));
                parents.push(root);
            }
        }
        let viewer = h.connect_viewer(&created, "delayed-feedback");
        h.locked(|core| {
            do_complete(core, &viewer);
        });
        let direct = h.wait_prepared_transport(&viewer.session_id, "direct");
        h.locked(|core| {
            let operation = |core: &Core| core.controller(&room).and_then(|c| c.operation());
            let before = operation(core).expect("operation");
            let parent_id = before
                .current
                .as_ref()
                .expect("current")
                .tuple
                .parent_peer_id
                .clone();
            let parent = parents
                .iter()
                .find(|p| p.peer_id == parent_id)
                .expect("parent")
                .clone();
            assert_eq!(
                parent.role == piik_protocol::Role::Viewer,
                relay_parent,
                "unexpected preparing parent"
            );
            let connection = direct.candidate.connection_id.clone();
            let mut stale = parent.clone();
            stale.session_id = "retired-session".to_owned();
            core.handle_route_transport_connected(&stale, direct.revision, &connection);
            let mut foreign = parent.clone();
            foreign.peer_id = "unrelated-peer".to_owned();
            core.handle_route_transport_connected(&foreign, direct.revision, &connection);
            core.handle_route_transport_connected(&parent, direct.revision, "retired-connection");
            core.handle_route_transport_connected(&parent, direct.revision + 1, &connection);
            assert_eq!(
                operation(core).expect("operation").wake_at_ms,
                before.wake_at_ms,
                "unrelated progress changed candidate timing"
            );
            core.handle_route_transport_connected(&parent, direct.revision, &connection);
            let after = operation(core).expect("operation");
            assert!(
                after.deadline_at_ms == before.deadline_at_ms
                    && after.wake_at_ms == before.deadline_at_ms,
                "parent progress did not retain the original total deadline"
            );
            do_ready(core, &parent, direct.revision);
            assert!(
                operation(core).is_some(),
                "parent committed a route without Viewer readiness"
            );
        });
        h.advance(150);
        assert_eq!(
            h.prepared_for(&viewer.session_id),
            Some(direct.clone()),
            "connected candidate was replaced before delayed Viewer readiness"
        );
        h.route_ready(&viewer, direct.revision);
        h.wait_for("the active peer edge", || {
            h.active_after(&viewer.session_id, direct.revision - 1)
                .is_some_and(|(_, assignment)| {
                    matches!(assignment.upstream, MediaRouteUpstream::Peer { .. })
                })
        });
    }
}

#[test]
fn router_keeps_controller_diagnostic_label_until_departed_relay_is_pruned() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let parent = h.connect_viewer(&created, "diagnostic-label-parent");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &parent);
    });
    let parent_prepare = h.wait_prepared(&parent.session_id);
    let temporary_root = h.connect_viewer(&created, "diagnostic-label-temporary-root");
    h.locked(|core| {
        do_ready(core, &parent, parent_prepare.revision);
        do_relay(core, &parent, 1);
        do_complete(core, &temporary_root);
    });
    let temporary_prepare =
        h.wait_prepared_upstream(&temporary_root.session_id, &peer(&host.peer_id));
    let child = h.connect_viewer(&created, "diagnostic-label-child");
    h.locked(|core| {
        do_ready(core, &temporary_root, temporary_prepare.revision);
        do_complete(core, &child);
    });
    let child_prepare = h.wait_prepared_upstream(&child.session_id, &peer(&parent.peer_id));
    h.locked(|core| {
        do_ready(core, &child, child_prepare.revision);
        do_disconnect(core, &temporary_root);
        core.remove_viewer(&room, &temporary_root.peer_id);
    });
    h.wait_for("the temporary root to lose its edge", || {
        h.active_edge(&room, &temporary_root.peer_id).is_none()
    });
    h.locked(|core| {
        let label = core.debug_peer(&room, &parent.peer_id);
        do_disconnect(core, &parent);
        core.remove_viewer(&room, &parent.peer_id);
        assert_eq!(
            core.debug_peer(&room, &parent.peer_id),
            label,
            "label after departure"
        );
    });
    let recovery =
        h.wait_prepared_matching(&child.session_id, "a recovery onto the host", |prepared| {
            prepared.revision > child_prepare.revision
                && prepared.assignment.upstream == peer(&host.peer_id)
        });
    h.route_ready(&child, recovery.revision);
    h.wait_for("the departed relay to be pruned", || {
        h.locked(|core| core.debug_peer(&room, &parent.peer_id)) == "viewer-unknown"
    });
}

#[test]
fn router_resolves_exact_direct_and_peer_relayed_viewer_evidence_sources() {
    let h = RouterHarness::new(options(1));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let root = h.connect_viewer(&created, "evidence-root");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &root);
    });
    let root_prepare = h.wait_prepared(&root.session_id);
    h.locked(|core| {
        assert!(
            do_edge(core, &created.room_id, &root.peer_id).is_none(),
            "a prepared candidate is not an active edge"
        );
        do_ready(core, &root, root_prepare.revision);
    });
    let expected_root = ActiveViewerMediaEdge {
        revision: root_prepare.revision,
        connection_id: root_prepare.candidate.connection_id.clone(),
        upstream: peer(&host.peer_id),
    };
    h.wait_for("the root active edge", || {
        h.active_edge(&created.room_id, &root.peer_id) == Some(expected_root.clone())
    });
    let child = h.connect_viewer(&created, "evidence-child");
    h.locked(|core| {
        do_relay(core, &root, 1);
        do_complete(core, &child);
    });
    let child_prepare = h.wait_prepared(&child.session_id);
    assert_eq!(child_prepare.transport(), "direct");
    h.route_ready(&child, child_prepare.revision);
    let expected_child = ActiveViewerMediaEdge {
        revision: child_prepare.revision,
        connection_id: child_prepare.candidate.connection_id.clone(),
        upstream: peer(&root.peer_id),
    };
    h.wait_for("the child active edge", || {
        h.active_edge(&created.room_id, &child.peer_id) == Some(expected_child.clone())
    });
}

#[test]
fn retired_pump_preserves_replacement_share_resource_waiter() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 1,
        with_sfu: true,
        sfu_ingress_capacity: 1,
        ..Default::default()
    });
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "replacement-waiter");
    let occupied = fence("9001", "occupied-share", "occupied-publication");
    h.locked(|core| {
        core.router.sfu_only = true;
        assert!(
            admission(core).reserve_publication(&occupied),
            "reserve occupied publication"
        );
        core.connect_participant(&host);
        core.connect_participant(&viewer);
        core.room_rt_mut(&room).expect("room").pumping = true;
        let identity = core.pump_identity(&room).expect("identity");
        let mut revision = core.controller_revision(&room);
        // Suspend the same queued remainder as request_pump so the new share
        // takes the authority lock before the old driver resumes.
        let pending = core.pump_room(&room, &mut revision, None, true);
        assert!(
            matches!(
                pending.as_ref().map(|p| &p.result),
                Some(crate::router::pump::PrepareResult::Denied(_))
            ),
            "expected queued denied candidate"
        );
        core.stop_room(&room);
        core.connect_participant(&host);
        let mut replacement_revision = core.controller_revision(&room);
        core.pump_room(&room, &mut replacement_revision, None, false);
        assert!(
            core.router.resource_waiters.has(&room),
            "replacement did not wait"
        );
        core.router.inflight += 1;
        let locker = core.locker.clone();
        let room = room.clone();
        std::thread::spawn(move || {
            locker.with(move |core| core.drive_pump(&room, identity, revision, pending, false));
        });
    });
    h.settle();
    h.locked(|core| {
        assert!(
            core.router.resource_waiters.has(&room),
            "retired pump erased the replacement share's SFU waiter"
        );
        release_fence(core, &occupied);
        core.wake_resource_waiters();
    });
    h.wait_prepared_transport(&viewer.session_id, "sfu");
}
