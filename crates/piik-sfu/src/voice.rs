//! Room voice junctions: one ICE-lite connection per member, with reusable
//! Opus receive and send slots. The SFU always offers; a [`VoiceDescription`]
//! revision fences answers from earlier negotiation on the same connection.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use piik_media_forwarding::Source;
use piik_media_session::{AudioAttachment, AudioSlot, AudioSlotOptions};
use piik_media_transport::sdp;
use piik_media_transport::str0m::change::SdpPendingOffer;
use piik_media_transport::str0m::media::{Direction, MediaKind, Mid};
use piik_media_transport::{ConnId, Connections};

mod signal;

use crate::media::{Media, Room, lock};
use crate::room::{MAX_AUDIO_BITRATE, RoomApp, candidate, sfu_connection};
use crate::runtime::{
    DeferredWork, IceCandidateInit, MediaError, PeerConnectionState, SdpType, SessionDescription,
    VoiceConnectionPlan, VoiceDescription, VoiceEventSink, VoiceMediaEvent, VoicePublication,
    VoiceRuntime, VoiceSignal, VoiceTrack,
};

/// The Host plus every Viewer.
pub const MAX_VOICE_PUBLICATIONS: usize = piik_protocol::MAX_VIEWERS_PER_ROOM_LIMIT + 1;
/// Remote candidates retained before the first answer.
const MAX_QUEUED_CANDIDATES: usize = 64;
const MAX_ANSWER_BYTES: usize = 128 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct VoiceKey {
    pub room: String,
    pub peer: String,
    pub session: String,
    pub connection: String,
}

impl VoiceKey {
    fn new(room: &str, peer: &str, session: &str, connection: &str) -> Self {
        Self {
            room: room.to_owned(),
            peer: peer.to_owned(),
            session: session.to_owned(),
            connection: connection.to_owned(),
        }
    }
}

/// The committed authority of one voice connection. It stays charged to its
/// room's capacity until the physical close returned by `close_voice` runs.
pub(crate) struct VoiceEntry {
    pub id: u64,
    pub room: Arc<Room>,
    pub retiring: bool,
    pub plan: VoiceConnectionPlan,
    pub version: u64,
    pub emit: VoiceEventSink,
    /// The connection's latest projected state, for `is_current_state`.
    pub state: PeerConnectionState,
}

struct ReceiveSlot {
    mid: Mid,
    publication: Option<VoicePublication>,
}

struct SendSlot {
    mid: Mid,
    publication: Option<VoicePublication>,
    output: AudioSlot,
    /// The attached input's token and write authority.
    input: Option<(u64, AudioAttachment)>,
    active: bool,
}

impl SendSlot {
    fn disable(&mut self) {
        self.active = false;
        self.output.detach_source();
        self.input = None;
    }
}

struct Binding {
    ssrc: u32,
    publication: VoicePublication,
    source: Source,
    token: u64,
}

struct PendingOffer {
    revision: u64,
    version: u64,
    incoming: HashMap<String, VoicePublication>,
    offer: SdpPendingOffer,
}

pub(crate) struct VoiceConn {
    key: VoiceKey,
    conn: ConnId,
    plan: VoiceConnectionPlan,
    version: u64,
    closed: bool,
    receive: Vec<ReceiveSlot>,
    send: Vec<SendSlot>,
    /// Answered receive sections and the publication each one carries.
    remote: HashMap<Mid, VoicePublication>,
    bindings: HashMap<Mid, Binding>,
    pending: Option<PendingOffer>,
    sequence: u64,
    negotiated_version: u64,
    renegotiate: bool,
    answered: bool,
    candidates: Vec<IceCandidateInit>,
}

fn next_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

fn not_current() -> MediaError {
    MediaError::failed("voice connection is no longer current")
}

fn normalize(mut plan: VoiceConnectionPlan) -> Result<VoiceConnectionPlan, MediaError> {
    let id_ok = |id: &str| !id.is_empty() && id.len() <= 128;
    if ![
        &plan.room_id,
        &plan.peer_id,
        &plan.session_id,
        &plan.connection_id,
    ]
    .iter()
    .all(|id| id_ok(id))
    {
        return Err(MediaError::failed("voice connection identity is invalid"));
    }
    if plan.publish.len() > MAX_VOICE_PUBLICATIONS || plan.subscribe.len() > MAX_VOICE_PUBLICATIONS
    {
        return Err(MediaError::failed("voice publication count is invalid"));
    }
    let mut seen = HashSet::new();
    for publication in plan.publish.iter().chain(&plan.subscribe) {
        if !id_ok(&publication.id)
            || !id_ok(&publication.peer_id)
            || !seen.insert(publication.id.clone())
        {
            return Err(MediaError::failed("voice publication identity is invalid"));
        }
    }
    plan.publish.sort_by(|a, b| a.id.cmp(&b.id));
    plan.subscribe.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(plan)
}

fn sync_receive(slots: &mut [ReceiveSlot], plan: &VoiceConnectionPlan) -> Vec<VoicePublication> {
    for slot in slots.iter_mut() {
        if slot
            .publication
            .as_ref()
            .is_some_and(|p| !plan.publish.contains(p))
        {
            slot.publication = None;
        }
    }
    let mut extra = Vec::new();
    for publication in &plan.publish {
        if slots
            .iter()
            .any(|s| s.publication.as_ref() == Some(publication))
        {
            continue;
        }
        match slots.iter_mut().find(|s| s.publication.is_none()) {
            Some(slot) => slot.publication = Some(publication.clone()),
            None => extra.push(publication.clone()),
        }
    }
    extra
}

fn sync_send(slots: &mut [SendSlot], plan: &VoiceConnectionPlan) -> Vec<VoicePublication> {
    for slot in slots.iter_mut() {
        if slot
            .publication
            .as_ref()
            .is_some_and(|p| !plan.subscribe.contains(p))
        {
            slot.disable();
            slot.publication = None;
        }
    }
    let mut extra = Vec::new();
    for publication in &plan.subscribe {
        if let Some(slot) = slots
            .iter_mut()
            .find(|s| s.publication.as_ref() == Some(publication))
        {
            slot.active = true;
            continue;
        }
        match slots.iter_mut().find(|s| s.publication.is_none()) {
            Some(slot) => {
                slot.publication = Some(publication.clone());
                slot.active = true;
            }
            None => extra.push(publication.clone()),
        }
    }
    extra
}

impl RoomApp {
    pub(crate) fn voice_create(
        &mut self,
        id: u64,
        key: VoiceKey,
        now: Instant,
        conns: &mut Connections,
    ) {
        let mut conn = sfu_connection(&self.udp, sdp::codec_config(&[], true), now);
        self.register(&mut conn);
        let conn = self.insert_voice_conn(id, conn, conns);
        self.voices.insert(
            id,
            VoiceConn {
                key,
                conn,
                plan: VoiceConnectionPlan::default(),
                version: 0,
                closed: false,
                receive: Vec::new(),
                send: Vec::new(),
                remote: HashMap::new(),
                bindings: HashMap::new(),
                pending: None,
                sequence: 0,
                negotiated_version: 0,
                renegotiate: false,
                answered: false,
                candidates: Vec::new(),
            },
        );
    }

    /// Changes RTP authorization at once; SDP follows in the deferred work.
    pub(crate) fn voice_configure(&mut self, id: u64, plan: VoiceConnectionPlan, version: u64) {
        let Some(voice) = self.voices.get_mut(&id).filter(|v| !v.closed) else {
            return;
        };
        for slot in &mut voice.send {
            if slot
                .publication
                .as_ref()
                .is_some_and(|p| !plan.subscribe.contains(p))
            {
                slot.disable();
            }
        }
        let revoked: Vec<Mid> = voice
            .bindings
            .iter()
            .filter(|(_, b)| !plan.publish.contains(&b.publication))
            .map(|(mid, _)| *mid)
            .collect();
        let tokens: Vec<u64> = revoked
            .iter()
            .filter_map(|mid| voice.bindings.remove(mid))
            .map(|b| b.token)
            .collect();
        voice.plan = plan;
        voice.version = version;
        for token in tokens {
            self.detach_input(token);
        }
    }

    /// Fences already-delivered input at every attached output of the room.
    fn detach_input(&mut self, token: u64) {
        for voice in self.voices.values_mut() {
            for slot in &mut voice.send {
                if slot.input.is_some_and(|(input, _)| input == token) {
                    slot.output.detach_source();
                    slot.input = None;
                }
            }
        }
    }

    /// Revokes delivery before the physical close runs.
    pub(crate) fn voice_retire(&mut self, id: u64) {
        let Some(voice) = self.voices.get_mut(&id) else {
            return;
        };
        voice.closed = true;
        for slot in &mut voice.send {
            slot.disable();
        }
        let tokens: Vec<u64> = voice.bindings.drain().map(|(_, b)| b.token).collect();
        for token in tokens {
            self.detach_input(token);
        }
    }

    pub(crate) fn voice_close(&mut self, id: u64, conns: &mut Connections) {
        self.voice_retire(id);
        if let Some(mut voice) = self.voices.remove(&id) {
            for slot in &mut voice.send {
                slot.output.close();
            }
            self.drop_conn(voice.conn, conns);
        }
    }

    pub(crate) fn voice_offer(
        &mut self,
        id: u64,
        force: bool,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let Some(mut voice) = self.voices.remove(&id) else {
            return Err(not_current());
        };
        let result = self.offer(id, &mut voice, force, conns);
        self.voices.insert(id, voice);
        result
    }

    /// Changes arriving while an answer is pending update RTP authorization
    /// at once and coalesce into the next offer.
    pub(super) fn offer(
        &mut self,
        id: u64,
        voice: &mut VoiceConn,
        force: bool,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        if voice.closed {
            return Err(not_current());
        }
        voice.renegotiate |= force;
        if voice.pending.is_some()
            || voice.sequence != 0
                && voice.version == voice.negotiated_version
                && !voice.renegotiate
        {
            return Ok(());
        }
        let plan = voice.plan.clone();
        let mut new_receive = sync_receive(&mut voice.receive, &plan)
            .into_iter()
            .map(Some)
            .collect::<Vec<_>>();
        // A silent room still negotiates its admitted transport; the idle
        // receive slot is reused by a later authorized publication.
        if voice.receive.is_empty() && new_receive.is_empty() {
            new_receive.push(None);
        }
        let new_send = sync_send(&mut voice.send, &plan);
        let conn = conns.get_mut(voice.conn).ok_or_else(not_current)?;
        let stream = voice.key.peer.clone();
        let receive_mids: Vec<Mid> = voice.receive.iter().map(|s| s.mid).collect();
        let idle = voice
            .receive
            .iter()
            .find(|s| s.publication.is_none())
            .or(voice.receive.first())
            .map(|s| s.mid);
        let send_mids: Vec<Mid> = voice.send.iter().map(|s| s.mid).collect();
        let (mut added_receive, mut added_send) = (Vec::new(), Vec::new());
        let (offer, pending) = conn
            .create_offer(|api| {
                for _ in &new_receive {
                    added_receive.push(api.add_media(
                        MediaKind::Audio,
                        Direction::RecvOnly,
                        Some(stream.clone()),
                        None,
                        None,
                    ));
                }
                for _ in &new_send {
                    added_send.push(api.add_media(
                        MediaKind::Audio,
                        Direction::SendOnly,
                        Some(stream.clone()),
                        None,
                        None,
                    ));
                }
                // A remote answer may have narrowed a section; restore ours.
                for mid in &receive_mids {
                    api.set_direction(*mid, Direction::RecvOnly);
                }
                for mid in &send_mids {
                    api.set_direction(*mid, Direction::SendOnly);
                }
                // str0m offers only for changes; a new track assignment or a
                // requested renegotiation reoffers the unchanged sections.
                if !api.has_changes()
                    && let Some(mid) = idle
                {
                    api.set_direction(mid, Direction::Inactive);
                    api.set_direction(mid, Direction::RecvOnly);
                }
            })
            .ok_or_else(|| MediaError::failed("voice offer has no media"))?;
        let offer = sdp::set_opus_fmtp(
            &offer,
            &[("maxaveragebitrate", &MAX_AUDIO_BITRATE.to_string())],
        )
        .map_err(|error| MediaError::failed(error.to_string()))?;
        for (mid, publication) in added_receive.into_iter().zip(new_receive) {
            voice.receive.push(ReceiveSlot { mid, publication });
        }
        for (mid, publication) in added_send.into_iter().zip(new_send) {
            voice.send.push(SendSlot {
                mid,
                publication: Some(publication),
                output: AudioSlot::new(AudioSlotOptions {
                    mid,
                    pt: sdp::OPUS_PT.into(),
                    source_ceiling: MAX_AUDIO_BITRATE,
                }),
                input: None,
                active: true,
            });
        }
        for slot in &mut voice.send {
            slot.active = !voice.closed
                && slot
                    .publication
                    .as_ref()
                    .is_some_and(|p| plan.subscribe.contains(p));
        }
        voice.sequence += 1;
        let mut tracks = Vec::new();
        let mut incoming = HashMap::new();
        for slot in &voice.receive {
            if let Some(publication) = &slot.publication {
                tracks.push(VoiceTrack {
                    mid: slot.mid.to_string(),
                    publication_id: publication.id.clone(),
                });
                incoming.insert(slot.mid.to_string(), publication.clone());
            }
        }
        for slot in &voice.send {
            if let Some(publication) = &slot.publication {
                tracks.push(VoiceTrack {
                    mid: slot.mid.to_string(),
                    publication_id: publication.id.clone(),
                });
            }
        }
        let description = VoiceDescription {
            revision: voice.sequence,
            sdp: SessionDescription {
                sdp_type: SdpType::Offer,
                sdp: offer,
            },
            tracks,
        };
        voice.pending = Some(PendingOffer {
            revision: voice.sequence,
            version: voice.version,
            incoming,
            offer: pending,
        });
        voice.renegotiate = false;
        self.emit_voice(
            id,
            &voice.key,
            VoiceMediaEvent {
                description: Some(description),
                ..Default::default()
            },
        );
        Ok(())
    }
}

impl Media {
    fn voice_entry(&self, key: &VoiceKey) -> Option<(u64, Arc<Room>)> {
        let registry = lock(&self.shared().registry);
        registry
            .voices
            .get(key)
            .filter(|entry| !entry.retiring && !registry.closed)
            .map(|entry| (entry.id, entry.room.clone()))
    }

    /// Number of voice connections charged to `room`, retiring ones included.
    pub fn voice_connection_count(&self, room: &str) -> usize {
        lock(&self.shared().registry)
            .voices
            .keys()
            .filter(|key| key.room == room)
            .count()
    }
}

impl VoiceRuntime for Media {
    /// Commits publication authority before returning SDP work, which the
    /// caller runs outside its authority lock against the current plan.
    fn configure_voice(
        &self,
        plan: VoiceConnectionPlan,
        emit: VoiceEventSink,
    ) -> Result<Option<DeferredWork>, MediaError> {
        let plan = normalize(plan)?;
        let key = VoiceKey::new(
            &plan.room_id,
            &plan.peer_id,
            &plan.session_id,
            &plan.connection_id,
        );
        let shared = self.shared();
        let mut registry = lock(&shared.registry);
        if registry.closed {
            return Err(MediaError::failed("voice media is closed"));
        }
        if let Some(entry) = registry.voices.get(&key)
            && entry.retiring
        {
            return Err(MediaError::failed("voice connection is retiring"));
        }
        if !registry.voices.contains_key(&key) {
            // One committed connection per member; the room's one transition
            // may replace both the Host junction and its first child.
            let count = registry
                .voices
                .keys()
                .filter(|k| k.room == key.room)
                .count();
            if count >= MAX_VOICE_PUBLICATIONS + 2 {
                return Err(MediaError::failed("voice connection capacity is exhausted"));
            }
            let id = registry.next_id();
            let room = registry.room(shared, &key.room);
            let create = key.clone();
            room.send(Box::new(move |app, now, conns| {
                app.voice_create(id, create, now, conns);
            }));
            registry.voices.insert(
                key.clone(),
                VoiceEntry {
                    id,
                    room,
                    retiring: false,
                    plan: VoiceConnectionPlan::default(),
                    version: 0,
                    emit: emit.clone(),
                    state: PeerConnectionState::New,
                },
            );
        }
        let entry = registry.voices.get_mut(&key).expect("voice entry");
        entry.emit = emit;
        if entry.plan.publish != plan.publish || entry.plan.subscribe != plan.subscribe {
            entry.version += 1;
        }
        entry.plan = plan.clone();
        let (id, version, room) = (entry.id, entry.version, entry.room.clone());
        room.send(Box::new(move |app, _, _| {
            app.voice_configure(id, plan, version)
        }));
        drop(registry);
        Ok(Some(Box::new(move || {
            room.call(move |app, _, conns| app.voice_offer(id, false, conns))
        })))
    }

    fn signal_voice(
        &self,
        room: &str,
        peer: &str,
        session: &str,
        connection: &str,
        signal: VoiceSignal,
    ) -> Result<(), MediaError> {
        let key = VoiceKey::new(room, peer, session, connection);
        let (id, room) = self.voice_entry(&key).ok_or_else(not_current)?;
        room.call(move |app, _, conns| app.voice_signal(id, signal, conns))
    }

    /// Revokes delivery synchronously, keeping the capacity charge until the
    /// caller runs the returned physical close outside its authority.
    fn close_voice(
        &self,
        room: &str,
        peer: &str,
        session: &str,
        connection: &str,
    ) -> Option<DeferredWork> {
        let key = VoiceKey::new(room, peer, session, connection);
        let shared = self.shared().clone();
        let (id, room) = {
            let mut registry = lock(&shared.registry);
            let entry = registry.voices.get_mut(&key)?;
            entry.retiring = true;
            (entry.id, entry.room.clone())
        };
        room.send(Box::new(move |app, _, _| app.voice_retire(id)));
        Some(Box::new(move || {
            let result = room.call(move |app, _, conns| {
                app.voice_close(id, conns);
                Ok(())
            });
            let mut registry = lock(&shared.registry);
            if registry
                .voices
                .get(&key)
                .is_some_and(|entry| entry.id == id)
            {
                registry.voices.remove(&key);
                registry.release_room(&key.room);
            }
            // A shard already stopped by `Media::close` has nothing left.
            if registry.closed { Ok(()) } else { result }
        }))
    }
}
