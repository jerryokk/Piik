//! Hybrid media router tests: route selection, signaling and recovery.

use std::collections::HashSet;
use std::sync::Arc;

use piik_ordered::Map;
use piik_protocol::{
    CandidateSignalOrigin, ConnectionAttempt, MediaRouteUpstream, RoutePolicy, RouteStatus,
    ServerMessage,
};
use piik_route::{
    CandidatePlan, CandidateTuple, CommittedEdge, CurrentAttempt, DemandReason, EndpointTransition,
    HostPublication, OperationSnapshot, PublicationPlan, Resource, ResourceKind, RouteSnapshot,
    TransitionKind, Transport, UpstreamKind,
};
use serde_json::{Value, json};

use super::fake_media::Barrier;
use super::router_harness::*;
use crate::router::{
    ActiveViewerMediaEdge, PeerSignalInput, QualityCopyContext, QualitySample, RouteParticipant,
    SignalAuthorization, quality_copy_context,
};
use crate::server::{Core, route_debug_enabled, route_debug_flag};
use piik_sfu::runtime::{MediaError, ResourceFence, Usage};

#[path = "router/router_exhaustion.rs"]
mod router_exhaustion;
#[path = "router/router_fallback.rs"]
mod router_fallback;
#[path = "router/router_quality.rs"]
mod router_quality;
#[path = "router/router_recovery.rs"]
mod router_recovery;
#[path = "router/router_signaling.rs"]
mod router_signaling;

/// Shared fixture builders for the router test groups.
const QUALITY_POLICY: RoutePolicy = RoutePolicy {
    peer_only: false,
    topology_optimization: true,
    nat_prediction: false,
};

fn observe(
    core: &mut Core,
    participant: &RouteParticipant,
    message: piik_protocol::SenderQualityEvidenceMessage,
) -> bool {
    core.router_observe_sender_quality_evidence(participant, &message)
}

/// The shared prefix of "commits P2P quality convergence after client
/// relative approval".
fn prepare_quality_convergence(h: &RouterHarness, debug: bool) -> QualityConvergence {
    let created = h.create_room();
    let host = h.connect_host(&created, Some(QUALITY_POLICY), "", "");
    let first = h.connect_viewer(&created, "quality-first");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &first);
    });
    let first_prepare = h.wait_prepared(&first.session_id);
    assert!(
        !first_prepare.candidate.quality_probe,
        "the join candidate must not be a quality probe"
    );
    let second = h.connect_viewer(&created, "quality-second");
    h.locked(|core| {
        do_ready(core, &first, first_prepare.revision);
        do_complete(core, &second);
    });
    let second_prepare = h.wait_prepared(&second.session_id);
    let first_edge = h.locked(|core| {
        do_ready(core, &second, second_prepare.revision);
        do_relay(core, &second, 3);
        let first_edge = do_must_edge(core, &created.room_id, &first.peer_id);
        let second_edge = do_must_edge(core, &created.room_id, &second.peer_id);
        assert_eq!(
            first_edge.upstream,
            peer(&host.peer_id),
            "both viewers must start on the host"
        );
        assert_eq!(
            second_edge.upstream,
            peer(&host.peer_id),
            "both viewers must start on the host"
        );
        assert!(observe(
            core,
            &host,
            sender_evidence(
                &second.peer_id,
                &second_edge.connection_id,
                "second-rtp",
                "track",
                100.0,
                second_edge.revision,
                "healthy"
            )
        ));
        (first_edge, second_edge.revision)
    });
    if debug {
        assert!(
            h.debug.called_with(
                &created.room_id,
                "sender-quality-evidence",
                &[
                    ("routeRevision", json!(first_edge.1)),
                    ("committedCopies", json!(2)),
                    ("candidateReservedCopies", json!(0)),
                    ("possibleCopies", json!(2)),
                    ("endpointCapacity", json!(2)),
                    ("operationReason", Value::Null),
                    ("candidateTransition", Value::Null),
                    ("sampleRole", json!("active")),
                ]
            ),
            "active sender evidence debug context missing"
        );
    }
    let first_edge = first_edge.0;
    h.locked(|core| {
        assert!(observe(
            core,
            &host,
            sender_evidence(
                &first.peer_id,
                &first_edge.connection_id,
                "first-rtp",
                "track",
                100.0,
                first_edge.revision,
                "healthy"
            )
        ));
        for window in 0..3 {
            assert!(
                observe(
                    core,
                    &host,
                    sender_evidence(
                        &first.peer_id,
                        &first_edge.connection_id,
                        "first-rtp",
                        "track",
                        f64::from(101 + window),
                        first_edge.revision,
                        "degraded"
                    )
                ),
                "degraded evidence must be accepted"
            );
        }
    });
    let quality_prepare = h.wait_prepared_above(&first.session_id, first_edge.revision);
    assert!(
        quality_prepare.candidate.quality_probe,
        "the convergence candidate must be a quality probe"
    );
    h.locked(|core| {
        assert!(
            observe(
                core,
                &second,
                sender_evidence(
                    &first.peer_id,
                    &quality_prepare.candidate.connection_id,
                    "candidate-rtp",
                    "track",
                    200.0,
                    quality_prepare.revision,
                    "healthy"
                )
            ),
            "candidate evidence must be accepted"
        );
    });
    if debug {
        assert!(
            h.debug.called_with(
                &created.room_id,
                "sender-quality-evidence",
                &[
                    ("routeRevision", json!(quality_prepare.revision)),
                    ("committedCopies", json!(0)),
                    ("candidateReservedCopies", json!(1)),
                    ("possibleCopies", json!(1)),
                    ("endpointCapacity", json!(2)),
                    ("operationReason", json!("quality-convergence")),
                    ("candidateTransition", json!("none")),
                    ("sampleRole", json!("candidate")),
                ]
            ),
            "candidate sender evidence debug context missing"
        );
    }
    QualityConvergence {
        created,
        host,
        first,
        second,
        first_edge,
        quality_prepare,
    }
}

fn sfu_options(capacity: i64) -> RouterOptions {
    RouterOptions {
        capacity,
        with_sfu: true,
        ..Default::default()
    }
}

fn timed_sfu(capacity: i64, prepare_timeout_ms: i64) -> RouterOptions {
    RouterOptions {
        capacity,
        with_sfu: true,
        prepare_timeout_ms,
        fake_timers: true,
        ..Default::default()
    }
}

struct QualityConvergence {
    created: piik_room::CreatedRoom,
    host: RouteParticipant,
    first: RouteParticipant,
    second: RouteParticipant,
    first_edge: ActiveViewerMediaEdge,
    quality_prepare: Prepare,
}

fn options(capacity: i64) -> RouterOptions {
    RouterOptions {
        capacity,
        ..Default::default()
    }
}

fn fence(room_id: &str, share: &str, publication: &str) -> ResourceFence {
    ResourceFence {
        room_id: room_id.to_owned(),
        share_generation: share.to_owned(),
        publication_generation: publication.to_owned(),
    }
}

fn release_fence(core: &mut Core, fence: &ResourceFence) {
    admission(core).begin_drain(fence);
    admission(core).complete_drain(fence);
}

fn committed_edge_template() -> CommittedEdge {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let host = h.connect_host(&created, None, "", "");
    let viewer = h.connect_viewer(&created, "edge-template");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let prepared = h.wait_prepared(&viewer.session_id);
    h.route_ready(&viewer, prepared.revision);
    h.locked(|core| {
        core.snapshot(&created.room_id)
            .and_then(|snapshot| snapshot.upstream_by_viewer.get(&viewer.peer_id).cloned())
            .expect("committed edge")
    })
}

fn offer_input(
    room_id: &str,
    source: &RouteParticipant,
    target: &RouteParticipant,
    connection_id: &str,
) -> PeerSignalInput {
    PeerSignalInput {
        room_id: room_id.to_owned(),
        source_peer_id: source.peer_id.clone(),
        source_session_id: source.session_id.clone(),
        target_peer_id: target.peer_id.clone(),
        target_session_id: target.session_id.clone(),
        connection_id: connection_id.to_owned(),
        signal_kind: "description",
        description_type: "offer",
        ..Default::default()
    }
}
