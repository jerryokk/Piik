//! SFU publication commands.

use crate::session::{Fail, Inner, Request, fail};
use crate::wire::*;

use super::{Handled, no_share, ok, servers, strict, value};

impl Inner {
    pub(super) async fn handle_publication(&self, request: &Request, payload: &[u8]) -> Handled {
        let identity: PublicationIdentity = serde_json::from_slice(payload)
            .ok()
            .filter(|identity: &PublicationIdentity| {
                valid_identities(&[
                    &identity.share_id,
                    &identity.publication_generation,
                    &identity.connection_id,
                ])
            })
            .ok_or(Fail::Violation("native publication identity is invalid"))?;
        // Validate the command before looking up a possibly retired target.
        enum Command {
            Prepare(Vec<Vec<String>>),
            Close,
            Media,
            Answer(String),
            Candidate(Option<IceCandidateInit>),
            Layers(usize),
        }
        const MALFORMED: &str = "native control message is invalid";
        let command = match request.kind.as_str() {
            "prepare-publication" => {
                let prepare: PreparePublicationRequest = strict(payload, MALFORMED)?;
                Command::Prepare(servers(&prepare.ice_servers)?)
            }
            "close-publication" | "publication-media" => {
                let _: PublicationRequest = strict(payload, MALFORMED)?;
                if request.kind == "close-publication" {
                    Command::Close
                } else {
                    Command::Media
                }
            }
            "publication-answer" => {
                let answer: PublicationAnswerRequest =
                    strict(payload, "native publication answer is invalid")?;
                if !valid_sdp(&answer.sdp) {
                    return Err(fail("relayed publication answer is invalid"));
                }
                Command::Answer(answer.sdp)
            }
            "publication-candidate" => {
                let candidate: PublicationCandidateRequest =
                    strict(payload, "native publication candidate is invalid")?;
                if !valid_candidate(candidate.candidate.as_ref()) {
                    return Err(fail("relayed publication candidate is invalid"));
                }
                Command::Candidate(candidate.candidate)
            }
            _ => {
                let layers: PublicationLayersRequest =
                    strict(payload, "native publication layer demand is invalid")?;
                let count = layers
                    .active_count
                    .filter(|count| (0..=3).contains(count))
                    .and_then(|count| usize::try_from(count).ok())
                    .ok_or(Fail::Violation(
                        "native publication layer demand is invalid",
                    ))?;
                Command::Layers(count)
            }
        };
        let generation = identity.publication_generation.as_str();
        let connection = identity.connection_id.as_str();
        let host = self.current(&identity.share_id);
        if let Command::Close = command {
            if let Some(host) = host {
                host.close_publication(generation, connection);
            }
            return ok(request.ack("publication-closed"));
        }
        let host = host.ok_or_else(no_share)?;
        let response = |kind: &str, sdp: String, media| PublicationResponse {
            envelope: request.envelope(kind),
            share_id: identity.share_id.clone(),
            publication_generation: identity.publication_generation.clone(),
            connection_id: identity.connection_id.clone(),
            sdp,
            media,
        };
        if let Command::Prepare(servers) = command {
            let (sdp, media) = host
                .prepare_publication(generation, connection, servers)
                .await?;
            return value(response("publication-offer", sdp, media));
        }
        let publication = host.publication(generation, connection)?;
        let accepted = match command {
            Command::Media => {
                return value(response(
                    "publication-media",
                    String::new(),
                    publication.media(),
                ));
            }
            Command::Answer(sdp) => {
                publication.set_answer(&sdp).await?;
                "publication-answer-accepted"
            }
            Command::Candidate(candidate) => {
                publication.add_remote_candidate(candidate).await?;
                "publication-candidate-accepted"
            }
            Command::Layers(count) => {
                publication.set_active_count(count).await?;
                "publication-layers-accepted"
            }
            Command::Prepare(_) | Command::Close => unreachable!("handled above"),
        };
        ok(request.ack(accepted))
    }
}
