//! VideoSend and warm-source tests on the virtual network: a str0m RTP-mode
//! sender with GoogCC and a str0m receiver.

mod common;

use std::time::Duration;

use common::*;
use piik_media_session::{
    AudioSlot, AudioSlotOptions, EncodedSource, InputClock, SourcePacket, TransportOptions,
    UpstreamKeyframe, VideoSend,
};
use piik_media_testkit::WireRtp;
use piik_media_testkit::str0m::media::KeyframeRequestKind;
use piik_media_testkit::str0m::{Input, Output};

struct Child {
    link: Link,
    input: EncodedSource,
    transport: VideoSend,
    requests: Vec<UpstreamKeyframe>,
    frame: u32,
}

impl Child {
    fn new(bitrates: &[u32], initial: i64, audio: bool) -> Self {
        let link = Link::new(Some(&[]), audio, initial);
        let input = EncodedSource::new(
            vp8_options("screen", "fixture", bitrates),
            InputClock::Local(link.anchor),
        )
        .unwrap();
        let transport = VideoSend::new(
            TransportOptions {
                mid: link.video.unwrap(),
                pt: link.video_pt,
                initial_bitrate: initial,
                forwarded: true,
            },
            input.source(),
        )
        .unwrap();
        Self {
            link,
            input,
            transport,
            requests: Vec::new(),
            frame: 0,
        }
    }

    fn deliver(&mut self, packets: &[SourcePacket], changed: bool) {
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        let source = self.input.source();
        if changed {
            self.requests.extend(self.transport.reconcile(rtc, source));
        }
        for packet in packets {
            self.requests
                .extend(self.transport.forward(rtc, source, packet, now));
        }
    }

    /// Writes one frame of `size` bytes on layer 0 and advances one period.
    fn frame(&mut self, size: usize, key: bool) -> Vec<Received> {
        let now = self.link.now();
        let pts = FRAME * self.frame;
        self.frame += 1;
        self.input
            .begin_frame(pts, self.link.anchor.system(now))
            .unwrap();
        let written = self
            .input
            .write_frame(
                0,
                &piik_encoded::Frame {
                    data: body(size, key),
                    pts,
                    duration: FRAME,
                    recovery: key,
                },
                None,
            )
            .unwrap();
        self.deliver(&written.packets, written.changes.any());
        self.advance(FRAME)
    }

    fn advance(&mut self, duration: Duration) -> Vec<Received> {
        let (events, received, _) = self.link.step(duration);
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        let source = self.input.source();
        for event in &events {
            self.requests
                .extend(self.transport.handle_event(rtc, source, event));
        }
        let changes = self.input.source_mut().poll(self.link.net.now());
        if changes.any() {
            self.deliver(&[], true);
        }
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        self.transport.poll(rtc, self.input.source(), now);
        received
    }
}

#[test]
fn transport_video_feedback_audio_and_close() {
    let mut child = Child::new(&[300_000, 3_000_000], 1_000_000, true);
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_max_spatial_layer(rtc, child.input.source(), 0);
    let source = child.input.source();
    assert_eq!(
        child.transport.state(source).video_budget,
        1_000_000,
        "audio changed the video capacity estimate"
    );
    assert_eq!(
        child.transport.required_active_count(source),
        1,
        "initial configured acquisition demand was lost"
    );
    // Unconnected: nothing is forwarded.
    assert!(child.frame(800, true).is_empty());
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();

    let mut video = Vec::new();
    for _ in 0..30 {
        video.extend(child.frame(800, true));
        if !video.is_empty() {
            break;
        }
    }
    let first = video.first().cloned().expect("first frame");
    assert_eq!(first.pt, *child.link.video_pt);

    // A subscriber PLI reaches the source owner as a keyframe request.
    child.requests.clear();
    let mid = child.link.video.unwrap();
    child
        .link
        .rx_rtc()
        .direct_api()
        .stream_rx_by_mid(mid, None)
        .unwrap()
        .request_keyframe(KeyframeRequestKind::Pli);
    child.advance(Duration::from_millis(100));
    assert!(
        child.requests.iter().any(|request| request.layer == 0),
        "subscriber PLI never reached the source"
    );

    // Lose one wire packet; str0m NACK/RTX repairs it into the primary stream.
    let video_ssrc = first.ssrc;
    let lost = (first.seq + 40) as u16;
    let mut dropped = false;
    child
        .link
        .net
        .set_drop_filter(Some(Box::new(move |_, _, bytes| {
            let hit = WireRtp::parse(bytes)
                .is_some_and(|rtp| rtp.ssrc == video_ssrc && rtp.sequence_number == lost);
            let drop = hit && !dropped;
            dropped |= hit;
            drop
        })));
    let mut slot = AudioSlot::new(AudioSlotOptions {
        mid: child.link.audio.unwrap(),
        pt: child.link.audio_pt,
        source_ceiling: 510_000,
    });
    let mut voice = piik_media_forwarding::Source::new(opus_options("voice")).unwrap();
    voice.bind_layer(0, 7000).unwrap();
    let attachment = slot.replace_source(&voice).unwrap();
    let mut audio_packets = 0;
    for index in 0..90u64 {
        let received = child.frame(800, index % 30 == 0);
        video.extend(received.iter().filter(|p| p.ssrc == video_ssrc).cloned());
        audio_packets += received.iter().filter(|p| p.ssrc != video_ssrc).count();
        let now = child.link.now();
        let opus = piik_media_forwarding::InboundPacket::new(
            piik_media_forwarding::CodecKind::Opus,
            0,
            10_000 + index,
            480_000 + index * 1600,
            &[0xf8, 0xff, 0xfe],
            now,
        )
        .ssrc(7000)
        .payload_type(111);
        voice.write_rtp(0, &opus).unwrap();
        let packet = SourcePacket {
            info: opus,
            payload: [0xf8, 0xff, 0xfe].into(),
        };
        let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
        slot.forward(rtc, attachment, &voice, &packet, now);
    }
    child.link.net.set_drop_filter(None);
    let lost_seq = video
        .iter()
        .find(|packet| packet.seq as u16 == lost)
        .expect("lost video packet was repaired through NACK/RTX");
    assert_eq!(lost_seq.ssrc, video_ssrc, "repair carries the primary SSRC");
    let mut seqs: Vec<u64> = video.iter().map(|p| p.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    assert_eq!(
        seqs.last().unwrap() - seqs.first().unwrap() + 1,
        seqs.len() as u64,
        "the output timeline has a hole"
    );
    assert!(audio_packets >= 15, "audio missing: {audio_packets}");
    let (_, observed) = child.transport.target_bitrate();
    assert!(observed, "connection feedback never observed");
    let egress = child.transport.egress();
    assert!(egress.packets >= 15 && egress.rtp_bytes > 0, "{egress:?}");
    assert_eq!(egress.dropped_frames, 0);
    assert_eq!(
        child.transport.required_active_count(child.input.source()),
        1
    );

    // Subscribed upper demand without an available layer is probe demand only.
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_max_spatial_layer(rtc, child.input.source(), 1);
    let goal = child.transport.bitrate_goal().expect("goal applied");
    assert!(
        goal.desired > goal.minimum,
        "no dormant upper demand: {goal:?}"
    );
    assert_eq!(child.transport.state(child.input.source()).target, 0);

    // A profile change withdraws that demand; the next goal stops probing.
    child
        .input
        .source_mut()
        .update_formats(vec![
            piik_media_forwarding::LayerFormat {
                width: 8,
                height: 8,
                bitrate: 300_000,
            },
            piik_media_forwarding::LayerFormat {
                width: 16,
                height: 16,
                bitrate: 600_000,
            },
        ])
        .unwrap();
    child.deliver(&[], true);
    let goal = child.transport.bitrate_goal().unwrap();
    let budget = child.transport.target_bitrate().0;
    if budget >= 600_000 {
        assert_eq!(
            goal.minimum, 0,
            "withdrawn demand retained a probe: {goal:?}"
        );
    }
    let before = video.len();
    for _ in 0..10 {
        video.extend(child.frame(800, false));
    }
    assert!(video.len() > before, "withdrawing demand interrupted video");

    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child.transport.close(rtc);
    child.transport.close(rtc);
    assert!(
        child
            .transport
            .set_connected(rtc, child.input.source())
            .is_err()
    );
    assert_eq!(
        child.transport.required_active_count(child.input.source()),
        0
    );
    child.advance(Duration::from_millis(200));
    for _ in 0..5 {
        assert!(
            child.frame(800, true).is_empty(),
            "closed transport forwarded"
        );
    }
}

/// A late child must establish video and its own TWCC even when its initial
/// estimate is below the source's measured rate.
#[test]
fn transport_bootstraps_warm_raw_source() {
    let mut child = Child::new(&[5_000_000], 3_000_000, false);
    for _ in 0..200 {
        child.frame(20_000, true);
        let (available, rates) =
            piik_media_forwarding::SourceView::layered_bitrate(child.input.source());
        if !available.is_empty() && rates[0][0] > 4_000_000 {
            break;
        }
    }
    let (_, rates) = piik_media_forwarding::SourceView::layered_bitrate(child.input.source());
    assert!(rates[0][0] > 4_000_000, "source did not establish its rate");
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();
    let mut packets = 0;
    for _ in 0..90 {
        packets += child.frame(20_000, true).len();
        if packets > 0 && child.transport.target_bitrate().1 {
            return;
        }
    }
    panic!(
        "warm source could not establish video/feedback: packets={packets} state={:?}",
        child.transport.state(child.input.source())
    );
}

#[test]
fn admission_does_not_age_a_stale_queue_sample() {
    let mut child = Child::new(&[500_000], 500_000, false);
    let now = child.link.now();
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();
    let packets = write_frame(
        &mut child.input,
        child.link.anchor,
        now,
        Duration::ZERO,
        &[(0, body(100, true), true)],
    );
    child.deliver(&packets, true);
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    rtc.handle_input(Input::Timeout(now)).unwrap();
    let mut sent = false;
    for _ in 0..1000 {
        match rtc.poll_output().unwrap() {
            Output::Timeout(at) => rtc.handle_input(Input::Timeout(at)).unwrap(),
            Output::Transmit(packet)
                if WireRtp::parse(&packet.contents).is_some_and(|rtp| {
                    rtp.payload_type == *child.link.video_pt && rtp.ssrc != 0
                }) =>
            {
                sent = true;
                break;
            }
            _ => {}
        }
    }
    assert!(sent, "source packet was not emitted");
    let sampled = {
        let mut api = rtc.direct_api();
        let tx = api
            .stream_tx_by_mid(child.link.video.unwrap(), None)
            .unwrap();
        let info = tx.queue_info().unwrap();
        assert!(
            info.packet_count() > 0,
            "fixture needs the pre-send snapshot"
        );
        assert!(info.first_unsent().is_some());
        info.created_at()
    };
    let later = sampled + Duration::from_millis(400);
    let packets = write_frame(
        &mut child.input,
        child.link.anchor,
        later,
        later.duration_since(now),
        &[(0, body(100, true), true)],
    );
    for packet in packets {
        child
            .transport
            .forward(rtc, child.input.source(), &packet, later);
    }
    assert_eq!(
        child.transport.egress().dropped_frames,
        0,
        "a drained queue's stale snapshot became a fictitious 400 ms backlog"
    );
    assert_eq!(child.transport.egress().packets, 2);
}

/// Send queues drain after a bandwidth reduction: frames larger than the
/// 200 ms drain bound at a collapsed estimate are refused whole, and the
/// output recovers from the next keyframe without a sequence gap.
#[test]
fn send_queue_drains_after_bandwidth_reduction() {
    let mut child = Child::new(&[500_000], 500_000, false);
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();
    let mut video = Vec::new();
    for _ in 0..30 {
        video.extend(child.frame(1_500, true));
    }
    assert!(!video.is_empty());
    // Collapse the link and keep feeding large frames.
    let tx = addr(TX);
    let rx = addr(RX);
    child.link.net.set_link(
        tx,
        rx,
        piik_media_testkit::netem::NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(
                piik_media_testkit::netem::Bitrate::kbps(80),
                piik_media_testkit::netem::DataSize::kbytes(8),
            ),
    );
    for index in 0..150 {
        video.extend(child.frame(12_000, index % 15 == 0));
    }
    let egress = child.transport.egress();
    assert!(
        egress.dropped_frames > 0,
        "bounded admission was not exercised: {egress:?} budget={}",
        child.transport.target_bitrate().0
    );
    // Restore the link: the queue drains in bounded time and video resumes.
    child
        .link
        .net
        .set_link(tx, rx, piik_media_testkit::netem::NetemConfig::new());
    let before = video.len();
    let mut resumed = Vec::new();
    // A paused output waits the paused minimum wait before the estimator
    // restarts; then video resumes on the recovered link.
    for index in 0..300 {
        resumed.extend(child.frame(1_500, index % 10 == 0));
    }
    assert!(
        resumed.len() > 30,
        "recovered link did not resume: {} egress={:?} budget={:?} state={:?} goal={:?} requests={}",
        resumed.len(),
        child.transport.egress(),
        child.transport.target_bitrate(),
        child.transport.state(child.input.source()),
        child.transport.bitrate_goal(),
        child.requests.len()
    );
    video.extend(resumed);
    let mut seqs: Vec<u64> = video[before..].iter().map(|p| p.seq).collect();
    seqs.sort_unstable();
    seqs.dedup();
    let tail = &seqs[seqs.len() / 2..];
    assert_eq!(
        tail.last().unwrap() - tail.first().unwrap() + 1,
        tail.len() as u64,
        "refused frames left an unrepairable hole"
    );
}

/// A forwarded source sends far below its committed capacity: a mostly static
/// screen encodes at a few hundred kbps while the connection can carry
/// megabytes. str0m's application-limited estimator converges on that sent
/// rate, and sizing the pacer from it would strangle a busier scene forever.
/// The committed capacity is this output's own measurement, so it stays.
#[test]
fn forwarded_output_keeps_the_committed_pacer_through_low_content() {
    let mut child = Child::new(&[300_000], 3_000_000, false);
    // A link that comfortably carries the commitment: without a limit the
    // estimator would keep climbing and the property would be vacuous.
    child.link.net.set_link(
        addr(TX),
        addr(RX),
        piik_media_testkit::netem::NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(
                piik_media_testkit::netem::Bitrate::mbps(8),
                piik_media_testkit::netem::DataSize::kbytes(32),
            ),
    );
    let rtc = &mut child.link.net.peer_mut(child.link.tx).rtc;
    child
        .transport
        .set_connected(rtc, child.input.source())
        .unwrap();
    // Ten seconds of sparse content, one 700-byte keyframe a frame.
    let mut payloads = 0;
    for index in 0..300 {
        payloads += child.frame(700, index % 30 == 0).len();
    }
    assert!(payloads > 0, "no video was forwarded");
    let state = child.transport.state(child.input.source());
    assert!(
        state.video_budget >= 3_000_000,
        "an application-limited estimate revoked committed capacity: {state:?}"
    );
    assert!(
        child
            .transport
            .bitrate_goal()
            .is_some_and(|goal| goal.desired >= 3_000_000),
        "the estimator stopped confirming the committed capacity: {:?}",
        child.transport.bitrate_goal()
    );
    assert!(!state.paused, "the only layer was paused: {state:?}");
    assert_eq!(child.transport.egress().dropped_frames, 0);
    // A busier scene still leaves at full rate rather than at the estimate the
    // sparse content produced.
    let before = child.transport.egress().packets;
    let mut burst = Vec::new();
    for index in 0..30 {
        burst.extend(child.frame(20_000, index % 10 == 0));
    }
    let sent = rate(&burst, Duration::from_secs(1));
    assert!(
        sent > 2_500_000.0,
        "the pacer stayed at the sparse content rate: {sent:.0} bps after {} packets",
        child.transport.egress().packets - before
    );
}
