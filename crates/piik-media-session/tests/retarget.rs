//! Retarget, source registration and constrained dormant recovery tests.

mod common;

use std::time::Duration;

use common::*;
use piik_encoded::Frame;
use piik_media_forwarding::{CodecKind, InboundPacket, Source};
use piik_media_session::{
    AudioSlot, AudioSlotOptions, EncodedSource, Error, InputClock, PublicationOptions,
    SimulcastSend, SourcePacket, TransportOptions, UpstreamKeyframe, VideoSend,
};
use piik_media_testkit::netem::{Bitrate, DataSize, NetemConfig};
use piik_media_testkit::str0m::Event;
use piik_media_testkit::str0m::rtp::rtcp::SenderInfo;

/// VideoSend or SimulcastSend behind one driver.
#[allow(clippy::large_enum_variant)] // one per test rig
enum Sender {
    VideoSend(VideoSend),
    SimulcastSend(SimulcastSend),
}

struct Rig {
    link: Link,
    sender: Sender,
    requests: Vec<UpstreamKeyframe>,
    reports: Vec<(u32, SenderInfo, std::time::Instant)>,
    pts: Duration,
}

impl Rig {
    fn new(simulcast: bool, source: &Source, initial: i64) -> Self {
        let link = if simulcast {
            Link::new(Some(&["q", "h"]), false, initial)
        } else {
            Link::new(Some(&[]), false, initial)
        };
        let (mid, pt) = (link.video.unwrap(), link.video_pt);
        let sender = if simulcast {
            Sender::SimulcastSend(
                SimulcastSend::new(
                    PublicationOptions {
                        mid,
                        pt,
                        initial_bitrate: initial,
                    },
                    source,
                )
                .unwrap(),
            )
        } else {
            Sender::VideoSend(
                VideoSend::new(
                    TransportOptions {
                        mid,
                        pt,
                        initial_bitrate: initial,
                        forwarded: false,
                    },
                    source,
                )
                .unwrap(),
            )
        };
        Self {
            link,
            sender,
            requests: Vec::new(),
            reports: Vec::new(),
            pts: Duration::ZERO,
        }
    }

    fn rtc(&mut self) -> &mut piik_media_testkit::str0m::Rtc {
        &mut self.link.net.peer_mut(self.link.tx).rtc
    }

    fn connect(&mut self, source: &Source) -> Result<(), Error> {
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        match &mut self.sender {
            Sender::VideoSend(transport) => transport.set_connected(rtc, source),
            Sender::SimulcastSend(publication) => publication.set_connected(rtc, source),
        }
    }

    fn replace(&mut self, current: &Source, next: &Source) -> Result<Vec<UpstreamKeyframe>, Error> {
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        match &mut self.sender {
            Sender::VideoSend(transport) => transport.replace_source(rtc, current, next),
            Sender::SimulcastSend(publication) => publication.replace_source(rtc, current, next),
        }
    }

    fn close(&mut self) {
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        match &mut self.sender {
            Sender::VideoSend(transport) => transport.close(rtc),
            Sender::SimulcastSend(publication) => publication.close(rtc),
        }
    }

    fn deliver(&mut self, source: &Source, packets: &[SourcePacket], changed: bool) {
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        match &mut self.sender {
            Sender::VideoSend(transport) => {
                if changed {
                    self.requests.extend(transport.reconcile(rtc, source));
                }
                for packet in packets {
                    self.requests
                        .extend(transport.forward(rtc, source, packet, now));
                }
            }
            Sender::SimulcastSend(publication) => {
                if changed {
                    self.requests.extend(publication.reconcile(rtc, source));
                }
                for packet in packets {
                    self.requests
                        .extend(publication.forward(rtc, source, packet, now));
                }
            }
        }
    }

    fn write(&mut self, input: &mut EncodedSource, sizes: &[Option<usize>]) -> Vec<Received> {
        let now = self.link.now();
        self.pts += FRAME;
        input
            .begin_frame(self.pts, self.link.anchor.system(now))
            .unwrap();
        let mut packets = Vec::new();
        let mut changed = false;
        for (layer, size) in sizes.iter().enumerate() {
            let Some(size) = size else { continue };
            let frame = Frame {
                data: body(*size, true),
                pts: self.pts,
                duration: FRAME,
                recovery: true,
            };
            let written = input.write_frame(layer, &frame, None).unwrap();
            changed |= written.changes.any();
            packets.extend(written.packets);
        }
        self.deliver(input.source(), &packets, changed);
        self.advance(input, FRAME)
    }

    fn advance(&mut self, input: &mut EncodedSource, duration: Duration) -> Vec<Received> {
        let (events, received, other) = self.link.step(duration);
        for event in other {
            if let Event::SenderFeedback(feedback) = event {
                self.reports.push((
                    *feedback.sender_info.ssrc,
                    feedback.sender_info,
                    feedback.received_at,
                ));
            }
        }
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        let source = input.source();
        match &mut self.sender {
            Sender::VideoSend(transport) => {
                for event in &events {
                    self.requests
                        .extend(transport.handle_event(rtc, source, event));
                }
                transport.poll(rtc, source, now);
            }
            Sender::SimulcastSend(publication) => {
                for event in &events {
                    self.requests
                        .extend(publication.handle_event(rtc, source, event));
                }
                publication.poll(rtc, source, now);
            }
        }
        let changes = input.source_mut().poll(now);
        if changes.any() {
            self.deliver(input.source(), &[], true);
        }
        received
    }
}

fn encoded(anchor: piik_media_session::ClockAnchor, stream: &str) -> EncodedSource {
    EncodedSource::new(
        vp8_options("screen", stream, &[90_000, 300_000]),
        InputClock::Local(anchor),
    )
    .unwrap()
}

#[test]
fn source_retarget_preserves_transport_and_publication() {
    for simulcast in [false, true] {
        let placeholder =
            Source::new(vp8_options("screen", "retarget", &[90_000, 300_000])).unwrap();
        let mut rig = Rig::new(simulcast, &placeholder, 1_000_000);
        let anchor = rig.link.anchor;
        let mut old = encoded(anchor, "retarget");
        let mut next = encoded(anchor, "retarget");
        let mut closed = encoded(anchor, "retarget");
        closed.close();
        let foreign = encoded(anchor, "different-stream");
        // Retargeting before connection attaches nothing and needs no recovery.
        assert!(rig.replace(old.source(), next.source()).unwrap().is_empty());
        assert!(rig.replace(next.source(), old.source()).unwrap().is_empty());
        rig.connect(old.source()).unwrap();
        let sizes = [Some(200), Some(400)];
        let mut received = Vec::new();
        for _ in 0..60 {
            received.extend(rig.write(&mut old, &sizes));
        }
        let ssrcs: Vec<u32> = {
            let mut ssrcs: Vec<u32> = received.iter().map(|p| p.ssrc).collect();
            ssrcs.sort_unstable();
            ssrcs.dedup();
            ssrcs
        };
        assert_eq!(
            ssrcs.len(),
            if simulcast { 2 } else { 1 },
            "outputs before retarget"
        );
        for rejected in [closed.source(), foreign.source()] {
            assert!(
                rig.replace(old.source(), rejected).is_err(),
                "invalid replacement"
            );
        }
        let recovery = rig.replace(old.source(), next.source()).unwrap();
        assert_eq!(recovery.len(), 2);
        assert!(
            recovery.iter().all(|request| request.force),
            "recovery is forced"
        );
        old.close();
        rig.pts = Duration::ZERO;
        let before = received.len();
        for _ in 0..60 {
            received.extend(rig.write(&mut next, &sizes));
        }
        for ssrc in &ssrcs {
            let frames: Vec<_> = received
                .iter()
                .filter(|p| p.ssrc == *ssrc && p.marker)
                .collect();
            let after = received[before..]
                .iter()
                .filter(|p| p.ssrc == *ssrc && p.marker)
                .count();
            assert!(after >= 6, "retarget media stalled on {ssrc}: {after}");
            for pair in frames.windows(2) {
                assert!(
                    (pair[1].timestamp.wrapping_sub(pair[0].timestamp) as i32) > 0,
                    "retarget broke the RTP timeline"
                );
                assert_eq!(pair[1].seq - pair[0].seq, 1, "retarget left a sequence gap");
            }
        }
        assert!(rig.link.net.peer(rig.link.tx).rtc.is_connected());
        match &rig.sender {
            Sender::VideoSend(transport) => {
                assert!(transport.state(next.source()).current >= 0);
                assert!(transport.egress().packets > 0);
            }
            Sender::SimulcastSend(publication) => {
                let counters = publication.counters(next.source());
                assert!(counters.frames[0] >= 6 && counters.frames[1] >= 6);
            }
        }
        rig.close();
        assert!(rig.replace(next.source(), old.source()).is_err());
        assert!(rig.connect(next.source()).is_err());
    }
}

/// Every wire sender report maps its output RTP time to the current source's
/// capture clock, before and after a replacement: no subscriber keeps an
/// older sender-report clock.
#[test]
fn source_registration_retains_current_clock() {
    for simulcast in [false, true] {
        let placeholder =
            Source::new(vp8_options("screen", "joining", &[90_000, 300_000])).unwrap();
        let mut rig = Rig::new(simulcast, &placeholder, 1_000_000);
        let anchor = rig.link.anchor;
        let mut input = encoded(anchor, "joining");
        rig.connect(input.source()).unwrap();
        let sizes = [Some(200), Some(400)];
        let check = |rig: &mut Rig, received: &[Received], label: &str| {
            let reports = std::mem::take(&mut rig.reports);
            let mut checked = 0;
            for (ssrc, info, at) in reports {
                // The last frame on this SSRC before the report.
                let Some(frame) = received.iter().rfind(|p| p.ssrc == ssrc && p.marker) else {
                    continue;
                };
                // The unimpaired link delivers at send time, so the report's
                // RTP-to-wallclock mapping must place this frame at its arrival
                // relative to the report's own arrival.
                let delta = frame.timestamp.wrapping_sub(info.rtp_time.numer() as u32) as i32;
                let projected = delta as f64 / 90_000.0;
                let observed = if frame.at >= at {
                    (frame.at - at).as_secs_f64()
                } else {
                    -(at - frame.at).as_secs_f64()
                };
                let error = Duration::from_secs_f64((projected - observed).abs());
                assert!(
                    error < Duration::from_millis(40),
                    "{label}: SR clock error {error:?}"
                );
                checked += 1;
            }
            assert!(checked > 0, "{label}: no sender report observed");
        };
        let mut received = Vec::new();
        for _ in 0..240 {
            received.extend(rig.write(&mut input, &sizes));
        }
        check(&mut rig, &received, "registered");
        // A replacement starts a new capture clock 2 s later.
        let mut next = encoded(anchor, "joining");
        rig.replace(input.source(), next.source()).unwrap();
        input.close();
        rig.pts = Duration::ZERO;
        rig.reports.clear();
        received.clear();
        for _ in 0..240 {
            received.extend(rig.write(&mut next, &sizes));
        }
        check(&mut rig, &received, "replaced");
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    VideoOnly,
    NumericAudio,
    WithinLowestOutput,
    EncodedFrameBursts,
    RawForwarding,
}

/// Constrained-link recovery of a dormant transport; virtual time makes it
/// cheap. The audio scenario uses synthetic Opus packets: the property is
/// that audio keeps flowing through video congestion, not decode.
#[test]
fn transport_constrained_dormant_recovery() {
    for scenario in [
        Scenario::VideoOnly,
        Scenario::NumericAudio,
        Scenario::WithinLowestOutput,
        Scenario::EncodedFrameBursts,
        Scenario::RawForwarding,
    ] {
        check_transport_recovery(scenario);
    }
}

/// Fixed raw lowest-output rate whose 30 fps frames fit one RTP packet.
const RAW_LOWEST: i64 = 260_000;

fn check_transport_recovery(scenario: Scenario) {
    let with_audio = scenario == Scenario::NumericAudio;
    let recover_lowest = scenario == Scenario::WithinLowestOutput;
    let bursts = scenario == Scenario::EncodedFrameBursts;
    let raw = scenario == Scenario::RawForwarding;
    let (mut duration, mut recovered_at) = (26.0, 20.0);
    let (mut low, mut high, mut ceiling, mut recovered_bitrate) =
        (300_000i64, 1_200_000i64, 6_000_000u64, 800_000.0);
    if recover_lowest {
        (duration, recovered_at) = (50.0, 44.0);
    }
    // GoogCC: the step to the high output can briefly cut the estimate during
    // probe settling, so the same 5 recovered windows get 10 s more to
    // accumulate after release.
    duration += 10.0;
    if bursts {
        (duration, recovered_at) = (12.0, 4.0);
        (low, high, ceiling, recovered_bitrate) = (1_200_000, 5_000_000, 12_000_000, 4_500_000.0);
    }
    let link = Link::new(Some(&[]), with_audio, high);
    let mut input = EncodedSource::new(
        vp8_options("recovery", "recovery", &[low as u32, high as u32]),
        InputClock::Local(link.anchor),
    )
    .unwrap();
    input.source_mut().set_lowest_layer_rate_controlled(!raw);
    let transport = VideoSend::new(
        TransportOptions {
            mid: link.video.unwrap(),
            pt: link.video_pt,
            initial_bitrate: high,
            forwarded: raw,
        },
        input.source(),
    )
    .unwrap();
    let mut rig = Rig {
        link,
        sender: Sender::VideoSend(transport),
        requests: Vec::new(),
        reports: Vec::new(),
        pts: Duration::ZERO,
    };
    let mut slot = with_audio.then(|| {
        AudioSlot::new(AudioSlotOptions {
            mid: rig.link.audio.unwrap(),
            pt: rig.link.audio_pt,
            source_ceiling: 510_000,
        })
    });
    let mut voice = Source::new(opus_options("voice")).unwrap();
    voice.bind_layer(0, 9000).unwrap();
    let attachment = slot
        .as_mut()
        .map(|slot| slot.replace_source(&voice).unwrap());
    let (tx, rx) = (addr(TX), addr(RX));
    // str0m's leaky bucket releases up to 40 ms of debt (at most 63 KB) as one
    // burst, so a whole 5 Mbps/30 fps frame leaves at once and would overflow
    // a 7.5 KB link queue. The burst scenario therefore uses a queue that holds
    // str0m's maximum burst; the property kept is that healthy capacity never
    // downgrades encoded frame bursts.
    let queue = if bursts {
        DataSize::bytes(64_000)
    } else {
        DataSize::bytes(7_500)
    };
    let shape = move |capacity: u64| {
        NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(Bitrate::bps(capacity), queue)
    };
    rig.link.net.set_link(tx, rx, shape(ceiling));
    rig.connect(input.source()).unwrap();
    let start = rig.link.now();
    let (mut dormant, mut minimum) = (false, high);
    let (mut recovered, mut audio_windows, mut lowest_windows) = (0, 0, 0);
    let mut last_capacity = ceiling;
    let mut window = Vec::new();
    let mut window_start = start;
    let mut audio_seq = 0u64;
    loop {
        let now = rig.link.now();
        let s = (now - start).as_secs_f64();
        if s >= duration {
            break;
        }
        let mut capacity = ceiling;
        if !bursts && (4.0..12.0).contains(&s) {
            capacity = if recover_lowest { 120_000 } else { 500_000 };
        }
        if recover_lowest && (12.0..28.0).contains(&s) {
            capacity = 800_000;
        }
        if capacity != last_capacity {
            rig.link.net.set_link(tx, rx, shape(capacity));
            last_capacity = capacity;
        }
        let Sender::VideoSend(transport) = &mut rig.sender else {
            unreachable!()
        };
        let state = transport.state(input.source());
        assert!(
            state.video_budget <= ceiling as i64 * 2,
            "{scenario:?}: probe promoted capacity far beyond the link: {state:?}"
        );
        let required_high = transport.required_active_count(input.source()) > 1;
        if (7.0..12.0).contains(&s) {
            dormant |= !required_high;
            minimum = minimum.min(state.video_budget);
        }
        transport.begin_frame();
        // str0m difference: probe padding reuses the largest cached packet
        // under twice the requested padding. A fixed 300 kbps raw frame
        // (1250 B) packetizes into a full packet plus a ~60 B tail, so every
        // probe cluster is padded with that tail and under-delivers (~640 kbps
        // measured on a 6 Mbps link). The raw output stays
        // fixed and budget-independent, just one packet a frame.
        let lowest = if raw {
            RAW_LOWEST
        } else {
            low.min(state.video_budget.max(1))
        };
        let sizes: Vec<Option<usize>> = [lowest, high]
            .into_iter()
            .enumerate()
            .map(|(layer, bitrate)| {
                (layer == 0 || required_high).then(|| {
                    (input.codec_budget(layer, bitrate) / 8 / 30).max(VP8_KEY.len() as i64) as usize
                })
            })
            .collect();
        if let (Some(slot), Some(attachment)) = (slot.as_mut(), attachment) {
            // 20 ms Opus frames around this video frame.
            for _ in 0..2 {
                let info = InboundPacket::new(
                    CodecKind::Opus,
                    0,
                    1_000 + audio_seq,
                    audio_seq * 960,
                    &[0xfc; 120],
                    now,
                )
                .ssrc(9000)
                .payload_type(111);
                audio_seq += 1;
                voice.write_rtp(0, &info).unwrap();
                let packet = SourcePacket {
                    info,
                    payload: [0xfc; 120].into(),
                };
                slot.forward(rig.rtc(), attachment, &voice, &packet, now);
            }
        }
        window.extend(rig.write(&mut input, &sizes));
        let now = rig.link.now();
        if now - window_start < Duration::from_secs(1) {
            continue;
        }
        let interval = now - window_start;
        let Sender::VideoSend(transport) = &rig.sender else {
            unreachable!()
        };
        let state = transport.state(input.source());
        let video_ssrc = rig.link.rx_ssrc(None);
        let video: Vec<_> = window
            .iter()
            .filter(|p| Some(p.ssrc) == video_ssrc)
            .cloned()
            .collect();
        let received_bps = rate(&video, interval);
        let audio = window.len() - video.len();
        if bursts && s >= 2.0 && (state.target != 1 || state.current != 1) {
            panic!("{scenario:?}: healthy capacity downgraded during measurement: {state:?}");
        }
        if s >= recovered_at && state.current == 1 && received_bps > recovered_bitrate {
            recovered += 1;
        }
        // Partial recovery must restore the current output before the larger
        // spatial output fits.
        if recover_lowest
            && capacity == 800_000
            && state.current == 0
            && (300_000..1_200_000).contains(&state.video_budget)
            && received_bps > 250_000.0
        {
            lowest_windows += 1;
        }
        if with_audio && s >= recovered_at && audio >= 35 {
            audio_windows += 1;
        }
        window.clear();
        window_start = now;
    }
    if recover_lowest {
        assert!(
            lowest_windows >= 5,
            "current output stayed degraded: {lowest_windows}"
        );
    }
    assert!(
        (bursts || (dormant && minimum < high))
            && recovered >= 5
            && (!with_audio || audio_windows >= 5),
        "{scenario:?}: missing constrained/dormant/released recovery: dormant={dormant} minimum={minimum} recovered={recovered} audio={audio_windows}"
    );
    rig.close();
}
