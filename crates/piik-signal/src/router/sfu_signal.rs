//! Embedded SFU signaling: authorization, SDP/ICE relay, subscriber prepare
//! and physical media callbacks.

use std::time::{Duration, Instant};

use piik_protocol::{Role, ServerMessage, SfuMedia, SfuSignalKind, SfuSignalMessage};
use piik_route::{DemandReason, OperationSnapshot, PublicationPlan, RouteSnapshot, UpstreamKind};

use crate::router::pump::{resource_fence, subscription_fence};
use crate::router::{RouteParticipant, route_debug};
use crate::runtime::spawn_blocking;
use crate::server::Core;
use crate::voice::signal::{ice_from_wire, ice_to_wire, sdp_from_wire, sdp_to_wire};
use piik_sfu::runtime::{
    LayerFormat, MediaError, MediaEvent, PeerConnectionState, PublicationMedia, SdpType,
    SessionDescription, SubscriptionFence,
};

fn publication_media(media: &SfuMedia) -> PublicationMedia {
    PublicationMedia {
        codec: media.codec.as_str().to_owned(),
        rids: media.layers.iter().map(|layer| layer.rid.clone()).collect(),
        formats: media
            .layers
            .iter()
            .map(|layer| LayerFormat {
                width: u32::try_from(layer.width).unwrap_or(0),
                height: u32::try_from(layer.height).unwrap_or(0),
                bitrate: u32::try_from(layer.bitrate).unwrap_or(0),
            })
            .collect(),
        audio: media.audio,
        audio_bitrate: u32::try_from(media.audio_bitrate).unwrap_or(0),
    }
}

/// The phase and revision a connection's failure report belongs to.
pub(crate) fn sfu_connection_phase(
    snapshot: &RouteSnapshot,
    role: Role,
    peer_id: &str,
    generation: &str,
    connection_id: &str,
) -> (bool, i64) {
    if let Some(op) = snapshot.operation.as_ref()
        && let Some(current) = op.current.as_ref()
        && current.tuple.kind == UpstreamKind::Sfu
    {
        let publication = snapshot.host_publication.as_ref();
        let viewer = role == Role::Viewer
            && op.child_peer_id == peer_id
            && current.connection_id == connection_id;
        let host = role == Role::Host
            && ((connection_id == generation
                && publication.is_none_or(|p| p.generation != generation))
                || (snapshot.room_media
                    && op.reason == DemandReason::PublicationAttach
                    && publication.is_some_and(|p| {
                        p.generation == generation && p.connection_id == connection_id
                    })));
        if viewer || host {
            return (true, current.revision);
        }
    }
    (false, snapshot.revision)
}

fn signal(
    kind: SfuSignalKind,
    revision: i64,
    generation: &str,
    connection_id: &str,
) -> SfuSignalMessage {
    SfuSignalMessage {
        revision,
        publication_generation: generation.to_owned(),
        connection_id: connection_id.to_owned(),
        kind,
        description: None,
        candidate: None,
        media: None,
        active_count: None,
    }
}

impl Core {
    pub(crate) fn sfu_signal_authorized(
        &self,
        participant: &RouteParticipant,
        message: &SfuSignalMessage,
    ) -> (SubscriptionFence, bool) {
        let mut fence = SubscriptionFence {
            resource: piik_sfu::runtime::ResourceFence {
                room_id: participant.room_id.clone(),
                share_generation: self.share_generation(&participant.room_id),
                publication_generation: message.publication_generation.clone(),
            },
            viewer_peer_id: String::new(),
            connection_id: message.connection_id.clone(),
        };
        let connected = self
            .connected_peer(&participant.room_id, &participant.peer_id)
            .is_some_and(|peer| peer.session_id == participant.session_id);
        let Some(fallback) = self.router.sfu.as_ref() else {
            return (fence, false);
        };
        let Some(snapshot) = self.snapshot(&participant.room_id) else {
            return (fence, false);
        };
        if self.router.closing || !connected {
            return (fence, false);
        }
        let room_media = self.room_media(&participant.room_id);
        if room_media {
            if snapshot.video_generation.is_empty() {
                return (fence, false);
            }
            fence.resource.share_generation = self
                .room_rt(&participant.room_id)
                .map(|rm| rm.media_generation.clone())
                .unwrap_or_default();
            // Opting into voice during an existing share preserves its
            // admitted fence, even though later publications are room-scoped.
            if let Some(resource) = snapshot
                .host_publication
                .as_ref()
                .filter(|p| p.generation == message.publication_generation)
                .and_then(|p| p.resource.as_ref())
            {
                fence.resource = resource_fence(resource);
            }
        }
        let current = snapshot
            .operation
            .as_ref()
            .and_then(|op| op.current.as_ref());
        let latest_revision =
            current.map_or(snapshot.revision, |c| snapshot.revision.max(c.revision));
        if message.revision > latest_revision || self.sfu_video_retiring(&fence.resource) {
            return (fence, false);
        }
        let admission = &fallback.admission;
        let operation = snapshot.operation.as_ref();
        if participant.role == Role::Host {
            if let Some(publication) = snapshot.host_publication.as_ref()
                && publication.physical_active
                && publication.generation == message.publication_generation
                && publication.connection_id == message.connection_id
                && publication.host_session_id == participant.session_id
            {
                return (fence.clone(), admission.has_publication(&fence.resource));
            }
            let allowed = participant.peer_id == self.host_peer_of(&participant.room_id)
                && current.is_some_and(|c| {
                    c.tuple.kind == UpstreamKind::Sfu
                        && c.tuple.publication != Some(PublicationPlan::Reuse)
                        && c.revision == message.revision
                })
                && message.connection_id == message.publication_generation
                && admission.has_publication(&fence.resource);
            return (fence, allowed);
        }
        fence.viewer_peer_id = participant.peer_id.clone();
        if room_media
            && let Some(edge) = snapshot.upstream_by_viewer.get(&participant.peer_id)
            && edge.kind == UpstreamKind::Sfu
            && edge.physical_active
            && edge.child_session_id == participant.session_id
            && edge.publication_generation == message.publication_generation
        {
            let active = edge.video_ready && edge.video_connection_id == message.connection_id;
            let attaching = operation.is_some_and(|op| {
                op.reason == DemandReason::PublicationAttach
                    && op.child_peer_id == participant.peer_id
                    && op.child_session_id == participant.session_id
                    && op.attach_connection_id == edge.connection_id
                    && op.current.as_ref().is_some_and(|c| {
                        c.tuple.kind == UpstreamKind::Sfu
                            && c.revision == message.revision
                            && c.connection_id == message.connection_id
                    })
            });
            if active || attaching {
                let Some(resource) = edge.resource.as_ref() else {
                    return (fence, false);
                };
                let fence = subscription_fence(resource);
                let allowed = admission.has_subscription(&fence);
                return (fence, allowed);
            }
        }
        if !admission.has_subscription(&fence) {
            return (fence, false);
        }
        if !room_media
            && snapshot
                .upstream_by_viewer
                .get(&participant.peer_id)
                .is_some_and(|edge| {
                    edge.physical_active
                        && edge.kind == UpstreamKind::Sfu
                        && edge.publication_generation == message.publication_generation
                        && edge.connection_id == message.connection_id
                        && edge.child_session_id == participant.session_id
                })
        {
            return (fence, true);
        }
        let allowed = operation.is_some_and(|op| {
            op.child_peer_id == participant.peer_id
                && op.child_session_id == participant.session_id
                && op.current.as_ref().is_some_and(|c| {
                    c.tuple.kind == UpstreamKind::Sfu
                        && c.revision == message.revision
                        && c.connection_id == message.connection_id
                })
        });
        (fence, allowed)
    }

    /// Each session's reader serializes its SDP/ICE; the media effect runs
    /// unlocked before the reader takes its next frame.
    pub(crate) fn handle_sfu_signal(
        &mut self,
        participant: &RouteParticipant,
        message: SfuSignalMessage,
    ) {
        let (fence, allowed) = self.sfu_signal_authorized(participant, &message);
        if !allowed {
            return;
        }
        let media = self
            .router
            .sfu
            .as_ref()
            .expect("SFU fallback")
            .media
            .clone();
        let host = participant.role == Role::Host;
        let mut prepared: Result<bool, MediaError> = Ok(false);
        // Register the physical owner before releasing authority: retirement
        // must see this handle even when the SDP effect has not started.
        if message.kind == SfuSignalKind::Description
            && host
            && message
                .description
                .as_ref()
                .is_some_and(|d| d.kind == piik_protocol::SdpType::Offer)
            && !media.has_publication(&fence.resource, &message.connection_id)
        {
            let Some(metadata) = message.media.as_ref() else {
                return;
            };
            prepared = media
                .prepare_publication(
                    &fence.resource,
                    &message.connection_id,
                    &publication_media(metadata),
                )
                .map(|()| true);
        }
        let locker = self.locker.clone();
        let participant = participant.clone();
        self.defer_io(move || {
            let mut description: Option<SessionDescription> = None;
            let result = prepared.and_then(|new_publication| {
                let effect = match message.kind {
                    SfuSignalKind::Media if host => {
                        message.media.as_ref().map_or(Ok(()), |metadata| {
                            media.update_publication(
                                &fence.resource,
                                &message.connection_id,
                                &publication_media(metadata),
                            )
                        })
                    }
                    SfuSignalKind::Description => {
                        match message.description.as_ref().map(sdp_from_wire) {
                            Some(incoming) if host => {
                                if incoming.sdp_type == SdpType::Offer {
                                    media
                                        .accept_publisher_offer(
                                            &fence.resource,
                                            &message.connection_id,
                                            incoming,
                                        )
                                        .map(|answer| description = Some(answer))
                                } else {
                                    Ok(())
                                }
                            }
                            Some(incoming) if incoming.sdp_type == SdpType::Answer => media
                                .apply_subscriber_answer(&fence, &message.connection_id, incoming),
                            _ => Ok(()),
                        }
                    }
                    SfuSignalKind::Candidate => {
                        let candidate = message
                            .candidate
                            .as_ref()
                            .map(ice_from_wire)
                            .unwrap_or_default();
                        if host {
                            media.add_publisher_ice(
                                &fence.resource,
                                &message.connection_id,
                                candidate,
                            )
                        } else {
                            media.add_subscriber_ice(&fence, &message.connection_id, candidate)
                        }
                    }
                    SfuSignalKind::Subscribe if !host => media
                        .restart_subscriber(&fence, &message.connection_id)
                        .map(|offer| description = Some(offer)),
                    _ => Ok(()),
                };
                effect.map(|()| new_publication)
            });
            locker.with(move |core| {
                core.finish_sfu_signal(&participant, &message, &fence, result, description)
            });
        });
    }

    fn finish_sfu_signal(
        &mut self,
        participant: &RouteParticipant,
        message: &SfuSignalMessage,
        fence: &SubscriptionFence,
        result: Result<bool, MediaError>,
        description: Option<SessionDescription>,
    ) {
        let new_publication = *result.as_ref().unwrap_or(&false);
        if !self.sfu_signal_authorized(participant, message).1 {
            if new_publication {
                self.retire_sfu_video_publication(&fence.resource, &message.connection_id);
            }
            return;
        }
        if let Err(error) = result {
            route_debug!(self, &participant.room_id, "sfu-signal-failed",
                "participant" => self.debug_peer(&participant.room_id, &participant.peer_id),
                "role" => participant.role.as_str(),
                "kind" => message.kind.as_str(),
                "error" => piik_diagnostics::safe_text(&error.to_string()));
            if message.kind == SfuSignalKind::Candidate
                && matches!(error, MediaError::CandidateRejected(_))
            {
                return;
            }
            self.fail_sfu_connection(participant, message);
            return;
        }
        if let Some(description) = description.filter(|d| !d.sdp.is_empty()) {
            self.send_sfu_description(&participant.session_id, message, &description);
        }
        if new_publication
            && let Some(op) = self
                .controller(&participant.room_id)
                .and_then(|c| c.operation())
            && op
                .current
                .as_ref()
                .is_some_and(|c| c.tuple.kind == UpstreamKind::Sfu)
        {
            self.prepare_sfu_subscriber(&participant.room_id, &op, &message.publication_generation);
        }
    }

    /// Prepares the subscriber offer unlocked, bounded by the operation deadline.
    pub(crate) fn prepare_sfu_subscriber(
        &mut self,
        room_id: &str,
        operation: &OperationSnapshot,
        generation: &str,
    ) {
        let Some(current) = operation.current.as_ref() else {
            return;
        };
        let message = signal(
            SfuSignalKind::Description,
            current.revision,
            generation,
            &current.connection_id,
        );
        let participant = RouteParticipant::new(
            room_id,
            Role::Viewer,
            &operation.child_peer_id,
            &operation.child_session_id,
        );
        let (fence, allowed) = self.sfu_signal_authorized(&participant, &message);
        if !allowed {
            return;
        }
        let media = self
            .router
            .sfu
            .as_ref()
            .expect("SFU fallback")
            .media
            .clone();
        let wait = (operation.deadline_at_ms - self.now())
            .max(0)
            .unsigned_abs();
        let deadline = Instant::now() + Duration::from_millis(wait);
        self.router.inflight += 1;
        let locker = self.locker.clone();
        spawn_blocking(move || {
            let result = media.prepare_subscriber(deadline, &fence, &message.connection_id);
            locker.with(move |core| {
                core.router.inflight -= 1;
                if !core.sfu_signal_authorized(&participant, &message).1 {
                    let media = media.clone();
                    let connection_id = message.connection_id.clone();
                    spawn_blocking(move || {
                        let _ = media.close_subscription(&fence, &connection_id);
                    });
                    return;
                }
                match result {
                    Err(error) => {
                        route_debug!(core, &participant.room_id, "sfu-subscriber-prepare-failed",
                            "participant" => core.debug_peer(&participant.room_id, &participant.peer_id),
                            "error" => piik_diagnostics::safe_text(&error.to_string()));
                        core.fail_sfu_connection(&participant, &message);
                    }
                    Ok(description) => core.send_sfu_description(&participant.session_id, &message, &description),
                }
            });
        });
    }

    fn send_sfu_description(
        &mut self,
        session_id: &str,
        message: &SfuSignalMessage,
        description: &SessionDescription,
    ) {
        let mut reply = message.clone();
        reply.kind = SfuSignalKind::Description;
        reply.description = sdp_to_wire(description);
        reply.media = None;
        reply.candidate = None;
        self.router_send(session_id, ServerMessage::SfuSignal(Box::new(reply)));
    }

    fn fail_sfu_connection(&mut self, participant: &RouteParticipant, message: &SfuSignalMessage) {
        let Some(snapshot) = self.snapshot(&participant.room_id) else {
            return;
        };
        let mut connection_id = message.connection_id.clone();
        let (prepare, revision) = sfu_connection_phase(
            &snapshot,
            participant.role,
            &participant.peer_id,
            &message.publication_generation,
            &connection_id,
        );
        if prepare
            && let Some(current) = snapshot
                .operation
                .as_ref()
                .and_then(|op| op.current.as_ref())
        {
            connection_id = current.connection_id.clone();
        }
        self.handle_route_failed(participant, revision, prepare, Some(&connection_id));
    }

    /// Media callbacks identify physical handles; signaling reconnect does
    /// not retire them.
    pub(crate) fn handle_sfu_media_event(&mut self, event: MediaEvent) {
        let room_id = event.fence.room_id.clone();
        let Some(snapshot) = self.snapshot(&room_id) else {
            return;
        };
        let room_media = self.room_media(&room_id);
        if self.router.closing
            || (!room_media && self.share_generation(&room_id) != event.fence.share_generation)
        {
            return;
        }
        let (peer_id, role) = if event.viewer_peer_id.is_empty() {
            (self.host_peer_of(&room_id), Role::Host)
        } else {
            (event.viewer_peer_id.clone(), Role::Viewer)
        };
        let peer = self.connected_peer(&room_id, &peer_id);
        if room_media {
            let mut expected = self
                .room_rt(&room_id)
                .map(|rm| rm.media_generation.clone())
                .unwrap_or_default();
            if let Some(resource) = snapshot
                .host_publication
                .as_ref()
                .filter(|p| p.generation == event.fence.publication_generation)
                .and_then(|p| p.resource.as_ref())
            {
                expected = resource.share_generation.clone();
            }
            if snapshot.video_generation.is_empty() || expected != event.fence.share_generation {
                return;
            }
        }
        let mut participant = RouteParticipant::new(
            &room_id,
            role,
            &peer_id,
            &peer
                .as_ref()
                .map(|p| p.session_id.clone())
                .unwrap_or_default(),
        );
        let (prepare, revision) = sfu_connection_phase(
            &snapshot,
            role,
            &peer_id,
            &event.fence.publication_generation,
            &event.connection_id,
        );
        let mut message = signal(
            SfuSignalKind::Candidate,
            revision,
            &event.fence.publication_generation,
            &event.connection_id,
        );
        if peer.is_none() && role == Role::Host && event.state == PeerConnectionState::Failed {
            if let Some(publication) = snapshot.host_publication.as_ref()
                && publication.generation == event.fence.publication_generation
                && publication.connection_id == event.connection_id
            {
                participant.session_id = publication.host_session_id.clone();
                self.fail_sfu_connection(&participant, &message);
            }
            return;
        }
        if !self.sfu_signal_authorized(&participant, &message).1 {
            return;
        }
        if let Some(active) = event.active_count {
            message.kind = SfuSignalKind::Layers;
            message.active_count = Some(active);
            self.router_send(
                &participant.session_id,
                ServerMessage::SfuSignal(Box::new(message)),
            );
            return;
        }
        if event.state != PeerConnectionState::Unknown {
            route_debug!(self, &room_id, "sfu-transport-state",
                "participant" => self.debug_peer(&room_id, &participant.peer_id),
                "connectionId" => piik_diagnostics::id(&event.connection_id),
                "publicationGeneration" => piik_diagnostics::id(&event.fence.publication_generation),
                "role" => role.as_str(),
                "phase" => if prepare { "prepare" } else { "active" },
                "revision" => revision,
                "state" => event.state.as_str());
            if event.state == PeerConnectionState::Failed {
                self.fail_sfu_connection(&participant, &message);
            }
            return;
        }
        message.candidate = event.candidate.as_ref().map(ice_to_wire);
        self.router_send(
            &participant.session_id,
            ServerMessage::SfuSignal(Box::new(message)),
        );
    }
}
