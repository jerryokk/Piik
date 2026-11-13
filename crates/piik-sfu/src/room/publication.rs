//! Publications of one room shard: publisher negotiation, forwarding state
//! and subscriber demand.

use std::collections::HashMap;
use std::time::Instant;

use piik_input_route::{InputChannel, Relay};
use piik_media_forwarding::{Codec, PliThrottle, Source, SourceOptions};
use piik_media_session::{Ingress, UpstreamKeyframe};
use piik_media_transport::sdp::{self, VideoCodec};
use piik_media_transport::str0m::Event;
use piik_media_transport::str0m::media::{Mid, Rid};
use piik_media_transport::{ConnEvent, Connections, PcState};

use crate::media::{lock, unavailable};
use crate::runtime::{
    IceCandidateInit, MediaError, MediaEvent, PeerConnectionState, PublicationMedia, ResourceFence,
    SdpType, SessionDescription,
};

use super::{
    MAX_AUDIO_BITRATE, Owner, Publication, RoomApp, Section, SubscriptionKey, candidate,
    candidate_error, failed, formats, forward_publication_audio, pc_state, reconcile_all,
    required_active_count, sections, sfu_connection, video_kind,
};

// ---------------------------------------------------------------------
// Publications.
// ---------------------------------------------------------------------

impl RoomApp {
    pub(crate) fn create_publication(
        &mut self,
        id: u64,
        fence: ResourceFence,
        connection_id: String,
        metadata: PublicationMedia,
        now: Instant,
        conns: &mut Connections,
    ) {
        let codec = VideoCodec::from_name(&metadata.codec).expect("validated codec");
        let conn = sfu_connection(&self.udp, sdp::codec_config(&[codec], true), now);
        let conn = conns.insert(conn);
        self.owners.insert(conn, Owner::Publisher(id));
        self.publications.insert(
            id,
            Publication {
                fence,
                connection_id,
                metadata,
                codec,
                conn,
                audio_mid: None,
                ingress: None,
                source: None,
                audio_source: None,
                input: None,
                subscribers: HashMap::new(),
                ready: false,
                failed: false,
                demand: None,
            },
        );
    }

    pub(super) fn live(&mut self, pid: u64) -> Result<&mut Publication, MediaError> {
        self.publications
            .get_mut(&pid)
            .filter(|p| !p.failed)
            .ok_or_else(unavailable)
    }

    pub(crate) fn update_publication(
        &mut self,
        pid: u64,
        metadata: PublicationMedia,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let publication = self.live(pid)?;
        if !crate::media::valid_publication_media(&metadata)
            || metadata.codec != publication.metadata.codec
            || metadata.rids != publication.metadata.rids
        {
            return Err(MediaError::failed(
                "SFU media update changed the publication identity",
            ));
        }
        if let Some(source) = publication.source.as_mut() {
            source.update_formats(formats(&metadata)).map_err(failed)?;
        }
        publication.metadata = metadata;
        let requests = reconcile_all(publication, conns);
        self.request_keyframes(pid, requests, Instant::now(), conns);
        self.refresh_demand(pid, true, conns);
        Ok(())
    }

    pub(crate) fn accept_publisher_offer(
        &mut self,
        pid: u64,
        offer: SessionDescription,
        conns: &mut Connections,
    ) -> Result<SessionDescription, MediaError> {
        let publication = self
            .publications
            .get_mut(&pid)
            .ok_or_else(|| MediaError::failed("SFU publication is closed"))?;
        if offer.sdp_type != SdpType::Offer {
            return Err(MediaError::failed("SFU publisher requires an offer"));
        }
        if publication.failed {
            return Err(MediaError::failed("SFU publication is closed"));
        }
        if publication.input.is_none() && offer.sdp.contains("\nm=application ") {
            publication.input = Some(Relay::new(None));
        }
        let mismatch = || MediaError::failed("SFU publisher codec does not match its declaration");
        let (filtered, selection) =
            sdp::select_offer(&offer.sdp, &[publication.codec]).map_err(|_| mismatch())?;
        let offered = sections(&filtered);
        let video: Vec<&Section> = offered
            .iter()
            .filter(|s| s.kind == "video" && s.sending)
            .collect();
        let audio: Vec<&Section> = offered
            .iter()
            .filter(|s| s.kind == "audio" && s.sending)
            .collect();
        if !audio.is_empty() && selection.audio.is_none() {
            return Err(mismatch());
        }
        if video.len() != 1 || audio.len() > 1 || publication.metadata.audio && audio.len() != 1 {
            return Err(MediaError::failed(
                "SFU publisher track count does not match its declaration",
            ));
        }
        let rids = &publication.metadata.rids;
        let declared = |rid: &String| rids.len() > 1 || !rid.is_empty();
        if video[0]
            .rids
            .iter()
            .any(|rid| !rids.contains(rid) || !declared(rid))
        {
            return Err(MediaError::failed("unexpected SFU publisher track"));
        }
        let video_mid = video[0]
            .mid
            .as_deref()
            .map(Mid::from)
            .ok_or_else(mismatch)?;
        let audio_mid = audio.first().and_then(|s| s.mid.as_deref()).map(Mid::from);
        let conn = conns.get_mut(publication.conn).ok_or_else(unavailable)?;
        let answer = conn.accept_offer(&filtered).map_err(failed)?;
        let bitrate = MAX_AUDIO_BITRATE.to_string();
        let answer = sdp::set_opus_fmtp(
            &answer,
            &[
                ("stereo", "1"),
                ("usedtx", "0"),
                ("maxaveragebitrate", bitrate.as_str()),
            ],
        )
        .map_err(failed)?;
        if publication.ingress.is_none() {
            let layers = rids
                .iter()
                .map(|rid| (!rid.is_empty()).then(|| Rid::from(rid.as_str())))
                .collect();
            publication.ingress =
                Some(Ingress::new(video_mid, layers, PliThrottle::default()).map_err(failed)?);
            let generation = publication.fence.publication_generation.clone();
            publication.source = Some(
                Source::new(SourceOptions {
                    id: generation.clone(),
                    stream_id: generation,
                    codec: Codec {
                        kind: video_kind(publication.codec),
                        payload_type: selection.video.map_or(0, |(_, pt)| pt),
                        fmtp: String::new(),
                        channels: 0,
                    },
                    formats: formats(&publication.metadata),
                    max_packets: 0,
                })
                .map_err(failed)?,
            );
        }
        publication.audio_mid = audio_mid.or(publication.audio_mid);
        self.register(conn);
        // Candidates travel in the answer; this ends gathering.
        self.emit(pid, MediaEvent::default());
        Ok(SessionDescription {
            sdp_type: SdpType::Answer,
            sdp: answer,
        })
    }

    pub(crate) fn add_publisher_ice(
        &mut self,
        pid: u64,
        init: &IceCandidateInit,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let publication = self.publications.get(&pid).ok_or_else(unavailable)?;
        conns
            .get_mut(publication.conn)
            .ok_or_else(unavailable)?
            .add_ice_candidate(&candidate(init))
            .map_err(candidate_error)
    }

    pub(super) fn fail_publication(&mut self, pid: u64, conns: &mut Connections) {
        let Some(publication) = self.publications.get_mut(&pid) else {
            return;
        };
        if publication.failed {
            return;
        }
        publication.failed = true;
        let conn = publication.conn;
        if let Some(input) = &publication.input {
            input.set_up_available(false);
        }
        // Preparing subscribers depended on this publisher's input.
        let waiting: Vec<SubscriptionKey> = publication
            .subscribers
            .iter()
            .filter(|(_, s)| s.reply.is_some())
            .map(|(key, _)| key.clone())
            .collect();
        for key in waiting {
            self.remove_subscriber(pid, &key, conns);
        }
        self.drop_conn(conn, conns);
        self.emit(
            pid,
            MediaEvent {
                state: PeerConnectionState::Failed,
                ..Default::default()
            },
        );
    }

    pub(crate) fn close_publication(&mut self, pid: u64, conns: &mut Connections) {
        let Some(publication) = self.publications.get(&pid) else {
            return;
        };
        let keys: Vec<SubscriptionKey> = publication.subscribers.keys().cloned().collect();
        let conn = publication.conn;
        for key in keys {
            self.remove_subscriber(pid, &key, conns);
        }
        let Some(mut publication) = self.publications.remove(&pid) else {
            return;
        };
        if let Some(input) = publication.input.take() {
            input.close();
        }
        if let Some(source) = publication.source.as_mut() {
            source.close();
        }
        if let Some(source) = publication.audio_source.as_mut() {
            source.close();
        }
        self.drop_conn(conn, conns);
    }

    pub(super) fn publisher_event(
        &mut self,
        pid: u64,
        now: Instant,
        event: ConnEvent,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get_mut(&pid) else {
            return;
        };
        match event {
            ConnEvent::State(state) => {
                // ICE loss releases all descendant input even before SCTP closes.
                if let Some(input) = &publication.input {
                    match state {
                        PcState::Connected => input.set_up_available(true),
                        PcState::Disconnected | PcState::Failed | PcState::Closed => {
                            input.set_up_available(false);
                        }
                        _ => {}
                    }
                }
                if state == PcState::Failed {
                    self.fail_publication(pid, conns);
                } else {
                    self.emit(
                        pid,
                        MediaEvent {
                            state: pc_state(state),
                            ..Default::default()
                        },
                    );
                }
            }
            ConnEvent::ChannelOpen(channel) => match &publication.input {
                Some(input) => {
                    if input.attach_up(channel.clone()) {
                        input.up_opened(&channel);
                    }
                }
                None => channel.close(),
            },
            ConnEvent::ChannelData(channel, data) => {
                if let Some(input) = &publication.input {
                    input.up_message(&channel, &data.data, !data.binary);
                }
            }
            ConnEvent::ChannelClose(channel) => {
                if let Some(input) = &publication.input {
                    input.up_closed(&channel);
                }
            }
            ConnEvent::Rtc(event) => self.publisher_rtc(pid, now, &event, conns),
        }
    }

    pub(super) fn publisher_rtc(
        &mut self,
        pid: u64,
        now: Instant,
        event: &Event,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get_mut(&pid).filter(|p| !p.failed) else {
            return;
        };
        let Some(conn) = conns.get_mut(publication.conn) else {
            return;
        };
        if let Event::RtpPacket(packet) = event
            && publication.audio_mid.is_some()
            && conn.rx_stream(packet.header.ssrc).map(|(mid, _)| mid) == publication.audio_mid
        {
            forward_publication_audio(publication, packet, now, conns);
            return;
        }
        let (Some(ingress), Some(source)) =
            (publication.ingress.as_mut(), publication.source.as_mut())
        else {
            return;
        };
        let received = match ingress.handle_event(conn.rtc(), source, event) {
            Ok(Some(received)) => received,
            Ok(None) => return,
            Err(_) => {
                self.fail_publication(pid, conns);
                return;
            }
        };
        let mut requests = Vec::new();
        for subscriber in publication.subscribers.values_mut() {
            let (Some(transport), Some(conn)) = (
                subscriber.transport.as_mut(),
                subscriber.conn.and_then(|id| conns.get_mut(id)),
            ) else {
                continue;
            };
            if received.changes.any() {
                requests.extend(transport.reconcile(conn.rtc(), source));
            }
            requests.extend(transport.forward(conn.rtc(), source, &received.packet, now));
        }
        let became_ready = !publication.ready
            && (0..source.layer_count()).any(|layer| source.layer_ssrc(layer).is_some());
        if let Some(conn) = conns.get_mut(publication.conn) {
            for request in requests {
                ingress.request_keyframe(conn.rtc(), request, now);
            }
        }
        if became_ready {
            publication.ready = true;
            let waiting: Vec<SubscriptionKey> = publication
                .subscribers
                .iter()
                .filter(|(_, s)| s.reply.is_some())
                .map(|(key, _)| key.clone())
                .collect();
            for key in waiting {
                self.complete_subscriber(pid, &key, now, conns);
            }
        }
        self.refresh_demand(pid, false, conns);
    }

    pub(super) fn request_keyframes(
        &mut self,
        pid: u64,
        requests: Vec<UpstreamKeyframe>,
        now: Instant,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get_mut(&pid) else {
            return;
        };
        if let (Some(ingress), Some(conn)) = (
            publication.ingress.as_mut(),
            conns.get_mut(publication.conn),
        ) {
            for request in requests {
                ingress.request_keyframe(conn.rtc(), request, now);
            }
        }
    }

    /// Reports active-layer demand on change after the first wake (a
    /// subscriber, transport or metadata change).
    pub(super) fn refresh_demand(&mut self, pid: u64, wake: bool, conns: &Connections) {
        let Some(publication) = self.publications.get_mut(&pid) else {
            return;
        };
        if !wake && publication.demand.is_none() {
            return;
        }
        let active = required_active_count(publication, conns);
        if publication.demand == Some(active) {
            return;
        }
        publication.demand = Some(active);
        let fence = publication.fence.clone();
        if let Some(shared) = self.shared.upgrade() {
            let mut registry = lock(&shared.registry);
            if let Some(entry) = registry.publications.get_mut(&fence)
                && entry.id == pid
            {
                entry.demand = active;
            }
        }
        self.emit(
            pid,
            MediaEvent {
                active_count: Some(active),
                ..Default::default()
            },
        );
    }
}
