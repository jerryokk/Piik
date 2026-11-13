//! Room voice: activation, membership and publication identity.
//!
//! Connections are physical facets of the route controller's graph, never
//! another topology. Maps are ordered so the wire projections are stable.

pub(crate) mod plan;
pub(crate) mod signal;

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use piik_protocol::{Role, RoomVoiceCommand, RoomVoiceErrorCode, ServerMessage};

use crate::runtime::{Completion, opaque_id};
use crate::server::Core;
use crate::session::AuthState;
use piik_sfu::runtime::{DeferredWork, VoiceConnectionPlan};

pub(crate) const DEFAULT_VOICE_BITRATE: i64 = 64_000;

#[derive(Debug, Clone, Default)]
pub(crate) struct VoiceMember {
    pub session_id: String,
    pub joined: bool,
    pub publication_id: String,
    pub deafened: Option<bool>,
}

/// One coalescing negotiation worker per physical edge.
pub(crate) struct VoiceNegotiation {
    pub run: Option<DeferredWork>,
    pub done: Arc<Completion>,
}

pub(crate) struct VoiceEdge {
    /// The physical connection ID.
    pub id: String,
    /// The route graph identity this facet belongs to.
    pub logical_id: String,
    /// Empty for the SFU.
    pub parent: String,
    pub child: String,
    /// `peer` or `sfu`.
    pub transport: &'static str,
    pub parent_session: String,
    pub child_session: String,
    pub pending: bool,
    pub revision: i64,
    pub send: HashMap<String, Vec<String>>,
    pub connected: HashMap<String, bool>,
    /// The committed SFU plan and the generation that identifies it.
    pub sfu_plan: Option<(u64, VoiceConnectionPlan)>,
    pub sfu_work: Option<VoiceNegotiation>,
}

impl VoiceEdge {
    pub(crate) fn connected(&self, peer: &str) -> bool {
        self.connected.get(peer).copied().unwrap_or(false)
    }
}

pub(crate) struct RoomVoice {
    pub id: u64,
    pub host_peer_id: String,
    pub bitrate: i64,
    pub revision: i64,
    pub members: BTreeMap<String, VoiceMember>,
    /// Keyed by logical ID.
    pub connections: BTreeMap<String, VoiceEdge>,
}

impl RoomVoice {
    pub(crate) fn enabled(&self) -> bool {
        self.members
            .get(&self.host_peer_id)
            .is_some_and(|host| host.joined)
    }

    fn leave_all(&mut self) {
        for member in self.members.values_mut() {
            member.joined = false;
            member.publication_id.clear();
        }
    }
}

impl Core {
    /// The room's voice state, created on first use, rebound to the current Host.
    pub(crate) fn voice_room(&mut self, room_id: &str) -> &mut RoomVoice {
        let host = self.store.get_connected_host(room_id);
        if !self.voices.contains_key(room_id) {
            let id = self.router.next_id();
            self.voices.insert(
                room_id.to_owned(),
                RoomVoice {
                    id,
                    host_peer_id: String::new(),
                    bitrate: DEFAULT_VOICE_BITRATE,
                    revision: 0,
                    members: BTreeMap::new(),
                    connections: BTreeMap::new(),
                },
            );
        }
        let voice = self.voices.get_mut(room_id).expect("voice room");
        if let Some(host) = host {
            if !voice.host_peer_id.is_empty() && voice.host_peer_id != host.peer_id {
                voice.leave_all();
                let previous = voice.host_peer_id.clone();
                voice.members.remove(&previous);
            }
            voice.host_peer_id = host.peer_id;
        }
        voice
    }

    /// Adds the authenticated participant to the voice roster. Membership
    /// starts unjoined; joining the channel stays an explicit command.
    pub(crate) fn join_voice_roster(&mut self, room_id: &str, peer_id: &str, session_id: &str) {
        let voice = self.voice_room(room_id);
        let member = voice.members.entry(peer_id.to_owned()).or_default();
        if member.session_id != session_id {
            member.session_id = session_id.to_owned();
            member.publication_id.clear();
        }
    }

    /// The member's current voice session, if it is still the live one.
    pub(crate) fn voice_session_in(
        &self,
        voice: &RoomVoice,
        room_id: &str,
        peer_id: &str,
    ) -> Option<u64> {
        let member = voice.members.get(peer_id)?;
        let key = self.session_key(&member.session_id)?;
        let session = self.session(key)?;
        let auth = session.authenticated.as_ref()?;
        (self.is_current_session(key) && auth.room_id == room_id && auth.peer_id == peer_id)
            .then_some(key)
    }

    pub(crate) fn voice_session(&self, room_id: &str, peer_id: &str) -> Option<u64> {
        self.voice_session_in(self.voices.get(room_id)?, room_id, peer_id)
    }

    pub(crate) fn voice_allowed_in(&self, voice: &RoomVoice, room_id: &str, peer_id: &str) -> bool {
        self.voice_session_in(voice, room_id, peer_id).is_some()
            && voice.enabled()
            && voice.members.get(peer_id).is_some_and(|m| m.joined)
    }

    pub(crate) fn voice_allowed(&self, room_id: &str, peer_id: &str) -> bool {
        if self.detached.is_some() {
            return false;
        }
        self.voices
            .get(room_id)
            .is_some_and(|voice| self.voice_allowed_in(voice, room_id, peer_id))
    }

    pub(crate) fn handle_room_voice_command(
        &mut self,
        key: u64,
        auth: &AuthState,
        command: RoomVoiceCommand,
    ) {
        let session_id = self.session_id_of(key);
        if self.voice_session(&auth.room_id, &auth.peer_id) != Some(key) {
            self.voice_error(key, RoomVoiceErrorCode::NotAllowed);
            return;
        }
        let voice = self.voice_room(&auth.room_id);
        let enabled = voice.enabled();
        let Some(member) = voice
            .members
            .get_mut(&auth.peer_id)
            .filter(|m| m.session_id == session_id)
        else {
            return;
        };
        match command {
            RoomVoiceCommand::Bitrate { bitrate } => {
                if auth.role != Role::Host {
                    self.voice_error(key, RoomVoiceErrorCode::NotAllowed);
                    return;
                }
                self.voices.get_mut(&auth.room_id).expect("voice").bitrate = bitrate;
                self.sync_room_voice(&auth.room_id);
                return;
            }
            RoomVoiceCommand::Deafen { deafened } => {
                member.deafened = Some(deafened);
                self.sync_room_voice(&auth.room_id);
                return;
            }
            RoomVoiceCommand::Join => {
                if auth.role != Role::Host && !enabled {
                    self.voice_error(key, RoomVoiceErrorCode::NotEnabled);
                    return;
                }
                member.joined = true;
            }
            RoomVoiceCommand::Leave => {
                if auth.role == Role::Host {
                    self.voices
                        .get_mut(&auth.room_id)
                        .expect("voice")
                        .leave_all();
                } else {
                    member.joined = false;
                    member.publication_id.clear();
                }
            }
            RoomVoiceCommand::Publish => {
                if !enabled || !member.joined {
                    self.voice_error(key, RoomVoiceErrorCode::NotAllowed);
                    return;
                }
                if member.publication_id.is_empty() {
                    member.publication_id = opaque_id();
                }
            }
            RoomVoiceCommand::Unpublish { publication_id } => {
                if publication_id == member.publication_id {
                    member.publication_id.clear();
                }
            }
        }
        self.sync_voice_demand(&auth.room_id);
        self.sync_room_voice(&auth.room_id);
    }

    pub(crate) fn voice_error(&mut self, key: u64, code: RoomVoiceErrorCode) {
        self.send(key, &ServerMessage::RoomVoiceError { code });
    }

    pub(crate) fn sync_voice_demand(&mut self, room_id: &str) {
        let Some(voice) = self.voices.get(room_id) else {
            return;
        };
        let listeners: Vec<String> = voice
            .members
            .keys()
            .filter(|peer| self.voice_allowed_in(voice, room_id, peer))
            .cloned()
            .collect();
        let now = self.now();
        if listeners.is_empty() {
            if self.room_media(room_id) && self.has_controller(room_id) {
                self.ctl(room_id, |c| c.set_voice_listeners::<String>(&[], now));
                if self.active_host_share_generation(room_id).is_empty() {
                    self.clear_deadline(room_id);
                    let released = self
                        .room_rt_mut(room_id)
                        .and_then(|rm| rm.controller.take())
                        .map(|mut controller| controller.dispose())
                        .unwrap_or_default();
                    self.release_resources(released);
                } else {
                    self.request_pump(room_id);
                }
            }
            return;
        }
        let Some(host) = self.store.get_connected_host(room_id) else {
            return;
        };
        if !self.ensure_room(room_id).room_media {
            let media_generation = opaque_id();
            {
                let runtime = self.room_rt_mut(room_id).expect("room");
                runtime.room_media = true;
                runtime.media_generation = media_generation;
            }
            if self.has_controller(room_id) {
                if let Some(share) = self
                    .controller(room_id)
                    .and_then(|c| c.snapshot().host_publication)
                    .and_then(|p| p.resource)
                {
                    self.room_rt_mut(room_id).expect("room").media_generation =
                        share.share_generation.clone();
                }
                let generation = self.active_host_share_generation(room_id);
                let paused =
                    !generation.is_empty() && self.share(room_id).paused_generation == generation;
                let released = self
                    .ctl(room_id, |c| {
                        c.set_video_publication(&generation, paused, now)
                    })
                    .unwrap_or_default();
                self.release_resources(released);
            }
        }
        let Some(host_auth) = self.authenticated_of(&host.session_id).cloned() else {
            return;
        };
        let mut participant = crate::router::RouteParticipant::new(
            &host_auth.room_id,
            host_auth.role,
            &host_auth.peer_id,
            &host.session_id,
        );
        participant.route_policy = Some(self.route_policy_of(room_id));
        self.connect_participant(&participant);
        let generation = self.active_host_share_generation(room_id);
        let paused = !generation.is_empty() && self.share(room_id).paused_generation == generation;
        let released = self
            .ctl(room_id, |c| {
                c.set_video_publication(&generation, paused, now)
            })
            .unwrap_or_default();
        self.release_resources(released);
        for peer in self.store.get_connected_viewers(room_id) {
            if generation.is_empty() && !self.voice_allowed(room_id, &peer.peer_id) {
                let now = self.now_some();
                self.ctl(room_id, |c| c.confirm_departure(&peer.peer_id, now));
                continue;
            }
            if let Some(auth) = self.authenticated_of(&peer.session_id).cloned() {
                let participant = crate::router::RouteParticipant::new(
                    &auth.room_id,
                    auth.role,
                    &auth.peer_id,
                    &peer.session_id,
                );
                self.connect_participant(&participant);
            }
        }
        self.ctl(room_id, |c| c.set_voice_listeners(&listeners, now));
        self.request_pump(room_id);
    }

    pub(crate) fn disconnect_room_voice(&mut self, room_id: &str, peer_id: &str, session_id: &str) {
        let Some(voice) = self.voices.get_mut(room_id) else {
            return;
        };
        if let Some(member) = voice.members.get_mut(peer_id)
            && member.session_id == session_id
        {
            member.publication_id.clear();
        }
        self.sync_voice_demand(room_id);
        self.sync_room_voice(room_id);
    }

    pub(crate) fn close_room_voice(&mut self, room_id: &str) {
        if let Some(mut voice) = self.voices.remove(room_id) {
            for edge in voice.connections.values_mut() {
                self.close_voice_edge(room_id, edge);
            }
        }
    }

    pub(crate) fn voice_route_failed(&mut self, room_id: &str, peer_id: &str) {
        if let Some(key) = self.voice_session(room_id, peer_id) {
            self.voice_error(key, RoomVoiceErrorCode::ConnectionUnavailable);
        }
    }
}
