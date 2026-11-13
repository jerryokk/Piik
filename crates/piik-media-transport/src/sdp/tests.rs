use std::time::Instant;

use str0m::Rtc;
use str0m::change::SdpOffer;
use str0m::media::{Direction, MediaKind};

use super::*;

fn offer(video: &[(u8, &str, &str)], audio_fmtp: Option<&str>) -> String {
    let mut sdp = String::from(
        "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n\
         a=ice-ufrag:abcd\r\na=ice-pwd:aaaaaaaaaaaaaaaaaaaaaaaa\r\n\
         a=fingerprint:sha-256 00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00:00\r\n\
         a=setup:actpass\r\n",
    );
    sdp += if audio_fmtp.is_some() {
        "a=group:BUNDLE 0 1\r\n"
    } else {
        "a=group:BUNDLE 0\r\n"
    };
    let pts: Vec<String> = video
        .iter()
        .flat_map(|(pt, ..)| [pt.to_string(), (pt + 1).to_string()])
        .collect();
    sdp += &format!(
        "m=video 9 UDP/TLS/RTP/SAVPF {}\r\nc=IN IP4 0.0.0.0\r\na=mid:0\r\na=sendonly\r\na=rtcp-mux\r\n",
        pts.join(" ")
    );
    for (pt, name, fmtp) in video {
        sdp += &format!("a=rtpmap:{pt} {name}/90000\r\na=rtcp-fb:{pt} nack\r\n");
        if !fmtp.is_empty() {
            sdp += &format!("a=fmtp:{pt} {fmtp}\r\n");
        }
        sdp += &format!(
            "a=rtpmap:{} rtx/90000\r\na=fmtp:{} apt={pt}\r\n",
            pt + 1,
            pt + 1
        );
    }
    if let Some(fmtp) = audio_fmtp {
        sdp += "m=audio 9 UDP/TLS/RTP/SAVPF 111 0\r\nc=IN IP4 0.0.0.0\r\na=mid:1\r\na=sendrecv\r\na=rtcp-mux\r\n\
                a=rtpmap:111 opus/48000/2\r\na=rtpmap:0 PCMU/8000\r\n";
        if !fmtp.is_empty() {
            sdp += &format!("a=fmtp:111 {fmtp}\r\n");
        }
    }
    sdp
}

fn h264(profile: &str) -> String {
    format!("level-asymmetry-allowed=1;packetization-mode=1;profile-level-id={profile}")
}

/// Receiver negotiation of forwardable H.264, through str0m's answer.
#[test]
fn answer_selects_forwardable_h264() {
    let cases: &[(&[&str], Option<&str>)] = &[
        (&["42e01f", "vp8"], Some("42e01f")),
        (&["640c1f", "42e01f"], Some("42e01f")),
        (&["4d001f"], None),
        (&["64001f"], None),
        (&["42001f"], None),
        (&[""], None),
        (&["42c01f"], Some("42c01f")),
        (&["42c01f", "vp8"], Some("42c01f")),
        (&["42e00b"], Some("42e00b")),
        (&["42e033"], Some("42e033")),
        (&["42e034"], Some("42e034")),
        (&["42e03c"], None),
        (&["42e035"], None),
        (&["42e11f"], None),
        (&["mode0"], None),
        // Chrome's offer: both CB 3.1 entries tie in str0m; the first is kept.
        (&["42e01f", "42c01f", "640c1f", "vp8"], Some("42e01f")),
    ];
    for (formats, want) in cases {
        let video: Vec<(u8, &str, String)> = formats
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let pt = 100 + 2 * i as u8;
                match *f {
                    "vp8" => (pt, "VP8", String::new()),
                    "" => (
                        pt,
                        "H264",
                        "level-asymmetry-allowed=1;packetization-mode=1".into(),
                    ),
                    "mode0" => (
                        pt,
                        "H264",
                        "packetization-mode=0;profile-level-id=42e01f".into(),
                    ),
                    profile => (pt, "H264", h264(profile)),
                }
            })
            .collect();
        let refs: Vec<(u8, &str, &str)> =
            video.iter().map(|(p, n, f)| (*p, *n, f.as_str())).collect();
        let text = offer(&refs, None);
        let selected = select_offer(&text, &[VideoCodec::H264]);
        let Some(want) = want else {
            assert_eq!(selected, Err(SdpError::NoSupportedVideo), "{formats:?}");
            continue;
        };
        let (filtered, selection) = selected.unwrap();
        let (codec, pt) = selection.video.unwrap();
        assert_eq!(codec, VideoCodec::H264);
        assert!(
            filtered.contains(&format!("profile-level-id={want}")),
            "{formats:?}"
        );
        assert_eq!(
            filtered.matches("a=rtpmap:").count(),
            2,
            "primary and its RTX"
        );

        // str0m answers exactly that payload with RTX and Piik's own level.
        let mut rtc = codec_config(&[VideoCodec::H264], false).build(Instant::now());
        let answer = rtc
            .sdp_api()
            .accept_offer(SdpOffer::from_sdp_string(&filtered).unwrap())
            .unwrap()
            .to_sdp_string();
        assert!(
            answer.contains(&format!("a=rtpmap:{pt} H264/90000")),
            "{answer}"
        );
        assert!(
            answer.contains(&format!("a=fmtp:{} apt={pt}", pt + 1)),
            "{answer}"
        );
        assert!(answer.contains("profile-level-id=42e034"), "{answer}");
    }
}

#[test]
fn selection_respects_allowed_codecs_and_offer_order() {
    let video = [(100, "H264", h264("42e01f")), (102, "VP8", String::new())];
    let refs: Vec<(u8, &str, &str)> = video.iter().map(|(p, n, f)| (*p, *n, f.as_str())).collect();
    let text = offer(&refs, Some("minptime=10;useinbandfec=1"));
    let both = [VideoCodec::H264, VideoCodec::Vp8];
    let (_, selection) = select_offer(&text, &both).unwrap();
    assert_eq!(selection.video, Some((VideoCodec::H264, 100)));
    assert_eq!(selection.audio, Some(111));
    let (filtered, selection) = select_offer(&text, &[VideoCodec::Vp8]).unwrap();
    assert_eq!(selection.video, Some((VideoCodec::Vp8, 102)));
    assert!(!filtered.contains("H264"));
    assert!(!filtered.contains("PCMU"), "audio keeps Opus only");
    assert!(filtered.contains("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"));
    // A rejected video section is not a missing codec.
    let rejected = text.replace("m=video 9", "m=video 0");
    assert!(select_offer(&rejected, &[VideoCodec::Vp8]).is_ok());
    // Bundle-only zero-port video still offers media.
    let bundled = rejected.replace("a=mid:0\r\n", "a=mid:0\r\na=bundle-only\r\n");
    assert_eq!(
        select_offer(&bundled, &[]).map(|(_, s)| s),
        Err(SdpError::NoSupportedVideo)
    );
    assert_eq!(
        select_offer("garbage", &both).map(|_| ()),
        Err(SdpError::Invalid)
    );
}

/// Offer side: one entry per profile, so str0m's `accept_answer` cannot panic
/// on a browser answer matching several of them.
#[test]
fn offer_declares_one_entry_per_codec_and_connects_to_a_browser_answer() {
    let mut piik = codec_config(&[VideoCodec::H264, VideoCodec::Vp8, VideoCodec::H264], true)
        .build(Instant::now());
    let h264_entries = piik
        .codec_config()
        .iter()
        .filter(|p| p.spec().codec == Codec::H264)
        .count();
    assert_eq!(h264_entries, 1);
    let mut api = piik.sdp_api();
    api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = api.apply().unwrap();
    let text = offer.to_sdp_string();
    assert!(text.contains("a=rtpmap:102 H264/90000"));
    assert!(text.contains("a=fmtp:103 apt=102"));
    assert!(text.contains("a=rtpmap:96 VP8/90000"));
    assert!(text.contains("a=fmtp:97 apt=96"));
    assert!(text.contains("a=rtpmap:111 opus/48000/2"));
    assert!(text.contains("a=fmtp:111 minptime=10;useinbandfec=1"));

    // A browser-like peer with str0m's default codec table answers.
    let mut browser = Rtc::builder().build(Instant::now());
    let answer = browser
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&text).unwrap())
        .unwrap();
    piik.sdp_api().accept_answer(pending, answer).unwrap();
}

#[test]
fn opus_fmtp_rewrite_replaces_and_inserts() {
    let text = offer(
        &[(96, "VP8", "")],
        Some("minptime=10;useinbandfec=1;stereo=0"),
    );
    let rewritten =
        set_opus_fmtp(&text, &[("stereo", "1"), ("maxaveragebitrate", "510000")]).unwrap();
    assert!(
        rewritten.contains(
            "a=fmtp:111 minptime=10;useinbandfec=1;stereo=1;maxaveragebitrate=510000\r\n"
        )
    );
    let bare = offer(&[(96, "VP8", "")], Some(""));
    let rewritten = set_opus_fmtp(&bare, &[("maxaveragebitrate", "192000")]).unwrap();
    assert!(
        rewritten.contains("a=rtpmap:111 opus/48000/2\r\na=fmtp:111 maxaveragebitrate=192000\r\n")
    );
    // Video untouched.
    assert!(rewritten.contains("a=rtpmap:96 VP8/90000"));
}

/// The rewritten text is what the remote sees; Piik reads the remote's
/// preference back with `remote_opus` because str0m keeps neither key.
#[test]
fn opus_preferences_round_trip_through_str0m() {
    let mut a = codec_config(&[VideoCodec::Vp8], true).build(Instant::now());
    let mut b = codec_config(&[VideoCodec::Vp8], true).build(Instant::now());
    let mut api = a.sdp_api();
    api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
    let (offer, pending) = api.apply().unwrap();
    let offer = set_opus_fmtp(
        &offer.to_sdp_string(),
        &[("stereo", "1"), ("maxaveragebitrate", "510000")],
    )
    .unwrap();
    assert_eq!(
        remote_opus(&offer).unwrap(),
        Some(RemoteOpus {
            pt: 111,
            stereo: true,
            max_average_bitrate: Some(510_000)
        })
    );
    let answer = b
        .sdp_api()
        .accept_offer(SdpOffer::from_sdp_string(&offer).unwrap())
        .unwrap()
        .to_sdp_string();
    // str0m drops both keys from its own answer.
    let plain = remote_opus(&answer).unwrap().unwrap();
    assert!(!plain.stereo && plain.max_average_bitrate.is_none());
    let answer = set_opus_fmtp(&answer, &[("maxaveragebitrate", "192000")]).unwrap();
    a.sdp_api()
        .accept_answer(
            pending,
            str0m::change::SdpAnswer::from_sdp_string(&answer).unwrap(),
        )
        .unwrap();
    assert_eq!(
        remote_opus(&answer).unwrap().unwrap().max_average_bitrate,
        Some(192_000)
    );
}

#[test]
fn remote_opus_ignores_invalid_limits_and_rejected_sections() {
    for (fmtp, stereo, limit) in [
        ("stereo=1", true, None),
        ("maxaveragebitrate=invalid", false, None),
        ("maxaveragebitrate=510001", false, None),
        (
            "maxaveragebitrate=510000;maxaveragebitrate=96000",
            false,
            Some(96_000),
        ),
        (" stereo = 1 ; maxaveragebitrate = 6000", true, Some(6_000)),
    ] {
        let text = offer(&[(96, "VP8", "")], Some(fmtp));
        let opus = remote_opus(&text).unwrap().unwrap();
        assert_eq!(
            (opus.stereo, opus.max_average_bitrate),
            (stereo, limit),
            "{fmtp}"
        );
        let rejected = text.replace("m=audio 9", "m=audio 0");
        assert_eq!(remote_opus(&rejected).unwrap(), None);
    }
}
