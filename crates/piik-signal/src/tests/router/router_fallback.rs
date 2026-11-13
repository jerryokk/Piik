//! SFU fallback preparation, bootstrap and release.

use super::*;

#[test]
fn router_keeps_peer_only_shares_out_of_configured_sfu_fallback() {
    let h = RouterHarness::new(sfu_options(1));
    let created = h.create_room();
    let policy = RoutePolicy {
        peer_only: true,
        topology_optimization: false,
        nat_prediction: false,
    };
    let host = h.connect_host(&created, Some(policy), "", "");
    let first = h.connect_viewer(&created, "peer-only-first");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first);
    });
    let direct = h.wait_prepared_transport(&first.session_id, "direct");
    let blocked = h.connect_viewer(&created, "peer-only-blocked");
    h.locked(|core| {
        do_ready(core, &first, direct.revision);
        do_complete(core, &blocked);
    });
    h.wait_failed(&blocked.session_id);
    assert_eq!(
        sfu_config_count(&h.all_messages()),
        0,
        "peer-only shares must never receive an sfu-config"
    );
}

#[test]
fn router_silently_keeps_working_media_when_quality_only_sfu_admission_is_denied() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 2,
        with_sfu: true,
        sfu_ingress_capacity: 1,
        debug: true,
        ..Default::default()
    });
    let occupied = fence(
        "9001",
        "occupied_share_12345678",
        "occupied_publication_12345678",
    );
    let created = h.create_room();
    let host = h.connect_host(&created, Some(QUALITY_POLICY), "", "");
    h.locked(|core| {
        assert!(
            admission(core).reserve_publication(&occupied),
            "the occupying publication must be reserved"
        );
        do_complete(core, &host);
    });
    let viewers = [
        h.connect_viewer(&created, "quality-sfu-denied-first"),
        h.connect_viewer(&created, "quality-sfu-denied-second"),
    ];
    let mut active = Vec::new();
    for viewer in &viewers {
        h.complete(viewer);
        let prepared = h.wait_prepared(&viewer.session_id);
        h.route_ready(viewer, prepared.revision);
        active.push(h.must_active_edge(&created.room_id, &viewer.peer_id));
    }
    let degrade = |viewer: &RouteParticipant| {
        h.locked(|core| {
            let edge = do_must_edge(core, &created.room_id, &viewer.peer_id);
            for (index, state) in ["healthy", "degraded", "degraded", "degraded"]
                .iter()
                .enumerate()
            {
                let timestamp = 100.0 + index as f64;
                assert!(
                    observe(
                        core,
                        &host,
                        sender_evidence(
                            &viewer.peer_id,
                            &edge.connection_id,
                            "viewer-rtp",
                            "track",
                            timestamp,
                            edge.revision,
                            state
                        )
                    ),
                    "{state} evidence must be accepted"
                );
            }
        });
    };
    // SFU relief requires every Host root to be persistently degraded.
    degrade(&viewers[1]);
    let regeneration = h.wait_prepared_above(&viewers[1].session_id, active[1].revision);
    h.route_failed(
        &viewers[1],
        regeneration.revision,
        "prepare",
        &regeneration.candidate.connection_id,
    );
    h.settle();
    degrade(&viewers[0]);
    h.wait_for("the quality-only SFU admission rejection", || {
        h.debug.called_with(
            &created.room_id,
            "candidate-preparation-denied",
            &[("rejectionBucket", json!("sfu-admission"))],
        )
    });
    h.settle();
    for (index, viewer) in viewers.iter().enumerate() {
        assert!(
            !h.messages(&viewer.session_id)
                .iter()
                .any(|message| matches!(message, ServerMessage::RouteStatus { .. })),
            "a denied quality-only candidate must stay silent"
        );
        let edge = h.must_active_edge(&created.room_id, &viewer.peer_id);
        assert_eq!(
            edge.connection_id, active[index].connection_id,
            "working media changed"
        );
        assert_eq!(edge.upstream, peer(&host.peer_id), "working media changed");
    }
    h.locked(|core| release_fence(core, &occupied));
}

#[test]
fn router_wakes_at_derived_direct_boundary_and_prepares_sfu_automatically() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "staged-deadline");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    h.wait_prepared_transport(&viewer.session_id, "direct");
    h.advance(150);
    h.wait_prepared_transport(&viewer.session_id, "sfu");
    assert!(
        !h.has_failed_status(&viewer.session_id),
        "the direct boundary must not report failure"
    );
}

#[test]
fn router_relay_preparation_failure_advances_without_waiting_for_deadline() {
    let h = RouterHarness::new(timed_sfu(2, 0));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let relay = h.connect_viewer(&created, "relay");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &relay);
    });
    h.settle();
    let root = h
        .prepared_for(&relay.session_id)
        .expect("relay must prepare its Host upstream");
    h.locked(|core| {
        do_ready(core, &relay, root.revision);
        do_relay(core, &relay, 2);
    });
    let child = h.connect_viewer(&created, "child");
    h.complete(&child);
    h.settle();
    let direct = h.prepared_for(&child.session_id).expect("child prepare");
    assert_eq!(
        direct.assignment.upstream,
        peer(&host.peer_id),
        "child must try the Host first"
    );
    h.route_failed(
        &host,
        direct.revision,
        "prepare",
        &direct.candidate.connection_id,
    );
    h.settle();
    let prepared = h.prepared_for(&child.session_id).expect("child prepare");
    assert_eq!(
        prepared.assignment.upstream,
        peer(&relay.peer_id),
        "child must next try the relay"
    );
    h.route_failed(
        &relay,
        prepared.revision,
        "prepare",
        &prepared.candidate.connection_id,
    );
    h.settle();
    let fallback = h.prepared_for(&child.session_id).expect("child prepare");
    assert!(
        fallback.transport() == "sfu" && fallback.revision > prepared.revision,
        "relay rejection must prepare SFU immediately"
    );
    h.route_failed(
        &relay,
        prepared.revision,
        "prepare",
        &prepared.candidate.connection_id,
    );
    h.settle();
    let current = h.prepared_for(&child.session_id).expect("child prepare");
    assert_eq!(
        current.candidate.connection_id, fallback.candidate.connection_id,
        "duplicate relay failure disturbed the next candidate"
    );
    assert_eq!(
        h.clock().now(),
        0,
        "preparation failure must advance without a timer wake"
    );
    assert_eq!(
        h.must_active_edge(&created.room_id, &relay.peer_id)
            .connection_id,
        root.candidate.connection_id
    );
}

#[test]
fn router_uses_direct_boundary_to_bootstrap_sfu_when_every_host_slot_is_full() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let first_root = h.connect_viewer(&created, "first-root");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first_root);
    });
    let first_prepare = h.wait_prepared_transport(&first_root.session_id, "direct");
    let second_root = h.connect_viewer(&created, "second-root");
    h.locked(|core| {
        do_ready(core, &first_root, first_prepare.revision);
        do_relay(core, &first_root, 2);
        do_complete(core, &second_root);
    });
    let second_prepare = h.wait_prepared_transport(&second_root.session_id, "direct");
    let waiting = h.connect_viewer(&created, "waiting");
    h.locked(|core| {
        do_ready(core, &second_root, second_prepare.revision);
        do_complete(core, &waiting);
    });
    h.wait_prepared_transport(&waiting.session_id, "direct");
    h.advance(150);
    let second_bootstrap = h.wait_prepared_transport(&second_root.session_id, "sfu");
    h.route_failed(
        &second_root,
        second_bootstrap.revision,
        "prepare",
        &second_bootstrap.candidate.connection_id,
    );
    let first_bootstrap = h.wait_prepared_transport(&first_root.session_id, "sfu");
    assert!(
        !h.has_failed_status(&second_root.session_id),
        "a failed carrier bootstrap must not fail the carrier"
    );
    h.route_failed(
        &first_root,
        first_bootstrap.revision,
        "prepare",
        &first_bootstrap.candidate.connection_id,
    );
    h.wait_failed(&waiting.session_id);
    for root in [&first_root, &second_root] {
        assert!(
            !h.has_failed_status(&root.session_id),
            "roots must not be told about the waiting viewer's exhaustion"
        );
    }
    assert!(
        h.has_failed_status(&waiting.session_id),
        "the waiting viewer must be told"
    );
}

#[test]
fn router_does_not_recreate_sfu_after_exhausted_reuse_and_carrier_teardown() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let room = created.room_id.clone();
    let host = h.connect_host(&created, None, "", "");
    let first_root = h.connect_viewer(&created, "reuse-loop-first-root");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first_root);
    });
    let first_direct = h.wait_prepared_transport(&first_root.session_id, "direct");
    let second_root = h.connect_viewer(&created, "reuse-loop-second-root");
    h.locked(|core| {
        do_ready(core, &first_root, first_direct.revision);
        do_relay(core, &first_root, 1);
        do_complete(core, &second_root);
    });
    let second_direct = h.wait_prepared_transport(&second_root.session_id, "direct");
    let departed_parent = h.connect_viewer(&created, "reuse-loop-departed-parent");
    h.locked(|core| {
        do_ready(core, &second_root, second_direct.revision);
        do_complete(core, &departed_parent);
    });
    let parent_direct =
        h.wait_prepared_upstream(&departed_parent.session_id, &peer(&first_root.peer_id));
    let demand = h.connect_viewer(&created, "reuse-loop-demand");
    h.locked(|core| {
        do_ready(core, &departed_parent, parent_direct.revision);
        do_relay(core, &departed_parent, 1);
        do_complete(core, &demand);
    });
    let demand_direct =
        h.wait_prepared_upstream(&demand.session_id, &peer(&departed_parent.peer_id));
    let departure_at = h.locked(|core| {
        do_ready(core, &demand, demand_direct.revision);
        let at = core.now();
        do_disconnect(core, &departed_parent);
        core.remove_viewer(&room, &departed_parent.peer_id);
        do_relay(core, &second_root, 1);
        at
    });
    h.wait_for("a direct recovery for the demand", || {
        h.own_prepare(&demand).is_some_and(|recovery| {
            recovery.revision > demand_direct.revision
                && recovery.candidate.child_peer_id == demand.peer_id
                && recovery.transport() == "direct"
        })
    });
    h.advance((departure_at + 150 - h.clock().now()).max(0));
    let mut carrier = None;
    h.wait_for("a carrier bootstrap", || {
        carrier = [&first_root, &second_root]
            .into_iter()
            .find(|viewer| {
                h.own_prepare(viewer)
                    .is_some_and(|prepared| prepared.transport() == "sfu")
            })
            .cloned();
        carrier.is_some()
    });
    let carrier = carrier.expect("carrier");
    let bootstrap = h.own_prepare(&carrier).expect("bootstrap");
    h.route_ready(&carrier, bootstrap.revision);
    h.wait_prepared_transport(&demand.session_id, "sfu");
    assert_eq!(h.usage().ingress, 1, "expected one admitted publication");
    let configs_after_bootstrap = sfu_config_count(&h.all_messages());
    assert!(
        configs_after_bootstrap >= 3,
        "sfu-config count after bootstrap = {configs_after_bootstrap}"
    );
    h.advance((departure_at + 300 - h.clock().now()).max(0));
    h.wait_for("the carrier direct convergence", || {
        h.own_prepare(&carrier)
            .is_some_and(|prepared| prepared.transport() == "direct")
    });
    assert_eq!(
        h.diagnostic(&room)
            .operation
            .map(|operation| operation.reason),
        Some(DemandReason::DirectConvergence)
    );
    let carrier_direct = h.own_prepare(&carrier).expect("carrier direct");
    h.route_ready(&carrier, carrier_direct.revision);
    h.wait_for("the carrier back on the host", || {
        h.active_edge(&room, &carrier.peer_id)
            .is_some_and(|edge| edge.upstream == peer(&host.peer_id))
    });
    h.wait_for("the publication drain", || h.media().deleted().len() == 1);
    h.advance(300);
    assert_eq!(
        h.usage().ingress,
        0,
        "retired publication remained admitted"
    );
    assert_eq!(sfu_config_count(&h.all_messages()), configs_after_bootstrap);
    assert_eq!(h.usage(), Usage::default());
}

#[test]
fn router_reports_late_final_sfu_bootstrap_ready_only_to_waiting_demand() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let first_root = h.connect_viewer(&created, "late-ready-first-root");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first_root);
    });
    let first_prepare = h.wait_prepared_transport(&first_root.session_id, "direct");
    let second_root = h.connect_viewer(&created, "late-ready-second-root");
    h.locked(|core| {
        do_ready(core, &first_root, first_prepare.revision);
        do_relay(core, &first_root, 2);
        do_complete(core, &second_root);
    });
    let second_prepare = h.wait_prepared_transport(&second_root.session_id, "direct");
    let waiting = h.connect_viewer(&created, "late-ready-waiting");
    h.locked(|core| {
        do_ready(core, &second_root, second_prepare.revision);
        do_complete(core, &waiting);
    });
    h.wait_prepared_transport(&waiting.session_id, "direct");
    h.advance(150);
    let first_carrier = h.wait_prepared_transport(&second_root.session_id, "sfu");
    h.route_failed(
        &second_root,
        first_carrier.revision,
        "prepare",
        &first_carrier.candidate.connection_id,
    );
    let final_carrier = h.wait_prepared_transport(&first_root.session_id, "sfu");
    h.set_time(451);
    h.route_ready(&first_root, final_carrier.revision);
    h.wait_for("exactly one exhaustion for the waiting viewer", || {
        h.count_route_status(&waiting.session_id, RouteStatus::FailedRouteExhausted) == 1
    });
    for carrier in [&first_root, &second_root] {
        assert!(
            !h.has_failed_status(&carrier.session_id),
            "carriers must not be told about the waiting viewer's exhaustion"
        );
    }
}

#[test]
fn router_reports_only_sfu_admission_waiting_while_bounded_capacity_is_unavailable() {
    let h = RouterHarness::new(timed_sfu(2, 300));
    let fences = [
        fence("9001", "capacity_share_a", "capacity_publication_a"),
        fence("9002", "capacity_share_b", "capacity_publication_b"),
    ];
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "sfu-admission-wait");
    h.locked(|core| {
        for fence in &fences {
            assert!(
                admission(core).reserve_publication(fence),
                "the capacity fence must be reserved"
            );
        }
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    h.wait_prepared_transport(&viewer.session_id, "direct");
    h.advance(150);
    h.wait_for("the sfu-admission waiting status", || {
        h.last_route_status(&viewer.session_id) == Some(RouteStatus::WaitingSfuAdmission)
    });
    h.locked(|core| {
        for fence in &fences {
            release_fence(core, fence);
        }
    });
}

#[test]
fn router_serializes_fresh_sfu_configuration_without_making_refresh_one_shot() {
    let h = RouterHarness::new(sfu_options(1));
    let created = h.create_room();
    let (_, first, _) = h.establish_sfu_room(&created);
    let config_count = || sfu_config_count(&h.messages(&first.session_id));
    let (active, initial) = h.locked(|core| {
        let active = do_must_edge(core, &created.room_id, &first.peer_id);
        assert_eq!(active.upstream, MediaRouteUpstream::Sfu);
        let initial = sfu_config_count(&sent(core, &first.session_id));
        core.refresh_sfu(&first, 0);
        (active, initial)
    });
    h.wait_for("the first refresh", || config_count() == initial + 1);
    h.locked(|core| core.refresh_sfu(&first, active.revision));
    h.wait_for("the second refresh", || config_count() == initial + 2);
}

#[test]
fn router_releases_exact_subscription_charge_after_physical_close() {
    let mut h = RouterHarness::new(RouterOptions {
        capacity: 1,
        with_sfu: true,
        sfu_ingress_capacity: 2,
        ..Default::default()
    });
    let active_room = h.create_room();
    let (_, first, _) = h.establish_sfu_room(&active_room);
    h.wait_usage(Usage {
        ingress: 1,
        egress: 2,
    });
    let waiting_room = h.create_room();
    let waiting_host = h.connect_host(&waiting_room, None, "", "");
    let waiting_viewer = h.connect_viewer(&waiting_room, "waiter");
    h.locked(|core| {
        let occupied = fence(
            "9001",
            "waiter_share_12345678",
            "waiter_publication_12345678",
        );
        assert!(
            admission(core).reserve_publication(&occupied),
            "the occupying publication must be reserved"
        );
        do_complete(core, &waiting_host);
        do_complete(core, &waiting_viewer);
    });
    let direct = h.wait_prepared(&waiting_viewer.session_id);
    h.route_failed(
        &waiting_viewer,
        direct.revision,
        "prepare",
        &direct.candidate.connection_id,
    );
    h.wait_for("the waiting status", || {
        h.last_route_status(&waiting_viewer.session_id) == Some(RouteStatus::WaitingSfuAdmission)
    });
    let gate = Barrier::new();
    h.media().set_subscription_drain_barrier(Some(gate.clone()));
    h.locked(|core| {
        do_disconnect(core, &first);
        core.remove_viewer(&active_room.room_id, &first.peer_id);
    });
    h.wait_for("one diagnostic child", || {
        h.diagnostic(&active_room.room_id).children.len() == 1
    });
    h.wait_for("the subscription drain attempt", || {
        has_viewer(&h.media().subscription_drain_attempts(), &first.peer_id)
    });
    assert_eq!(
        h.usage(),
        Usage {
            ingress: 2,
            egress: 2
        },
        "usage during drain"
    );
    gate.open();
    h.wait_for("the drained subscription", || {
        has_viewer(&h.media().drained_subscriptions(), &first.peer_id)
    });
    h.wait_usage(Usage {
        ingress: 2,
        egress: 1,
    });
    h.close().expect("close");
    assert_eq!(h.usage(), Usage::default(), "usage after close");
}

#[test]
fn router_drains_every_pending_sfu_publication_during_close_even_when_one_drain_rejects() {
    let mut h = RouterHarness::new(RouterOptions {
        capacity: 1,
        with_sfu: true,
        prepare_timeout_ms: 1_000,
        sfu_ingress_capacity: 2_000,
        ..Default::default()
    });
    let room_a = h.create_room();
    h.establish_sfu_room(&room_a);
    let room_b = h.create_room();
    h.establish_sfu_room(&room_b);
    h.media().set_fail_delete(true);
    h.locked(|core| {
        core.stop_room(&room_a.room_id);
        core.stop_room(&room_b.room_id);
    });
    h.wait_for("two delete attempts", || h.media().delete_calls() == 2);
    h.media().set_fail_delete(false);
    let media = Arc::downgrade(h.media.as_ref().expect("media"));
    let failing_room = room_a.room_id.clone();
    h.media()
        .set_close_publication(Some(Arc::new(move |fence: &ResourceFence| {
            if fence.room_id == failing_room {
                return Err(MediaError::failed("room deletion failed"));
            }
            media
                .upgrade()
                .expect("media")
                .real_close_publication(fence)
        })));
    let error = h
        .close()
        .expect_err("close must report the shutdown drain failure");
    assert!(
        error.contains("SFU drain failed during shutdown"),
        "close error = {error}"
    );
    let deleted: Vec<String> = h
        .media()
        .deleted()
        .into_iter()
        .map(|fence| fence.room_id)
        .collect();
    assert_eq!(deleted, vec![room_b.room_id.clone()]);
}

#[test]
fn router_creates_one_host_publication_and_reuses_exact_viewer_subscriptions() {
    let h = RouterHarness::new(sfu_options(1));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let first = h.connect_viewer(&created, "sfu_first");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first);
    });
    let first_direct = h.wait_prepared(&first.session_id);
    let second = h.connect_viewer(&created, "sfu_second");
    h.locked(|core| {
        do_ready(core, &first, first_direct.revision);
        do_complete(core, &second);
    });
    let first_sfu = h.wait_prepared_transport(&first.session_id, "sfu");
    h.route_ready(&first, first_sfu.revision);
    let second_sfu = h.wait_prepared_transport(&second.session_id, "sfu");
    h.route_ready(&second, second_sfu.revision);
    h.wait_usage(Usage {
        ingress: 1,
        egress: 2,
    });
    assert_eq!(
        sfu_config_count(&h.messages(&host.session_id)),
        1,
        "host sfu-config count"
    );
}
