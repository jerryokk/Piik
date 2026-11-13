//! One outbound Native peer connection on its own UDP socket and its quality
//! samples.

use std::collections::HashMap;
use std::time::{Instant, SystemTime};

use piik_media_forwarding::{OutputState, Source};
use piik_media_session::{
    AudioAttachment, AudioSlot, AudioSlotOptions, SourcePacket, TransportOptions, UpstreamKeyframe,
    VideoSend,
};
use piik_media_transport::sdp::{OPUS_PT, VideoCodec};
use piik_media_transport::str0m::media::{Direction, MediaKind, Pt};
use piik_media_transport::str0m::{Event, Rtc};
use piik_media_transport::{ConnEvent, ConnId, Connection, Connections, IceCandidateInit, PcState};

use crate::audio::AudioSource;
use crate::codec;
use crate::engine::{App, Engine, Owner, next_key};
use crate::error::{Error, Result};
use crate::events::{ChannelEvent, EdgeEvent, EventSink, StateWatch, discard};
use crate::gather::IceServerUrls;
use crate::groups::MediaId;
use crate::quality::{Baseline, QualitySample, unix_millis, window};
use crate::signal::{Link, Signal, strip_end_of_candidates, trickle_sdp};
use crate::source::{Consumer, ORIGINAL, VideoSource};

mod api;

pub use api::Edge;

#[derive(Clone, Default)]
pub struct EdgeOptions {
    pub connection_id: String,
    pub ice_servers: Vec<IceServerUrls>,
    pub audio: Option<AudioSource>,
    /// A local preview bridge: complete SDP, no STUN or gateway discovery,
    /// and its own capacity slot outside the route bound.
    pub local: bool,
    /// Adopt this engine's completed connection preparation.
    pub punched: bool,
    /// Receives candidates, state, path, channel and transport events.
    pub events: Option<EventSink>,
}

/// One audio send slot with its current attachment.
pub(crate) struct AudioOutput {
    pub slot: AudioSlot,
    pub attachment: Option<AudioAttachment>,
}

pub(crate) struct EdgeState {
    pub source: u64,
    pub media: MediaId,
    pub audio: Option<u64>,
    pub conn: ConnId,
    pub signal: Signal,
    pub state: StateWatch,
    pub transport: Option<VideoSend>,
    pub audio_output: Option<AudioOutput>,
    pub local: bool,
    codec: VideoCodec,
    initial_bitrate: i64,
    max_layer: i32,
    baseline: Baseline,
    frames: u64,
    video_bytes: u64,
    pub closed: bool,
}

impl EdgeState {
    pub fn output_state(&self, source: &Source) -> OutputState {
        match &self.transport {
            Some(transport) => transport.state(source),
            None => OutputState {
                video_budget: self.initial_bitrate,
                ..OutputState::CLOSED
            },
        }
    }

    pub fn target_bitrate(&self) -> (i64, bool) {
        self.transport
            .as_ref()
            .map_or((self.initial_bitrate, false), VideoSend::target_bitrate)
    }

    pub fn required_active_count(&self, source: &Source) -> usize {
        self.transport
            .as_ref()
            .map_or(0, |transport| transport.required_active_count(source))
    }

    pub fn max_spatial_layer(&self) -> i32 {
        self.transport.as_ref().map_or(self.max_layer, |transport| {
            transport.output().forwarder().max_layer().spatial
        })
    }

    pub fn begin_frame(&mut self) {
        if let Some(transport) = &mut self.transport {
            transport.begin_frame();
        }
    }

    pub fn forward(
        &mut self,
        rtc: &mut Rtc,
        source: &Source,
        packets: &[SourcePacket],
        changed: bool,
        now: Instant,
        requests: &mut Vec<UpstreamKeyframe>,
    ) {
        let Some(transport) = &mut self.transport else {
            return;
        };
        if changed {
            requests.extend(transport.reconcile(rtc, source));
        }
        for packet in packets {
            let before = transport.egress().packets;
            requests.extend(transport.forward(rtc, source, packet, now));
            if transport.egress().packets > before {
                self.video_bytes += packet.payload.len() as u64;
                if packet.info.marker && !packet.payload.is_empty() {
                    self.frames += 1;
                }
            }
        }
    }

    pub fn replace_source(
        &mut self,
        rtc: &mut Rtc,
        current: &Source,
        next: &Source,
    ) -> Vec<UpstreamKeyframe> {
        self.transport
            .as_mut()
            .and_then(|transport| transport.replace_source(rtc, current, next).ok())
            .unwrap_or_default()
    }

    /// Adds the video, audio and requested channels to the first offer.
    fn offer(&mut self, conn: &mut Connection, restart: bool, source: &Source) -> Result<String> {
        let audio = self.audio.is_some();
        let mut mids = (None, None);
        let offer = self.signal.offer(conn, restart, |api| {
            mids.0 = Some(api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None));
            if audio {
                mids.1 =
                    Some(api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None));
            }
        })?;
        if let Some(mid) = mids.0 {
            let mut transport = VideoSend::new(
                TransportOptions {
                    mid,
                    pt: Pt::new_with_value(codec::video_pt(self.codec)),
                    initial_bitrate: self.initial_bitrate,
                    // A relay's local bridge forwards the received encoding;
                    // the Host's own edge forwards its capture. Either way the
                    // source is not this edge's encoder, so capacity is held
                    // only on confirmed overuse.
                    forwarded: true,
                },
                source,
            )?;
            transport.set_max_spatial_layer(conn.rtc(), source, self.max_layer);
            self.transport = Some(transport);
        }
        if let Some(mid) = mids.1 {
            self.audio_output = Some(AudioOutput {
                slot: AudioSlot::new(AudioSlotOptions {
                    mid,
                    pt: Pt::new_with_value(OPUS_PT),
                    source_ceiling: piik_native_audio::MAX_BITRATE,
                }),
                attachment: None,
            });
        }
        Ok(offer)
    }

    fn mid(&self) -> String {
        self.transport
            .as_ref()
            .map_or_else(|| "0".to_owned(), |transport| transport.mid().to_string())
    }
}

/// Route edges plus reservations within capacity; one local bridge apart.
pub(crate) fn admit(
    edges: &HashMap<u64, bool>,
    reservations: usize,
    capacity: usize,
    local: bool,
) -> Result<()> {
    if local {
        return if edges.values().any(|local| *local) {
            Err(Error::SourceCapacity)
        } else {
            Ok(())
        };
    }
    let routes = edges.values().filter(|local| !**local).count();
    if routes + reservations >= capacity {
        return Err(Error::SourceCapacity);
    }
    Ok(())
}

/// What a new edge needs on the shard.
struct NewEdge {
    source: u64,
    audio: Option<u64>,
    local: bool,
    codec: VideoCodec,
    signal: Signal,
}

impl App {
    /// Admission for an edge's video and optional audio.
    pub(crate) fn admit_edge(&self, source: u64, audio: Option<u64>, local: bool) -> Result<()> {
        if self.closed {
            return Err(Error::EngineClosed);
        }
        let state = self
            .sources
            .get(&source)
            .filter(|state| !state.closed)
            .ok_or(Error::Closed)?;
        admit(&state.edges, state.reservations, state.capacity, local)?;
        if let Some(audio) = audio {
            let state = self
                .audios
                .get(&audio)
                .filter(|state| !state.closed)
                .ok_or(Error::Invalid("native audio source is closed"))?;
            admit(&state.edges, state.reservations, state.capacity, local)?;
        }
        Ok(())
    }

    fn insert_edge(
        &mut self,
        conns: &mut Connections,
        new: NewEdge,
        conn: Connection,
    ) -> Result<u64> {
        self.admit_edge(new.source, new.audio, new.local)?;
        let state = &self.sources[&new.source];
        // An edge starts at the highest available output.
        let highest = (0..state.format_count)
            .rev()
            .find(|layer| {
                state
                    .output_bitrates
                    .get(*layer)
                    .is_none_or(|bitrate| *bitrate != 0)
            })
            .ok_or(Error::Invalid("native output layer is unavailable"))?;
        let key = next_key();
        let id = conns.insert(conn);
        self.owners.insert(id, Owner::Edge(key));
        let state = new.signal.state.clone();
        self.edges.insert(
            key,
            EdgeState {
                source: new.source,
                media: ORIGINAL,
                audio: new.audio,
                conn: id,
                signal: new.signal,
                state,
                transport: None,
                audio_output: None,
                local: new.local,
                codec: new.codec,
                initial_bitrate: self.settings.initial_bitrate,
                max_layer: highest as i32,
                baseline: Baseline::default(),
                frames: 0,
                video_bytes: 0,
                closed: false,
            },
        );
        if let Some(state) = self.sources.get_mut(&new.source) {
            state.edges.insert(key, new.local);
        }
        if let Some(audio) = new.audio
            && let Some(state) = self.audios.get_mut(&audio)
        {
            state.edges.insert(key, new.local);
        }
        Ok(key)
    }

    /// The transport is up; forwarding begins acquiring its source.
    pub(crate) fn connect_edge(&mut self, conns: &mut Connections, key: u64) -> bool {
        let App {
            edges,
            sources,
            audios,
            ..
        } = self;
        let Some(edge) = edges.get_mut(&key) else {
            return false;
        };
        let (Some(state), Some(conn)) = (sources.get(&edge.source), conns.get_mut(edge.conn))
        else {
            return false;
        };
        let (Some(source), Some(transport)) = (state.source_of(edge.media), &mut edge.transport)
        else {
            return false;
        };
        if transport.set_connected(conn.rtc(), source).is_err() {
            return false;
        }
        if let (Some(output), Some(audio)) = (
            &mut edge.audio_output,
            edge.audio.and_then(|key| audios.get(&key)),
        ) && output.attachment.is_none()
        {
            output.attachment = output.slot.replace_source(&audio.source).ok();
        }
        true
    }

    pub(crate) fn close_edge(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(mut edge) = self.edges.remove(&key) else {
            return;
        };
        edge.closed = true;
        if let Some(conn) = conns.get_mut(edge.conn)
            && let Some(transport) = &mut edge.transport
        {
            transport.close(conn.rtc());
        }
        if let Some(output) = &mut edge.audio_output {
            output.slot.close();
        }
        self.owners.remove(&edge.conn);
        conns.close(edge.conn);
        edge.signal.close();
        if let Some(audio) = edge.audio
            && let Some(state) = self.audios.get_mut(&audio)
        {
            state.edges.remove(&key);
        }
        if let Some(state) = self.sources.get_mut(&edge.source) {
            state.groups.forget(&Consumer::Edge(key));
        }
        self.detach_edge(conns, edge.source, key, now);
    }

    pub(crate) fn edge_event(
        &mut self,
        conns: &mut Connections,
        key: u64,
        event: ConnEvent,
        now: Instant,
    ) {
        let Some(edge) = self.edges.get_mut(&key) else {
            return;
        };
        match event {
            ConnEvent::State(state) => {
                let transport = !edge.signal.channels.is_empty();
                if state == PcState::Connected {
                    if !self.connect_edge(conns, key) {
                        // Retirement can race the connected snapshot; the
                        // failure belongs to this consumer only.
                        self.close_edge(conns, key, now);
                        return;
                    }
                    let source = self.edges[&key].source;
                    if let Some(state) = self.sources.get_mut(&source) {
                        state.request_recovery(None);
                    }
                }
                if let Some(edge) = self.edges.get_mut(&key) {
                    edge.signal.on_state(state, transport);
                }
            }
            ConnEvent::ChannelOpen(channel) => edge
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Open(channel))),
            ConnEvent::ChannelData(channel, data) => {
                edge.signal.emit(EdgeEvent::Channel(ChannelEvent::Data {
                    channel,
                    binary: data.binary,
                    data: data.data,
                }));
            }
            ConnEvent::ChannelClose(channel) => edge
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Close(channel))),
            ConnEvent::Rtc(event) => self.edge_rtc_event(conns, key, &event, now),
        }
    }

    fn edge_rtc_event(&mut self, conns: &mut Connections, key: u64, event: &Event, now: Instant) {
        let App { edges, sources, .. } = self;
        let Some(edge) = edges.get_mut(&key) else {
            return;
        };
        if let Event::PeerStats(stats) = event {
            edge.signal.on_stats(stats);
        }
        let Some(state) = sources.get_mut(&edge.source) else {
            return;
        };
        let request = {
            let (Some(source), Some(transport), Some(conn)) = (
                state.source_of(edge.media),
                &mut edge.transport,
                conns.get_mut(edge.conn),
            ) else {
                return;
            };
            transport.handle_event(conn.rtc(), source, event)
        };
        if let Some(request) = request {
            state.keyframe(edge.media, request, now);
        }
    }

    pub(crate) fn poll_edge(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let App { edges, sources, .. } = self;
        let Some(edge) = edges.get_mut(&key) else {
            return;
        };
        let (Some(state), Some(conn)) = (sources.get(&edge.source), conns.get_mut(edge.conn))
        else {
            return;
        };
        if let (Some(source), Some(transport)) = (state.source_of(edge.media), &mut edge.transport)
        {
            transport.poll(conn.rtc(), source, now);
        }
    }

    fn edge_offer(&mut self, conns: &mut Connections, key: u64, restart: bool) -> Result<String> {
        let App { edges, sources, .. } = self;
        let edge = edges.get_mut(&key).ok_or(Error::Closed)?;
        let conn = conns.get_mut(edge.conn).ok_or(Error::Closed)?;
        let source = sources
            .get(&edge.source)
            .and_then(|state| state.source_of(edge.media))
            .ok_or(Error::Closed)?;
        edge.offer(conn, restart, source)
    }

    fn edge_set_answer(
        &mut self,
        conns: &mut Connections,
        key: u64,
        answer: &str,
        offer_id: &str,
    ) -> Result<()> {
        let edge = self
            .edges
            .get_mut(&key)
            .ok_or(Error::Invalid("native media edge is closed"))?;
        let conn = conns.get_mut(edge.conn).ok_or(Error::Closed)?;
        let audio = edge.audio.is_some();
        edge.signal.answer(conn, answer, offer_id, audio)
    }

    fn edge_add_candidate(
        &mut self,
        conns: &mut Connections,
        key: u64,
        candidate: IceCandidateInit,
    ) -> Result<()> {
        let Some(edge) = self.edges.get_mut(&key) else {
            return Ok(());
        };
        let Some(conn) = conns.get_mut(edge.conn) else {
            return Ok(());
        };
        edge.signal.candidate(conn, candidate)
    }

    fn edge_set_target(&mut self, conns: &mut Connections, key: u64, layer: usize) -> Result<()> {
        let App { edges, sources, .. } = self;
        let edge = edges.get_mut(&key).ok_or(Error::Closed)?;
        let state = sources
            .get_mut(&edge.source)
            .filter(|state| !state.closed)
            .ok_or(Error::Closed)?;
        if layer >= state.format_count {
            return Err(Error::Invalid("native output layer is invalid"));
        }
        if state
            .output_bitrates
            .get(layer)
            .is_some_and(|bitrate| *bitrate == 0)
        {
            return Err(Error::Invalid("native output layer is unavailable"));
        }
        let layer = layer as i32;
        let changed = edge.max_spatial_layer() != layer;
        edge.max_layer = layer;
        if let (Some(transport), Some(conn), Some(source)) = (
            &mut edge.transport,
            conns.get_mut(edge.conn),
            state.source_of(edge.media),
        ) {
            transport.set_max_spatial_layer(conn.rtc(), source, layer);
        }
        if changed {
            state.request_recovery(None);
        }
        Ok(())
    }

    fn edge_quality(&mut self, key: u64, now: SystemTime) -> Option<QualitySample> {
        let App {
            edges,
            sources,
            audios,
            ..
        } = self;
        let edge = edges.get_mut(&key)?;
        let audio_bytes = edge
            .audio
            .and_then(|audio| audios.get(&audio))
            .map_or(0, |audio| audio.bytes);
        let previous = edge.baseline;
        edge.baseline = Baseline {
            at: Some(now),
            frames: edge.frames,
            video_bytes: edge.video_bytes,
            audio_bytes,
        };
        let window = window(previous.at, now)?;
        if edge.frames < previous.frames
            || edge.video_bytes < previous.video_bytes
            || audio_bytes < previous.audio_bytes
        {
            return None;
        }
        let source = sources.get(&edge.source)?.source_of(edge.media)?;
        let frames = edge.frames - previous.frames;
        let bytes = edge.video_bytes - previous.video_bytes + audio_bytes - previous.audio_bytes;
        let seconds = window.as_secs_f64();
        let output = edge.output_state(source);
        let (width, height) = usize::try_from(output.current)
            .ok()
            .and_then(|layer| source.formats().get(layer))
            .map_or((0, 0), |format| (format.width, format.height));
        let mut sample = QualitySample {
            sample_timestamp_ms: unix_millis(now),
            sample_window_ms: window.as_millis() as i64,
            rtp_stats_id: edge.conn.to_string(),
            track_identifier: source.id().to_owned(),
            state: "unknown".into(),
            reason: None,
            interval_frames_sent: frames,
            frames_per_second: frames as f64 / seconds,
            bitrate_kbps: (bytes * 8) as f64 / seconds / 1000.0,
            available_outgoing_kbps: 0.0,
            width,
            height,
        };
        let (target, observed) = edge.target_bitrate();
        if !edge.state.is(PcState::Connected) || !observed || frames == 0 || bytes == 0 {
            return Some(sample);
        }
        sample.available_outgoing_kbps = target as f64 / 1000.0;
        // Allocation and ordinary congestion own the limitation, not a source
        // bitrate spike or an unsuccessful recovery probe.
        let limited = edge
            .transport
            .as_ref()
            .is_some_and(|transport| transport.bandwidth_limited(source));
        sample.state = if limited { "degraded" } else { "healthy" }.into();
        sample.reason = Some(if limited { "bandwidth" } else { "none" }.into());
        Some(sample)
    }
}
