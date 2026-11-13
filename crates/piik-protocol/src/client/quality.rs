//! Viewer, sender and SFU publisher quality-evidence decoding.

use serde_json::Value;

use crate::client::{
    ClientMessage, SenderQualityEvidenceMessage, SfuPublisherQualityEvidenceMessage,
    require_opaque, revision,
};
use crate::codec::{Fields, Int, Num, Result, ensure, in_range, in_range_num, object};
use crate::constants::MAX_SAFE_INTEGER;
use crate::scalars::{utf16_length, valid_opaque_id};
use crate::types::{SenderQualityDiagnostics, SenderQualityReason, SenderQualityState};

pub(crate) fn validate_evidence_window(sequence: Int, window_ms: Int) -> Result<()> {
    ensure(
        in_range(sequence, 0, MAX_SAFE_INTEGER),
        "sequence is out of range",
    )?;
    ensure(
        in_range(window_ms, 1_000, 5_000),
        "windowMs is out of range",
    )
}

fn bounded_identity(value: &Option<String>, key: &str) -> Result<()> {
    ensure(
        value
            .as_deref()
            .is_none_or(|text| (1..=256).contains(&utf16_length(text))),
        &format!("{key} length is out of range"),
    )
}

fn sample_timestamp(f: &Fields<'_>) -> Result<Option<Num>> {
    f.require_nullable(&["sampleTimestampMs"])?;
    let sample: Option<Num> = f.opt("sampleTimestampMs")?;
    ensure(
        sample.is_none_or(|sample| in_range_num(sample, 0.0, MAX_SAFE_INTEGER as Num)),
        "sampleTimestampMs is out of range",
    )?;
    Ok(sample)
}

fn reason_matches_state(
    state: SenderQualityState,
    reason: Option<SenderQualityReason>,
    message: &str,
) -> Result<()> {
    let matches = match state {
        SenderQualityState::Healthy => reason == Some(SenderQualityReason::None),
        SenderQualityState::Degraded => matches!(
            reason,
            Some(SenderQualityReason::Bandwidth | SenderQualityReason::Cpu)
        ),
        SenderQualityState::Unknown => reason.is_none(),
    };
    ensure(matches, message)
}

pub(crate) fn decode_sender_quality_evidence(value: &Value) -> Result<ClientMessage> {
    let f = object(
        value,
        &[
            "type",
            "childPeerId",
            "connectionId",
            "rtpStatsId",
            "trackIdentifier",
            "sampleTimestampMs",
            "routeRevision",
            "state",
            "diagnostics",
        ],
    )?;
    f.require(&[
        "type",
        "childPeerId",
        "connectionId",
        "routeRevision",
        "state",
        "diagnostics",
    ])?;
    f.require_nullable(&["rtpStatsId", "trackIdentifier"])?;
    let child_peer_id = f.string("childPeerId")?;
    let connection_id = f.string("connectionId")?;
    ensure(
        valid_opaque_id(&child_peer_id) && valid_opaque_id(&connection_id),
        "sender quality evidence ids are not opaque ids",
    )?;
    let rtp_stats_id = f.opt("rtpStatsId")?;
    let track_identifier = f.opt("trackIdentifier")?;
    bounded_identity(&rtp_stats_id, "rtpStatsId")?;
    bounded_identity(&track_identifier, "trackIdentifier")?;
    let sample_timestamp_ms = sample_timestamp(&f)?;
    let route_revision = revision(&f, "routeRevision")?;
    let state: SenderQualityState = f.value("state")?;
    let diagnostics: SenderQualityDiagnostics = f.value("diagnostics")?;
    // superRefine: known sender quality needs exact RTP identity.
    ensure(
        state == SenderQualityState::Unknown
            || (rtp_stats_id.is_some()
                && track_identifier.is_some()
                && sample_timestamp_ms.is_some()),
        "Known sender quality needs exact RTP identity",
    )?;
    reason_matches_state(
        state,
        diagnostics.reason,
        "Sender quality reason must match its native state",
    )?;
    Ok(ClientMessage::SenderQualityEvidence(Box::new(
        SenderQualityEvidenceMessage {
            child_peer_id,
            connection_id,
            rtp_stats_id,
            track_identifier,
            sample_timestamp_ms,
            route_revision,
            state,
            diagnostics,
        },
    )))
}

pub(crate) fn decode_sfu_publisher_quality_evidence(value: &Value) -> Result<ClientMessage> {
    let keys = [
        "type",
        "publicationGeneration",
        "routeRevision",
        "state",
        "sampleTimestampMs",
        "diagnostics",
    ];
    let f = object(value, &keys)?;
    f.require(&[
        "type",
        "publicationGeneration",
        "routeRevision",
        "state",
        "diagnostics",
    ])?;
    let publication_generation = require_opaque(
        &f,
        "publicationGeneration",
        "publicationGeneration is not an opaque id",
    )?;
    let route_revision = revision(&f, "routeRevision")?;
    let state: SenderQualityState = f.value("state")?;
    let sample_timestamp_ms = sample_timestamp(&f)?;
    let diagnostics: SenderQualityDiagnostics = f.value("diagnostics")?;
    // superRefine: known SFU publisher quality needs an exact sample.
    ensure(
        state == SenderQualityState::Unknown || sample_timestamp_ms.is_some(),
        "Known SFU publisher quality needs an exact sample",
    )?;
    reason_matches_state(
        state,
        diagnostics.reason,
        "SFU publisher reason must match its native state",
    )?;
    Ok(ClientMessage::SfuPublisherQualityEvidence(Box::new(
        SfuPublisherQualityEvidenceMessage {
            publication_generation,
            route_revision,
            state,
            sample_timestamp_ms,
            diagnostics,
        },
    )))
}
