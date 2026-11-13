//! Audio send slot and ingress repair tests.

mod common;

use std::time::{Duration, SystemTime};

use common::*;
use piik_media_forwarding::{CodecKind, InboundPacket, PliThrottle, Source, SourceView};
use piik_media_session::{AudioSlot, AudioSlotOptions, Ingress, SourcePacket, UpstreamKeyframe};
use piik_media_testkit::str0m::media::{
    Direction, Frequency, MediaKind, MediaTime, Rid, Simulcast, SimulcastLayer,
};
use piik_media_testkit::str0m::rtp::RawPacket;
use piik_media_testkit::str0m::rtp::rtcp::Rtcp;
use piik_media_testkit::str0m::{Event, Rtc};
use piik_media_testkit::{Network, WireRtp, negotiate};

fn opus_source(id: &str, ssrc: u32) -> Source {
    let mut source = Source::new(opus_options(id)).unwrap();
    source.bind_layer(0, ssrc).unwrap();
    source
}

#[test]
fn audio_slot_retains_clock_across_input_lifetime_and_loss() {
    let mut link = Link::new(None, true, 300_000);
    let mut slot = AudioSlot::new(AudioSlotOptions {
        mid: link.audio.unwrap(),
        pt: link.audio_pt,
        source_ceiling: 510_000,
    });
    let write = |link: &mut Link,
                 slot: &mut AudioSlot,
                 attachment,
                 source: &mut Source,
                 ssrc: u32,
                 seq: u64,
                 ts: u64|
     -> Option<Received> {
        let now = link.now();
        let info = InboundPacket::new(CodecKind::Opus, 0, seq, ts, &[0xf8, 0xff, 0xfe], now)
            .ssrc(ssrc)
            .payload_type(111);
        source.write_rtp(0, &info).unwrap();
        let packet = SourcePacket {
            info,
            payload: [0xf8, 0xff, 0xfe].into(),
        };
        let rtc = &mut link.net.peer_mut(link.tx).rtc;
        slot.forward(rtc, attachment, source, &packet, now);
        let (_, received, _) = link.step(Duration::from_millis(20));
        received.into_iter().next()
    };
    let mut first = opus_source("first", 1000);
    let stale = slot.replace_source(&first).unwrap();
    assert_eq!(
        slot.replace_source(&first).unwrap(),
        stale,
        "same source keeps its token"
    );
    let a = write(
        &mut link, &mut slot, stale, &mut first, 1000, 20_000, 9_600_000,
    )
    .unwrap();
    let b = write(
        &mut link, &mut slot, stale, &mut first, 1000, 20_001, 9_600_960,
    )
    .unwrap();
    assert_eq!((b.seq - a.seq, b.timestamp - a.timestamp), (1, 960));
    // A lost packet's sequence/timestamp space and a later DTX gap survive.
    let c = write(
        &mut link, &mut slot, stale, &mut first, 1000, 20_003, 9_602_880,
    )
    .unwrap();
    let late = write(
        &mut link, &mut slot, stale, &mut first, 1000, 20_002, 9_601_920,
    )
    .unwrap();
    let d = write(
        &mut link, &mut slot, stale, &mut first, 1000, 20_004, 9_607_680,
    )
    .unwrap();
    assert_eq!((c.seq - b.seq, c.timestamp - b.timestamp), (2, 1920));
    assert_eq!((late.seq - b.seq, late.timestamp - b.timestamp), (1, 960));
    assert_eq!((d.seq - c.seq, d.timestamp - c.timestamp), (1, 4800));
    // str0m reports this slot's own SSRC.
    let (_, _, events) = link.step(Duration::from_secs(6));
    assert!(
        events.iter().any(|event| matches!(
            event,
            Event::SenderFeedback(feedback) if *feedback.sender_info.ssrc == a.ssrc
        )),
        "output did not report its own sender identity"
    );

    let mut next = opus_source("next", 2000);
    let attached = slot.replace_source(&next).unwrap();
    assert_ne!(attached, stale);
    // A detached input's late packet and retirement cannot touch the slot.
    assert!(
        write(
            &mut link, &mut slot, stale, &mut first, 1000, 20_005, 9_608_640
        )
        .is_none()
    );
    slot.source_closed(stale);
    assert_eq!(slot.attached(), Some("next"));
    first.close();
    let e = write(
        &mut link, &mut slot, attached, &mut next, 2000, 1000, 480_000,
    )
    .unwrap();
    assert_eq!(
        e.ssrc, d.ssrc,
        "independent input changed the slot identity"
    );
    assert_eq!(e.seq - d.seq, 1);
    assert!((e.timestamp.wrapping_sub(d.timestamp) as i32) > 0);

    // Retiring the current input detaches it; the slot accepts a later one,
    // including one reusing its predecessor's SSRC.
    slot.source_closed(attached);
    next.close();
    assert!(slot.attached().is_none() && !slot.is_closed());
    let mut last = opus_source("last", 2000);
    let reused = slot.replace_source(&last).unwrap();
    let f = write(&mut link, &mut slot, reused, &mut last, 2000, 100, 48_000).unwrap();
    assert_eq!((f.ssrc, f.seq - e.seq), (e.ssrc, 1));
    assert!((f.timestamp.wrapping_sub(e.timestamp) as i32) > 0);
    slot.detach_source();
    assert!(write(&mut link, &mut slot, reused, &mut last, 2000, 101, 48_960).is_none());
    assert!(!slot.is_closed());

    // Receiver admission: an omitted maxaveragebitrate uses codec defaults.
    let answer = |fmtp: &str| {
        format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n\
             a=rtpmap:111 opus/48000/2\r\na=fmtp:111 {fmtp}\r\n"
        )
    };
    assert!(slot.check_receiver(&answer("minptime=10")).is_err());
    assert!(
        slot.check_receiver(&answer("maxaveragebitrate=510000"))
            .is_ok()
    );
    slot.close();
    assert!(slot.replace_source(&last).is_err());
}

#[test]
fn ingress_repair_restores_primary_and_reports() {
    const RIDS: [&str; 3] = ["q", "h", "f"];
    let mut net = Network::new();
    net.set_min_step(Duration::from_micros(500));
    let publisher = net.add_peer(Rtc::builder(), addr(TX));
    let sfu = net.add_peer(
        Rtc::builder().set_rtp_mode(true).enable_raw_packets(true),
        addr(RX),
    );
    let mid = negotiate(&mut net, publisher, sfu, |api| {
        let mut simulcast = Simulcast::new();
        for rid in RIDS {
            simulcast.add_send_layer(SimulcastLayer::new(rid));
        }
        api.add_media(
            MediaKind::Video,
            Direction::SendOnly,
            None,
            None,
            Some(simulcast),
        )
    })
    .unwrap()
    .result;
    assert!(
        net.run_until(
            |n| n.peer(publisher).rtc.is_connected() && n.peer(sfu).rtc.is_connected(),
            Duration::from_secs(5)
        )
        .unwrap()
    );
    let pt = net
        .peer(publisher)
        .rtc
        .codec_config()
        .iter()
        .find(|params| params.spec().codec == piik_media_testkit::str0m::format::Codec::Vp8)
        .unwrap()
        .pt();
    let low_ssrc = *net
        .peer_mut(publisher)
        .rtc
        .direct_api()
        .stream_tx_by_mid(mid, Some(Rid::from("q")))
        .unwrap()
        .ssrc();
    let dropped = std::rc::Rc::new(std::cell::Cell::new(None::<u16>));
    let filter_dropped = dropped.clone();
    let mut first_seq = None;
    net.set_drop_filter(Some(Box::new(move |_, _, bytes| {
        let Some(rtp) = WireRtp::parse(bytes) else {
            return false;
        };
        if rtp.ssrc != low_ssrc {
            return false;
        }
        let target = first_seq
            .get_or_insert(rtp.sequence_number)
            .wrapping_add(16);
        if rtp.sequence_number == target && filter_dropped.get().is_none() {
            filter_dropped.set(Some(target));
            return true;
        }
        false
    })));
    let mut options = vp8_options("publisher", "repair", &[100_000, 300_000, 1_000_000]);
    options.max_packets = 0;
    let mut source = Source::new(options).unwrap();
    let mut ingress = Ingress::new(
        mid,
        RIDS.iter().map(|rid| Some(Rid::from(*rid))).collect(),
        PliThrottle::default(),
    )
    .unwrap();
    let mut accepted: Vec<(usize, u64)> = Vec::new();
    let mut nacks: Vec<(std::time::Instant, u16)> = Vec::new();
    let mut frame = 0u64;
    let mut payload = VP8_KEY.to_vec();
    payload.resize(200, 0);
    let mut run = |net: &mut Network,
                   source: &mut Source,
                   ingress: &mut Ingress,
                   accepted: &mut Vec<(usize, u64)>,
                   nacks: &mut Vec<(std::time::Instant, u16)>,
                   frames: u64| {
        for _ in 0..frames {
            let now = net.now();
            for rid in RIDS {
                net.peer_mut(publisher)
                    .rtc
                    .writer(mid)
                    .unwrap()
                    .rid(rid.into())
                    .write(
                        pt,
                        now,
                        MediaTime::new(frame * 3000, Frequency::NINETY_KHZ),
                        payload.clone(),
                    )
                    .unwrap();
            }
            frame += 1;
            net.run_for(Duration::from_millis(33)).unwrap();
            let events = std::mem::take(&mut net.peer_mut(sfu).events);
            for (at, event) in events {
                if let Event::RawPacket(raw) = &event
                    && let RawPacket::RtcpTx(Rtcp::Nack(nack)) = raw.as_ref()
                {
                    nacks.extend(nack.reports.iter().map(|entry| (at, entry.pid)));
                }
                let rtc = &mut net.peer_mut(sfu).rtc;
                if let Some(packet) = ingress.handle_event(rtc, source, &event).unwrap() {
                    accepted.push((
                        packet.packet.info.layer as usize,
                        packet.packet.info.ext_seq,
                    ));
                }
            }
            net.peer_mut(publisher).events.clear();
        }
    };
    run(
        &mut net,
        &mut source,
        &mut ingress,
        &mut accepted,
        &mut nacks,
        60,
    );
    let target = dropped.get().expect("loss was exercised");
    let repaired: Vec<_> = accepted
        .iter()
        .filter(|(layer, seq)| *layer == 0 && *seq as u16 == target)
        .collect();
    assert_eq!(
        repaired.len(),
        1,
        "repair restored the admitted primary packet once"
    );
    assert!(
        nacks.iter().any(|(_, pid)| *pid == target),
        "the hole was NACKed"
    );
    let ssrcs: Vec<_> = (0..3)
        .map(|layer| ingress.layer_ssrc(layer).unwrap())
        .collect();
    assert_eq!(ssrcs[0], low_ssrc);
    assert!(
        ssrcs[0] != ssrcs[1] && ssrcs[1] != ssrcs[2],
        "RIDs share an SSRC"
    );
    // Once repaired, the hole is not NACKed again.
    let settled = nacks.len();
    run(
        &mut net,
        &mut source,
        &mut ingress,
        &mut accepted,
        &mut nacks,
        30,
    );
    assert!(
        nacks[settled..].iter().all(|(_, pid)| *pid != target),
        "repaired primary still NACKed"
    );
    // Sender reports reach every layer and map RTP time to the sender clock.
    run(
        &mut net,
        &mut source,
        &mut ingress,
        &mut accepted,
        &mut nacks,
        90,
    );
    // Every RID wrote the same media time at the same wallclock, so each
    // layer's own report maps one frame to the same capture instant.
    let reports: Vec<_> = (0..3)
        .map(|layer| {
            source
                .current_sender_report(layer)
                .expect("layer sender report")
        })
        .collect();
    let probe = reports[0].rtp_timestamp;
    let captured: Vec<SystemTime> = (0..3)
        .map(|layer| source.reference_time(layer, probe).unwrap())
        .collect();
    for layer in 1..3 {
        let error = captured[layer]
            .duration_since(captured[0])
            .unwrap_or_else(|e| e.duration());
        assert!(
            error < Duration::from_millis(2),
            "layer {layer} clock error {error:?}"
        );
    }
    // Local projection follows report arrival on this connection's clock.
    let local = source.local_time(0, captured[0]).unwrap();
    assert!(local <= net.now() && net.now() - local < Duration::from_secs(10));
    assert_eq!(source.layered_bitrate().0, vec![0, 1, 2]);
    // Keyframe requests are throttled per layer; forced requests pass.
    let now = net.clock().now();
    let rtc = &mut net.peer_mut(sfu).rtc;
    let high = UpstreamKeyframe {
        layer: 2,
        force: false,
    };
    assert!(ingress.request_keyframe(rtc, high, now));
    assert!(!ingress.request_keyframe(rtc, high, now));
    assert!(ingress.request_keyframe(
        rtc,
        UpstreamKeyframe {
            layer: 2,
            force: true
        },
        now
    ));
    net.run_for(Duration::from_millis(100)).unwrap();
    let requests: Vec<_> = net
        .peer(publisher)
        .events
        .iter()
        .filter_map(|(_, event)| match event {
            Event::KeyframeRequest(request) => Some(request.rid),
            _ => None,
        })
        .collect();
    assert!(!requests.is_empty() && requests.iter().all(|rid| *rid == Some(Rid::from("f"))));
}
