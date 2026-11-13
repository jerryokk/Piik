//! Shared virtual-network fixture: one str0m sender in RTP mode with BWE and
//! one RTP-mode receiver, negotiated through SDP so RTX, TWCC and the MID/RID
//! extensions follow the real offer/answer path.

#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use piik_encoded::Frame;
use piik_media_forwarding::{Codec, CodecKind, LayerFormat, SourceOptions};
use piik_media_session::{ClockAnchor, EncodedSource, SourcePacket};
use piik_media_testkit::str0m;
use piik_media_testkit::{Network, PeerId, negotiate};
use str0m::bwe::Bitrate;
use str0m::format::Codec as WireCodec;
use str0m::media::{Direction, MediaKind, Mid, Pt, Rid, Simulcast, SimulcastLayer};
use str0m::{Event, Rtc};

/// VP8-shaped keyframe body (P bit clear); tests exercise RTP, not decoding.
pub const VP8_KEY: [u8; 12] = [0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 8, 0, 8, 0, 0, 0];
pub const FRAME: Duration = Duration::from_nanos(33_333_333);

pub fn addr(value: &str) -> SocketAddr {
    value.parse().unwrap()
}

pub const TX: &str = "10.0.0.1:1000";
pub const RX: &str = "10.0.0.2:2000";

pub fn vp8_options(id: &str, stream: &str, bitrates: &[u32]) -> SourceOptions {
    SourceOptions {
        id: id.into(),
        stream_id: stream.into(),
        codec: Codec {
            kind: CodecKind::Vp8,
            payload_type: 96,
            fmtp: String::new(),
            channels: 0,
        },
        formats: bitrates
            .iter()
            .enumerate()
            .map(|(layer, bitrate)| LayerFormat {
                width: 8 << layer,
                height: 8 << layer,
                bitrate: *bitrate,
            })
            .collect(),
        max_packets: 500,
    }
}

pub fn opus_options(id: &str) -> SourceOptions {
    SourceOptions {
        id: id.into(),
        stream_id: id.into(),
        codec: Codec {
            kind: CodecKind::Opus,
            payload_type: 111,
            fmtp: "minptime=10;useinbandfec=1".into(),
            channels: 2,
        },
        formats: Vec::new(),
        max_packets: 0,
    }
}

/// A frame body of `size` bytes: keyframe or delta header, then filler.
pub fn body(size: usize, key: bool) -> Vec<u8> {
    let mut data = vec![0u8; size.max(VP8_KEY.len())];
    data[..VP8_KEY.len()].copy_from_slice(&VP8_KEY);
    if !key {
        data[0] |= 1;
    }
    data
}

/// One received RTP packet as the receiver's str0m surfaces it.
#[derive(Clone, Debug)]
pub struct Received {
    pub ssrc: u32,
    pub pt: u8,
    pub seq: u64,
    pub timestamp: u32,
    pub marker: bool,
    pub payload: Arc<[u8]>,
    pub at: Instant,
}

pub struct Link {
    pub net: Network,
    pub tx: PeerId,
    pub rx: PeerId,
    pub video: Option<Mid>,
    pub audio: Option<Mid>,
    pub video_pt: Pt,
    pub audio_pt: Pt,
    pub anchor: ClockAnchor,
}

fn pt_of(rtc: &Rtc, codec: WireCodec) -> Pt {
    rtc.codec_config()
        .iter()
        .find(|params| params.spec().codec == codec)
        .map(|params| params.pt())
        .expect("codec negotiated")
}

impl Link {
    /// Sender offers send-only video (simulcast when `rids` has several
    /// entries) and optionally audio, then both sides connect.
    pub fn new(video_rids: Option<&[&str]>, audio: bool, initial_bitrate: i64) -> Self {
        let mut net = Network::new();
        net.set_min_step(Duration::from_micros(500));
        let stats = Some(Duration::from_millis(250));
        let tx = net.add_peer(
            Rtc::builder()
                .set_rtp_mode(true)
                .enable_bwe(Some(Bitrate::bps(initial_bitrate as u64)))
                .set_stats_interval(stats),
            addr(TX),
        );
        let rx = net.add_peer(
            Rtc::builder().set_rtp_mode(true).set_stats_interval(stats),
            addr(RX),
        );
        let (video, audio) = negotiate(&mut net, tx, rx, |api| {
            let video = video_rids.map(|rids| {
                let simulcast = (rids.len() > 1).then(|| {
                    let mut simulcast = Simulcast::new();
                    for rid in rids {
                        simulcast.add_send_layer(SimulcastLayer::new(rid));
                    }
                    simulcast
                });
                api.add_media(MediaKind::Video, Direction::SendOnly, None, None, simulcast)
            });
            let audio = audio
                .then(|| api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None));
            (video, audio)
        })
        .unwrap()
        .result;
        let ok = net
            .run_until(
                |n| n.peer(tx).rtc.is_connected() && n.peer(rx).rtc.is_connected(),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(ok, "peers connect");
        let video_pt = pt_of(&net.peer(tx).rtc, WireCodec::Vp8);
        let audio_pt = pt_of(&net.peer(tx).rtc, WireCodec::Opus);
        let anchor = ClockAnchor {
            instant: net.now(),
            system: SystemTime::now(),
        };
        Self {
            net,
            tx,
            rx,
            video,
            audio,
            video_pt,
            audio_pt,
            anchor,
        }
    }

    pub fn now(&self) -> Instant {
        self.net.now()
    }

    pub fn rx_rtc(&mut self) -> &mut Rtc {
        &mut self.net.peer_mut(self.rx).rtc
    }

    pub fn take(&mut self, peer: PeerId) -> Vec<Event> {
        std::mem::take(&mut self.net.peer_mut(peer).events)
            .into_iter()
            .map(|(_, event)| event)
            .collect()
    }

    /// Advances the network and returns (sender events, received RTP, other
    /// receiver events).
    pub fn step(&mut self, duration: Duration) -> (Vec<Event>, Vec<Received>, Vec<Event>) {
        self.net.run_for(duration).unwrap();
        let tx = self.take(self.tx);
        let mut received = Vec::new();
        let mut other = Vec::new();
        for event in self.take(self.rx) {
            match event {
                Event::RtpPacket(packet) => received.push(Received {
                    ssrc: *packet.header.ssrc,
                    pt: *packet.header.payload_type,
                    seq: *packet.seq_no,
                    timestamp: packet.header.timestamp,
                    marker: packet.header.marker,
                    payload: packet.payload,
                    at: packet.timestamp,
                }),
                event => other.push(event),
            }
        }
        (tx, received, other)
    }

    /// The receiver's SSRC for a video RID.
    pub fn rx_ssrc(&mut self, rid: Option<&str>) -> Option<u32> {
        let mid = self.video?;
        self.rx_rtc()
            .direct_api()
            .stream_rx_by_mid(mid, rid.map(Rid::from))
            .map(|stream| *stream.ssrc())
    }
}

/// Writes one capture input at `now` to the listed layers.
pub fn write_frame(
    input: &mut EncodedSource,
    anchor: ClockAnchor,
    now: Instant,
    pts: Duration,
    layers: &[(usize, Vec<u8>, bool)],
) -> Vec<SourcePacket> {
    input.begin_frame(pts, anchor.system(now)).unwrap();
    let mut packets = Vec::new();
    for (layer, data, recovery) in layers {
        let frame = Frame {
            data: data.clone(),
            pts,
            duration: FRAME,
            recovery: *recovery,
        };
        packets.extend(input.write_frame(*layer, &frame, None).unwrap().packets);
    }
    packets
}

/// Sum of payload bits per second over `window`.
pub fn rate(packets: &[Received], window: Duration) -> f64 {
    let bytes: usize = packets.iter().map(|packet| packet.payload.len()).sum();
    bytes as f64 * 8.0 / window.as_secs_f64()
}
