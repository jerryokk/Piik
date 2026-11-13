//! SimulcastSend, publication clock and recovery tests on the virtual network.

mod common;

use std::time::Duration;

use common::*;
use piik_encoded::Frame;
use piik_media_forwarding::SourceView;
use piik_media_session::{
    EncodedSource, InputClock, PublicationOptions, SimulcastSend, SourcePacket, UpstreamKeyframe,
};
use piik_media_testkit::WireRtp;
use piik_media_testkit::netem::{Bitrate, DataSize, NetemConfig};
use piik_media_testkit::str0m::media::{KeyframeRequestKind, Rid};

const RIDS: [&str; 2] = ["q", "h"];

pub struct Host {
    pub link: Link,
    pub input: EncodedSource,
    pub publication: SimulcastSend,
    pub requests: Vec<UpstreamKeyframe>,
    pub pts: Duration,
}

impl Host {
    pub fn new(bitrates: &[u32], initial: i64) -> Self {
        let rids: Vec<&str> = RIDS[..bitrates.len()].to_vec();
        let link = Link::new(Some(&rids), false, initial);
        let input = EncodedSource::new(
            vp8_options("screen", "publication", bitrates),
            InputClock::Local(link.anchor),
        )
        .unwrap();
        let publication = SimulcastSend::new(
            PublicationOptions {
                mid: link.video.unwrap(),
                pt: link.video_pt,
                initial_bitrate: initial,
            },
            input.source(),
        )
        .unwrap();
        Self {
            link,
            input,
            publication,
            requests: Vec::new(),
            pts: Duration::ZERO,
        }
    }

    pub fn connect(&mut self) {
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        self.publication
            .set_connected(rtc, self.input.source())
            .unwrap();
    }

    pub fn deliver(&mut self, packets: &[SourcePacket], changed: bool) {
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        let source = self.input.source();
        if changed {
            self.requests
                .extend(self.publication.reconcile(rtc, source));
        }
        for packet in packets {
            self.requests
                .extend(self.publication.forward(rtc, source, packet, now));
        }
    }

    /// Writes one input to `layers` (sizes) and advances `FRAME`.
    pub fn frame(&mut self, sizes: &[Option<usize>], key: bool) -> Vec<Received> {
        let now = self.link.now();
        self.pts += FRAME;
        let pts = self.pts;
        self.input
            .begin_frame(pts, self.link.anchor.system(now))
            .unwrap();
        let mut changed = false;
        let mut packets = Vec::new();
        for (layer, size) in sizes.iter().enumerate() {
            let Some(size) = size else { continue };
            let mut data = body(*size, key);
            let tail = data.len() - 8;
            let sent = (now - self.link.anchor.instant).as_nanos() as u64;
            data[tail..].copy_from_slice(&sent.to_be_bytes());
            let written = self
                .input
                .write_frame(
                    layer,
                    &Frame {
                        data,
                        pts,
                        duration: FRAME,
                        recovery: key,
                    },
                    None,
                )
                .unwrap();
            changed |= written.changes.any();
            packets.extend(written.packets);
        }
        self.deliver(&packets, changed);
        self.advance(FRAME)
    }

    fn frame_recovering(&mut self, sizes: &[Option<usize>]) -> Vec<Received> {
        let key = !std::mem::take(&mut self.requests).is_empty();
        self.frame(sizes, key)
    }

    pub fn advance(&mut self, duration: Duration) -> Vec<Received> {
        let (events, received, _) = self.link.step(duration);
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        let source = self.input.source();
        for event in &events {
            self.requests
                .extend(self.publication.handle_event(rtc, source, event));
        }
        let changes = self.input.source_mut().poll(self.link.net.now());
        if changes.any() {
            self.deliver(&[], true);
        }
        let now = self.link.net.now();
        let rtc = &mut self.link.net.peer_mut(self.link.tx).rtc;
        self.publication.poll(rtc, self.input.source(), now);
        received
    }

    pub fn ssrcs(&mut self) -> Vec<Option<u32>> {
        let count = self.input.source().layer_count();
        (0..count)
            .map(|layer| {
                let rid = (count > 1).then_some(RIDS[layer]);
                self.link.rx_ssrc(rid)
            })
            .collect()
    }
}

fn per_layer(host: &mut Host, received: &[Received]) -> Vec<usize> {
    let ssrcs = host.ssrcs();
    ssrcs
        .iter()
        .map(|ssrc| {
            received
                .iter()
                .filter(|p| Some(p.ssrc) == *ssrc && p.marker)
                .count()
        })
        .collect()
}

#[test]
fn publication_refused_frame_waits_for_recovery() {
    let mut host = Host::new(&[90_000, 300_000], 1_000_000);
    host.connect();
    for _ in 0..30 {
        host.frame(&[Some(800), Some(800)], true);
    }
    // A scene-change burst fills the real str0m send queue. Refusing a delta
    // frame must also withhold its dependants, not just close their RTP gap.
    host.frame(&[Some(800), Some(400_000)], true);
    let before = host.publication.counters(host.input.source());
    host.requests.clear();
    host.frame(&[Some(800), Some(800)], false);
    let refused = host.publication.counters(host.input.source());
    assert!(refused.egress.dropped_frames > before.egress.dropped_frames);
    host.advance(Duration::from_secs(2));
    host.requests.clear();
    host.frame(&[Some(800), Some(800)], false);
    let waiting = host.publication.counters(host.input.source());
    assert_eq!(
        waiting.frames[1], refused.frames[1],
        "dependent delta was forwarded"
    );
    assert!(
        waiting.frames[0] > refused.frames[0],
        "healthy RID was withheld"
    );
    assert!(host.requests.iter().any(|request| request.layer == 1));
    host.frame(&[Some(800), Some(800)], true);
    assert!(host.publication.counters(host.input.source()).frames[1] > waiting.frames[1]);
}

#[test]
fn publication_simulcast_reuse_demand_and_close() {
    let mut host = Host::new(&[90_000, 300_000], 1_000_000);
    assert_eq!(host.publication.video_budget(), 1_000_000);
    assert_eq!(
        host.publication.required_active_count(),
        0,
        "unconnected demand"
    );
    assert_eq!(host.publication.rid(0), Some(Rid::from("q")));
    assert_eq!(host.publication.rid(1), Some(Rid::from("h")));
    host.connect();
    host.connect();
    assert_eq!(host.publication.required_active_count(), 2);
    assert_eq!(
        host.publication.bitrate_goal().unwrap().desired,
        390_000,
        "known encoding demand must not wait for a measured rate window"
    );
    let low = 90_000 / 8 / 30;
    let high = 300_000 / 8 / 30;
    let mut received = Vec::new();
    for _ in 0..30 {
        received.extend(host.frame(&[Some(low), Some(high)], true));
    }
    let frames = per_layer(&mut host, &received);
    assert!(
        frames[0] >= 3 && frames[1] >= 3,
        "RID media missing: {frames:?}"
    );
    // Each RID carries its own encoding's payload unchanged after the descriptor.
    let ssrcs = host.ssrcs();
    for (layer, size) in [low, high].into_iter().enumerate() {
        let last = received
            .iter()
            .rfind(|p| Some(p.ssrc) == ssrcs[layer] && p.marker)
            .unwrap();
        assert!(
            last.payload.len() <= size + 4 && last.payload.len() > 8,
            "layer {layer}"
        );
    }
    let counters = host.publication.counters(host.input.source());
    assert!(
        counters.frames[0] > 0 && counters.frames[1] > 0,
        "{counters:?}"
    );

    // A subscriber PLI on one RID names that layer.
    host.requests.clear();
    let mid = host.link.video.unwrap();
    host.link
        .rx_rtc()
        .direct_api()
        .stream_rx_by_mid(mid, Some(Rid::from("h")))
        .unwrap()
        .request_keyframe(KeyframeRequestKind::Pli);
    host.advance(Duration::from_millis(100));
    assert!(host.requests.iter().any(|request| request.layer == 1));

    // Loss on the low RID is repaired through negotiated RTX.
    let low_ssrc = ssrcs[0].unwrap();
    let last_seq = received
        .iter()
        .filter(|p| p.ssrc == low_ssrc)
        .map(|p| p.seq)
        .max()
        .unwrap();
    let lost = (last_seq + 5) as u16;
    let mut dropped = false;
    host.link
        .net
        .set_drop_filter(Some(Box::new(move |_, _, bytes| {
            let hit = WireRtp::parse(bytes)
                .is_some_and(|rtp| rtp.ssrc == low_ssrc && rtp.sequence_number == lost);
            let drop = hit && !dropped;
            dropped |= hit;
            drop
        })));
    let mut repaired = Vec::new();
    for _ in 0..30 {
        repaired.extend(host.frame_recovering(&[Some(low), Some(high)]));
    }
    host.link.net.set_drop_filter(None);
    assert!(
        repaired
            .iter()
            .any(|p| p.ssrc == low_ssrc && p.seq as u16 == lost),
        "publication did not recover loss through RTX"
    );

    // Measured rates exist; a constrained link commits a shared limit.
    for _ in 0..150 {
        host.frame_recovering(&[Some(low), Some(high)]);
    }
    let (_, rates) = host.input.source().layered_bitrate();
    assert!(
        rates[0][0] > 0 && rates[1][0] > 0,
        "both encodings measured"
    );
    let (tx, rx) = (addr(TX), addr(RX));
    host.link.net.set_link(
        tx,
        rx,
        NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(Bitrate::kbps(200), DataSize::kbytes(8)),
    );
    let mut limited = false;
    for _ in 0..300 {
        host.frame_recovering(&[Some(low), Some(high)]);
        limited |= host.publication.counters(host.input.source()).limited;
    }
    let budget = host.publication.video_budget();
    assert!(
        limited && budget < 1_000_000,
        "no shared limit: budget={budget}"
    );
    let paused_high = !host
        .publication
        .allocator()
        .track(1)
        .unwrap()
        .target_layer()
        .is_valid();
    let window: Vec<_> = (0..30)
        .flat_map(|_| host.frame_recovering(&[Some(low), Some(high)]))
        .collect();
    let frames = per_layer(&mut host, &window);
    assert!(frames[0] > 0, "low RID stopped under the shared limit");
    if paused_high {
        assert!(frames[1] <= 2, "paused high RID kept sending: {frames:?}");
    }

    // Publisher demand: the inactive high RID stops; low continues and the
    // publication is no longer limited once low fits.
    let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
    host.publication
        .set_active_count(rtc, host.input.source(), 1)
        .unwrap();
    assert_eq!(host.publication.required_active_count(), 1);
    host.link.net.set_link(tx, rx, NetemConfig::new());
    for _ in 0..30 {
        host.frame_recovering(&[Some(low), None]);
    }
    let window: Vec<_> = (0..30)
        .flat_map(|_| host.frame_recovering(&[Some(low), None]))
        .collect();
    let frames = per_layer(&mut host, &window);
    assert!(frames[0] > 20 && frames[1] == 0, "{frames:?}");
    let mut cleared = false;
    for _ in 0..300 {
        host.frame_recovering(&[Some(low), None]);
        if !host.publication.counters(host.input.source()).limited {
            cleared = true;
            break;
        }
    }
    assert!(
        cleared,
        "limitation stayed after low recovered and high deactivated"
    );

    // A rate-controlled encoder constrained inside its sole RID is limited.
    host.input
        .source_mut()
        .set_lowest_layer_rate_controlled(true);
    let budget = host.publication.lowest_layer_budget(host.input.source());
    assert_eq!(
        host.publication.counters(host.input.source()).limited,
        90_000 > budget,
        "encoder ceiling vs budget {budget}"
    );
    host.input
        .source_mut()
        .set_lowest_layer_rate_controlled(false);

    // Reactivation resumes the high RID once allocation allows it.
    let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
    host.publication
        .set_active_count(rtc, host.input.source(), 2)
        .unwrap();
    let mut resumed = false;
    for index in 0..600 {
        let window = host.frame(&[Some(low), Some(high)], index % 30 == 0);
        if per_layer(&mut host, &window)[1] > 0 {
            resumed = true;
            break;
        }
    }
    assert!(resumed, "requested high RID did not resume");

    let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
    host.publication.close(rtc);
    assert_eq!(host.publication.required_active_count(), 0);
    host.advance(Duration::from_millis(300));
    for _ in 0..5 {
        assert!(host.frame(&[Some(low), Some(high)], true).is_empty());
    }
}

/// A dormant RID resumes after a capture replacement with its elapsed wire
/// clock equal to the capture clock, though RIDs last emitted different PTS.
#[test]
fn publication_dormant_layer_retains_clock_across_generation() {
    let mut host = Host::new(&[100_000, 1_000_000], 2_000_000);
    host.connect();
    for _ in 0..30 {
        host.frame(&[Some(200), Some(400)], true);
    }
    let ssrcs = host.ssrcs();
    let high = ssrcs[1].unwrap();
    // The last high frame, then only low frames until a replacement input.
    let mut last_high = None;
    for received in host.frame(&[Some(200), Some(400)], true) {
        if received.ssrc == high && received.marker {
            last_high = Some((
                received.timestamp,
                host.link.anchor.system(host.link.now() - FRAME),
            ));
        }
    }
    let mut window = Vec::new();
    for _ in 0..10 {
        window.extend(host.frame(&[Some(200), None], true));
    }
    let (high_ts, high_capture) = last_high.expect("high frame missing");
    host.input.begin_generation();
    host.pts = Duration::ZERO;
    // The new capture runs 5 s with only the low rendition.
    for _ in 0..150 {
        host.frame(&[Some(200), None], true);
    }
    let resume_at = host.link.now();
    let resumed = host.frame(&[Some(200), Some(400)], true);
    let mut late = resumed;
    for _ in 0..3 {
        late.extend(host.advance(Duration::from_millis(10)));
    }
    let first = late
        .iter()
        .find(|p| p.ssrc == high && p.marker)
        .expect("dormant high rendition resumed");
    let actual = Duration::from_nanos(
        u64::from(first.timestamp.wrapping_sub(high_ts)) * 1_000_000_000 / 90_000,
    );
    let expected = host
        .link
        .anchor
        .system(resume_at)
        .duration_since(high_capture)
        .unwrap();
    let mismatch = actual.abs_diff(expected);
    assert!(
        mismatch <= Duration::from_millis(1),
        "replacement changed the wire capture clock by {mismatch:?}"
    );
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scenario {
    Healthy,
    Brief,
    Periodic,
    Sustained,
    PauseResume,
    SourceReplacement,
}

/// Constrained-link publication recovery; virtual time makes it cheap. The
/// final windows must deliver the encoder ceiling with sustained frame
/// arrivals and low age.
#[test]
fn publication_constrained_recovery() {
    for scenario in [
        Scenario::Healthy,
        Scenario::Brief,
        Scenario::Periodic,
        Scenario::Sustained,
        Scenario::PauseResume,
        Scenario::SourceReplacement,
    ] {
        check_publication_recovery(scenario);
    }
}

fn check_publication_recovery(scenario: Scenario) {
    let mut host = Host::new(&[300_000], 1_200_000);
    host.input
        .source_mut()
        .set_lowest_layer_rate_controlled(true);
    let (tx, rx) = (addr(TX), addr(RX));
    let shape = |capacity: u64| {
        NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(Bitrate::bps(capacity), DataSize::bytes(7_500))
    };
    host.link.net.set_link(tx, rx, shape(6_000_000));
    host.connect();
    let start = host.link.now();
    let mut ceiling = 300_000i64;
    let (mut muted, mut resumed, mut replaced) = (false, false, false);
    let (mut recovered, mut paused_windows) = (0, 0);
    let mut window = Vec::new();
    let mut last_capacity = 0;
    let mut window_start = start;
    loop {
        let now = host.link.now();
        let elapsed = now - start;
        if elapsed >= Duration::from_secs(60) {
            break;
        }
        let s = elapsed.as_secs_f64();
        let constrained = match scenario {
            Scenario::Healthy => false,
            Scenario::Brief => (12.0..13.0).contains(&s),
            Scenario::Periodic => [12.0, 22.0, 32.0]
                .iter()
                .any(|t| (*t..t + 1.0).contains(&s)),
            _ => (8.0..30.0).contains(&s),
        };
        let capacity = if constrained { 120_000 } else { 6_000_000 };
        if capacity != last_capacity {
            host.link.net.set_link(tx, rx, shape(capacity));
            last_capacity = capacity;
        }
        if scenario == Scenario::PauseResume && s >= 32.0 && !muted {
            let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
            host.publication
                .set_active_count(rtc, host.input.source(), 0)
                .unwrap();
            muted = true;
        }
        if scenario == Scenario::PauseResume && s >= 38.0 && !resumed {
            let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
            host.publication
                .set_active_count(rtc, host.input.source(), 1)
                .unwrap();
            resumed = true;
        }
        if scenario == Scenario::SourceReplacement && s >= 32.0 && !replaced {
            let mut next = EncodedSource::new(
                vp8_options("screen", "publication", &[600_000]),
                InputClock::Local(host.link.anchor),
            )
            .unwrap();
            next.source_mut().set_lowest_layer_rate_controlled(true);
            let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
            let recovery = host
                .publication
                .replace_source(rtc, host.input.source(), next.source())
                .unwrap();
            assert!(recovery.iter().all(|request| request.force));
            host.input.close();
            host.input = next;
            host.pts = Duration::ZERO;
            ceiling = 600_000;
            replaced = true;
        }
        let budget = host.publication.lowest_layer_budget(host.input.source());
        assert!(
            budget <= 6_000_000,
            "unmeasured capacity beyond the link: {budget}"
        );
        let rtp = ceiling.min(budget.max(1));
        let size = (host.input.codec_budget(0, rtp) / 8 / 30).max(VP8_KEY.len() as i64 + 8);
        window.extend(host.frame(&[Some(size as usize)], true));
        let now = host.link.now();
        if now - window_start < Duration::from_secs(1) {
            continue;
        }
        let interval = now - window_start;
        let received_bps = rate(&window, interval);
        let frames: Vec<_> = window
            .iter()
            .filter(|p| p.marker && p.payload.len() >= 8)
            .collect();
        let mut ages: Vec<Duration> = frames
            .iter()
            .map(|p| {
                let tail: [u8; 8] = p.payload[p.payload.len() - 8..].try_into().unwrap();
                let sent =
                    host.link.anchor.instant + Duration::from_nanos(u64::from_be_bytes(tail));
                p.at.saturating_duration_since(sent)
            })
            .collect();
        ages.sort_unstable();
        let p95 = ages
            .get(ages.len().saturating_sub(1) * 95 / 100)
            .copied()
            .unwrap_or_default();
        let arrivals = frames.len() as f64 / interval.as_secs_f64();
        if muted && !resumed && s >= 34.0 {
            assert!(
                window.is_empty(),
                "{scenario:?}: paused publication kept sending"
            );
            paused_windows += 1;
        }
        let budget = host.publication.lowest_layer_budget(host.input.source());
        if s >= 50.0
            && budget >= ceiling
            && received_bps > ceiling as f64 * 0.9
            && received_bps < ceiling as f64 * 1.2
            && arrivals >= 27.0
            && p95 < Duration::from_millis(100)
        {
            // GoogCC's AIMD can cut a healthy estimate briefly after ALR
            // probing, so the property is 5 of the final 10 windows rather
            // than 5 consecutive ones.
            recovered += 1;
        }
        window.clear();
        window_start = now;
    }
    assert!(
        recovered >= 5 && (scenario != Scenario::PauseResume || paused_windows >= 2),
        "{scenario:?}: missing sustained recovery/pause: recovered={recovered} paused={paused_windows} budget={}",
        host.publication.lowest_layer_budget(host.input.source())
    );
    let rtc = &mut host.link.net.peer_mut(host.link.tx).rtc;
    host.publication.close(rtc);
    assert_eq!(
        host.publication
            .allocator()
            .additional_probe_demand(host.input.source()),
        0
    );
}
