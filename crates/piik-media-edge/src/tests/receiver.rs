// Receivers and their overlap. The upstream peer is a Piik edge (the real
// Native upstream) or a fixed SDP text where only offer parsing matters.

use crate::receiver::offer_sends_codec;

use super::*;

/// A minimal offer for receiver admission: one video section per
/// `(payload, codec, fmtp)` plus optional Opus.
fn offer(video: &[(u8, &str, &str)], opus_fmtp: Option<&str>) -> String {
    let bundle = if opus_fmtp.is_some() { "0 1" } else { "0" };
    let mut sdp = format!(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE {bundle}\r\n\
         a=fingerprint:sha-256 \
         00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF\r\n",
    );
    let formats: Vec<String> = video.iter().map(|(pt, _, _)| pt.to_string()).collect();
    sdp += &format!(
        "m=video 9 UDP/TLS/RTP/SAVPF {}\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\na=sendonly\r\n\
         a=ice-ufrag:upst\r\na=ice-pwd:upstreampasswordupstream\r\na=setup:actpass\r\na=rtcp-mux\r\n\
         a=msid:upstream screen\r\na=ssrc:42 cname:upstream\r\n",
        formats.join(" ")
    );
    for (pt, codec, fmtp) in video {
        sdp += &format!("a=rtpmap:{pt} {codec}/90000\r\n");
        if !fmtp.is_empty() {
            sdp += &format!("a=fmtp:{pt} {fmtp}\r\n");
        }
    }
    if let Some(fmtp) = opus_fmtp {
        sdp += "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\nc=IN IP4 0.0.0.0\r\na=mid:1\r\na=sendonly\r\n\
                a=ice-ufrag:upst\r\na=ice-pwd:upstreampasswordupstream\r\na=setup:actpass\r\na=rtcp-mux\r\n\
                a=msid:upstream audio\r\na=ssrc:43 cname:upstream\r\na=rtpmap:111 opus/48000/2\r\n";
        sdp += &format!("a=fmtp:111 {fmtp}\r\n");
    }
    sdp
}

#[test]
fn offer_sends_codec_distinguishes_bundled_and_rejected_media() {
    for (name, port, attributes, want) in [
        ("ordinary", 9, "a=sendonly\r\n", true),
        ("bundled", 0, "a=bundle-only\r\na=sendonly\r\n", true),
        ("rejected", 0, "a=sendonly\r\n", false),
        (
            "bundled-inactive",
            0,
            "a=bundle-only\r\na=inactive\r\n",
            false,
        ),
        (
            "bundled-receive-only",
            0,
            "a=bundle-only\r\na=recvonly\r\n",
            false,
        ),
    ] {
        let offer = format!(
            "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\na=group:BUNDLE 0 1\r\n\
             m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\na=rtpmap:96 VP8/90000\r\n\
             m=audio {port} UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=rtpmap:111 opus/48000/2\r\n{attributes}"
        );
        assert_eq!(
            offer_sends_codec(&offer, "audio", "opus").unwrap(),
            want,
            "{name}"
        );
    }
    assert!(offer_sends_codec("garbage", "audio", "opus").is_err());
}

#[tokio::test]
async fn receiver_advertises_local_stereo_preference() {
    for remote in ["", ";stereo=0", ";stereo=1"] {
        let engine = loopback_engine();
        let fmtp = format!("minptime=10;useinbandfec=1{remote}");
        let offer = offer(&[(96, "VP8", "")], Some(&fmtp));
        let (receiver, answer) = engine
            .new_receiver(ReceiverOptions {
                offer: offer.clone(),
                edge_capacity: 1,
                ..ReceiverOptions::default()
            })
            .await
            .unwrap();
        for (attempt, answer) in [
            answer,
            receiver
                .renegotiate(&offer, Vec::new())
                .await
                .unwrap()
                .expect("same media shape"),
        ]
        .into_iter()
        .enumerate()
        {
            let audio = answer.split("m=audio").nth(1).expect("audio section");
            assert!(
                audio.contains("stereo=1") && audio.contains("maxaveragebitrate=510000"),
                "answer {attempt} lost local stereo preference: {audio}"
            );
            assert!(!answer.contains("a=end-of-candidates"));
        }
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn receiver_retains_repair_for_only_the_selected_codec() {
    for (name, primary, rtx) in [("vp8", "VP8", 97u8), ("h264", "H264", 103)] {
        let engine = loopback_engine();
        let fmtp = if name == "h264" {
            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
        } else {
            ""
        };
        let pt = rtx - 1;
        let other = if name == "vp8" {
            (102, "H264", "packetization-mode=1;profile-level-id=64001f")
        } else {
            (96, "VP8", "")
        };
        let mut offer = offer(&[(pt, primary, fmtp), (rtx, "rtx", ""), other], None);
        offer = offer.replace(
            &format!("a=rtpmap:{rtx} rtx/90000\r\n"),
            &format!("a=rtpmap:{rtx} rtx/90000\r\na=fmtp:{rtx} apt={pt}\r\n"),
        );
        let (receiver, answer) = engine
            .new_receiver(ReceiverOptions {
                offer: offer.clone(),
                edge_capacity: 1,
                ..ReceiverOptions::default()
            })
            .await
            .unwrap();
        assert_eq!(receiver.codec(), name);
        let restarted = offer
            .replace("a=ice-ufrag:upst", "a=ice-ufrag:rest")
            .replace("upstreampasswordupstream", "restartedpasswordrestart");
        let renegotiated = receiver
            .renegotiate(&restarted, Vec::new())
            .await
            .unwrap()
            .expect("restart reused the receiver");
        for answer in [answer, renegotiated] {
            let video = answer
                .split("m=video")
                .nth(1)
                .unwrap()
                .split("m=")
                .next()
                .unwrap();
            let formats: Vec<&str> = video
                .lines()
                .next()
                .unwrap()
                .split_whitespace()
                .skip(2)
                .collect();
            assert_eq!(
                formats,
                vec![pt.to_string(), rtx.to_string()],
                "one {name} primary and its repair: {video}"
            );
            assert!(
                video.contains(&format!("apt={pt}")),
                "repair for a different primary"
            );
        }
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn receiver_codec_matches_the_single_negotiated_answer() {
    let h264 = (
        102,
        "H264",
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034",
    );
    let vp8 = (96, "VP8", "");
    for (preferences, want) in [([h264, vp8], "h264"), ([vp8, h264], "vp8")] {
        let engine = loopback_engine();
        let (receiver, answer) = engine
            .new_receiver(ReceiverOptions {
                offer: offer(&preferences, None),
                edge_capacity: 1,
                ..ReceiverOptions::default()
            })
            .await
            .unwrap();
        let lower = answer.to_ascii_lowercase();
        assert_eq!(receiver.codec(), want);
        assert!(lower.contains(&format!("{want}/90000")));
        assert!(
            !(lower.contains("vp8/90000") && lower.contains("h264/90000")),
            "{answer}"
        );
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn receiver_negotiates_forwardable_h264() {
    for (name, formats, want) in [
        ("browser-auto", vec!["42e01f", "vp8"], "42e01f"),
        ("safari-order", vec!["640c1f", "42e01f"], "42e01f"),
        ("main", vec!["4d001f"], ""),
        ("high", vec!["64001f"], ""),
        ("baseline", vec!["42001f"], ""),
        ("missing-profile", vec![""], ""),
        ("equivalent-baseline", vec!["42c01f"], "42c01f"),
        (
            "equivalent-baseline-with-vp8",
            vec!["42c01f", "vp8"],
            "42c01f",
        ),
        ("small-browser-output", vec!["42e00b"], "42e00b"),
        ("level-5.1", vec!["42e033"], "42e033"),
        ("level-ceiling", vec!["42e034"], "42e034"),
        ("over-ceiling", vec!["42e03c"], ""),
        ("invalid-level", vec!["42e035"], ""),
        ("invalid-constraints", vec!["42e11f"], ""),
        ("mode-zero", vec!["42e01f;packetization-mode=0"], ""),
    ] {
        let fmtps: Vec<String> = formats
            .iter()
            .map(|format| {
                let mut fmtp = String::from("level-asymmetry-allowed=1;packetization-mode=1");
                if !format.is_empty() {
                    fmtp += &format!(";profile-level-id={format}");
                }
                fmtp
            })
            .collect();
        let video: Vec<(u8, &str, &str)> = formats
            .iter()
            .enumerate()
            .map(|(index, format)| {
                let pt = 96 + index as u8;
                if *format == "vp8" {
                    (pt, "VP8", "")
                } else {
                    (pt, "H264", fmtps[index].as_str())
                }
            })
            .collect();
        let offer = offer(&video, None);
        let engine = loopback_engine();
        let result = engine
            .new_receiver(ReceiverOptions {
                offer: offer.clone(),
                edge_capacity: 1,
                ..ReceiverOptions::default()
            })
            .await;
        if want.is_empty() {
            assert!(result.is_err(), "{name}: unsupported format was admitted");
            engine.close().await.unwrap();
            continue;
        }
        let (receiver, answer) = result.unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(receiver.codec(), "h264", "{name}");
        // str0m answers with its own Level 5.2 ceiling (level asymmetry is
        // allowed); the selected offer payload identifies the profile.
        let pt = 96 + formats.iter().position(|format| *format == want).unwrap();
        assert!(
            answer
                .lines()
                .any(|line| line == format!("m=video 9 UDP/TLS/RTP/SAVPF {pt}")),
            "{name}: {answer}"
        );
        // A profile-only change is a changed media shape: rejected before it
        // mutates the current connection or source.
        let changed = offer.replace(
            &format!("profile-level-id={want}"),
            "profile-level-id=4d001f",
        );
        assert_eq!(
            receiver.renegotiate(&changed, Vec::new()).await.unwrap(),
            None,
            "{name}"
        );
        let source = receiver.source().clone();
        assert!(
            receiver
                .renegotiate(&offer, Vec::new())
                .await
                .unwrap()
                .is_some(),
            "{name}"
        );
        assert_eq!(receiver.source(), &source);
        engine.close().await.unwrap();
    }
}

#[tokio::test]
async fn receiver_forwards_encoded_video_to_a_native_edge() {
    let host = loopback_engine();
    let source = host.new_source("vp8", 1, 1, None).await.unwrap();
    // Host -> relay Viewer, then relay Viewer -> downstream Viewer.
    let upstream = Peer::connect(&host, &source, "upstream").await;
    let relay_source = upstream.receiver.source().clone();
    let downstream = Peer::connect(&upstream.viewer, &relay_source, "native-relay-edge").await;
    write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(1))
        .await
        .unwrap();
    let received = downstream.wait_packets(1).await;
    let descriptor = piik_media_forwarding::Vp8Descriptor::parse(&received[0].1).unwrap();
    assert_eq!(
        &received[0].1[descriptor.header_len..],
        VP8_KEY_8X8,
        "relayed payload changed"
    );
    // A downstream PLI travels to the relay source, whose recovery owner is
    // the upstream connection.
    downstream.receiver.request_key_frame().await;
    write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(2))
        .await
        .unwrap();
    downstream.wait_packets(2).await;
    assert_eq!(downstream.edge.state(), PcState::Connected);
    downstream.close().await;
    upstream.close().await;
    host.close().await.unwrap();
}

#[tokio::test]
async fn receiver_gathering_survey_and_restart_generations() {
    // Every candidate carries the answer's ufrag; ordinary renegotiation
    // keeps the gathering; an ICE restart starts one new survey and retires
    // the old generation.
    let stun = piik_stun::listen(&["127.0.0.1:0"]).await.unwrap();
    let servers = vec![vec![format!("stun:{}", stun.addresses()[0])]];
    let engine = loopback_engine();
    let offer = offer(&[(96, "VP8", "")], Some("minptime=10;useinbandfec=1"));
    let mut events = events();
    let (receiver, answer) = engine
        .new_receiver(ReceiverOptions {
            offer: offer.clone(),
            ice_servers: servers.clone(),
            edge_capacity: 1,
            events: Some(events.sink.clone()),
            ..ReceiverOptions::default()
        })
        .await
        .unwrap();
    assert!(receiver.has_audio());
    let ufrag = |sdp: &str| {
        sdp.lines()
            .find_map(|line| line.strip_prefix("a=ice-ufrag:"))
            .unwrap()
            .to_owned()
    };
    let gather = async |events: &mut Events, ufrag: &str| {
        let mut survey = 0;
        let mut seen = 0;
        let end = loop {
            let event = wait_event(&mut events.receiver, Duration::from_secs(6), |event| {
                matches!(event, EdgeEvent::LocalCandidate { .. })
            })
            .await
            .expect("gathering did not complete");
            let EdgeEvent::LocalCandidate { candidate, current } = event else {
                unreachable!()
            };
            let Some(candidate) = candidate else {
                break current;
            };
            seen += 1;
            assert_eq!(
                candidate.username_fragment.as_deref(),
                Some(ufrag),
                "lost its SDP generation"
            );
            survey += usize::from(candidate.candidate.starts_with("candidate:ns"));
        };
        assert!(seen > 0, "gathering produced no candidates");
        (survey, end)
    };
    let (survey, first) = gather(&mut events, &ufrag(&answer)).await;
    assert_eq!(survey, 1, "one survey candidate per gathering");
    assert!(first(), "current gathering lost delivery authority");

    let ordinary = receiver
        .renegotiate(&offer, servers.clone())
        .await
        .unwrap()
        .unwrap();
    assert!(!ordinary.contains("a=end-of-candidates"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        drain(&mut events.receiver)
            .iter()
            .all(|event| !matches!(event, EdgeEvent::LocalCandidate { .. })),
        "ordinary renegotiation restarted gathering"
    );
    assert!(first(), "ordinary renegotiation retired the gathering");

    let restarted = offer
        .replace("a=ice-ufrag:upst", "a=ice-ufrag:rest")
        .replace("upstreampasswordupstream", "restartedpasswordrestart");
    let answer = receiver
        .renegotiate(&restarted, servers)
        .await
        .unwrap()
        .unwrap();
    let (survey, second) = gather(&mut events, &ufrag(&answer)).await;
    assert_eq!(survey, 1, "restart reused a completed STUN observation");
    assert!(
        !first() && second(),
        "queued candidates did not keep their gathering owner"
    );
    receiver.close().await;
    assert!(!second(), "retired receiver still authorizes queued events");
    engine.close().await.unwrap();
    stun.close().await;
}

#[tokio::test]
async fn native_receiver_overlap_keeps_current_media() {
    for outcome in ["rollback", "commit"] {
        let host = loopback_engine();
        let source = host.new_source("vp8", 2, 1, None).await.unwrap();
        let current = Peer::connect(&host, &source, "current").await;
        write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(1))
            .await
            .unwrap();
        current.wait_packets(1).await;
        let candidate = Peer::connect(&host, &source, "candidate").await;
        write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(2))
            .await
            .unwrap();
        current.wait_packets(2).await;
        candidate.wait_packets(1).await;
        let (retired, surviving) = if outcome == "commit" {
            (current, candidate)
        } else {
            (candidate, current)
        };
        let before = surviving.received().await.len();
        retired.receiver.close().await;
        retired.edge.close().await;
        write_source_frame(&source, VP8_KEY_8X8, Duration::from_secs(3))
            .await
            .unwrap();
        surviving.wait_packets(before + 1).await;
        assert_eq!(
            surviving.receiver.state(),
            PcState::Connected,
            "{outcome}: surviving receiver closed"
        );
        retired.close().await;
        surviving.close().await;
        host.close().await.unwrap();
    }
}
