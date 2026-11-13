//! One inbound native connection whose encoded media becomes ordinary bounded
//! sources. Local playback and downstream P2P edges attach without a decoder
//! or encoder.

use std::sync::Arc;
use std::time::Instant;

use piik_media_forwarding::{SenderReport, ntp_from_system_time};
use piik_media_transport::sdp::{VideoCodec, forwardable_h264, select_offer, set_opus_fmtp};
use piik_media_transport::str0m::Event;
use piik_media_transport::str0m::media::{KeyframeRequestKind, MediaKind, Mid};
use piik_media_transport::str0m::rtp::Ssrc;
use piik_media_transport::{
    ConnEvent, ConnId, Connection, Connections, IceCandidateInit, PcState, PunchedSocket,
};

use crate::audio::{AudioPacket, AudioSource};
use crate::codec;
use crate::engine::{App, Engine, Owner, next_key};
use crate::error::{Error, Result};
use crate::events::{ChannelEvent, EdgeEvent, EventSink, StateWatch, discard};
use crate::gather::IceServerUrls;
use crate::relay::{RelayOptions, relay_backend};
use crate::signal::{Link, Signal, trickle_sdp};
use crate::source::VideoSource;

/// One received video RTP packet as the source and relay consume it.
#[derive(Debug, Clone)]
pub(crate) struct ReceivedRtp {
    pub sequence_number: u16,
    pub ext_seq: u64,
    pub ext_timestamp: u64,
    pub timestamp: u32,
    pub marker: bool,
    pub payload: Arc<[u8]>,
    pub arrival: Instant,
    pub size: usize,
}

#[derive(Default)]
pub struct ReceiverOptions {
    pub offer: String,
    pub ice_servers: Vec<IceServerUrls>,
    pub edge_capacity: usize,
    pub relay: Option<RelayOptions>,
    /// A consumed preparation; ownership transfers even if the offer fails.
    pub punched_socket: Option<PunchedSocket>,
    /// Candidates, state, path, data channels and the input transport.
    pub events: Option<EventSink>,
}

pub(crate) struct ReceiverState {
    pub source: u64,
    pub audio: Option<u64>,
    pub conn: ConnId,
    pub signal: Signal,
    codec: VideoCodec,
    video_mid: Option<Mid>,
    audio_mid: Option<Mid>,
    video_ssrc: Option<Ssrc>,
    mid: String,
    /// Accepted received video packets.
    pub video_packets: u64,
    /// Test record of received `(timestamp, payload)`.
    #[cfg(test)]
    pub received: Vec<(u32, Arc<[u8]>)>,
}

/// One SDP media section's kind and attributes.
struct Section<'a> {
    kind: &'a str,
    port: &'a str,
    formats: Vec<&'a str>,
    attributes: Vec<&'a str>,
}

fn sections(sdp: &str) -> Vec<Section<'_>> {
    let mut sections: Vec<Section<'_>> = Vec::new();
    for line in sdp.lines() {
        if let Some(media) = line.strip_prefix("m=") {
            let mut fields = media.split_whitespace();
            let kind = fields.next().unwrap_or_default();
            let port = fields.next().unwrap_or_default();
            let _proto = fields.next();
            sections.push(Section {
                kind,
                port,
                formats: fields.collect(),
                attributes: Vec::new(),
            });
        } else if let (Some(section), Some(attribute)) =
            (sections.last_mut(), line.strip_prefix("a="))
        {
            section.attributes.push(attribute);
        }
    }
    sections
}

/// Whether an active, sending section offers
/// `codec_name` (H.264 only when forwardable). A zero-port `bundle-only`
/// section still offers media on the bundle's transport (RFC 9143).
pub(crate) fn offer_sends_codec(sdp: &str, kind: &str, codec_name: &str) -> Result<bool> {
    if !sdp.starts_with("v=") {
        return Err(Error::Invalid("native media receiver SDP is invalid"));
    }
    for section in sections(sdp) {
        let bundle_only = section.attributes.contains(&"bundle-only");
        if section.kind != kind || section.port == "0" && !bundle_only {
            continue;
        }
        if section
            .attributes
            .iter()
            .any(|a| *a == "inactive" || *a == "recvonly")
        {
            continue;
        }
        for attribute in &section.attributes {
            let Some(rtpmap) = attribute.strip_prefix("rtpmap:") else {
                continue;
            };
            let Some((payload, rest)) = rtpmap.split_once(' ') else {
                continue;
            };
            let name = rest.split('/').next().unwrap_or_default();
            if !section.formats.contains(&payload) || !name.eq_ignore_ascii_case(codec_name) {
                continue;
            }
            let fmtp = section
                .attributes
                .iter()
                .find_map(|a| a.strip_prefix(&format!("fmtp:{payload} ")))
                .unwrap_or_default();
            if !codec_name.eq_ignore_ascii_case("h264") || forwardable_h264(fmtp) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// The first section's mid, which trickled candidates name.
fn first_mid(sdp: &str) -> String {
    sections(sdp)
        .first()
        .and_then(|section| {
            section
                .attributes
                .iter()
                .find_map(|a| a.strip_prefix("mid:"))
        })
        .unwrap_or("0")
        .to_owned()
}

/// Opus stereo is a local receive preference, not one inherited from the
/// offer; it applies to every answer.
fn answer_policy(answer: &str) -> Result<String> {
    let max = piik_native_audio::MAX_BITRATE.to_string();
    let answer = set_opus_fmtp(answer, &[("stereo", "1"), ("maxaveragebitrate", &max)])?;
    // A trickled answer: only the merged gathering announces completion.
    Ok(trickle_sdp(&answer))
}

impl App {
    /// Sends a PLI on the received video.
    pub(crate) fn receiver_request_keyframe(
        &mut self,
        conns: &mut Connections,
        key: u64,
        _now: Instant,
    ) {
        let Some(receiver) = self.receivers.get(&key) else {
            return;
        };
        let (Some(ssrc), Some(conn)) = (receiver.video_ssrc, conns.get_mut(receiver.conn)) else {
            return;
        };
        if let Some(stream) = conn.rtc().direct_api().stream_rx(&ssrc) {
            stream.request_keyframe(KeyframeRequestKind::Pli);
        }
    }

    pub(crate) fn close_receiver(&mut self, conns: &mut Connections, key: u64, now: Instant) {
        let Some(mut receiver) = self.receivers.remove(&key) else {
            return;
        };
        self.owners.remove(&receiver.conn);
        conns.close(receiver.conn);
        receiver.signal.close();
        self.close_source(conns, receiver.source, now);
        if let Some(audio) = receiver.audio {
            self.close_audio(conns, audio, now);
        }
    }

    pub(crate) fn receiver_event(
        &mut self,
        conns: &mut Connections,
        key: u64,
        event: ConnEvent,
        now: Instant,
    ) {
        let Some(receiver) = self.receivers.get_mut(&key) else {
            return;
        };
        match event {
            ConnEvent::State(state) => {
                receiver.signal.on_state(state, true);
                if state == PcState::Connected {
                    // A retained source may have lost dependencies while ICE
                    // restarted; refresh it before downstream decoders must.
                    self.receiver_request_keyframe(conns, key, now);
                }
            }
            ConnEvent::ChannelOpen(channel) => receiver
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Open(channel))),
            ConnEvent::ChannelData(channel, data) => {
                receiver.signal.emit(EdgeEvent::Channel(ChannelEvent::Data {
                    channel,
                    binary: data.binary,
                    data: data.data,
                }));
            }
            ConnEvent::ChannelClose(channel) => receiver
                .signal
                .emit(EdgeEvent::Channel(ChannelEvent::Close(channel))),
            ConnEvent::Rtc(event) => self.receiver_rtc_event(conns, key, &event, now),
        }
    }

    fn receiver_rtc_event(
        &mut self,
        conns: &mut Connections,
        key: u64,
        event: &Event,
        now: Instant,
    ) {
        let Some(receiver) = self.receivers.get_mut(&key) else {
            return;
        };
        match event {
            Event::PeerStats(stats) => receiver.signal.on_stats(stats),
            Event::RtpPacket(packet) => {
                let Some(conn) = conns.get_mut(receiver.conn) else {
                    return;
                };
                let Some((mid, _)) = conn.rx_stream(packet.header.ssrc) else {
                    return;
                };
                if Some(mid) == receiver.video_mid {
                    receiver.video_ssrc = Some(packet.header.ssrc);
                    let received = ReceivedRtp {
                        sequence_number: packet.header.sequence_number,
                        ext_seq: *packet.seq_no,
                        ext_timestamp: packet.time.numer(),
                        timestamp: packet.header.timestamp,
                        marker: packet.header.marker,
                        payload: packet.payload.clone(),
                        arrival: packet.timestamp,
                        size: packet.header.header_len + packet.payload.len(),
                    };
                    receiver.video_packets += 1;
                    #[cfg(test)]
                    receiver
                        .received
                        .push((packet.header.timestamp, packet.payload.clone()));
                    let source = receiver.source;
                    // Each edge owns its write failure.
                    let _ = self.write_received_rtp(conns, source, &received, now);
                } else if Some(mid) == receiver.audio_mid
                    && let Some(audio) = receiver.audio
                {
                    let received = AudioPacket {
                        ext_seq: *packet.seq_no,
                        ext_timestamp: packet.time.numer(),
                        marker: packet.header.marker,
                        payload: packet.payload.clone(),
                        arrival: packet.timestamp,
                    };
                    let _ = self.write_audio(conns, audio, &received, now);
                }
            }
            Event::SenderFeedback(feedback) if Some(feedback.mid) == receiver.video_mid => {
                let info = &feedback.sender_info;
                if receiver.video_ssrc != Some(info.ssrc) {
                    return;
                }
                let source = receiver.source;
                let _ = self.sender_report(
                    source,
                    SenderReport {
                        rtp_timestamp: info.rtp_time.numer() as u32,
                        ntp_time: ntp_from_system_time(info.ntp_time),
                        packets: info.sender_packet_count,
                        octets: u64::from(info.sender_octet_count),
                        at: feedback.received_at,
                        at_adjusted: feedback.received_at,
                    },
                );
            }
            _ => {}
        }
    }

    fn receiver_mids(conn: &Connection, answer: &str) -> (Option<Mid>, Option<Mid>) {
        let mut mids = (None, None);
        for section in sections(answer) {
            let Some(mid) = section
                .attributes
                .iter()
                .find_map(|a| a.strip_prefix("mid:"))
            else {
                continue;
            };
            let mid = Mid::from(mid);
            match conn.media_kind(mid) {
                Some(MediaKind::Video) if mids.0.is_none() => mids.0 = Some(mid),
                Some(MediaKind::Audio) if mids.1.is_none() => mids.1 = Some(mid),
                _ => {}
            }
        }
        mids
    }

    fn receiver_renegotiate(
        &mut self,
        conns: &mut Connections,
        key: u64,
        offer: &str,
        servers: Vec<IceServerUrls>,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> Result<Option<String>> {
        let has_audio = offer_sends_codec(offer, "audio", "opus")?;
        let receiver = self
            .receivers
            .get_mut(&key)
            .ok_or(Error::Invalid("native media receiver is closed"))?;
        let same_video = offer_sends_codec(offer, "video", receiver.codec.name())?;
        if !same_video || has_audio != receiver.audio.is_some() {
            return Ok(None);
        }
        receiver.signal.servers = servers;
        let conn = conns
            .get_mut(receiver.conn)
            .ok_or(Error::Invalid("native media receiver is closed"))?;
        let (narrowed, _) = select_offer(offer, &[receiver.codec])?;
        let before = conn.local_ufrag();
        let answer = conn.accept_offer(&narrowed)?;
        let ufrag = conn.local_ufrag();
        if ufrag != before {
            // A remote ICE restart starts a new gathering generation; ordinary
            // renegotiation leaves the current one and its fence alone.
            let signal = &receiver.signal;
            signal.link.start_gathering(
                cancel,
                &signal.servers,
                true,
                &receiver.mid,
                &ufrag,
                signal.sink.clone(),
            );
        }
        answer_policy(&answer).map(Some)
    }
}

/// One inbound native connection. Cheap to clone.
#[derive(Clone)]
pub struct Receiver {
    engine: Engine,
    pub(crate) key: u64,
    source: VideoSource,
    audio: Option<AudioSource>,
    state: StateWatch,
}

impl std::fmt::Debug for Receiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Receiver")
            .field("codec", &self.source.codec())
            .field("audio", &self.audio.is_some())
            .field("state", &self.state.get())
            .finish()
    }
}

impl Engine {
    /// Accepts the offer and returns the trickled answer.
    pub async fn new_receiver(&self, options: ReceiverOptions) -> Result<(Receiver, String)> {
        let ReceiverOptions {
            offer,
            ice_servers,
            edge_capacity,
            relay,
            punched_socket,
            events,
        } = options;
        if offer.is_empty() {
            return Err(Error::Invalid("native media receiver input is invalid"));
        }
        let has_audio = offer_sends_codec(&offer, "audio", "opus")?;
        let (narrowed, selection) = select_offer(&offer, &[VideoCodec::H264, VideoCodec::Vp8])
            .map_err(|_| Error::Invalid("native media receiver offer has no supported video"))?;
        let (video, _) = selection.video.ok_or(Error::Invalid(
            "native media receiver offer has no supported video",
        ))?;
        let settings = self.settings();
        let mut link = Link::open(settings, punched_socket, &ice_servers, true)?;
        let mut conn = Connection::new(
            codec::rtc_config(&[video], has_audio, settings.initial_bitrate),
            Instant::now(),
        );
        link.bind(self.inbox(), &mut conn);
        let answer = conn.accept_offer(&narrowed)?;
        link.add_punched_peer(&mut conn)?;
        let (video_mid, audio_mid) = App::receiver_mids(&conn, &answer);
        let mid = first_mid(&answer);
        let ufrag = conn.local_ufrag();
        let answer = answer_policy(&answer)?;
        let state = StateWatch::default();
        let mut signal = Signal::new(
            link,
            events.unwrap_or_else(discard),
            ice_servers,
            state.clone(),
        );
        // The remote offer is applied: trickled candidates apply at once.
        signal.remote_described = true;
        let relay = relay.filter(|relay| relay_backend(video, Some(relay)).is_some());
        let layers = if relay.is_some() { 2 } else { 1 };
        let cancel = self.inner.cancel.clone();
        let (key, source, audio) = self
            .call(move |app, conns, _| {
                let source = app.new_source(video, edge_capacity, layers, relay, None)?;
                let audio = if has_audio {
                    match app.new_audio(edge_capacity, 0) {
                        Ok(audio) => Some(audio),
                        Err(error) => {
                            app.sources.remove(&source);
                            return Err(error);
                        }
                    }
                } else {
                    None
                };
                let key = next_key();
                if let Some(state) = app.sources.get_mut(&source) {
                    // Local output slots all derive from this one upstream
                    // encoded stream.
                    state.upstream = Some(key);
                }
                let id = conns.insert(conn);
                app.owners.insert(id, Owner::Receiver(key));
                signal.link.start_gathering(
                    &cancel,
                    &signal.servers,
                    true,
                    &mid,
                    &ufrag,
                    signal.sink.clone(),
                );
                app.receivers.insert(
                    key,
                    ReceiverState {
                        source,
                        audio,
                        conn: id,
                        signal,
                        codec: video,
                        video_mid,
                        audio_mid,
                        video_ssrc: None,
                        mid,
                        video_packets: 0,
                        #[cfg(test)]
                        received: Vec::new(),
                    },
                );
                Ok::<_, Error>((key, source, audio))
            })
            .await??;
        Ok((
            Receiver {
                engine: self.clone(),
                key,
                source: VideoSource::handle(self.clone(), source, video),
                audio: audio.map(|audio| self.relayed_audio(audio)),
                state,
            },
            answer,
        ))
    }
}

impl Receiver {
    pub fn source(&self) -> &VideoSource {
        &self.source
    }

    pub fn audio_source(&self) -> Option<&AudioSource> {
        self.audio.as_ref()
    }

    pub fn has_audio(&self) -> bool {
        self.audio.is_some()
    }

    pub fn codec(&self) -> &'static str {
        self.source.codec()
    }

    pub fn state(&self) -> PcState {
        self.state.get()
    }

    pub fn state_watch(&self) -> StateWatch {
        self.state.clone()
    }

    /// Keeps the encoded source and its consumers when the offer describes
    /// the same media shape. `None` means a changed codec or audio shape that
    /// needs replacement.
    pub async fn renegotiate(
        &self,
        offer: &str,
        servers: Vec<IceServerUrls>,
    ) -> Result<Option<String>> {
        if offer.is_empty() {
            return Err(Error::Invalid("native media receiver input is invalid"));
        }
        let key = self.key;
        let offer = offer.to_owned();
        let cancel = self.engine.inner.cancel.clone();
        self.engine
            .call(move |app, conns, _| {
                app.receiver_renegotiate(conns, key, &offer, servers, &cancel)
            })
            .await?
    }

    /// Adds a trickled remote candidate; invalid and late ones are dropped.
    pub async fn add_remote_candidate(&self, candidate: Option<IceCandidateInit>) -> Result<()> {
        let key = self.key;
        let candidate = candidate.unwrap_or_default();
        self.engine
            .call(move |app, conns, _| {
                let Some(receiver) = app.receivers.get_mut(&key) else {
                    return Ok(());
                };
                let Some(conn) = conns.get_mut(receiver.conn) else {
                    return Ok(());
                };
                receiver.signal.candidate(conn, candidate)
            })
            .await?
    }

    /// Requests a key frame with a PLI upstream.
    pub async fn request_key_frame(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.receiver_request_keyframe(conns, key, now))
            .await;
    }

    /// Retires the connection, its sources and their edges.
    pub async fn close(&self) {
        let key = self.key;
        let _ = self
            .engine
            .call(move |app, conns, now| app.close_receiver(conns, key, now))
            .await;
        self.state.set(PcState::Closed);
    }
}

#[cfg(test)]
mod tests;
