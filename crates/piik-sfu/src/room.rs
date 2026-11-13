//! One room's shard: every publication, subscriber and voice `Rtc` of the
//! room, and the synchronous forwarding chain between them.

use std::collections::{HashMap, VecDeque};
use std::sync::mpsc as std_mpsc;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use piik_input_route::Relay;
use piik_media_forwarding::{Codec, CodecKind, InboundPacket, LayerFormat, Source, SourceOptions};
use piik_media_session::{
    AudioAttachment, AudioSlot, Ingress, SourcePacket, UpstreamKeyframe, VideoSend,
};
use piik_media_transport::sdp::{self, VideoCodec};
use piik_media_transport::str0m::RtcConfig;
use piik_media_transport::str0m::change::SdpPendingOffer;
use piik_media_transport::str0m::media::{Mid, Pt};
use piik_media_transport::str0m::rtp::RtpPacket;
use piik_media_transport::{
    CandidateError, ConnEvent, ConnId, Connection, Connections, DataChannel, Inbox, PcState,
    ShardApp, SharedRoute, SharedUdp,
};

use crate::media::{Dispatch, Job, Reply, Shared, drain_jobs, lock};
use crate::runtime::{
    IceCandidateInit, MediaError, MediaEvent, PeerConnectionState, PublicationMedia, ResourceFence,
    SessionDescription,
};
use crate::voice::VoiceConn;

mod publication;
mod subscriber;

/// Highest Opus rate any publisher may send; subscribers must admit it.
pub(crate) const MAX_AUDIO_BITRATE: u32 = piik_protocol::MAX_AUDIO_BITRATE as u32;
/// Receiver report and egress statistics interval of SFU connections.
const STATS_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct SubscriptionKey {
    pub peer_id: String,
    pub connection_id: String,
}

impl SubscriptionKey {
    fn edge(&self) -> String {
        format!("{}:{}", self.peer_id, self.connection_id)
    }
}

#[derive(Clone)]
enum Owner {
    Publisher(u64),
    Subscriber(u64, SubscriptionKey),
    Voice(u64),
}

type InputLoss = Box<dyn Fn() + Send + Sync>;

struct Subscriber {
    /// Admission belongs to the room edge; its video transport can be replaced.
    resource_connection_id: String,
    token: u64,
    /// The waiting `prepare_subscriber` call, until its offer exists.
    reply: Option<Reply<SessionDescription>>,
    conn: Option<ConnId>,
    transport: Option<VideoSend>,
    audio: Option<(AudioSlot, Option<AudioAttachment>)>,
    pending: Option<SdpPendingOffer>,
    input: Option<(Arc<DataChannel>, InputLoss)>,
}

pub(super) struct Publication {
    fence: ResourceFence,
    connection_id: String,
    metadata: PublicationMedia,
    codec: VideoCodec,
    conn: ConnId,
    audio_mid: Option<Mid>,
    ingress: Option<Ingress>,
    source: Option<Source>,
    audio_source: Option<Source>,
    input: Option<Relay<DataChannel>>,
    subscribers: HashMap<SubscriptionKey, Subscriber>,
    /// The first video layer is bound; subscribers may offer.
    ready: bool,
    failed: bool,
    /// Last reported active-layer demand; `None` before the first wake.
    demand: Option<i64>,
}

pub(crate) struct RoomApp {
    shared: Weak<Shared>,
    pub(crate) udp: Arc<SharedUdp>,
    jobs: Arc<Mutex<VecDeque<Job>>>,
    events: std_mpsc::Sender<Dispatch>,
    inbox: Arc<OnceLock<Inbox>>,
    publications: HashMap<u64, Publication>,
    owners: HashMap<ConnId, Owner>,
    pub(crate) voices: HashMap<u64, VoiceConn>,
}

pub(crate) fn pc_state(state: PcState) -> PeerConnectionState {
    match state {
        PcState::New => PeerConnectionState::New,
        PcState::Connecting => PeerConnectionState::Connecting,
        PcState::Connected => PeerConnectionState::Connected,
        PcState::Disconnected => PeerConnectionState::Disconnected,
        PcState::Failed => PeerConnectionState::Failed,
        PcState::Closed => PeerConnectionState::Closed,
    }
}

pub(crate) fn candidate(init: &IceCandidateInit) -> piik_media_transport::IceCandidateInit {
    piik_media_transport::IceCandidateInit {
        candidate: init.candidate.clone(),
        sdp_mid: init.sdp_mid.clone(),
        sdp_m_line_index: init.sdp_m_line_index,
        username_fragment: init.username_fragment.clone(),
    }
}

/// Content errors are disposable; lifecycle errors fail the connection.
pub(crate) fn candidate_error(error: CandidateError) -> MediaError {
    match error {
        CandidateError::Invalid(_) => MediaError::CandidateRejected(error.to_string()),
        _ => MediaError::Failed(error.to_string()),
    }
}

fn failed(error: impl std::fmt::Display) -> MediaError {
    MediaError::failed(error.to_string())
}

/// An ICE-lite SFU connection with the port's current host candidates.
pub(crate) fn sfu_connection(udp: &SharedUdp, config: RtcConfig, now: Instant) -> Connection {
    let mut conn = Connection::new(config.set_ice_lite(true), now);
    for host in udp.candidates().unwrap_or_default() {
        conn.add_local_candidate(host);
    }
    conn.set_egress(udp.egress());
    conn
}

/// One m-section of an SDP text, as far as publication admission needs it.
pub(super) struct Section {
    kind: String,
    mid: Option<String>,
    /// The remote sends on it (`sendonly`/`sendrecv`, or no direction).
    sending: bool,
    rids: Vec<String>,
}

pub(super) fn sections(sdp: &str) -> Vec<Section> {
    let mut out: Vec<Section> = Vec::new();
    let mut active = Vec::new();
    for line in sdp.lines().map(str::trim_end) {
        if let Some(media) = line.strip_prefix("m=") {
            let fields: Vec<&str> = media.split_whitespace().collect();
            active.push(fields.get(1).is_some_and(|port| !port.starts_with('0')));
            out.push(Section {
                kind: fields.first().copied().unwrap_or_default().to_owned(),
                mid: None,
                sending: true,
                rids: Vec::new(),
            });
            continue;
        }
        let (Some(section), Some(attribute)) = (out.last_mut(), line.strip_prefix("a=")) else {
            continue;
        };
        if let Some(mid) = attribute.strip_prefix("mid:") {
            section.mid = Some(mid.trim().to_owned());
        } else if matches!(attribute, "recvonly" | "inactive") {
            section.sending = false;
        } else if attribute == "bundle-only" {
            *active.last_mut().expect("section") = true;
        } else if let Some(rid) = attribute.strip_prefix("rid:") {
            let mut fields = rid.split_whitespace();
            if let (Some(id), Some("send")) = (fields.next(), fields.next()) {
                section.rids.push(id.to_owned());
            }
        }
    }
    out.into_iter()
        .zip(active)
        .filter_map(|(section, active)| active.then_some(section))
        .collect()
}

fn video_kind(codec: VideoCodec) -> CodecKind {
    match codec {
        VideoCodec::H264 => CodecKind::H264,
        VideoCodec::Vp8 => CodecKind::Vp8,
    }
}

fn video_pt(codec: VideoCodec) -> Pt {
    match codec {
        VideoCodec::H264 => sdp::H264_PT.into(),
        VideoCodec::Vp8 => sdp::VP8_PT.into(),
    }
}

fn formats(media: &PublicationMedia) -> Vec<LayerFormat> {
    media
        .formats
        .iter()
        .map(|format| LayerFormat {
            width: format.width,
            height: format.height,
            bitrate: format.bitrate,
        })
        .collect()
}

pub(crate) fn opus_source(id: String, stream_id: String, ssrc: u32) -> Option<Source> {
    let mut source = Source::new(SourceOptions {
        id,
        stream_id,
        codec: Codec {
            kind: CodecKind::Opus,
            payload_type: sdp::OPUS_PT,
            fmtp: sdp::OPUS_FMTP.to_owned(),
            channels: 2,
        },
        formats: Vec::new(),
        max_packets: 0,
    })
    .ok()?;
    source.bind_layer(0, ssrc).ok()?;
    Some(source)
}

/// One received Opus packet as an audio slot forwards it.
pub(crate) fn audio_packet(packet: &RtpPacket) -> SourcePacket {
    let info = InboundPacket::new(
        CodecKind::Opus,
        0,
        *packet.seq_no,
        packet.time.numer(),
        &packet.payload,
        packet.timestamp,
    )
    .marker(packet.header.marker)
    .ssrc(*packet.header.ssrc)
    .payload_type(sdp::OPUS_PT);
    SourcePacket {
        info,
        payload: Arc::clone(&packet.payload),
    }
}

impl RoomApp {
    pub(crate) fn new(
        shared: Weak<Shared>,
        udp: Arc<SharedUdp>,
        jobs: Arc<Mutex<VecDeque<Job>>>,
        events: std_mpsc::Sender<Dispatch>,
        inbox: Arc<OnceLock<Inbox>>,
    ) -> Self {
        Self {
            shared,
            udp,
            jobs,
            events,
            inbox,
            publications: HashMap::new(),
            owners: HashMap::new(),
            voices: HashMap::new(),
        }
    }

    /// Routes the connection's current local ufrag (again after a restart).
    pub(crate) fn register(&self, conn: &mut Connection) {
        if let Some(inbox) = self.inbox.get() {
            self.udp.register(
                &conn.local_ufrag(),
                SharedRoute {
                    conn: conn.id(),
                    inbox: inbox.clone(),
                },
            );
        }
    }

    pub(crate) fn insert_voice_conn(
        &mut self,
        id: u64,
        conn: Connection,
        conns: &mut Connections,
    ) -> ConnId {
        let conn = conns.insert(conn);
        self.owners.insert(conn, Owner::Voice(id));
        conn
    }

    /// Closes a connection and stops routing its datagrams.
    pub(crate) fn drop_conn(&mut self, conn: ConnId, conns: &mut Connections) {
        conns.close(conn);
        self.udp.unregister(conn);
        self.owners.remove(&conn);
    }

    pub(crate) fn shared(&self) -> Option<Arc<Shared>> {
        self.shared.upgrade()
    }

    /// Runs `f` on the dispatcher thread, without shard or media locks.
    pub(crate) fn dispatch(&self, f: impl FnOnce(&Arc<Shared>) + Send + 'static) {
        let shared = self.shared.clone();
        let _ = self.events.send(Box::new(move || {
            if let Some(shared) = shared.upgrade() {
                f(&shared);
            }
        }));
    }

    /// Publication callbacks reach authority only while this exact handle is
    /// the registered one.
    fn emit(&self, pid: u64, mut event: MediaEvent) {
        let Some(publication) = self.publications.get(&pid) else {
            return;
        };
        if publication.failed && event.state != PeerConnectionState::Failed {
            return;
        }
        event.fence = publication.fence.clone();
        if event.connection_id.is_empty() {
            event.connection_id = publication.connection_id.clone();
        }
        self.dispatch(move |shared| {
            let sink = {
                let registry = lock(&shared.registry);
                let current = !registry.closed
                    && registry
                        .publications
                        .get(&event.fence)
                        .is_some_and(|entry| entry.id == pid);
                if current {
                    registry.events.clone()
                } else {
                    None
                }
            };
            if let Some(sink) = sink {
                sink(event);
            }
        });
    }

    fn emit_subscriber(&self, pid: u64, key: &SubscriptionKey, mut event: MediaEvent) {
        let current = self
            .publications
            .get(&pid)
            .is_some_and(|p| !p.failed && p.subscribers.contains_key(key));
        if current {
            event.viewer_peer_id = key.peer_id.clone();
            event.connection_id = key.connection_id.clone();
            self.emit(pid, event);
        }
    }

    // ---------------------------------------------------------------------
    // Shard lifecycle.
    // ---------------------------------------------------------------------

    pub(crate) fn shutdown(&mut self, _now: Instant, conns: &mut Connections) {
        let publications: Vec<u64> = self.publications.keys().copied().collect();
        for pid in publications {
            self.close_publication(pid, conns);
        }
        let voices: Vec<u64> = self.voices.keys().copied().collect();
        for id in voices {
            self.voice_close(id, conns);
        }
    }
}

/// Every attached output re-reads a changed source.
pub(super) fn reconcile_all(
    publication: &mut Publication,
    conns: &mut Connections,
) -> Vec<UpstreamKeyframe> {
    let mut requests = Vec::new();
    let Some(source) = publication.source.as_ref() else {
        return requests;
    };
    for subscriber in publication.subscribers.values_mut() {
        if let (Some(transport), Some(conn)) = (
            subscriber.transport.as_mut(),
            subscriber.conn.and_then(|id| conns.get_mut(id)),
        ) {
            requests.extend(transport.reconcile(conn.rtc(), source));
        }
    }
    requests
}

pub(super) fn required_active_count(publication: &Publication, conns: &Connections) -> i64 {
    if publication.failed {
        return 0;
    }
    let count = publication.metadata.formats.len() as i64;
    let mut active = 0;
    for subscriber in publication.subscribers.values() {
        let (Some(transport), Some(conn), Some(source)) = (
            subscriber.transport.as_ref(),
            subscriber.conn,
            publication.source.as_ref(),
        ) else {
            // A preparing subscriber bootstraps every layer.
            return count;
        };
        match conns.get(conn).map_or(PcState::Closed, Connection::state) {
            PcState::New | PcState::Connecting => return count,
            PcState::Connected | PcState::Disconnected => {
                active = active.max(transport.required_active_count(source) as i64);
            }
            PcState::Failed | PcState::Closed => {}
        }
    }
    active.min(count)
}

pub(super) fn forward_publication_audio(
    publication: &mut Publication,
    packet: &RtpPacket,
    now: Instant,
    conns: &mut Connections,
) {
    let ssrc = *packet.header.ssrc;
    if publication.audio_source.is_none() {
        let generation = &publication.fence.publication_generation;
        let Some(source) = opus_source(format!("{generation}:audio"), generation.clone(), ssrc)
        else {
            return;
        };
        for subscriber in publication.subscribers.values_mut() {
            if let Some((slot, attachment)) = subscriber.audio.as_mut() {
                *attachment = slot.replace_source(&source).ok();
            }
        }
        publication.audio_source = Some(source);
    }
    let Some(source) = publication.audio_source.as_ref() else {
        return;
    };
    if source.layer_ssrc(0) != Some(ssrc) {
        return;
    }
    let forwarded = audio_packet(packet);
    for subscriber in publication.subscribers.values_mut() {
        // A connecting or retired subscriber owns its write failure.
        if let (Some((slot, Some(attachment))), Some(conn)) = (
            subscriber.audio.as_mut(),
            subscriber.conn.and_then(|id| conns.get_mut(id)),
        ) {
            slot.forward(conn.rtc(), *attachment, source, &forwarded, now);
        }
    }
}

impl ShardApp for RoomApp {
    type Command = ();

    fn on_command(&mut self, now: Instant, (): (), conns: &mut Connections) {
        for job in drain_jobs(&self.jobs) {
            job(self, now, conns);
        }
    }

    fn on_event(&mut self, now: Instant, conn: ConnId, event: ConnEvent, conns: &mut Connections) {
        match self.owners.get(&conn).cloned() {
            Some(Owner::Publisher(pid)) => self.publisher_event(pid, now, event, conns),
            Some(Owner::Subscriber(pid, key)) => {
                self.subscriber_event(pid, &key, now, event, conns)
            }
            Some(Owner::Voice(id)) => self.voice_event(id, now, event, conns),
            None => {}
        }
    }

    fn poll_timeout(&self) -> Option<Instant> {
        self.publications
            .values()
            .filter(|p| !p.failed)
            .flat_map(|publication| {
                let source = publication.source.as_ref().and_then(Source::poll_deadline);
                publication
                    .subscribers
                    .values()
                    .filter_map(|s| s.transport.as_ref().and_then(VideoSend::poll_deadline))
                    .chain(source)
            })
            .min()
    }

    fn on_timeout(&mut self, now: Instant, conns: &mut Connections) {
        let ids: Vec<u64> = self.publications.keys().copied().collect();
        for pid in ids {
            let Some(publication) = self.publications.get_mut(&pid).filter(|p| !p.failed) else {
                continue;
            };
            let Some(source) = publication.source.as_mut() else {
                continue;
            };
            let changes = source.poll(now);
            let mut requests = if changes.any() {
                reconcile_all(publication, conns)
            } else {
                Vec::new()
            };
            let Some(source) = publication.source.as_ref() else {
                continue;
            };
            for subscriber in publication.subscribers.values_mut() {
                if let (Some(transport), Some(conn)) = (
                    subscriber.transport.as_mut(),
                    subscriber.conn.and_then(|id| conns.get_mut(id)),
                ) {
                    transport.poll(conn.rtc(), source, now);
                    if changes.any() {
                        requests.extend(transport.reconcile(conn.rtc(), source));
                    }
                }
            }
            self.request_keyframes(pid, requests, now, conns);
            self.refresh_demand(pid, false, conns);
        }
    }

    fn on_shutdown(&mut self, now: Instant, conns: &mut Connections) {
        self.shutdown(now, conns);
        for conn in self.owners.keys() {
            self.udp.unregister(*conn);
        }
        self.owners.clear();
    }
}
