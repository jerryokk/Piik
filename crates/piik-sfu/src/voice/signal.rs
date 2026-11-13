//! Voice signaling: offer generation, answers, candidates and receive
//! forwarding.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};
use std::time::Instant;

use piik_input_route::InputChannel as _;
use piik_media_transport::str0m::Event;
use piik_media_transport::str0m::media::Mid;
use piik_media_transport::str0m::rtp::RtpPacket;
use piik_media_transport::{CandidateError, ConnEvent, ConnId, Connections};

use super::{
    Binding, MAX_ANSWER_BYTES, MAX_QUEUED_CANDIDATES, MAX_VOICE_PUBLICATIONS, RoomApp, VoiceConn,
    VoiceKey, candidate, next_token, not_current,
};
use crate::media::{Shared, lock};
use crate::room::{audio_packet, opus_source, pc_state};
use crate::runtime::{
    CurrentStateCheck, IceCandidateInit, MediaError, PeerConnectionState, SdpType, VoiceMediaEvent,
    VoiceSignal,
};
impl RoomApp {
    pub(super) fn voice_signal(
        &mut self,
        id: u64,
        signal: VoiceSignal,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let Some(mut voice) = self.voices.remove(&id) else {
            return Err(not_current());
        };
        let result = self.signal(id, &mut voice, signal, conns);
        self.voices.insert(id, voice);
        result
    }

    fn signal(
        &mut self,
        id: u64,
        voice: &mut VoiceConn,
        signal: VoiceSignal,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        if voice.closed {
            return Err(not_current());
        }
        let count = usize::from(signal.description.is_some())
            + usize::from(signal.candidate.is_some())
            + usize::from(signal.renegotiate);
        if count != 1 {
            return Err(MediaError::failed("one voice signal is required"));
        }
        if signal.renegotiate {
            return self.offer(id, voice, true, conns);
        }
        if let Some(init) = signal.candidate {
            if !voice.answered {
                if voice.candidates.len() < MAX_QUEUED_CANDIDATES {
                    voice.candidates.push(init);
                }
                return Ok(());
            }
            return add_candidate(conns, voice.conn, &init);
        }
        let answer = signal.description.expect("one signal");
        let stale = || MediaError::failed("voice answer is stale or invalid");
        let pending = voice
            .pending
            .as_ref()
            .filter(|p| p.revision == answer.revision)
            .ok_or_else(stale)?;
        if answer.sdp.sdp_type != SdpType::Answer
            || answer.tracks.len() > MAX_VOICE_PUBLICATIONS
            || answer.sdp.sdp.len() > MAX_ANSWER_BYTES
        {
            return Err(stale());
        }
        let mut remote = HashMap::new();
        let mut seen = HashSet::new();
        for track in &answer.tracks {
            let publication = pending.incoming.get(&track.mid).filter(|p| {
                p.id == track.publication_id
                    && !remote.contains_key(&Mid::from(track.mid.as_str()))
                    && !seen.contains(&p.id)
            });
            let Some(publication) = publication else {
                return Err(MediaError::failed(
                    "voice publication is not authorized for this media section",
                ));
            };
            seen.insert(publication.id.clone());
            remote.insert(Mid::from(track.mid.as_str()), publication.clone());
        }
        let pending = voice.pending.take().expect("pending offer");
        let conn = conns.get_mut(voice.conn).ok_or_else(not_current)?;
        conn.accept_answer(pending.offer, &answer.sdp.sdp)
            .map_err(|error| MediaError::failed(error.to_string()))?;
        voice.answered = true;
        // Inputs moved to another section lose their binding.
        let moved: Vec<Mid> = voice
            .bindings
            .iter()
            .filter(|(mid, b)| remote.get(mid) != Some(&b.publication))
            .map(|(mid, _)| *mid)
            .collect();
        let tokens: Vec<u64> = moved
            .iter()
            .filter_map(|mid| voice.bindings.remove(mid))
            .map(|b| b.token)
            .collect();
        voice.remote = remote;
        voice.negotiated_version = pending.version;
        let mut result = Ok(());
        // Candidates are disposable input, not the SDP transaction: a rejected
        // candidate must not strand an applied answer in pending.
        for init in std::mem::take(&mut voice.candidates) {
            if let Err(error) = add_candidate(conns, voice.conn, &init) {
                result = Err(error);
                break;
            }
        }
        for token in tokens {
            self.detach_input(token);
        }
        result?;
        self.offer(id, voice, false, conns)
    }

    pub(crate) fn voice_event(
        &mut self,
        id: u64,
        now: Instant,
        event: ConnEvent,
        conns: &mut Connections,
    ) {
        match event {
            ConnEvent::State(state) => {
                let Some(voice) = self.voices.get(&id) else {
                    return;
                };
                let key = voice.key.clone();
                let state = pc_state(state);
                self.set_voice_state(id, &key, state);
                self.emit_voice(
                    id,
                    &key,
                    VoiceMediaEvent {
                        state,
                        ..Default::default()
                    },
                );
            }
            ConnEvent::ChannelOpen(channel) => channel.close(),
            ConnEvent::Rtc(event) => {
                if let Event::RtpPacket(packet) = event.as_ref()
                    && let Some(mut voice) = self.voices.remove(&id)
                {
                    self.voice_receive(&mut voice, now, packet, conns);
                    self.voices.insert(id, voice);
                }
            }
            ConnEvent::ChannelData(..) | ConnEvent::ChannelClose(_) => {}
        }
    }

    /// Forwards one authorized input packet to every subscribed output of
    /// the room. `voice` is out of `self.voices` meanwhile; a connection
    /// never subscribes to its own publications.
    fn voice_receive(
        &mut self,
        voice: &mut VoiceConn,
        now: Instant,
        packet: &RtpPacket,
        conns: &mut Connections,
    ) {
        if voice.closed {
            return;
        }
        let ssrc = *packet.header.ssrc;
        let Some(conn) = conns.get_mut(voice.conn) else {
            return;
        };
        let Some((mid, _)) = conn.rx_stream(packet.header.ssrc) else {
            return;
        };
        if !voice.receive.iter().any(|slot| slot.mid == mid) {
            return;
        }
        let Some(publication) = voice.remote.get(&mid).cloned() else {
            return;
        };
        if !voice.plan.publish.contains(&publication) {
            return;
        }
        let rebind = voice
            .bindings
            .get(&mid)
            .is_none_or(|b| b.publication != publication || b.ssrc != ssrc);
        if rebind {
            if !voice.bindings.contains_key(&mid) && voice.bindings.len() >= MAX_VOICE_PUBLICATIONS
            {
                return;
            }
            if let Some(old) = voice.bindings.remove(&mid) {
                self.detach_input(old.token);
            }
            let token = next_token();
            let Some(source) = opus_source(
                format!("{}#{token}", publication.id),
                publication.peer_id.clone(),
                ssrc,
            ) else {
                return;
            };
            voice.bindings.insert(
                mid,
                Binding {
                    ssrc,
                    publication: publication.clone(),
                    source,
                    token,
                },
            );
        }
        let binding = &voice.bindings[&mid];
        let forwarded = audio_packet(packet);
        for other in self.voices.values_mut().filter(|other| !other.closed) {
            if !other.plan.subscribe.contains(&publication) {
                continue;
            }
            let Some(conn) = conns.get_mut(other.conn) else {
                continue;
            };
            for slot in other
                .send
                .iter_mut()
                .filter(|slot| slot.active && slot.publication.as_ref() == Some(&publication))
            {
                if slot.input.map(|(token, _)| token) != Some(binding.token) {
                    match slot.output.replace_source(&binding.source) {
                        Ok(attachment) => slot.input = Some((binding.token, attachment)),
                        Err(_) => continue,
                    }
                }
                if let Some((_, attachment)) = slot.input {
                    slot.output
                        .forward(conn.rtc(), attachment, &binding.source, &forwarded, now);
                }
            }
        }
    }

    fn set_voice_state(&self, id: u64, key: &VoiceKey, state: PeerConnectionState) {
        let Some(shared) = self.shared() else {
            return;
        };
        let mut registry = lock(&shared.registry);
        if let Some(entry) = registry.voices.get_mut(key)
            && entry.id == id
        {
            entry.state = state;
        }
    }

    /// Voice callbacks reach authority only while this exact connection is
    /// the committed, unretired one.
    pub(super) fn emit_voice(&self, id: u64, key: &VoiceKey, mut event: VoiceMediaEvent) {
        let key = key.clone();
        self.dispatch(move |shared| {
            let emit = {
                let registry = lock(&shared.registry);
                registry
                    .voices
                    .get(&key)
                    .filter(|entry| entry.id == id && !entry.retiring && !registry.closed)
                    .map(|entry| entry.emit.clone())
            };
            let Some(emit) = emit else {
                return;
            };
            event.room_id.clone_from(&key.room);
            event.peer_id.clone_from(&key.peer);
            event.session_id.clone_from(&key.session);
            event.connection_id.clone_from(&key.connection);
            let state = event.state;
            let weak: Weak<Shared> = Arc::downgrade(shared);
            let check: CurrentStateCheck = Arc::new(move || {
                weak.upgrade().is_some_and(|shared| {
                    let registry = lock(&shared.registry);
                    registry.voices.get(&key).is_some_and(|entry| {
                        entry.id == id && !entry.retiring && entry.state == state
                    })
                })
            });
            event.is_current_state = Some(check);
            emit(event);
        });
    }
}

/// Content errors are disposable; lifecycle errors fail the connection.
fn add_candidate(
    conns: &mut Connections,
    conn: ConnId,
    init: &IceCandidateInit,
) -> Result<(), MediaError> {
    let conn = conns.get_mut(conn).ok_or_else(not_current)?;
    match conn.add_ice_candidate(&candidate(init)) {
        Ok(()) | Err(CandidateError::Invalid(_)) => Ok(()),
        Err(error) => Err(MediaError::failed(error.to_string())),
    }
}
