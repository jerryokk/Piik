//! Controller tests: SFU quality relief and publisher evidence.

use super::*;
use crate::*;

use SenderQualityState::{Degraded, Healthy, Unknown};

const GENERATION: &str = "publication_generation_12345678";

fn sfu_quality() -> Options {
    Options {
        sfu_enabled: true,
        quality_convergence_enabled: true,
        ..opts()
    }
}

fn sfu_quality_begin(
    at: i64,
    connection: &str,
    generation: &str,
    publication_connection: &str,
) -> BeginInput {
    BeginInput {
        host_session_id: "host_session".to_owned(),
        ..begin_publication(
            at,
            connection,
            generation,
            publication_connection,
            sfu_create_overlap("sfu-edge", "sfu-publication", "host-overlap"),
        )
    }
}

/// A at cap 1 and B at cap 0 under the Host; A's regeneration fails, then B
/// is also persistently degraded so the quality set gains the SFU suffix.
fn host_relief_operation() -> (Controller, OperationSnapshot) {
    let mut routes = controller(2, sfu_quality());
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    observe_persistent_degraded(&mut routes, A, "a_from_host", 99);
    let first = must(routes.reconcile(103).operation);
    assert_eq!(first.candidates[0].tuple, regenerate_tuple(HOST));
    let attempt = begin_operation(
        &mut routes,
        begin(
            104,
            "a_regeneration_failed",
            direct_overlap("a_regeneration_overlap"),
        ),
    );
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&attempt).revision, "a_regeneration_failed"),
                105
            )
            .accepted
    );
    assert!(routes.set_effective_capacity(B, &session_of(B), 1, None));
    observe_persistent_degraded(&mut routes, B, "b_from_host", 104);
    let operation = must(routes.reconcile(108).operation);
    assert_eq!(operation.child_peer_id, A);
    (routes, operation)
}

// TS 2086
#[test]
fn keeps_bounded_sfu_after_direct_candidates_when_every_host_root_is_degraded() {
    let (mut routes, operation) = host_relief_operation();
    assert_eq!(
        tuples(&operation),
        [peer_tuple(B), sfu_tuple(PublicationPlan::Create)]
    );
    let direct_attempt =
        begin_operation(&mut routes, begin(109, "quality_peer_candidate", direct()));
    let failed = routes.candidate_failed(
        &guard_for(
            A,
            current(&direct_attempt).revision,
            "quality_peer_candidate",
        ),
        110,
    );
    assert!(failed.accepted);
    assert!(failed.failed_peer_ids.is_empty());

    let started = must(
        begin_candidate(
            &mut routes,
            sfu_quality_begin(
                111,
                "quality_sfu_candidate",
                GENERATION,
                "publication_connection_12345678",
            ),
        )
        .operation,
    );
    let revision = current(&started).revision;
    let guard = guard_for(A, revision, "quality_sfu_candidate");
    let ready = routes.candidate_ready(&guard, 112, None, CandidateProof::default());
    assert!(ready.accepted && !ready.committed);
    for at in [113, 114] {
        let observed = routes.observe_sfu_publisher_quality_evidence(
            publisher_sample(GENERATION, revision, Healthy, at),
            None,
        );
        assert!(observed.accepted && !observed.committed);
        let ready = routes.candidate_ready(&guard, at, None, CandidateProof::default());
        assert!(ready.accepted && !ready.committed);
    }
    assert!(
        routes
            .observe_sfu_publisher_quality_evidence(
                publisher_sample(GENERATION, revision, Healthy, 115),
                None
            )
            .committed
    );
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Sfu, "quality_sfu_candidate")
    );
}

// TS 2201
#[test]
fn rejects_sfu_quality_candidate_on_degraded_publisher_evidence() {
    let (mut routes, _) = host_relief_operation();
    let peer_attempt = begin_operation(&mut routes, begin(109, "quality_peer_candidate", direct()));
    assert!(
        routes
            .candidate_failed(
                &guard_for(A, current(&peer_attempt).revision, "quality_peer_candidate"),
                110
            )
            .accepted
    );
    let sfu_attempt = begin_operation(
        &mut routes,
        sfu_quality_begin(
            111,
            "quality_sfu_degraded",
            "publication_generation_degraded",
            "publication_connection_degraded",
        ),
    );
    let attempt = current(&sfu_attempt);
    let ready = routes.candidate_ready(
        &guard_for(A, attempt.revision, &attempt.connection_id),
        112,
        None,
        CandidateProof::default(),
    );
    assert!(ready.accepted && !ready.committed);
    let sample = publisher_sample(
        "publication_generation_degraded",
        attempt.revision,
        Degraded,
        113,
    );
    let degraded = routes.observe_sfu_publisher_quality_evidence(sample, None);
    assert!(degraded.accepted && !degraded.committed);
    no_operation(routes.reconcile(114).operation);
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.parent_peer_id.as_str()),
        (UpstreamKind::Peer, HOST)
    );
}

#[test]
fn abandons_host_relief_when_another_host_root_is_no_longer_degraded() {
    for state in [Healthy, Unknown] {
        let mut routes = controller(2, sfu_quality());
        add_viewer(&mut routes, A, 1, None);
        add_viewer(&mut routes, B, 0, None);
        routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
        routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
        observe_persistent_degraded(&mut routes, A, "a_from_host", 99);
        observe_persistent_degraded(&mut routes, B, "b_from_host", 99);
        routes.reconcile(103);
        let started = must(
            begin_candidate(
                &mut routes,
                sfu_quality_begin(
                    104,
                    "a_from_sfu",
                    GENERATION,
                    "publication_connection_12345678",
                ),
            )
            .operation,
        );
        let revision = current(&started).revision;
        let sample = SenderState {
            child_peer_id: B,
            connection_id: "b_from_host",
            state: Some(state),
            accepted_at_ms: 105,
            ..SenderState::default()
        };
        assert!(observe_sender_state(&mut routes, sample).accepted);
        assert!(
            !routes
                .candidate_ready(
                    &guard_for(A, revision, "a_from_sfu"),
                    106,
                    None,
                    CandidateProof::default()
                )
                .committed
        );
        no_operation(routes.snapshot().operation);
        let edge = edge_of(&routes, A);
        assert_eq!(
            (
                edge.kind,
                edge.parent_peer_id.as_str(),
                edge.connection_id.as_str()
            ),
            (UpstreamKind::Peer, HOST, "a_from_host")
        );
    }
}

// TS 2350
#[test]
fn uses_sfu_reuse_only_for_multi_edge_host_relief_with_healthy_ingress() {
    let mut routes = controller(3, sfu_quality());
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
    routes.hydrate_host_publication(GENERATION, res("publication_resource"), "");
    let revision = routes.revision();
    observe_persistent_degraded(&mut routes, A, "a_from_host", 99);
    observe_persistent_degraded(&mut routes, B, "b_from_host", 99);
    no_operation(routes.reconcile(103).operation);
    observe_persistent_sfu_healthy(&mut routes, GENERATION, revision, 104);
    assert_eq!(
        must(routes.reconcile(105).operation).candidates[0].tuple,
        sfu_tuple(PublicationPlan::Reuse)
    );
    let started = must(
        begin_candidate(
            &mut routes,
            begin(106, "a_from_sfu_reuse", sfu_reuse("a_subscription")),
        )
        .operation,
    );
    let attempt_revision = current(&started).revision;
    let mut unknown = publisher_sample(GENERATION, revision, Unknown, 107);
    unknown.sample_timestamp_ms = None;
    assert!(
        routes
            .observe_sfu_publisher_quality_evidence(unknown, None)
            .accepted
    );
    assert!(
        !routes
            .candidate_ready(
                &guard_for(A, attempt_revision, "a_from_sfu_reuse"),
                108,
                None,
                CandidateProof::default()
            )
            .committed
    );
    for at in [2_100, 4_100, 6_100] {
        observe(&mut routes, A, "a_from_host", Degraded, at);
        observe(&mut routes, B, "b_from_host", Degraded, at);
        let settled = routes.observe_sfu_publisher_quality_evidence(
            publisher_sample(GENERATION, revision, Healthy, at),
            None,
        );
        assert_eq!(settled.committed, at == 6_100, "{at}");
    }
    let edge = edge_of(&routes, A);
    assert_eq!(
        (edge.kind, edge.connection_id.as_str()),
        (UpstreamKind::Sfu, "a_from_sfu_reuse")
    );
}

// TS 2435
#[test]
fn accepts_sfu_fed_peer_parent_from_decoded_progress_without_freeze_stats() {
    let mut routes = controller(2, sfu_quality());
    add_viewer(&mut routes, A, 1, None);
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_from_host"));
    routes.hydrate_host_publication(GENERATION, res("publication_resource"), "");
    routes.hydrate_edge(B, sfu_edge(GENERATION, "b_from_sfu", "b_subscription"));
    for plan in routes.build_candidates(B, false) {
        if plan.tuple.kind == UpstreamKind::Peer {
            routes.consume_candidate_opportunity(B, &plan);
        }
    }
    let revision = routes.revision();
    observe_persistent_sfu_healthy(&mut routes, GENERATION, revision, 100);
    let mut metrics = quality_metrics();
    metrics.freeze_count_delta = None;
    metrics.freeze_duration_ms_delta = None;
    metrics.pause_count_delta = None;
    metrics.pause_duration_ms_delta = None;
    let window = quality_window(B, revision, "b_from_sfu", sfu_upstream(), 1, 100, metrics);
    assert_eq!(
        routes.observe_quality_evidence(window),
        RouteQualityEvidenceResult::Accepted
    );
    observe_persistent_degraded(&mut routes, A, "a_from_host", 99);
    assert_eq!(
        must(routes.reconcile(103).operation).candidates[0].tuple,
        peer_tuple(B)
    );
}
