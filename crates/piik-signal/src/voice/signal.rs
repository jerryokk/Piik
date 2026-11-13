//! Voice SDP/ICE relay, transport evidence and voice-driven candidate settlement.

use std::collections::HashSet;

use piik_protocol::{
    IceCandidate, Nullable, Role, RoomVoiceErrorCode, RoomVoiceSignal, RoomVoiceSignalBody,
    SdpType, ServerMessage, VoiceTransportState,
};
use piik_route::{
    CandidateGuard, CandidateProof, EdgeGuard, PublicationGuard, PublicationPlan, UpstreamKind,
};

use crate::router::RouteParticipant;
use crate::server::Core;
use crate::session::AuthState;
use piik_sfu::runtime::{
    IceCandidateInit, PeerConnectionState, SessionDescription, VoiceDescription, VoiceMediaEvent,
    VoiceSignal, VoiceTrack,
};

pub(crate) fn ice_from_wire(candidate: &IceCandidate) -> IceCandidateInit {
    IceCandidateInit {
        candidate: candidate.candidate.clone(),
        sdp_mid: candidate.sdp_mid.value().cloned(),
        sdp_m_line_index: candidate
            .sdp_m_line_index
            .value()
            .and_then(|index| u16::try_from(*index).ok()),
        username_fragment: candidate.username_fragment.value().cloned(),
    }
}

pub(crate) fn ice_to_wire(candidate: &IceCandidateInit) -> IceCandidate {
    let nullable = |value: &Option<String>| value.clone().map_or(Nullable::Null, Nullable::Value);
    IceCandidate {
        candidate: candidate.candidate.clone(),
        sdp_mid: nullable(&candidate.sdp_mid),
        sdp_m_line_index: candidate
            .sdp_m_line_index
            .map_or(Nullable::Null, |index| Nullable::Value(i64::from(index))),
        username_fragment: nullable(&candidate.username_fragment),
    }
}

pub(crate) fn sdp_from_wire(description: &piik_protocol::SessionDescription) -> SessionDescription {
    SessionDescription {
        sdp_type: match description.kind {
            SdpType::Offer => piik_sfu::runtime::SdpType::Offer,
            SdpType::Answer => piik_sfu::runtime::SdpType::Answer,
        },
        sdp: description.sdp.clone(),
    }
}

/// `None` for Pion's pranswer and rollback, which the wire cannot carry.
pub(crate) fn sdp_to_wire(
    description: &SessionDescription,
) -> Option<piik_protocol::SessionDescription> {
    let kind = match description.sdp_type {
        piik_sfu::runtime::SdpType::Offer => SdpType::Offer,
        piik_sfu::runtime::SdpType::Answer => SdpType::Answer,
        _ => return None,
    };
    Some(piik_protocol::SessionDescription {
        kind,
        sdp: description.sdp.clone(),
    })
}

fn voice_signal_from_wire(body: &RoomVoiceSignalBody) -> Option<VoiceSignal> {
    Some(match body {
        RoomVoiceSignalBody::Description(description) => VoiceSignal {
            description: Some(VoiceDescription {
                revision: u64::try_from(description.revision).ok()?,
                sdp: sdp_from_wire(&description.sdp),
                tracks: description
                    .tracks
                    .iter()
                    .map(|track| VoiceTrack {
                        mid: track.mid.clone(),
                        publication_id: track.publication_id.clone(),
                    })
                    .collect(),
            }),
            ..VoiceSignal::default()
        },
        RoomVoiceSignalBody::Candidate(candidate) => VoiceSignal {
            candidate: Some(ice_from_wire(candidate)),
            ..VoiceSignal::default()
        },
        RoomVoiceSignalBody::Renegotiate => VoiceSignal {
            renegotiate: true,
            ..VoiceSignal::default()
        },
    })
}

impl Core {
    /// The current voice edge `id` the peer's exact session owns, by logical ID.
    pub(crate) fn voice_connection(
        &self,
        room_id: &str,
        peer_id: &str,
        session_id: &str,
        id: &str,
    ) -> Option<String> {
        let voice = self.voices.get(room_id)?;
        voice
            .connections
            .values()
            .find(|edge| {
                edge.id == id
                    && ((edge.parent == peer_id && edge.parent_session == session_id)
                        || (edge.child == peer_id && edge.child_session == session_id))
            })
            .map(|edge| edge.logical_id.clone())
    }

    pub(crate) fn handle_room_voice_signal(
        &mut self,
        key: u64,
        auth: &AuthState,
        mut message: RoomVoiceSignal,
    ) {
        let session_id = self.session_id_of(key);
        if self.voice_session(&auth.room_id, &auth.peer_id) != Some(key) {
            return;
        }
        let Some(logical) = self.voice_connection(
            &auth.room_id,
            &auth.peer_id,
            &session_id,
            &message.connection_id,
        ) else {
            return;
        };
        if !self.voice_allowed(&auth.room_id, &auth.peer_id) {
            return;
        }
        let edge = &self.voices[&auth.room_id].connections[&logical];
        if edge.transport == "sfu" {
            let Some(media) = self.voice_runtime() else {
                return;
            };
            let plan = edge.sfu_plan.as_ref().map(|(id, _)| *id);
            let edge_id = edge.id.clone();
            let Some(incoming) = voice_signal_from_wire(&message.body) else {
                self.voice_error(key, RoomVoiceErrorCode::ConnectionUnavailable);
                return;
            };
            let locker = self.locker.clone();
            let (room_id, peer_id) = (auth.room_id.clone(), auth.peer_id.clone());
            // The SDP effect runs unlocked, before this reader's next frame.
            self.defer_io(move || {
                let runtime = media.voice().expect("voice runtime");
                let result =
                    runtime.signal_voice(&room_id, &peer_id, &session_id, &edge_id, incoming);
                if result.is_err() {
                    locker.with(|core| {
                        let current = core
                            .voice_connection(&room_id, &peer_id, &session_id, &edge_id)
                            .filter(|current| *current == logical)
                            .and_then(|logical| {
                                core.voices.get(&room_id)?.connections.get(&logical)
                            })
                            .is_some_and(|edge| edge.sfu_plan.as_ref().map(|(id, _)| *id) == plan);
                        if current {
                            core.voice_error(key, RoomVoiceErrorCode::ConnectionUnavailable);
                        }
                    });
                }
            });
            return;
        }
        let other = if auth.peer_id == edge.parent {
            edge.child.clone()
        } else {
            edge.parent.clone()
        };
        let is_parent = auth.peer_id == edge.parent;
        let is_child = auth.peer_id == edge.child;
        let own_sends = edge.send.get(&auth.peer_id).cloned().unwrap_or_default();
        let Some(target) = self.voice_session(&auth.room_id, &other) else {
            return;
        };
        if let RoomVoiceSignalBody::Description(description) = &mut message.body {
            if (description.sdp.kind == SdpType::Offer) != is_parent {
                return;
            }
            let mut seen = HashSet::new();
            let mut tracks = Vec::with_capacity(description.tracks.len());
            for track in &description.tracks {
                // MIDs and publication IDs share one namespace.
                if seen.contains(&track.mid) || seen.contains(&track.publication_id) {
                    self.voice_error(key, RoomVoiceErrorCode::NotAllowed);
                    return;
                }
                seen.insert(track.mid.clone());
                seen.insert(track.publication_id.clone());
                if own_sends.contains(&track.publication_id) {
                    tracks.push(track.clone());
                }
            }
            // Leaving voice can overtake an offer in flight; finish its SDP
            // transaction without reviving a retired publication binding.
            description.tracks = tracks;
        }
        if message.body == RoomVoiceSignalBody::Renegotiate && !is_child {
            return;
        }
        self.send(target, &ServerMessage::RoomVoiceSignal(Box::new(message)));
    }

    pub(crate) fn on_voice_media_event(&mut self, event: VoiceMediaEvent) {
        if self.closing {
            return;
        }
        let Some(logical) = self.voice_connection(
            &event.room_id,
            &event.peer_id,
            &event.session_id,
            &event.connection_id,
        ) else {
            return;
        };
        let edge = &self.voices[&event.room_id].connections[&logical];
        if edge.transport != "sfu" {
            return;
        }
        let edge_id = edge.id.clone();
        let Some(key) = self.voice_session(&event.room_id, &event.peer_id) else {
            return;
        };
        let body = if let Some(description) = &event.description {
            sdp_to_wire(&description.sdp).map(|sdp| {
                RoomVoiceSignalBody::Description(piik_protocol::VoiceDescription {
                    revision: i64::try_from(description.revision).unwrap_or(i64::MAX),
                    sdp,
                    tracks: description
                        .tracks
                        .iter()
                        .map(|track| piik_protocol::VoiceTrack {
                            mid: track.mid.clone(),
                            publication_id: track.publication_id.clone(),
                        })
                        .collect(),
                })
            })
        } else {
            event
                .candidate
                .as_ref()
                .map(|candidate| RoomVoiceSignalBody::Candidate(ice_to_wire(candidate)))
        };
        if let Some(body) = body {
            self.send(
                key,
                &ServerMessage::RoomVoiceSignal(Box::new(RoomVoiceSignal {
                    connection_id: edge_id,
                    body,
                })),
            );
        }
        if event.state == PeerConnectionState::Connected {
            if event
                .is_current_state
                .as_ref()
                .is_some_and(|current| !current())
            {
                return;
            }
            if let Some(edge) = self.voice_edge_mut(&event.room_id, &logical) {
                edge.connected.insert(String::new(), true);
            }
            self.commit_voice_candidate(&event.room_id);
            self.sync_room_voice(&event.room_id);
        }
        if event.state == PeerConnectionState::Failed {
            self.fail_voice_connection(&event.room_id, &logical);
        }
    }

    pub(crate) fn voice_edge_mut(
        &mut self,
        room_id: &str,
        logical: &str,
    ) -> Option<&mut crate::voice::VoiceEdge> {
        self.voices.get_mut(room_id)?.connections.get_mut(logical)
    }

    pub(crate) fn handle_room_voice_transport(
        &mut self,
        key: u64,
        auth: &AuthState,
        connection_id: &str,
        state: VoiceTransportState,
    ) {
        let session_id = self.session_id_of(key);
        if self.voice_session(&auth.room_id, &auth.peer_id) != Some(key) {
            return;
        }
        let Some(logical) =
            self.voice_connection(&auth.room_id, &auth.peer_id, &session_id, connection_id)
        else {
            return;
        };
        if state == VoiceTransportState::Failed {
            self.fail_voice_connection(&auth.room_id, &logical);
            return;
        }
        if let Some(edge) = self.voice_edge_mut(&auth.room_id, &logical) {
            edge.connected.insert(auth.peer_id.clone(), true);
        }
        self.commit_voice_candidate(&auth.room_id);
        self.sync_room_voice(&auth.room_id);
    }

    pub(crate) fn commit_voice_candidate(&mut self, room_id: &str) {
        if !self.room_media(room_id) {
            return;
        }
        let Some(op) = self.controller(room_id).and_then(|c| c.operation()) else {
            return;
        };
        let Some(current) = op.current.as_ref() else {
            return;
        };
        if !op.attach_connection_id.is_empty() {
            return;
        }
        if self
            .voices
            .get(room_id)
            .is_none_or(|voice| !voice.connections.contains_key(&current.connection_id))
            || !self.voice_candidate_ready(room_id, &current.connection_id)
        {
            return;
        }
        if !op.video_generation.is_empty() {
            let pending = self
                .room_rt(room_id)
                .and_then(|rm| rm.pending_media.clone());
            if let Some(prepared) = pending.filter(|p| p.connection_id == current.connection_id)
                && let Some(ready) = prepared.video_ready
                && let Some(child) = self
                    .store
                    .get_connected_viewer(room_id, &op.child_peer_id)
                    .filter(|child| child.session_id == op.child_session_id)
            {
                let participant =
                    RouteParticipant::new(room_id, Role::Viewer, &child.peer_id, &child.session_id);
                self.handle_route_ready(&participant, ready);
            }
            return;
        }
        let guard = CandidateGuard {
            child_peer_id: op.child_peer_id.clone(),
            child_session_id: op.child_session_id.clone(),
            revision: current.revision,
            connection_id: current.connection_id.clone(),
        };
        let now = self.now();
        let progress = self
            .ctl(room_id, |c| c.candidate_transport_connected(&guard, now))
            .unwrap_or_default();
        self.release_resources(progress.released);
        if !progress.accepted {
            return;
        }
        let Some(settled) = self.with_commit(room_id, |c, commit| {
            c.candidate_ready(&guard, now, commit, CandidateProof::default())
        }) else {
            return;
        };
        self.release_resources(settled.released);
        if settled.accepted {
            self.broadcast_active(room_id);
            self.request_pump(room_id);
        }
    }

    pub(crate) fn voice_candidate_ready(&self, room_id: &str, connection_id: &str) -> bool {
        if self.detached.is_some() {
            return true;
        }
        let Some(voice) = self.voices.get(room_id) else {
            return true;
        };
        let Some(edge) = voice.connections.get(connection_id) else {
            return true;
        };
        if !edge.connected(&edge.parent) || !edge.connected(&edge.child) {
            return false;
        }
        if edge.transport != "sfu" {
            return true;
        }
        let Some(op) = self.controller(room_id).and_then(|c| c.operation()) else {
            return false;
        };
        if op.current.as_ref().is_some_and(|current| {
            current.connection_id == connection_id
                && current.tuple.publication != Some(PublicationPlan::Reuse)
        }) {
            let Some(prepared) = self
                .room_rt(room_id)
                .and_then(|rm| rm.pending_media.as_ref())
                .filter(|prepared| prepared.connection_id == connection_id)
            else {
                return false;
            };
            if let Some(root) = voice.connections.get(&prepared.publication_generation) {
                return root.connected("") && root.connected(&root.child);
            }
            return !self.voice_allowed(room_id, &self.host_peer_of(room_id));
        }
        true
    }

    fn fail_voice_connection(&mut self, room_id: &str, logical: &str) {
        if !self.has_controller(room_id) {
            return;
        }
        let Some(edge) = self
            .voices
            .get(room_id)
            .and_then(|voice| voice.connections.get(logical))
        else {
            return;
        };
        let (pending, parent, child, transport) = (
            edge.pending,
            edge.parent.clone(),
            edge.child.clone(),
            edge.transport,
        );
        let op = self.controller(room_id).and_then(|c| c.operation());
        if pending
            && let Some(op) = op.as_ref()
            && let Some(current) = op.current.as_ref()
        {
            let guard = CandidateGuard {
                child_peer_id: op.child_peer_id.clone(),
                child_session_id: op.child_session_id.clone(),
                revision: current.revision,
                connection_id: current.connection_id.clone(),
            };
            let now = self.now();
            let failed = self
                .ctl(room_id, |c| c.candidate_failed(&guard, now))
                .unwrap_or_default();
            self.release_resources(failed.released);
            self.broadcast_active(room_id);
            self.request_pump(room_id);
            return;
        }
        if let Some(edge) = self.voice_edge_mut(room_id, logical) {
            edge.connected.insert(child.clone(), false);
            edge.connected.insert(parent.clone(), false);
        }
        let snapshot = self.snapshot(room_id).expect("controller");
        let host_peer_id = self.host_peer_of(room_id);
        // A companion audio connection is not evidence that the committed
        // picture path failed: recreate that facet on the proven route.
        let video_facet = snapshot
            .upstream_by_viewer
            .iter()
            .any(|(edge_child, current)| {
                current.usable
                    && current.physical_active
                    && current.video_ready
                    && !snapshot.video_generation.is_empty()
                    && current.video_generation == snapshot.video_generation
                    && ((*edge_child == child && current.connection_id == logical)
                        || (child == host_peer_id
                            && transport == "sfu"
                            && current.kind == UpstreamKind::Sfu
                            && current.publication_generation == logical))
            });
        if video_facet {
            if let Some(mut edge) = self
                .voices
                .get_mut(room_id)
                .and_then(|voice| voice.connections.remove(logical))
            {
                self.close_voice_edge(room_id, &mut edge);
            }
            self.sync_room_voice(room_id);
            return;
        }
        let now = self.now_some();
        if let Some(current) = snapshot
            .upstream_by_viewer
            .get(&child)
            .filter(|current| current.connection_id == logical)
        {
            let guard = EdgeGuard {
                child_peer_id: child.clone(),
                child_session_id: current.child_session_id.clone(),
                parent_session_id: current.parent_session_id.clone(),
                route_revision: snapshot.revision,
                connection_id: current.connection_id.clone(),
            };
            self.ctl(room_id, |c| c.invalidate_edge(guard, now));
            self.request_pump(room_id);
        } else if let Some(publication) = snapshot.host_publication.as_ref()
            && transport == "sfu"
            && child == host_peer_id
            && logical == publication.generation
        {
            let guard = PublicationGuard {
                host_session_id: publication.host_session_id.clone(),
                route_revision: snapshot.revision,
                generation: publication.generation.clone(),
                connection_id: publication.connection_id.clone(),
            };
            self.ctl(room_id, |c| c.invalidate_host_publication(guard, now));
            self.request_pump(room_id);
        }
        for peer in [parent, child] {
            if let Some(key) = self.voice_session(room_id, &peer) {
                self.voice_error(key, RoomVoiceErrorCode::ConnectionUnavailable);
            }
        }
    }
}
