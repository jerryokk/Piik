//! Request dispatch. Malformed commands close the control connection; valid
//! commands that cannot complete fail only their request, and an
//! already-retired teardown target is ACKed.

use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use piik_loopback::{CancellationToken, ControlEvent, ProtocolViolation};
use piik_native_capture::CaptureTarget;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::events::to_value;
use crate::session::{Fail, Inner, Request, fail};
use crate::wire::*;

type Handled = Result<Option<ControlEvent>, Fail>;

fn ok(event: ControlEvent) -> Handled {
    Ok(Some(event))
}

fn value(value: impl serde::Serialize) -> Handled {
    Ok(Some(ControlEvent::new(to_value(&value))))
}

/// Decodes a strict request, or fails with the given violation.
fn strict<T: serde::de::DeserializeOwned>(
    payload: &[u8],
    violation: &'static str,
) -> Result<T, Fail> {
    decode_strict(payload).ok_or(Fail::Violation(violation))
}

fn servers(values: &[IceServer]) -> Result<Vec<Vec<String>>, Fail> {
    ice_servers(values).map_err(Fail::Violation)
}

fn edge_capacity(value: i64) -> Option<usize> {
    (1..=MAX_EDGE_CAPACITY)
        .contains(&value)
        .then(|| usize::try_from(value).ok())
        .flatten()
}

fn no_share() -> Fail {
    fail("native share does not exist")
}

/// The identities a request trace may log; source titles, SDP, candidates
/// and media never enter it.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct TraceIdentity {
    share_id: Option<String>,
    connection_id: Option<String>,
    source_connection_id: Option<String>,
    publication_generation: Option<String>,
}

mod media;
mod publication;

impl Inner {
    pub(crate) async fn handle(
        self: &Arc<Self>,
        _cancel: &CancellationToken,
        payload: &[u8],
    ) -> Result<Option<ControlEvent>, ProtocolViolation> {
        let envelope = piik_loopback::decode_envelope(payload)
            .map_err(|_| ProtocolViolation::new("native control envelope is invalid"))?;
        let mut request = Request {
            id: envelope.id,
            kind: envelope.kind,
            report_source_cancellation: false,
        };
        let started = Instant::now();
        trace_started(&request, payload);
        let result = self.dispatch(&mut request, payload).await;
        let result = match result {
            Ok(event) => Ok(event),
            Err(Fail::Violation(reason)) => Err(ProtocolViolation::new(reason)),
            Err(Fail::Operation(error)) => Ok(Some(ControlEvent::new(request.failure(&error)))),
        };
        // Asynchronous completions are traced by their outcome only.
        if let Ok(Some(event)) = &result {
            trace_ended(&request, started, event.value.get("code"), false);
        } else if let Err(violation) = &result {
            tracing::debug!(
                target: "piik_native_control",
                event = "native-request-ended",
                request_id = %request.id,
                request_type = %piik_diagnostics::safe_text(&request.kind),
                failed = true,
                error = %violation,
                "piik-client"
            );
        }
        result
    }

    async fn dispatch(self: &Arc<Self>, request: &mut Request, payload: &[u8]) -> Handled {
        match request.kind.as_str() {
            "prepare-punch" | "run-punch" | "close-punch" => {
                self.handle_punch(request, payload).await
            }
            "prepare-publication"
            | "publication-media"
            | "publication-answer"
            | "publication-candidate"
            | "publication-layers"
            | "close-publication" => self.handle_publication(request, payload).await,
            "capture-options" => {
                let _: EnvelopeRequest =
                    strict(payload, "native capture-options request is invalid")?;
                value(CaptureOptionsResponse {
                    envelope: request.envelope("capture-options"),
                    adapters: self
                        .config
                        .capabilities
                        .adapters
                        .iter()
                        .map(AdapterView::from)
                        .collect(),
                })
            }
            "list-microphones" => {
                let _: EnvelopeRequest =
                    strict(payload, "native microphone list request is invalid")?;
                if !self.config.capabilities.microphone {
                    return Err(fail("native microphone is unavailable"));
                }
                let devices = self
                    .cancelable(self.config.sources.list_microphones())
                    .await??;
                value(MicrophoneListResponse {
                    envelope: request.envelope("microphone-list"),
                    devices,
                })
            }
            "list-sources" => {
                let _: EnvelopeRequest = strict(payload, "native list-sources request is invalid")?;
                let sources = self
                    .cancelable(self.config.sources.list_sources())
                    .await??;
                value(SourceListResponse {
                    envelope: request.envelope("source-list"),
                    sources,
                })
            }
            "source-preview" => {
                let preview: SourcePreviewRequest =
                    strict(payload, "native source-preview request is invalid")?;
                // An unavailable preview is an empty image, not a request failure.
                let data = match self
                    .cancelable(self.config.sources.preview_source(&preview.source))
                    .await?
                {
                    Ok(image) => base64::engine::general_purpose::STANDARD.encode(image),
                    Err(error) => {
                        request.failure(&error.into());
                        String::new()
                    }
                };
                value(SourcePreviewResponse {
                    envelope: request.envelope("source-preview"),
                    source_key: capture_target_key(&preview.source),
                    mime: "image/bmp",
                    data,
                })
            }
            "start-share" => self.handle_start_share(request, payload),
            "set-remote-input-peers" => {
                let peers: RemoteInputPeersRequest =
                    strict(payload, "native input endpoints are invalid")?;
                if !valid_identity(&peers.share_id) || peers.peers.len() > MAX_REMOTE_INPUT_PEERS {
                    return Err(Fail::Violation("native input endpoints are invalid"));
                }
                let host = self.current(&peers.share_id).ok_or_else(no_share)?;
                host.set_remote_input_peers(&peers.peers)?;
                ok(request.ack("remote-control-set"))
            }
            "set-remote-control-policy" => {
                let policy: RemoteControlPolicyRequest =
                    strict(payload, "native remote-control policy is invalid")?;
                if !valid_identity(&policy.share_id) {
                    return Err(Fail::Violation("native remote-control policy is invalid"));
                }
                let host = self.current(&policy.share_id).ok_or_else(no_share)?;
                // Revocation is independent of capture preparation and its slot.
                if policy.mode == "off" {
                    let apply = host.prepare_remote_input_policy(
                        &policy.mode,
                        &policy.password,
                        policy.co_play,
                    )?;
                    apply.await?;
                    return ok(request.ack("remote-control-set"));
                }
                let ack = request.response("remote-control-set");
                let prepared = host.clone();
                Ok(self.run_host_operation(&host, request, move || {
                    let apply = prepared.prepare_remote_input_policy(
                        &policy.mode,
                        &policy.password,
                        policy.co_play,
                    )?;
                    Ok(Box::pin(async move { apply.await.map(|()| ack) }))
                }))
            }
            "set-remote-control" => {
                let decision: RemoteControlRequest =
                    strict(payload, "native remote-control decision is invalid")?;
                let Some(allow) = decision
                    .allow
                    .filter(|_| valid_identities(&[&decision.share_id, &decision.request_id]))
                else {
                    return Err(Fail::Violation("native remote-control decision is invalid"));
                };
                let host = self.current(&decision.share_id).ok_or_else(no_share)?;
                host.decide_remote_input(&decision.request_id, allow)?;
                ok(request.ack("remote-control-set"))
            }
            "set-remote-control-simultaneous" => {
                let policy: RemoteControlSimultaneousRequest =
                    strict(payload, "native simultaneous control policy is invalid")?;
                let Some(enabled) = policy.enabled.filter(|_| valid_identity(&policy.share_id))
                else {
                    return Err(Fail::Violation(
                        "native simultaneous control policy is invalid",
                    ));
                };
                let host = self.current(&policy.share_id).ok_or_else(no_share)?;
                host.set_remote_input_simultaneous(enabled)?;
                ok(request.ack("remote-control-set"))
            }
            "get-remote-stop-shortcut" | "set-remote-stop-shortcut" => {
                self.handle_stop_shortcut(request, payload)
            }
            "set-share-volume" => {
                let volume: ShareVolumeRequest =
                    strict(payload, "native share volume request is invalid")?;
                let Some(level) = volume.volume.filter(|_| valid_identity(&volume.share_id)) else {
                    return Err(Fail::Violation("native share volume request is invalid"));
                };
                let host = self.current(&volume.share_id).ok_or_else(no_share)?;
                host.set_volume(level)?;
                ok(request.ack("share-volume-set"))
            }
            "set-microphone" => self.handle_microphone(request, payload).await,
            "update-share" => self.handle_update_share(request, payload).await,
            "replace-share-source" => self.handle_replace_source(request, payload),
            "prepare-edge" => {
                let edge: PrepareEdgeRequest =
                    strict(payload, "native prepare-edge request is invalid")?;
                if !valid_identities(&[&edge.share_id, &edge.connection_id]) {
                    return Err(Fail::Violation("native prepare-edge request is invalid"));
                }
                let servers = servers(&edge.ice_servers)?;
                let sdp = if let Some(host) = self
                    .current(&edge.share_id)
                    .filter(|_| edge.source_connection_id.is_empty())
                {
                    host.prepare_edge(&edge.connection_id, servers, edge.punched)
                        .await?
                } else if let Some(viewer) = self
                    .current_viewer(&edge.share_id)
                    .filter(|_| valid_identity(&edge.source_connection_id))
                {
                    viewer
                        .prepare_edge(
                            &edge.source_connection_id,
                            &edge.connection_id,
                            servers,
                            edge.punched,
                        )
                        .await?
                } else {
                    return Err(fail("native media source does not exist"));
                };
                value(EdgeOfferResponse {
                    envelope: request.envelope("edge-offer"),
                    share_id: edge.share_id,
                    connection_id: edge.connection_id,
                    sdp,
                })
            }
            "prepare-local-edge" => {
                let edge: PrepareLocalEdgeRequest =
                    strict(payload, "native prepare-local-edge request is invalid")?;
                if !valid_identities(&[&edge.share_id, &edge.connection_id]) {
                    return Err(Fail::Violation(
                        "native prepare-local-edge request is invalid",
                    ));
                }
                let sdp = if let Some(host) = self
                    .current(&edge.share_id)
                    .filter(|_| edge.source_connection_id.is_empty())
                {
                    host.prepare_local_edge(&edge.connection_id, &request.id, edge.restart)
                        .await?
                } else if let Some(viewer) = self
                    .current_viewer(&edge.share_id)
                    .filter(|_| valid_identity(&edge.source_connection_id))
                {
                    viewer
                        .prepare_local_edge(
                            &edge.source_connection_id,
                            &edge.connection_id,
                            &request.id,
                            edge.restart,
                        )
                        .await?
                } else {
                    return Err(fail("native media source does not exist"));
                };
                value(EdgeOfferResponse {
                    envelope: request.envelope("edge-offer"),
                    share_id: edge.share_id,
                    connection_id: edge.connection_id,
                    sdp,
                })
            }
            "receive-offer" => {
                let offer: ReceiveOfferRequest =
                    strict(payload, "native receive-offer request is invalid")?;
                let capacity = edge_capacity(offer.edge_capacity)
                    .filter(|_| valid_identities(&[&offer.share_id, &offer.connection_id]))
                    .ok_or(Fail::Violation("native receive-offer request is invalid"))?;
                if !valid_sdp(&offer.sdp) {
                    return Err(fail("relayed offer is invalid"));
                }
                let servers = servers(&offer.ice_servers)?;
                let viewer = self.ensure_viewer(&offer.share_id, capacity).await?;
                let result = viewer
                    .accept_offer(
                        &offer.connection_id,
                        &offer.sdp,
                        servers,
                        offer.reuse_receiver,
                        offer.punched,
                    )
                    .await?;
                value(ReceiveAnswerResponse {
                    envelope: request.envelope("receive-answer"),
                    share_id: offer.share_id,
                    connection_id: offer.connection_id,
                    sdp: result.answer_sdp,
                    audio: result.audio,
                    codec: result.codec,
                    reused: offer.reuse_receiver.then_some(result.reused),
                })
            }
            "receive-candidate" => {
                let candidate: CandidateRequest =
                    strict(payload, "native receive-candidate request is invalid")?;
                if !valid_identities(&[&candidate.share_id, &candidate.connection_id]) {
                    return Err(Fail::Violation(
                        "native receive-candidate request is invalid",
                    ));
                }
                if !valid_candidate(candidate.candidate.as_ref()) {
                    return Err(fail("relayed candidate is invalid"));
                }
                let viewer = self
                    .current_viewer(&candidate.share_id)
                    .ok_or_else(|| fail("native Viewer session does not exist"))?;
                viewer
                    .add_receiver_candidate(&candidate.connection_id, candidate.candidate)
                    .await?;
                ok(request.ack("receive-candidate-accepted"))
            }
            "close-receiver" => {
                let close: ConnectionRequest =
                    strict(payload, "native close-receiver request is invalid")?;
                if !valid_identities(&[&close.share_id, &close.connection_id]) {
                    return Err(Fail::Violation("native close-receiver request is invalid"));
                }
                if let Some(viewer) = self.current_viewer(&close.share_id) {
                    viewer.close_receiver(&close.connection_id).await;
                }
                ok(request.ack("receiver-closed"))
            }
            "stop-receive" => {
                let stop: ShareRequest = strict(payload, "native stop-receive request is invalid")?;
                if !valid_identity(&stop.share_id) {
                    return Err(Fail::Violation("native stop-receive request is invalid"));
                }
                self.stop_viewer(&stop.share_id).await;
                ok(request.ack("receive-stopped"))
            }
            "edge-answer" => {
                let answer: EdgeAnswerRequest =
                    strict(payload, "native edge-answer request is invalid")?;
                if !valid_identities(&[&answer.share_id, &answer.connection_id])
                    || (!answer.offer_id.is_empty() && !valid_identity(&answer.offer_id))
                {
                    return Err(Fail::Violation("native edge-answer request is invalid"));
                }
                if !valid_sdp(&answer.sdp) {
                    return Err(fail("relayed answer is invalid"));
                }
                let media = self
                    .current_media(&answer.share_id)
                    .ok_or_else(|| fail("native media session does not exist"))?;
                media
                    .set_answer(&answer.connection_id, &answer.sdp, &answer.offer_id)
                    .await?;
                ok(request.ack("edge-answer-accepted"))
            }
            "edge-candidate" => {
                let candidate: CandidateRequest =
                    strict(payload, "native edge-candidate request is invalid")?;
                if !valid_identities(&[&candidate.share_id, &candidate.connection_id]) {
                    return Err(Fail::Violation("native edge-candidate request is invalid"));
                }
                if !valid_candidate(candidate.candidate.as_ref()) {
                    return Err(fail("relayed candidate is invalid"));
                }
                let media = self
                    .current_media(&candidate.share_id)
                    .ok_or_else(|| fail("native media session does not exist"))?;
                media
                    .add_candidate(&candidate.connection_id, candidate.candidate)
                    .await?;
                ok(request.ack("edge-candidate-accepted"))
            }
            "close-edge" => {
                let close: ConnectionRequest =
                    strict(payload, "native close-edge request is invalid")?;
                if !valid_identities(&[&close.share_id, &close.connection_id]) {
                    return Err(Fail::Violation("native close-edge request is invalid"));
                }
                if let Some(media) = self.current_media(&close.share_id) {
                    media.close_edge(&close.connection_id).await;
                }
                ok(request.ack("edge-closed"))
            }
            "stop-share" => {
                let stop: ShareRequest = strict(payload, "native stop-share request is invalid")?;
                if !valid_identity(&stop.share_id) {
                    return Err(Fail::Violation("native stop-share request is invalid"));
                }
                self.stop_share(&stop.share_id).await?;
                ok(request.ack("share-stopped"))
            }
            "pause-share" => {
                let pause: PauseShareRequest =
                    strict(payload, "native pause-share request is invalid")?;
                if !valid_identity(&pause.share_id) {
                    return Err(Fail::Violation("native pause-share request is invalid"));
                }
                let host = self.current(&pause.share_id).ok_or_else(no_share)?;
                host.set_paused(pause.paused);
                tracing::debug!(
                    target: "piik_native_control",
                    event = "native-share-paused",
                    request_id = %request.id,
                    share = %piik_diagnostics::id(&pause.share_id),
                    paused = pause.paused,
                    "piik-client"
                );
                ok(request.ack("share-paused"))
            }
            _ => Err(Fail::Violation("native control message is unsupported")),
        }
    }

    /// Races a source operation against session close.
    async fn cancelable<T>(&self, operation: impl Future<Output = T>) -> Result<T, Fail> {
        tokio::select! {
            result = operation => Ok(result),
            () = self.cancel.cancelled() => Err(fail("native control session is closed")),
        }
    }

    fn validate_audio_exclusion(
        &self,
        source: &CaptureTarget,
        audio: bool,
        excluded: Option<&CaptureTarget>,
    ) -> Result<(), Fail> {
        let Some(excluded) = excluded else {
            return Ok(());
        };
        if !audio || source.kind != "display" || excluded.kind != "window" || !excluded.valid() {
            return Err(Fail::Violation("native audio exclusion target is invalid"));
        }
        let capabilities = &self.config.capabilities;
        if !capabilities.summary().process_audio_exclusion || !capabilities.system_audio {
            return Err(fail("native audio exclusion is unavailable"));
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn validate_audio_exclusion_for_test(
        &self,
        source: &CaptureTarget,
        audio: bool,
        excluded: Option<&CaptureTarget>,
    ) -> Result<(), Fail> {
        self.validate_audio_exclusion(source, audio, excluded)
    }
}

fn trace_started(request: &Request, payload: &[u8]) {
    if !tracing::enabled!(target: "piik_native_control", tracing::Level::DEBUG) {
        return;
    }
    let identity: TraceIdentity = serde_json::from_slice(payload).unwrap_or_default();
    let id = |value: &Option<String>| piik_diagnostics::id(value.as_deref().unwrap_or_default());
    tracing::debug!(
        target: "piik_native_control",
        event = "native-request-started",
        request_id = %request.id,
        request_type = %piik_diagnostics::safe_text(&request.kind),
        share = %id(&identity.share_id),
        connection = %id(&identity.connection_id),
        source_connection = %id(&identity.source_connection_id),
        publication = %id(&identity.publication_generation),
        "piik-client"
    );
}

pub(crate) fn trace_ended(request: &Request, started: Instant, code: Option<&Value>, failed: bool) {
    let code = code.and_then(Value::as_str).unwrap_or_default();
    tracing::debug!(
        target: "piik_native_control",
        event = "native-request-ended",
        request_id = %request.id,
        request_type = %piik_diagnostics::safe_text(&request.kind),
        duration_ms = started.elapsed().as_millis() as u64,
        failed = failed || !code.is_empty(),
        code,
        "piik-client"
    );
}

/// `{"prediction": …}` for punch-prepared; `null` when none was observed.
pub(crate) fn prediction_value(prediction: Option<&piik_protocol::PunchPrediction>) -> Value {
    prediction.map_or(Value::Null, |prediction| {
        json!({"address": prediction.address, "port": prediction.port, "step": prediction.step})
    })
}
