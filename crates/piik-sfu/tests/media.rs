//! SFU media tests on real 127.0.0.1 UDP with str0m peers:
//! a str0m publisher (frame mode, simulcast RIDs, audio and the input data
//! channel) and RTP-mode str0m viewers.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use common::*;
use piik_input_route::LABEL;
use piik_media_transport::sdp::{self, set_opus_fmtp};
use piik_media_transport::str0m::format::Codec;
use piik_media_transport::str0m::media::{
    Direction, Frequency, MediaKind, MediaTime, Mid, Simulcast, SimulcastLayer,
};
use piik_media_transport::str0m::{Event, Rtc};
use piik_sfu::runtime::{
    IceCandidateInit, LayerFormat, MediaError, PeerConnectionState, PublicationMedia, Runtime,
    SdpType, SessionDescription,
};

const PUBLISHER: &str = "publisher_12345678";
const SUBSCRIBER: &str = "subscriber_12345678";

/// A VP8 keyframe (P bit clear) without payload descriptor.
const VP8_KEY: [u8; 20] = [
    0x10, 0x02, 0x00, 0x9d, 0x01, 0x2a, 0x08, 0x00, 0x08, 0x00, 0x00, 0x47, 0x08, 0x85, 0x85, 0x88,
    0x85, 0x84, 0x88, 0x02,
];

fn h264_key() -> Vec<u8> {
    let nals: [&[u8]; 3] = [
        &[
            0x67, 0x42, 0xc0, 0x1f, 0x0f, 0xd9, 0x1f, 0x88, 0x88, 0x84, 0x00, 0x00, 0x03, 0x00,
            0x04, 0x00, 0x00, 0x03, 0x00, 0xc8, 0x3c, 0x60, 0xc9, 0x20,
        ],
        &[0x68, 0x87, 0xcb, 0x83, 0xcb, 0x20],
        &[0x65, 0x88, 0x84, 0x0a, 0xf2, 0x62, 0x80, 0x00, 0xa7, 0xbe],
    ];
    let mut data = Vec::new();
    for nal in nals {
        data.extend_from_slice(&[0, 0, 0, 1]);
        data.extend_from_slice(nal);
    }
    data
}

fn metadata(codec: &str, rids: &[&str]) -> PublicationMedia {
    let formats = if rids.len() == 1 {
        vec![LayerFormat {
            width: 8,
            height: 8,
            bitrate: 1_000_000,
        }]
    } else {
        vec![
            LayerFormat {
                width: 8,
                height: 8,
                bitrate: 250_000,
            },
            LayerFormat {
                width: 8,
                height: 8,
                bitrate: 1_000_000,
            },
        ]
    };
    PublicationMedia {
        codec: codec.into(),
        rids: rids.iter().map(|rid| (*rid).to_owned()).collect(),
        formats,
        audio: true,
        audio_bitrate: 64_000,
    }
}

#[test]
fn unsupported_publication_description_does_not_allocate() {
    let f = fixture();
    for format in [
        LayerFormat {
            width: 32768,
            height: 32768,
            bitrate: 5_000_000,
        },
        LayerFormat {
            width: 1920,
            height: 1080,
            bitrate: 240_000_001,
        },
        LayerFormat {
            width: u32::MAX,
            height: u32::MAX,
            bitrate: u32::MAX,
        },
    ] {
        let media = PublicationMedia {
            codec: "vp8".into(),
            rids: vec![String::new()],
            formats: vec![format],
            ..Default::default()
        };
        assert!(
            f.media
                .prepare_publication(&fence(), PUBLISHER, &media)
                .is_err()
        );
        assert!(!f.media.has_publication(&fence(), PUBLISHER));
    }
    let mut media = metadata("vp8", &["q", "q"]);
    assert!(
        f.media
            .prepare_publication(&fence(), PUBLISHER, &media)
            .is_err(),
        "duplicate RIDs"
    );
    media.codec = "av1".into();
    assert!(
        f.media
            .prepare_publication(&fence(), PUBLISHER, &media)
            .is_err()
    );
    f.media.close().unwrap();
}

#[test]
fn publication_admits_supported_bitrate_ceilings() {
    let f = fixture();
    let mut media = PublicationMedia {
        codec: "h264".into(),
        rids: vec![String::new()],
        formats: vec![LayerFormat {
            width: 1920,
            height: 1080,
            bitrate: 240_000_000,
        }],
        ..Default::default()
    };
    f.media
        .prepare_publication(&fence(), PUBLISHER, &media)
        .unwrap();
    assert!(
        f.media
            .prepare_publication(&fence(), "replacement_12345678", &media)
            .is_err(),
        "a repeated prepare replaced a live publication"
    );
    // A candidate before the remote description keeps its lifecycle error.
    assert!(matches!(
        f.media
            .add_publisher_ice(&fence(), PUBLISHER, IceCandidateInit::default()),
        Err(MediaError::Failed(_))
    ));
    for bitrate in [5_000_000, 100_000_001, 240_000_000] {
        media.formats[0].bitrate = bitrate;
        f.media
            .update_publication(&fence(), PUBLISHER, &media)
            .unwrap();
    }
    media.formats[0].bitrate += 1;
    assert!(
        f.media
            .update_publication(&fence(), PUBLISHER, &media)
            .is_err()
    );
    media.formats[0].bitrate = 1_000_000;
    media.rids = vec!["f".into()];
    assert!(
        f.media
            .update_publication(&fence(), PUBLISHER, &media)
            .is_err(),
        "an update changed the publication identity"
    );
    assert!(f.media.has_publication(&fence(), PUBLISHER));
    assert!(!f.media.has_publication(&fence(), "stale_12345678"));
    f.media.close().unwrap();
    assert!(!f.media.has_publication(&fence(), PUBLISHER));
}

#[test]
fn demand_bootstraps_waiting_subscriber_and_stops_after_exact_close() {
    let f = fixture();
    let events = Events::default();
    events.install(&f.media);
    let mut media = metadata("vp8", &["q", "h"]);
    media.audio = false;
    f.media
        .prepare_publication(&fence(), PUBLISHER, &media)
        .unwrap();
    assert_eq!(f.media.publication_demand(&fence(), PUBLISHER), Ok(0));
    let fence = subscription("viewer_12345678", "connection_12345678");
    let prepared = {
        let (media, fence) = (f.media.clone(), fence.clone());
        std::thread::spawn(move || {
            media.prepare_subscriber(Instant::now() + WAIT, &fence, &fence.connection_id)
        })
    };
    until(|| events.active_counts() == [2], "bootstrap demand");
    assert_eq!(f.media.publication_demand(&fence.resource, ""), Ok(2));
    f.media
        .close_subscription(&fence, &fence.connection_id)
        .unwrap();
    assert!(
        prepared.join().unwrap().is_err(),
        "a retired subscriber acquired a later input"
    );
    until(|| events.active_counts() == [2, 0], "no-subscriber demand");
    f.media.close().unwrap();
}

#[test]
fn prepare_subscriber_gives_up_at_its_deadline() {
    let f = fixture();
    let events = Events::default();
    events.install(&f.media);
    f.media
        .prepare_publication(&fence(), PUBLISHER, &metadata("vp8", &[""]))
        .unwrap();
    let fence = subscription("viewer_12345678", "connection_12345678");
    let started = Instant::now();
    let result = f.media.prepare_subscriber(
        Instant::now() + Duration::from_millis(200),
        &fence,
        &fence.connection_id,
    );
    assert!(result.is_err());
    assert!(started.elapsed() < Duration::from_secs(2));
    // The abandoned attempt is retired and its demand withdrawn.
    until(|| events.active_counts() == [1, 0], "deadline cleanup");
    // The same identity may prepare again.
    let again = {
        let (media, fence) = (f.media.clone(), fence.clone());
        std::thread::spawn(move || {
            media.prepare_subscriber(
                Instant::now() + Duration::from_millis(200),
                &fence,
                &fence.connection_id,
            )
        })
    };
    assert!(again.join().unwrap().is_err());
    f.media.close().unwrap();
}

struct Publisher {
    peer: Arc<Peer>,
    stop: Arc<AtomicBool>,
    writer: Option<std::thread::JoinHandle<()>>,
}

impl Drop for Publisher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(writer) = self.writer.take() {
            let _ = writer.join();
        }
    }
}

fn pt_of(rtc: &mut Rtc, mid: Mid, codec: Codec) -> piik_media_transport::str0m::media::Pt {
    rtc.writer(mid)
        .unwrap()
        .payload_params()
        .find(|params| params.spec().codec == codec)
        .map(|params| params.pt())
        .expect("negotiated codec")
}

fn publish(f: &Fixture, codec: &str, rids: &[&str]) -> Publisher {
    let peer = Arc::new(Peer::new(Rtc::builder()));
    let (offer, pending, video, audio) = peer.with(|rtc| {
        let mut api = rtc.sdp_api();
        let simulcast = (rids.len() > 1).then(|| {
            let mut simulcast = Simulcast::new();
            for rid in rids {
                simulcast.add_send_layer(SimulcastLayer::new(rid));
            }
            simulcast
        });
        let video = api.add_media(MediaKind::Video, Direction::SendOnly, None, None, simulcast);
        let audio = api.add_media(MediaKind::Audio, Direction::SendOnly, None, None, None);
        api.add_channel(LABEL.to_owned());
        let (offer, pending) = api.apply().unwrap();
        (offer.to_sdp_string(), pending, video, audio)
    });
    let answer = f
        .media
        .accept_publisher_offer(
            &fence(),
            PUBLISHER,
            SessionDescription {
                sdp_type: SdpType::Offer,
                sdp: offer,
            },
        )
        .unwrap();
    assert!(answer.sdp.contains("a=ice-lite"));
    assert!(answer.sdp.contains("maxaveragebitrate=510000"));
    peer.accept(pending, &answer);
    let wire = if codec == "vp8" {
        Codec::Vp8
    } else {
        Codec::H264
    };
    let (video_pt, audio_pt) =
        peer.with(|rtc| (pt_of(rtc, video, wire), pt_of(rtc, audio, Codec::Opus)));
    let data = if codec == "vp8" {
        VP8_KEY.to_vec()
    } else {
        h264_key()
    };
    let stop = Arc::new(AtomicBool::new(false));
    let rids: Vec<String> = rids.iter().map(|rid| (*rid).to_owned()).collect();
    let writer = {
        let (peer, stop) = (peer.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut frame = 0u64;
            while !stop.load(Ordering::Relaxed) {
                peer.with(|rtc| {
                    if !rtc.is_connected() {
                        return;
                    }
                    let now = Instant::now();
                    for rid in &rids {
                        let Some(mut writer) = rtc.writer(video) else {
                            continue;
                        };
                        if !rid.is_empty() {
                            writer = writer.rid(rid.as_str().into());
                        }
                        let _ = writer.write(
                            video_pt,
                            now,
                            MediaTime::new(frame * 3000, Frequency::NINETY_KHZ),
                            data.clone(),
                        );
                    }
                    if let Some(writer) = rtc.writer(audio) {
                        let _ = writer.write(
                            audio_pt,
                            now,
                            MediaTime::new(frame * 1600, Frequency::FORTY_EIGHT_KHZ),
                            vec![0xf8, 0xff, 0xfe],
                        );
                    }
                });
                frame += 1;
                std::thread::sleep(Duration::from_millis(33));
            }
        })
    };
    Publisher {
        peer,
        stop,
        writer: Some(writer),
    }
}

const OPUS_FRAME: [u8; 3] = [0xf8, 0xff, 0xfe];

/// Distinct media packets a viewer received: (video, audio). Probe padding
/// resends cached video, so video counts distinct sequence numbers.
fn received(viewer: &Peer) -> (usize, usize) {
    let state = viewer.lock();
    let mut video = std::collections::HashSet::new();
    let mut audio = std::collections::HashSet::new();
    for event in &state.events {
        if let Event::RtpPacket(packet) = event {
            if *packet.payload == OPUS_FRAME {
                audio.insert(*packet.seq_no);
            } else if packet.payload.len() > OPUS_FRAME.len() {
                video.insert((*packet.header.ssrc, *packet.seq_no));
            }
        }
    }
    (video.len(), audio.len())
}

fn receiver_answer(answer: String) -> String {
    set_opus_fmtp(&answer, &[("stereo", "1"), ("maxaveragebitrate", "510000")]).unwrap()
}

fn run_fanout(codec: &str, rids: &[&str]) {
    let f = fixture();
    let events = Events::default();
    events.install(&f.media);
    let media = metadata(codec, rids);
    f.media
        .prepare_publication(&fence(), PUBLISHER, &media)
        .unwrap();

    // A waiting subscription is registered before input readiness and its
    // retirement ends the wait.
    let waiting = subscription("viewer_123456", "canceled_123456");
    let prepared = {
        let (media, fence) = (f.media.clone(), waiting.clone());
        std::thread::spawn(move || {
            media.prepare_subscriber(Instant::now() + WAIT, &fence, "canceled_123456")
        })
    };
    until(
        || events.active_counts().first() == Some(&(rids.len() as i64)),
        "waiting subscriber demand",
    );
    f.media
        .close_subscription(&waiting, "canceled_123456")
        .unwrap();
    assert!(prepared.join().unwrap().is_err());

    let publisher = publish(&f, codec, rids);
    until(|| publisher.peer.connected(), "publisher connects");

    let fences = [
        subscription("viewer_123456", "admitted_123456"),
        subscription("viewer_abcdef", "admitted_abcdef"),
    ];
    let mut viewers = Vec::new();
    for (index, fence) in fences.iter().enumerate() {
        let offer = f
            .media
            .prepare_subscriber(Instant::now() + WAIT, fence, SUBSCRIBER)
            .unwrap();
        assert_eq!(offer.sdp_type, SdpType::Offer);
        let viewer = Peer::new(Rtc::builder().set_rtp_mode(true));
        let answer = viewer.answer(&offer, receiver_answer);
        if index == 1 {
            // One limited receiver must not bind incompatible audio or
            // disturb its already-playing sibling.
            let mut limited = answer.clone();
            limited.sdp = limited
                .sdp
                .replace("maxaveragebitrate=510000", "maxaveragebitrate=192000");
            assert_ne!(limited.sdp, answer.sdp);
            assert!(
                f.media
                    .apply_subscriber_answer(fence, SUBSCRIBER, limited)
                    .is_err(),
                "shared audio accepted an insufficient receiver limit"
            );
            let before = received(&viewers[0]);
            until(
                || {
                    let now = received(&viewers[0]);
                    now.0 >= before.0 + 3 && now.1 >= before.1 + 3
                },
                "rejection interrupted the healthy sibling",
            );
        }
        f.media
            .apply_subscriber_answer(fence, SUBSCRIBER, answer)
            .unwrap();
        viewers.push(viewer);
    }
    until_with(
        || {
            viewers.iter().all(|viewer| {
                let (video, audio) = received(viewer);
                video >= 3 && audio >= 3
            })
        },
        || {
            let summary = |viewer: &Peer| {
                let state = viewer.lock();
                let mut kinds = std::collections::BTreeMap::<String, usize>::new();
                for event in &state.events {
                    let name = format!("{event:?}");
                    let name = name.split(['(', ' ', '{']).next().unwrap_or("").to_owned();
                    *kinds.entry(name).or_default() += 1;
                }
                let mut packets = std::collections::BTreeMap::<(u8, usize), usize>::new();
                for event in &state.events {
                    if let Event::RtpPacket(p) = event {
                        *packets
                            .entry((*p.header.payload_type, p.payload.len()))
                            .or_default() += 1;
                    }
                }
                format!("{kinds:?} packets={packets:?}")
            };
            format!(
                "media reaches both viewers: {:?} {:?} connected={} {} | {} | {}",
                received(&viewers[0]),
                received(&viewers[1]),
                viewers[0].connected(),
                viewers[1].connected(),
                summary(&viewers[0]),
                summary(&viewers[1]),
            )
        },
    );
    until(
        || {
            events
                .states(SUBSCRIBER)
                .contains(&PeerConnectionState::Connected)
        },
        "subscriber state events",
    );

    if codec == "vp8" && rids.len() == 1 {
        // The Viewer's input channel relays through the publisher's.
        until(
            || publisher.peer.send_text(r#"{"ready":true}"#),
            "publisher input",
        );
        until(
            || {
                viewers[0]
                    .texts()
                    .iter()
                    .any(|text| text == r#"{"ready":true}"#)
            },
            "viewer input ready",
        );
        const REQUEST: &str = r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","epoch":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","data":"current-request"}"#;
        assert!(viewers[0].send_text(REQUEST));
        until(
            || publisher.peer.texts().iter().any(|text| text == REQUEST),
            "viewer request reaches publisher",
        );
        const REPLY: &str = r#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","epoch":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","data":"host-reply"}"#;
        assert!(publisher.peer.send_text(REPLY));
        until(
            || viewers[0].texts().iter().any(|text| text == REPLY),
            "host reply fans out",
        );
    }

    // ICE restart keeps the committed transport.
    let restart = f.media.restart_subscriber(&fences[1], SUBSCRIBER).unwrap();
    assert_eq!(restart.sdp_type, SdpType::Offer);
    let answer = viewers[1].answer(&restart, receiver_answer);
    f.media
        .apply_subscriber_answer(&fences[1], SUBSCRIBER, answer)
        .unwrap();
    let before = received(&viewers[1]).0;
    until(
        || received(&viewers[1]).0 >= before + 5,
        "media after restart",
    );

    // Live metadata updates keep physical media; identity changes are refused.
    let mut updated = media.clone();
    updated.formats.last_mut().unwrap().bitrate /= 2;
    updated.audio = false;
    f.media
        .update_publication(&fence(), PUBLISHER, &updated)
        .unwrap();
    updated.codec = "unsupported".into();
    assert!(
        f.media
            .update_publication(&fence(), PUBLISHER, &updated)
            .is_err()
    );
    f.media
        .update_publication(&fence(), PUBLISHER, &media)
        .unwrap();

    // Exact retirement: a stale identity changes nothing.
    f.media
        .close_subscription(&fences[0], "stale_connection")
        .unwrap();
    let before = received(&viewers[0]).0;
    until(
        || received(&viewers[0]).0 >= before + 3,
        "stale close kept media",
    );
    f.media.close_subscription(&fences[0], SUBSCRIBER).unwrap();
    assert!(
        f.media
            .add_subscriber_ice(&fences[0], SUBSCRIBER, IceCandidateInit::default())
            .is_err(),
        "retired media accepted signaling"
    );
    std::thread::sleep(Duration::from_millis(200));
    let stopped = received(&viewers[0]).0;
    let sibling = received(&viewers[1]).0;
    until(
        || received(&viewers[1]).0 >= sibling + 5,
        "retirement disturbed the sibling",
    );
    assert_eq!(
        received(&viewers[0]).0,
        stopped,
        "retired viewer kept media"
    );

    // Closing the room edge's admitted connection retires its children.
    f.media
        .close_subscription(&fences[1], &fences[1].connection_id)
        .unwrap();
    assert!(
        f.media.restart_subscriber(&fences[1], SUBSCRIBER).is_err(),
        "edge retirement kept its child connection"
    );

    f.media.close_publication(&fence()).unwrap();
    f.media.close_publication(&fence()).unwrap();
    assert!(!f.media.has_publication(&fence(), PUBLISHER));
    f.media.close().unwrap();
}

#[test]
fn one_publication_two_subscribers_vp8() {
    run_fanout("vp8", &[""]);
}

#[test]
fn one_publication_two_subscribers_vp8_simulcast() {
    run_fanout("vp8", &["q", "f"]);
}

#[test]
fn one_publication_two_subscribers_h264() {
    run_fanout("h264", &[""]);
}

#[test]
fn one_publication_two_subscribers_h264_simulcast() {
    run_fanout("h264", &["q", "f"]);
}

#[test]
fn retire_publication_detaches_only_the_exact_video_facet() {
    let f = fixture();
    let media = metadata("vp8", &[""]);
    f.media
        .prepare_publication(&fence(), PUBLISHER, &media)
        .unwrap();
    assert!(f.media.supports_retire_publication());
    assert!(
        f.media
            .retire_publication(&fence(), "stale_12345678")
            .is_none()
    );
    let close = f
        .media
        .retire_publication(&fence(), PUBLISHER)
        .expect("physical close");
    assert!(!f.media.has_publication(&fence(), PUBLISHER));
    // A replacement may be prepared before the old facet's close runs.
    f.media
        .prepare_publication(&fence(), "replacement_12345678", &media)
        .unwrap();
    close().unwrap();
    assert!(f.media.has_publication(&fence(), "replacement_12345678"));
    f.media.close().unwrap();
    assert!(
        f.media
            .prepare_publication(&fence(), PUBLISHER, &media)
            .is_err(),
        "closed media admitted a publication"
    );
}

#[test]
fn publisher_offer_must_match_its_declaration() {
    let f = fixture();
    f.media
        .prepare_publication(&fence(), PUBLISHER, &metadata("h264", &[""]))
        .unwrap();
    let peer = Peer::new(sdp::codec_config(&[sdp::VideoCodec::Vp8], true).set_rtp_mode(false));
    let offer = peer.with(|rtc| {
        let mut api = rtc.sdp_api();
        api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
        api.apply().unwrap().0.to_sdp_string()
    });
    let offer = SessionDescription {
        sdp_type: SdpType::Offer,
        sdp: offer,
    };
    assert!(
        f.media
            .accept_publisher_offer(&fence(), PUBLISHER, offer.clone())
            .is_err(),
        "a VP8 offer matched an H.264 declaration"
    );
    let answer = SessionDescription {
        sdp_type: SdpType::Answer,
        ..offer
    };
    assert!(
        f.media
            .accept_publisher_offer(&fence(), PUBLISHER, answer)
            .is_err()
    );
    f.media.close().unwrap();
}
