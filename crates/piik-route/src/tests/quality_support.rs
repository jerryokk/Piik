//! Shared scenario helpers: quality samples, NAT preparation and the SFU
//! retirement fixtures.

use piik_protocol::{NatTraversalPath, ViewerQualityEvidenceMetrics};

use super::*;
use crate::*;

#[derive(Default)]
pub(crate) struct SenderState<'a> {
    pub(crate) child_peer_id: &'a str,
    pub(crate) connection_id: &'a str,
    pub(crate) state: Option<SenderQualityState>,
    pub(crate) accepted_at_ms: i64,
    /// Default `HOST`.
    pub(crate) parent_peer_id: &'a str,
    /// Default `<connectionId>-rtp\0track`.
    pub(crate) sender_identity: &'a str,
}

pub(crate) fn observe_sender_state(
    routes: &mut Controller,
    input: SenderState<'_>,
) -> SenderQualityEvidenceResult {
    let parent = if input.parent_peer_id.is_empty() {
        HOST
    } else {
        input.parent_peer_id
    };
    let identity = if input.sender_identity.is_empty() {
        format!("{}-rtp\0track", input.connection_id)
    } else {
        input.sender_identity.to_owned()
    };
    let revision = routes.revision();
    routes.observe_sender_quality_evidence(
        sender_sample(
            parent,
            input.child_peer_id,
            revision,
            input.connection_id,
            &identity,
            input.state.unwrap(),
            input.accepted_at_ms,
        ),
        None,
    )
}

/// Shorthand for `observe_sender_state` with the default parent and identity.
pub(crate) fn observe(
    routes: &mut Controller,
    child: &str,
    connection: &str,
    state: SenderQualityState,
    at: i64,
) -> SenderQualityEvidenceResult {
    observe_sender_state(
        routes,
        SenderState {
            child_peer_id: child,
            connection_id: connection,
            state: Some(state),
            accepted_at_ms: at,
            ..SenderState::default()
        },
    )
}

/// The explicit sample literal (`sampleTimestampMs == acceptedAtMs`).
pub(crate) fn sender_sample(
    parent_peer_id: &str,
    child_peer_id: &str,
    revision: i64,
    connection_id: &str,
    identity: &str,
    state: SenderQualityState,
    at: i64,
) -> SenderQualityEvidenceInput {
    SenderQualityEvidenceInput {
        parent_peer_id: parent_peer_id.to_owned(),
        parent_session_id: parent_session(parent_peer_id),
        child_peer_id: child_peer_id.to_owned(),
        route_revision: revision,
        connection_id: connection_id.to_owned(),
        sender_identity: Some(identity.to_owned()),
        sample_timestamp_ms: Some(at),
        state,
        accepted_at_ms: at,
    }
}

/// `state: unknown, senderIdentity: null, sampleTimestampMs: null`.
pub(crate) fn unknown_sample(
    parent_peer_id: &str,
    child_peer_id: &str,
    revision: i64,
    connection_id: &str,
    at: i64,
) -> SenderQualityEvidenceInput {
    SenderQualityEvidenceInput {
        sender_identity: None,
        sample_timestamp_ms: None,
        ..sender_sample(
            parent_peer_id,
            child_peer_id,
            revision,
            connection_id,
            "",
            SenderQualityState::Unknown,
            at,
        )
    }
}

#[track_caller]
pub(crate) fn observe_persistent_degraded(
    routes: &mut Controller,
    child: &str,
    connection: &str,
    at: i64,
) {
    assert!(observe(routes, child, connection, SenderQualityState::Healthy, at).accepted);
    for offset in 1..=3 {
        assert!(
            observe(
                routes,
                child,
                connection,
                SenderQualityState::Degraded,
                at + offset
            )
            .accepted
        );
    }
}

pub(crate) fn publisher_sample(
    generation: &str,
    revision: i64,
    state: SenderQualityState,
    at: i64,
) -> SfuPublisherQualityEvidenceInput {
    SfuPublisherQualityEvidenceInput {
        host_peer_id: HOST.to_owned(),
        host_session_id: "host_session".to_owned(),
        publication_generation: generation.to_owned(),
        route_revision: revision,
        state,
        sample_timestamp_ms: Some(at),
        accepted_at_ms: at,
    }
}

#[track_caller]
pub(crate) fn observe_persistent_sfu_healthy(
    routes: &mut Controller,
    generation: &str,
    revision: i64,
    at: i64,
) -> SenderQualityEvidenceResult {
    let mut result = SettleResult::default();
    for offset in 0..3 {
        result = routes.observe_sfu_publisher_quality_evidence(
            publisher_sample(
                generation,
                revision,
                SenderQualityState::Healthy,
                at + offset,
            ),
            None,
        );
        assert!(result.accepted);
    }
    result
}

pub(crate) fn quality_metrics() -> ViewerQualityEvidenceMetrics {
    let mut metrics = ViewerQualityEvidenceMetrics::empty(NatTraversalPath::Unknown);
    metrics.frames_decoded_delta = Some(120);
    metrics.freeze_count_delta = Some(1);
    metrics.freeze_duration_ms_delta = Some(250.0);
    metrics.pause_count_delta = Some(0);
    metrics.pause_duration_ms_delta = Some(0.0);
    metrics
}

pub(crate) fn peer_upstream(peer_id: &str) -> QualityUpstream {
    QualityUpstream {
        kind: UpstreamKind::Peer,
        peer_id: peer_id.to_owned(),
    }
}

pub(crate) fn sfu_upstream() -> QualityUpstream {
    QualityUpstream {
        kind: UpstreamKind::Sfu,
        peer_id: String::new(),
    }
}

pub(crate) fn quality_window(
    child: &str,
    revision: i64,
    connection: &str,
    upstream: QualityUpstream,
    epoch: i64,
    at: i64,
    metrics: ViewerQualityEvidenceMetrics,
) -> RouteQualityEvidenceInput {
    RouteQualityEvidenceInput {
        child_peer_id: child.to_owned(),
        child_session_id: session_of(child),
        route_revision: revision,
        connection_id: connection.to_owned(),
        upstream,
        presentation_epoch: epoch,
        window_ms: 2_000,
        metrics,
        accepted_at_ms: at,
    }
}

/// Ports the `bounded NAT candidate acquisition` helper.
#[track_caller]
pub(crate) fn prepare_peer(
    routes: &mut Controller,
    now_ms: i64,
    id: &str,
) -> (OperationSnapshot, OperationSnapshot, CandidateGuard) {
    let operation = must(routes.reconcile(now_ms).operation);
    let prepared = begin_operation(routes, begin(now_ms, id, direct()));
    let guard = CandidateGuard {
        child_peer_id: prepared.child_peer_id.clone(),
        child_session_id: prepared.child_session_id.clone(),
        revision: current(&prepared).revision,
        connection_id: id.to_owned(),
    };
    (operation, prepared, guard)
}

#[track_caller]
pub(crate) fn attempt_current(operation: &OperationSnapshot) -> i64 {
    current(operation)
        .connection_attempt
        .expect("expected a connection attempt")
        .current
}
