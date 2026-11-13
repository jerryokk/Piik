//! Route tests: hole punching, recovery, SFU-only policy and viewer route
//! preference.

use piik_protocol::{
    DEFAULT_ROUTE_POLICY, MediaRouteUpstream, Role, RoutePolicy, ViewerRoutePreference,
};
use serde_json::json;

use super::fake_media::FakeMedia;
use super::harness::*;
use super::router_harness::{
    RouterHarness, RouterOptions, do_complete, do_failed, do_ready, sender_evidence,
};
use super::wire::*;
use crate::router::{
    CANDIDATE_CONNECTION_ID_PREFIX, PeerSignalInput, RouteParticipant, SfuFallback,
    SignalAuthorization,
};
use piik_sfu::runtime::{Admission, AdmissionOptions};

const QUALITY_POLICY: RoutePolicy = RoutePolicy {
    peer_only: false,
    topology_optimization: true,
    nat_prediction: false,
};

fn swap(input: &mut PeerSignalInput) {
    std::mem::swap(&mut input.source_peer_id, &mut input.target_peer_id);
    std::mem::swap(&mut input.source_session_id, &mut input.target_session_id);
}

// ---------------------------------------------------------------------------
// punch
// ---------------------------------------------------------------------------

#[test]
fn punch_authorization_belongs_only_to_current_peer_retry() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 2,
        ..Default::default()
    });
    let created = h.create_room();
    let host = h.connect_host(
        &created,
        Some(RoutePolicy {
            peer_only: true,
            topology_optimization: false,
            nat_prediction: true,
        }),
        "",
        "",
    );
    let viewer = h.connect_viewer(&created, "punch-viewer");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    for attempt in 1..=2 {
        let prepared = h.wait_prepared_matching(&viewer.session_id, "peer retry", |message| {
            message
                .candidate
                .connection_attempt
                .is_some_and(|progress| progress.current == attempt)
        });
        h.locked(|core| {
            let mut base = PeerSignalInput {
                room_id: created.room_id.clone(),
                source_peer_id: host.peer_id.clone(),
                source_session_id: host.session_id.clone(),
                target_peer_id: viewer.peer_id.clone(),
                target_session_id: viewer.session_id.clone(),
                connection_id: prepared.candidate.connection_id.clone(),
                ..Default::default()
            };
            for phase in ["prepare", "start", "ready", "complete", "offer"] {
                let mut input = base.clone();
                input.signal_kind = "punch";
                input.punch_phase = phase;
                if phase == "ready" || phase == "complete" {
                    swap(&mut input);
                }
                if phase == "offer" {
                    input.signal_kind = "description";
                    input.punch_phase = "";
                    input.description_type = "offer";
                    input.punched = true;
                }
                let want = if attempt > 1 {
                    SignalAuthorization::Probe
                } else {
                    SignalAuthorization::Denied
                };
                assert_eq!(
                    core.peer_signal_authorization(&input),
                    want,
                    "attempt {attempt} {phase}"
                );
                let mut stale = input.clone();
                stale.connection_id = "retired_connection".into();
                assert_eq!(
                    core.peer_signal_authorization(&stale),
                    SignalAuthorization::Denied,
                    "stale generation authorized"
                );
                let mut stale = input.clone();
                stale.source_session_id = "retired_session".into();
                assert_eq!(
                    core.peer_signal_authorization(&stale),
                    SignalAuthorization::Denied,
                    "stale session authorized"
                );
                swap(&mut input);
                assert_eq!(
                    core.peer_signal_authorization(&input),
                    SignalAuthorization::Denied,
                    "wrong phase direction authorized: {phase}"
                );
            }
            if attempt == 1 {
                do_failed(
                    core,
                    &viewer,
                    prepared.revision,
                    "prepare",
                    Some(&prepared.candidate.connection_id),
                );
            } else {
                do_ready(core, &viewer, prepared.revision);
                base.signal_kind = "punch";
                base.punch_phase = "prepare";
                assert_eq!(
                    core.peer_signal_authorization(&base),
                    SignalAuthorization::Denied,
                    "committed edge authorized another preparation"
                );
            }
        });
    }
}

// ---------------------------------------------------------------------------
// recovery
// ---------------------------------------------------------------------------

fn connection_payload(connection_id: &str) -> String {
    json!({"payload": {"connectionId": connection_id}}).to_string()
}

#[test]
fn signal_rebuild_supersedes_overlapping_quality_preparation() {
    for order in ["prepare-before-restart", "restart-before-prepare"] {
        let h = Harness::start(HarnessOptions {
            endpoint_media_copy_capacity: 2,
            ..Default::default()
        });
        let host = h.open_client();
        let host_auth = authenticate(
            &host,
            &h.room,
            "host",
            "recovery-host",
            1,
            super::router_harness::TEST_SHARE_GENERATION,
            Presence {
                route_policy: Some(QUALITY_POLICY),
                ..Default::default()
            },
        );
        let viewer = h.open_client();
        let viewer_auth = authenticate(
            &viewer,
            &h.room,
            "viewer",
            "recovery-viewer",
            0,
            "",
            Presence::default(),
        );
        commit_prepared_route(&viewer);
        let room = h.room.room_id.clone();
        let (host_session, viewer_session, active) = h.locked(|core| {
            let parent = core.store.get_connected_host(&room).expect("host");
            let child = core
                .store
                .get_connected_viewer(&room, &viewer_auth.peer_id())
                .expect("viewer");
            let active = core
                .resolve_active_viewer_media_edge(&room, &viewer_auth.peer_id())
                .expect("active edge");
            (parent.session_id, child.session_id, active)
        });
        let restart = || {
            viewer.send_json(
                json!({"type": "restart-request", "targetPeerId": host_auth.peer_id(),
                "connectionId": active.connection_id, "rebuild": true}),
            );
            expect_match(&host.next("restart-request").raw, r#"{"rebuild":true}"#);
        };
        if order == "restart-before-prepare" {
            restart();
        }
        h.locked(|core| {
            let mut participant =
                RouteParticipant::new(&room, Role::Host, &host_auth.peer_id(), &host_session);
            participant.route_policy = Some(QUALITY_POLICY);
            for (index, state) in ["healthy", "degraded", "degraded", "degraded"]
                .into_iter()
                .enumerate()
            {
                core.router_observe_sender_quality_evidence(
                    &participant,
                    &sender_evidence(
                        &viewer_auth.peer_id(),
                        &active.connection_id,
                        "recovery-sender",
                        "recovery-track",
                        100.0 + index as f64,
                        active.revision,
                        state,
                    ),
                );
            }
        });
        let pending = next_prepared_route(&viewer);
        assert!(
            pending
                .value
                .pointer("/candidate/qualityProbe")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
                && pending.upstream_kind() == "peer"
                && pending.upstream_peer() == host_auth.peer_id(),
            "{order}: expected a same-parent quality preparation"
        );
        assert!(
            active
                .connection_id
                .starts_with(CANDIDATE_CONNECTION_ID_PREFIX)
                && pending
                    .connection_id()
                    .starts_with(CANDIDATE_CONNECTION_ID_PREFIX),
            "prepared connections must remain distinguishable after retirement"
        );
        if order == "prepare-before-restart" {
            restart();
        }

        // A same-parent probe must not intercept the retained connection's
        // offer, answer or ICE. The probe's own signaling also stays valid.
        host.send_json(signal_description(
            &viewer_auth.peer_id(),
            &active.connection_id,
            "offer",
        ));
        expect_match(
            &viewer.next("signal").raw,
            &connection_payload(&active.connection_id),
        );
        viewer.send_json(signal_description(
            &host_auth.peer_id(),
            &active.connection_id,
            "answer",
        ));
        host.next("signal");
        viewer.send_json(signal_candidate(
            &host_auth.peer_id(),
            &active.connection_id,
        ));
        host.next("signal");
        host.send_json(signal_description(
            &viewer_auth.peer_id(),
            &pending.connection_id(),
            "offer",
        ));
        expect_match(
            &viewer.next("signal").raw,
            &connection_payload(&pending.connection_id()),
        );

        const REPLACEMENT: &str = "manual_replacement_connection";
        host.send_json(signal_description(
            &viewer_auth.peer_id(),
            REPLACEMENT,
            "offer",
        ));
        expect_match(&viewer.next("signal").raw, &connection_payload(REPLACEMENT));
        let settled = next_active_route_after(&viewer, pending.revision());
        assert_eq!(
            settled.upstream_peer(),
            host_auth.peer_id(),
            "manual recovery must keep the committed parent"
        );
        assert_eq!(
            active.upstream,
            MediaRouteUpstream::peer(host_auth.peer_id())
        );
        viewer.send_json(signal_description(
            &host_auth.peer_id(),
            REPLACEMENT,
            "answer",
        ));
        host.next("signal");

        h.locked(|core| {
            assert!(
                core.snapshot(&room).expect("snapshot").operation.is_none(),
                "superseded preparation must release the serial operation"
            );
            for retired in [pending.connection_id(), active.connection_id.clone()] {
                let authorization = core.peer_signal_authorization(&PeerSignalInput {
                    room_id: room.clone(),
                    source_peer_id: host_auth.peer_id(),
                    source_session_id: host_session.clone(),
                    target_peer_id: viewer_auth.peer_id(),
                    target_session_id: viewer_session.clone(),
                    connection_id: retired,
                    signal_kind: "description",
                    description_type: "offer",
                    ..Default::default()
                });
                assert_eq!(
                    authorization,
                    SignalAuthorization::Denied,
                    "a late retired offer must not replace the recovered connection"
                );
            }
            let current = core
                .resolve_active_viewer_media_edge(&room, &viewer_auth.peer_id())
                .expect("current");
            assert!(
                current.connection_id == REPLACEMENT && current.revision == settled.revision(),
                "recovered edge = {current:?}"
            );
        });
    }
}

// ---------------------------------------------------------------------------
// SFU-only
// ---------------------------------------------------------------------------

fn sfu(ingress: usize, egress: usize, prepare_timeout_ms: i64) -> SfuFallback {
    SfuFallback {
        media: FakeMedia::new(),
        admission: Admission::new(AdmissionOptions {
            ingress_capacity: ingress,
            egress_capacity: egress,
        }),
        prepare_timeout_ms,
    }
}

#[test]
fn sfu_only_rejects_peer_only_before_acquiring_share_authority() {
    let h = Harness::start(HarnessOptions {
        sfu_only: true,
        nat_prediction_enabled: true,
        sfu: Some(sfu(2, 20, 0)),
        ..Default::default()
    });
    let mut policy = RoutePolicy {
        peer_only: true,
        topology_optimization: true,
        nat_prediction: true,
    };
    let rejected = h.open_client();
    rejected.send_json(
        json!({"type": "authenticate", "protocol": piik_protocol::SIGNALING_PROTOCOL,
        "roomId": h.room.room_id, "role": "host", "clientId": "peer-only-host",
        "token": h.room.host_token, "shareGeneration": "peer_only_share_generation",
        "routePolicy": policy_json(policy)}),
    );
    expect_match(&rejected.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
    expect_close_code(&rejected, 4003);

    // An idle room remains available. Refusing a conflicting share preserves it.
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "idle-host",
        -1,
        "",
        Presence {
            route_policy: Some(policy),
            ..Default::default()
        },
    );
    host.send_json(
        json!({"type": "start-sharing", "shareGeneration": "sfu_only_share_generation",
        "routePolicy": policy_json(policy)}),
    );
    expect_match(
        &host.next("sharing-start-failed").raw,
        r#"{"code":"FORBIDDEN"}"#,
    );
    policy.peer_only = false;
    host.send_json(
        json!({"type": "start-sharing", "shareGeneration": "sfu_only_share_generation",
        "routePolicy": policy_json(policy)}),
    );
    expect_match(
        &host.next("sharing-started").raw,
        r#"{"routePolicy":{"peerOnly":false,"topologyOptimization":false,"natPrediction":false}}"#,
    );
}

// ---------------------------------------------------------------------------
// viewer route preference
// ---------------------------------------------------------------------------

fn preferring(preference: ViewerRoutePreference) -> Presence {
    Presence {
        route_preference: Some(preference),
        ..Default::default()
    }
}

#[test]
fn viewer_route_preference_policy_and_reconnect() {
    let h = Harness::start(HarnessOptions {
        viewer_disconnect_grace_ms: 500,
        ..Default::default()
    });
    let default_viewer = h.open_client();
    authenticate(
        &default_viewer,
        &h.room,
        "viewer",
        "default-viewer",
        -1,
        "",
        Presence::default(),
    );
    expect_match(
        &default_viewer.next("viewer-route-preference").raw,
        r#"{"preference":"auto","accepted":true}"#,
    );
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "choice-viewer",
        -1,
        "",
        preferring(ViewerRoutePreference::Peer),
    );
    expect_match(
        &viewer.next("viewer-route-preference").raw,
        r#"{"preference":"p2p","accepted":true}"#,
    );
    viewer.send_json(json!({"type": "set-viewer-route-preference", "preference": "sfu"}));
    expect_match(
        &viewer.next("viewer-route-preference").raw,
        r#"{"preference":"auto","accepted":false}"#,
    );
    // The rejected choice does not terminate signaling or change the saved intent.
    viewer.send_json(json!({"type": "signaling-challenge", "sequence": 1}));
    viewer.next("signaling-challenge-response");
    let replacement = h.open_client();
    authenticate(
        &replacement,
        &h.room,
        "viewer",
        "choice-viewer",
        -1,
        "",
        preferring(ViewerRoutePreference::Peer),
    );
    expect_match(
        &replacement.next("viewer-route-preference").raw,
        r#"{"preference":"p2p","accepted":true}"#,
    );
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "choice-host",
        -1,
        "",
        Presence {
            ..Default::default()
        },
    );
    host.send_json(json!({"type": "set-viewer-route-preference", "preference": "p2p"}));
    expect_match(&host.next("error").raw, r#"{"code":"FORBIDDEN"}"#);
}

#[test]
fn viewer_route_preference_cannot_override_host_or_site() {
    for sfu_only in [false, true] {
        let h = Harness::start(HarnessOptions {
            sfu_only,
            sfu: Some(sfu(2, 20, 300)),
            ..Default::default()
        });
        let viewer = h.open_client();
        let preference = if sfu_only {
            ViewerRoutePreference::Peer
        } else {
            ViewerRoutePreference::Sfu
        };
        authenticate(
            &viewer,
            &h.room,
            "viewer",
            "policy-viewer",
            -1,
            "",
            preferring(preference),
        );
        if sfu_only {
            expect_match(
                &viewer.next("viewer-route-preference").raw,
                r#"{"preference":"auto","accepted":true}"#,
            );
        } else {
            expect_match(
                &viewer.next("viewer-route-preference").raw,
                r#"{"preference":"sfu","accepted":true}"#,
            );
        }
        let host = h.open_client();
        let mut policy = DEFAULT_ROUTE_POLICY;
        policy.peer_only = !sfu_only;
        authenticate(
            &host,
            &h.room,
            "host",
            "policy-host",
            -1,
            "policy-share-generation",
            Presence {
                route_policy: Some(policy),
                ..Default::default()
            },
        );
        if !sfu_only {
            expect_match(
                &viewer.next("viewer-route-preference").raw,
                r#"{"preference":"auto","accepted":true}"#,
            );
        }
        viewer.send_json(
            json!({"type": "set-viewer-route-preference", "preference": preference.as_str()}),
        );
        expect_match(
            &viewer.next("viewer-route-preference").raw,
            r#"{"preference":"auto","accepted":false}"#,
        );
    }
}
