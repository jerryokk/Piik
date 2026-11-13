//! Subscriber preparation, negotiation, forwarding and removal for one SFU
//! publication.

use std::sync::Arc;
use std::time::Instant;

use piik_input_route::{InputChannel, LABEL};
use piik_media_session::{AudioSlot, AudioSlotOptions, TransportOptions, VideoSend};
use piik_media_transport::sdp;
use piik_media_transport::str0m::bwe::Bitrate;
use piik_media_transport::str0m::media::{Direction, MediaKind};
use piik_media_transport::{ConnEvent, Connections, PcState};

use crate::media::{Reply, unavailable};
use crate::runtime::{
    IceCandidateInit, MediaError, MediaEvent, SdpType, SessionDescription, SubscriptionFence,
};

use super::{
    MAX_AUDIO_BITRATE, Owner, RoomApp, STATS_INTERVAL, Subscriber, SubscriptionKey, candidate,
    candidate_error, failed, pc_state, sfu_connection, video_pt,
};

impl RoomApp {
    // ---------------------------------------------------------------------
    // Subscribers.
    // ---------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)] // one shard job's captured request
    pub(crate) fn prepare_subscriber(
        &mut self,
        pid: u64,
        key: SubscriptionKey,
        resource_connection_id: String,
        token: u64,
        reply: Reply<SessionDescription>,
        now: Instant,
        conns: &mut Connections,
    ) {
        let Some(publication) = self
            .publications
            .get_mut(&pid)
            .filter(|p| !p.failed && !p.subscribers.contains_key(&key))
        else {
            let _ = reply.send(Err(MediaError::failed(
                "SFU subscription media is unavailable",
            )));
            return;
        };
        publication.subscribers.insert(
            key.clone(),
            Subscriber {
                resource_connection_id,
                token,
                reply: Some(reply),
                conn: None,
                transport: None,
                audio: None,
                pending: None,
                input: None,
            },
        );
        let ready = publication.ready;
        self.refresh_demand(pid, true, conns);
        if ready {
            self.complete_subscriber(pid, &key, now, conns);
        }
    }

    /// Builds the subscriber connection once the publisher's video exists
    /// and answers the waiting preparation.
    pub(super) fn complete_subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        now: Instant,
        conns: &mut Connections,
    ) {
        let result = self.build_subscriber(pid, key, now, conns);
        let Some(subscriber) = self
            .publications
            .get_mut(&pid)
            .and_then(|p| p.subscribers.get_mut(key))
        else {
            return;
        };
        let Some(reply) = subscriber.reply.take() else {
            return;
        };
        match result {
            Ok(offer) => {
                let _ = reply.send(Ok(offer));
                // Candidates travel in the offer; this ends gathering.
                self.emit_subscriber(pid, key, MediaEvent::default());
            }
            Err(error) => {
                let _ = reply.send(Err(error));
                self.remove_subscriber(pid, key, conns);
            }
        }
        self.refresh_demand(pid, true, conns);
    }

    pub(super) fn build_subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        now: Instant,
        conns: &mut Connections,
    ) -> Result<SessionDescription, MediaError> {
        let publication = self.publications.get_mut(&pid).ok_or_else(unavailable)?;
        let source = publication.source.as_ref().ok_or_else(unavailable)?;
        let initial = publication
            .metadata
            .formats
            .last()
            .map_or(1_000_000, |format| i64::from(format.bitrate));
        let config = sdp::codec_config(&[publication.codec], true)
            .enable_bwe(Some(Bitrate::bps(initial.unsigned_abs())))
            .set_stats_interval(Some(STATS_INTERVAL));
        let mut conn = sfu_connection(&self.udp, config, now);
        let stream = Some(source.stream_id().to_owned());
        let input = publication.input.is_some();
        let (mut video, mut audio) = (None, None);
        let (offer, pending) = conn
            .create_offer(|api| {
                video = Some(api.add_media(
                    MediaKind::Video,
                    Direction::SendOnly,
                    stream.clone(),
                    Some("video".to_owned()),
                    None,
                ));
                audio = Some(api.add_media(
                    MediaKind::Audio,
                    Direction::SendOnly,
                    stream,
                    Some("audio".to_owned()),
                    None,
                ));
                if input {
                    api.add_channel(LABEL.to_owned());
                }
            })
            .ok_or_else(unavailable)?;
        let (Some(video), Some(audio)) = (video, audio) else {
            return Err(unavailable());
        };
        let transport = VideoSend::new(
            TransportOptions {
                mid: video,
                pt: video_pt(publication.codec),
                initial_bitrate: initial,
                forwarded: false,
            },
            source,
        )
        .map_err(failed)?;
        let mut slot = AudioSlot::new(AudioSlotOptions {
            mid: audio,
            pt: sdp::OPUS_PT.into(),
            source_ceiling: MAX_AUDIO_BITRATE,
        });
        let attachment = publication
            .audio_source
            .as_ref()
            .and_then(|source| slot.replace_source(source).ok());
        self.register(&mut conn);
        let conn = conns.insert(conn);
        self.owners
            .insert(conn, Owner::Subscriber(pid, key.clone()));
        let subscriber = self
            .publications
            .get_mut(&pid)
            .and_then(|p| p.subscribers.get_mut(key))
            .ok_or_else(unavailable)?;
        subscriber.conn = Some(conn);
        subscriber.transport = Some(transport);
        subscriber.audio = Some((slot, attachment));
        subscriber.pending = Some(pending);
        Ok(SessionDescription {
            sdp_type: SdpType::Offer,
            sdp: offer,
        })
    }

    pub(super) fn subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
    ) -> Result<&mut Subscriber, MediaError> {
        self.live(pid)?
            .subscribers
            .get_mut(key)
            .filter(|s| s.transport.is_some())
            .ok_or_else(|| MediaError::failed("SFU subscription media is unavailable"))
    }

    /// A timed-out or abandoned preparation retires its exact attempt.
    pub(crate) fn cancel_subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        token: u64,
        conns: &mut Connections,
    ) {
        let current = self
            .publications
            .get(&pid)
            .and_then(|p| p.subscribers.get(key))
            .is_some_and(|s| s.token == token);
        if current {
            self.remove_subscriber(pid, key, conns);
            self.refresh_demand(pid, true, conns);
        }
    }

    pub(crate) fn apply_subscriber_answer(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        answer: &SessionDescription,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let subscriber = self.subscriber(pid, key)?;
        if answer.sdp_type != SdpType::Answer {
            return Err(MediaError::failed("SFU subscriber requires an answer"));
        }
        if let Some((slot, _)) = &subscriber.audio {
            slot.check_receiver(&answer.sdp).map_err(failed)?;
        }
        let pending = subscriber
            .pending
            .take()
            .ok_or_else(|| MediaError::failed("SFU subscriber has no pending offer"))?;
        let conn = subscriber
            .conn
            .and_then(|id| conns.get_mut(id))
            .ok_or_else(unavailable)?;
        conn.accept_answer(pending, &answer.sdp).map_err(failed)?;
        // An accepted ICE restart replaces the local ufrag.
        self.register(conn);
        Ok(())
    }

    pub(crate) fn add_subscriber_ice(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        init: &IceCandidateInit,
        conns: &mut Connections,
    ) -> Result<(), MediaError> {
        let subscriber = self.subscriber(pid, key)?;
        subscriber
            .conn
            .and_then(|id| conns.get_mut(id))
            .ok_or_else(unavailable)?
            .add_ice_candidate(&candidate(init))
            .map_err(candidate_error)
    }

    pub(crate) fn restart_subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        conns: &mut Connections,
    ) -> Result<SessionDescription, MediaError> {
        let subscriber = self.subscriber(pid, key)?;
        let conn = subscriber
            .conn
            .and_then(|id| conns.get_mut(id))
            .ok_or_else(unavailable)?;
        let (offer, pending) = conn
            .create_offer(|api| {
                api.ice_restart(true);
            })
            .ok_or_else(unavailable)?;
        subscriber.pending = Some(pending);
        self.emit_subscriber(pid, key, MediaEvent::default());
        Ok(SessionDescription {
            sdp_type: SdpType::Offer,
            sdp: offer,
        })
    }

    /// Closes the exact connection, or every connection of the Viewer's
    /// admitted room edge when `connection_id` names it.
    pub(crate) fn close_subscription(
        &mut self,
        pid: u64,
        fence: &SubscriptionFence,
        connection_id: &str,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get(&pid) else {
            return;
        };
        let keys: Vec<SubscriptionKey> = publication
            .subscribers
            .iter()
            .filter(|(key, subscriber)| {
                key.peer_id == fence.viewer_peer_id
                    && (key.connection_id == connection_id
                        || connection_id == fence.connection_id
                            && subscriber.resource_connection_id == connection_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        if keys.is_empty() {
            return;
        }
        for key in keys {
            self.remove_subscriber(pid, &key, conns);
        }
        self.refresh_demand(pid, true, conns);
    }

    pub(super) fn remove_subscriber(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get_mut(&pid) else {
            return;
        };
        let Some(mut subscriber) = publication.subscribers.remove(key) else {
            return;
        };
        if let Some(reply) = subscriber.reply.take() {
            let _ = reply.send(Err(MediaError::failed(
                "SFU subscription media is unavailable",
            )));
        }
        if let (Some((channel, _)), Some(input)) = (subscriber.input.take(), &publication.input) {
            input.down_closed(&key.edge(), &channel);
        }
        if let Some((slot, _)) = subscriber.audio.as_mut() {
            slot.close();
        }
        if let Some(conn) = subscriber.conn {
            if let (Some(transport), Some(rtc)) =
                (subscriber.transport.as_mut(), conns.get_mut(conn))
            {
                transport.close(rtc.rtc());
            }
            self.drop_conn(conn, conns);
        }
    }

    pub(super) fn subscriber_event(
        &mut self,
        pid: u64,
        key: &SubscriptionKey,
        now: Instant,
        event: ConnEvent,
        conns: &mut Connections,
    ) {
        let Some(publication) = self.publications.get_mut(&pid).filter(|p| !p.failed) else {
            return;
        };
        let Some(subscriber) = publication.subscribers.get_mut(key) else {
            return;
        };
        match event {
            ConnEvent::State(mut state) => {
                if state == PcState::Connected
                    && let (Some(transport), Some(source), Some(conn)) = (
                        subscriber.transport.as_mut(),
                        publication.source.as_ref(),
                        subscriber.conn.and_then(|id| conns.get_mut(id)),
                    )
                    && transport.set_connected(conn.rtc(), source).is_err()
                {
                    state = PcState::Failed;
                }
                if matches!(
                    state,
                    PcState::Disconnected | PcState::Failed | PcState::Closed
                ) && let Some((_, lost)) = &subscriber.input
                {
                    lost();
                }
                self.emit_subscriber(
                    pid,
                    key,
                    MediaEvent {
                        state: pc_state(state),
                        ..Default::default()
                    },
                );
                self.refresh_demand(pid, true, conns);
            }
            ConnEvent::ChannelOpen(channel) => match &publication.input {
                Some(input) if channel.label() == LABEL && subscriber.input.is_none() => {
                    let edge = key.edge();
                    let lost = input.attach_down(&edge, channel.clone());
                    input.down_opened(&edge, &channel);
                    subscriber.input = Some((channel, lost));
                }
                _ => channel.close(),
            },
            ConnEvent::ChannelData(channel, data) => {
                if let Some(input) = &publication.input {
                    input.down_message(&key.edge(), &channel, &data.data, !data.binary);
                }
            }
            ConnEvent::ChannelClose(channel) => {
                if let Some(input) = &publication.input {
                    input.down_closed(&key.edge(), &channel);
                }
                if subscriber
                    .input
                    .as_ref()
                    .is_some_and(|(current, _)| Arc::ptr_eq(current, &channel))
                {
                    subscriber.input = None;
                }
            }
            ConnEvent::Rtc(event) => {
                let (Some(transport), Some(source), Some(conn)) = (
                    subscriber.transport.as_mut(),
                    publication.source.as_ref(),
                    subscriber.conn.and_then(|id| conns.get_mut(id)),
                ) else {
                    return;
                };
                let request = transport.handle_event(conn.rtc(), source, &event);
                self.request_keyframes(pid, request.into_iter().collect(), now, conns);
                self.refresh_demand(pid, false, conns);
            }
        }
    }
}
