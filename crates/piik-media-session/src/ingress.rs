//! Received publisher RTP into a `Source`: str0m (RTP mode) owns SRTP,
//! dedupe, NACK and RTX; this binds each RID's SSRC to its layer, records
//! accepted packets and sender reports, and sends throttled keyframe requests.

use std::sync::Arc;
use std::time::Instant;

use piik_media_forwarding::{
    InboundPacket, PliThrottle, SenderReport, Source, SourceChanges, ntp_from_system_time,
};
use str0m::media::{KeyframeRequestKind, Mid, Rid};
use str0m::rtp::RtpPacket;
use str0m::{Event, Rtc};

use crate::{Error, SourcePacket, UpstreamKeyframe};

/// One accepted packet and the source events its owner reconciles.
#[derive(Clone, Debug)]
pub struct IngressPacket {
    pub packet: SourcePacket,
    pub changes: SourceChanges,
}

#[derive(Debug)]
pub struct Ingress {
    mid: Mid,
    /// Layer index to RID; `None` for a single encoding.
    rids: Vec<Option<Rid>>,
    ssrcs: Vec<Option<u32>>,
    throttle: PliThrottle,
}

impl Ingress {
    /// `rids[layer]` is that layer's simulcast RID in ascending quality.
    pub fn new(mid: Mid, rids: Vec<Option<Rid>>, throttle: PliThrottle) -> Result<Self, Error> {
        if rids.is_empty() || (rids.len() > 1 && rids.iter().any(Option::is_none)) {
            return Err(Error::Invalid);
        }
        let ssrcs = vec![None; rids.len()];
        Ok(Self {
            mid,
            rids,
            ssrcs,
            throttle,
        })
    }

    pub fn mid(&self) -> Mid {
        self.mid
    }

    pub fn layer_ssrc(&self, layer: usize) -> Option<u32> {
        self.ssrcs.get(layer).copied().flatten()
    }

    /// Learns a receive stream's layer from str0m's RID binding.
    fn layer_of(&mut self, rtc: &mut Rtc, source: &mut Source, ssrc: u32) -> Option<usize> {
        if let Some(layer) = self.ssrcs.iter().position(|bound| *bound == Some(ssrc)) {
            return Some(layer);
        }
        let mut api = rtc.direct_api();
        let layer = self.rids.iter().position(|rid| {
            api.stream_rx_by_mid(self.mid, *rid)
                .is_some_and(|stream| *stream.ssrc() == ssrc)
        })?;
        if self.ssrcs[layer].is_some() {
            // A different SSRC for a bound layer needs a new source.
            return None;
        }
        source.bind_layer(layer, ssrc).ok()?;
        self.ssrcs[layer] = Some(ssrc);
        Some(layer)
    }

    /// Records publisher RTP and sender reports. Other events are ignored.
    pub fn handle_event(
        &mut self,
        rtc: &mut Rtc,
        source: &mut Source,
        event: &Event,
    ) -> Result<Option<IngressPacket>, Error> {
        match event {
            Event::RtpPacket(packet) => self.receive(rtc, source, packet),
            Event::SenderFeedback(feedback) if feedback.mid == self.mid => {
                let Some(layer) = self.rids.iter().position(|rid| *rid == feedback.rid) else {
                    return Ok(None);
                };
                let info = &feedback.sender_info;
                let ssrc = *info.ssrc;
                if self.ssrcs[layer] != Some(ssrc) {
                    return Ok(None);
                }
                // Assumption: arrival time stands for the remote capture
                // clock; a propagation-delay adjustment needs RTT samples
                // str0m keeps internal.
                source.sender_report(
                    layer,
                    ssrc,
                    SenderReport {
                        rtp_timestamp: info.rtp_time.numer() as u32,
                        ntp_time: ntp_from_system_time(info.ntp_time),
                        packets: info.sender_packet_count,
                        octets: u64::from(info.sender_octet_count),
                        at: feedback.received_at,
                        at_adjusted: feedback.received_at,
                    },
                )?;
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn receive(
        &mut self,
        rtc: &mut Rtc,
        source: &mut Source,
        packet: &RtpPacket,
    ) -> Result<Option<IngressPacket>, Error> {
        let ssrc = *packet.header.ssrc;
        let Some(layer) = self.layer_of(rtc, source, ssrc) else {
            return Ok(None);
        };
        let info = InboundPacket::new(
            source.codec().kind,
            layer as i32,
            *packet.seq_no,
            packet.time.numer(),
            &packet.payload,
            packet.timestamp,
        )
        .marker(packet.header.marker)
        .ssrc(ssrc)
        .payload_type(*packet.header.payload_type)
        .size(packet.header.header_len + packet.payload.len());
        let info = info.reference(source.reference_time(layer, packet.header.timestamp));
        if info.keyframe {
            self.throttle.keyframe_received(layer);
        }
        let changes = source.write_rtp(layer, &info)?;
        Ok(Some(IngressPacket {
            packet: SourcePacket {
                info,
                payload: Arc::clone(&packet.payload),
            },
            changes,
        }))
    }

    /// Sends a PLI for `request.layer` unless the throttle holds it. Returns
    /// whether a request was sent.
    pub fn request_keyframe(
        &mut self,
        rtc: &mut Rtc,
        request: UpstreamKeyframe,
        now: Instant,
    ) -> bool {
        let Some(rid) = self.rids.get(request.layer).copied() else {
            return false;
        };
        if self.ssrcs[request.layer].is_none() {
            return false;
        }
        if !self.throttle.request(request.layer, request.force, now) {
            return false;
        }
        let mut api = rtc.direct_api();
        let Some(stream) = api.stream_rx_by_mid(self.mid, rid) else {
            return false;
        };
        stream.request_keyframe(KeyframeRequestKind::Pli);
        true
    }
}
