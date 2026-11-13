//! C0 spikes: each test pins the actual str0m 0.25.0 behavior behind one
//! premise of `docs/architecture/media.md`. The forwarding side always
//! runs in RTP mode.

use std::net::SocketAddr;
use std::time::Duration;

use piik_media_testkit::netem::{Bitrate, DataSize, NetemConfig};
use piik_media_testkit::str0m;
use piik_media_testkit::{Network, PeerId, WireRtp, connect_direct, negotiate, negotiate_with};
use str0m::bwe::BweKind;
use str0m::format::{Codec, FormatParams};
use str0m::media::{
    Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Rid, Simulcast,
    SimulcastLayer,
};
use str0m::rtp::rtcp::Rtcp;
use str0m::rtp::{RawPacket, RtpWrite, SeqNo, Ssrc};
use str0m::{Event, Rtc, RtcConfig};

fn addr(s: &str) -> SocketAddr {
    s.parse().unwrap()
}

fn rtp_mode() -> RtcConfig {
    Rtc::builder().set_rtp_mode(true).enable_raw_packets(true)
}

fn connected(net: &Network, peers: &[PeerId]) -> bool {
    peers.iter().all(|p| net.peer(*p).rtc.is_connected())
}

fn connect(net: &mut Network, peers: &[PeerId]) {
    let ok = net
        .run_until(|n| connected(n, peers), Duration::from_secs(5))
        .unwrap();
    assert!(ok, "peers must connect within 5 s of virtual time");
}

fn vp8_pt(net: &Network, peer: PeerId) -> str0m::format::PayloadParams {
    net.peer(peer)
        .rtc
        .codec_config()
        .iter()
        .find(|p| p.spec().codec == Codec::Vp8)
        .cloned()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Spikes 1 and 2: RTX and NACK in RTP mode.
// ---------------------------------------------------------------------------

const MAIN: u32 = 42;
const RTX: u32 = 44;

struct RtxPair {
    net: Network,
    tx: PeerId,
    rx: PeerId,
    pt: str0m::media::Pt,
}

fn rtx_pair() -> RtxPair {
    let mut net = Network::new();
    let tx = net.add_peer(rtp_mode(), addr("10.0.0.1:1000"));
    let rx = net.add_peer(rtp_mode(), addr("10.0.0.2:2000"));
    connect_direct(&mut net, tx, rx);
    connect(&mut net, &[tx, rx]);

    let mid: Mid = "vid".into();
    {
        let mut api = net.peer_mut(tx).rtc.direct_api();
        api.declare_media(mid, MediaKind::Video);
        api.declare_stream_tx(MAIN.into(), Some(RTX.into()), mid, None);
    }
    {
        let mut api = net.peer_mut(rx).rtc.direct_api();
        api.declare_media(mid, MediaKind::Video);
        api.expect_stream_rx(MAIN.into(), Some(RTX.into()), mid, None);
    }
    let pt = vp8_pt(&net, tx).pt();
    RtxPair { net, tx, rx, pt }
}

/// Writes `seqs` 20 ms apart. `nackable` decides per sequence number.
fn write_seqs(pair: &mut RtxPair, seqs: std::ops::Range<u64>, nackable: impl Fn(u64) -> bool) {
    for seq in seqs {
        let now = pair.net.now();
        pair.net
            .peer_mut(pair.tx)
            .rtc
            .direct_api()
            .stream_tx(&MAIN.into())
            .unwrap()
            .write_rtp(
                RtpWrite::new(
                    pair.pt,
                    seq.into(),
                    seq as u32 * 3000,
                    now,
                    [0x10, 0, 0, seq as u8],
                )
                .marker(true)
                .nackable(nackable(seq)),
            );
        pair.net.run_for(Duration::from_millis(20)).unwrap();
    }
}

/// Drop the first wire transmission of each listed sequence number on the
/// main SSRC. RTX repairs use another SSRC and pass.
fn drop_once(net: &mut Network, seqs: &'static [u16]) {
    let mut dropped = Vec::new();
    net.set_drop_filter(Some(Box::new(move |_, _, bytes| {
        let Some(rtp) = WireRtp::parse(bytes) else {
            return false;
        };
        let hit = rtp.ssrc == MAIN && seqs.contains(&rtp.sequence_number);
        if hit && !dropped.contains(&rtp.sequence_number) {
            dropped.push(rtp.sequence_number);
            return true;
        }
        false
    })));
}

fn nacked(events: &[(std::time::Instant, Event)], tx: bool, seq: u16) -> usize {
    events
        .iter()
        .filter(|(_, e)| match e.as_raw_packet() {
            Some(RawPacket::RtcpTx(Rtcp::Nack(n))) if tx => n.reports.iter().any(|r| r.pid == seq),
            Some(RawPacket::RtcpRx(Rtcp::Nack(n))) if !tx => n.reports.iter().any(|r| r.pid == seq),
            _ => false,
        })
        .count()
}

fn rtx_sent_for(events: &[(std::time::Instant, Event)], seq: u16) -> usize {
    events
        .iter()
        .filter(|(_, e)| match e.as_raw_packet() {
            // RtpTx carries the whole serialized packet; the RTX original
            // sequence number follows the header.
            Some(RawPacket::RtpTx(h, packet)) => {
                *h.ssrc == RTX
                    && packet.get(h.header_len..h.header_len + 2) == Some(&seq.to_be_bytes()[..])
            }
            _ => false,
        })
        .count()
}

fn received(events: &[(std::time::Instant, Event)]) -> Vec<(u32, u16, SeqNo, u8)> {
    events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(p) => Some((
                *p.header.ssrc,
                p.header.sequence_number,
                p.seq_no,
                *p.header.payload_type,
            )),
            _ => None,
        })
        .collect()
}

/// Premise 1: in RTP mode the receiver NACKs a hole and the RTX repair is
/// surfaced as the original packet (main SSRC, original seq and PT).
#[test]
fn spike1_rtp_mode_nack_and_rtx_unwrap() {
    let mut pair = rtx_pair();
    write_seqs(&mut pair, 47_000..47_020, |_| true);
    drop_once(&mut pair.net, &[47_025]);
    write_seqs(&mut pair, 47_020..47_040, |_| true);
    pair.net.run_for(Duration::from_millis(500)).unwrap();

    let rx = &pair.net.peer(pair.rx).events;
    let tx = &pair.net.peer(pair.tx).events;
    assert!(nacked(rx, true, 47_025) >= 1, "receiver NACKs the hole");
    assert!(nacked(tx, false, 47_025) >= 1, "sender receives the NACK");
    assert_eq!(rtx_sent_for(tx, 47_025), 1, "one RTX repair is sent");

    let packets = received(rx);
    let repaired: Vec<_> = packets.iter().filter(|p| p.1 == 47_025).collect();
    assert_eq!(repaired.len(), 1, "repair surfaces exactly once");
    let (ssrc, _, seq_no, pt) = *repaired[0];
    assert_eq!(ssrc, MAIN, "repair carries the main SSRC");
    assert_eq!(*seq_no, 47_025, "extended seq is the original");
    assert_eq!(pt, *pair.pt, "repair carries the media PT");
    assert!(packets.iter().all(|p| p.0 == MAIN), "no RTX SSRC leaks");
    assert_eq!(packets.len(), 40, "every packet arrives once");
}

/// Premise 2: the sender answers NACKs from its RTX cache only for packets
/// written with `nackable(true)`; the receiver NACKs regardless.
#[test]
fn spike2_nack_response_requires_nackable_write() {
    let mut pair = rtx_pair();
    write_seqs(&mut pair, 47_000..47_020, |_| true);
    drop_once(&mut pair.net, &[47_023, 47_027]);
    write_seqs(&mut pair, 47_020..47_040, |seq| seq != 47_027);
    pair.net.run_for(Duration::from_secs(2)).unwrap();

    let rx = &pair.net.peer(pair.rx).events;
    let tx = &pair.net.peer(pair.tx).events;
    assert_eq!(rtx_sent_for(tx, 47_023), 1, "nackable packet repaired");
    assert!(nacked(rx, true, 47_027) >= 2, "receiver keeps NACKing");
    assert!(nacked(tx, false, 47_027) >= 2, "sender sees those NACKs");
    assert_eq!(rtx_sent_for(tx, 47_027), 0, "non-nackable is never resent");

    let seqs: Vec<u16> = received(rx).iter().map(|p| p.1).collect();
    assert!(seqs.contains(&47_023));
    assert!(!seqs.contains(&47_027));
}

// ---------------------------------------------------------------------------
// Spike 3: Opus fmtp.
// ---------------------------------------------------------------------------

fn fmtp_line(sdp: &str, pt: u8) -> Option<&str> {
    let prefix = format!("a=fmtp:{pt} ");
    sdp.split("\r\n")
        .find_map(|l| l.strip_prefix(prefix.as_str()))
}

fn opus_pt(sdp: &str) -> u8 {
    sdp.split("\r\n")
        .find_map(|l| {
            let rest = l.strip_prefix("a=rtpmap:")?;
            let (pt, codec) = rest.split_once(' ')?;
            codec
                .to_ascii_lowercase()
                .starts_with("opus/")
                .then(|| pt.parse().unwrap())
        })
        .expect("opus rtpmap")
}

/// Append `extra` to the fmtp line of the Opus PT, as Piik's SDP rewrite does.
fn add_opus_fmtp(sdp: String, extra: &str) -> String {
    let pt = opus_pt(&sdp);
    let prefix = format!("a=fmtp:{pt} ");
    sdp.split("\r\n")
        .map(|l| {
            if l.starts_with(&prefix) {
                format!("{l};{extra}")
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn stereo_opus() -> RtcConfig {
    let mut config = Rtc::builder().clear_codecs();
    config.codec_config().add_config(
        111.into(),
        None,
        Codec::Opus,
        Frequency::FORTY_EIGHT_KHZ,
        Some(2),
        FormatParams {
            min_p_time: Some(10),
            use_inband_fec: Some(true),
            stereo: Some(true),
            ..Default::default()
        },
    );
    config
}

/// Premise: str0m keeps `stereo` but drops `maxaveragebitrate`, and accepts
/// SDP text rewritten to carry it. `stereo` is our own param only: a remote
/// `stereo=1` is not adopted into the negotiated config.
#[test]
fn spike3_opus_fmtp_rewrite() {
    let mut net = Network::new();
    let a = net.add_peer(stereo_opus(), addr("10.0.0.1:1000"));
    let b = net.add_peer(Rtc::builder().set_rtp_mode(true), addr("10.0.0.2:2000"));

    // Local SDP from str0m: stereo survives, maxaveragebitrate cannot be set.
    let negotiated = negotiate_with(
        &mut net,
        a,
        b,
        |api| api.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None),
        |offer| {
            assert_eq!(
                fmtp_line(&offer, 111),
                Some("minptime=10;stereo=1;useinbandfec=1")
            );
            add_opus_fmtp(offer, "maxaveragebitrate=510000")
        },
        |answer| {
            let pt = opus_pt(&answer);
            let fmtp = fmtp_line(&answer, pt).unwrap().to_string();
            // The remote parse dropped maxaveragebitrate and did not adopt
            // stereo: the answer is the answerer's own default params.
            assert!(!fmtp.contains("maxaveragebitrate"), "{fmtp}");
            assert!(!fmtp.contains("stereo"), "{fmtp}");
            add_opus_fmtp(answer, "stereo=1;maxaveragebitrate=192000")
        },
    )
    .unwrap();
    assert!(negotiated.offer.contains("maxaveragebitrate=510000"));
    assert!(negotiated.answer.contains("maxaveragebitrate=192000"));

    // The answerer's negotiated Opus params carry no remote stereo.
    let b_opus = net
        .peer(b)
        .rtc
        .codec_config()
        .iter()
        .find(|p| p.spec().codec == Codec::Opus)
        .unwrap()
        .spec()
        .format;
    assert_eq!(b_opus.stereo, None);
    let a_opus = net
        .peer(a)
        .rtc
        .codec_config()
        .iter()
        .find(|p| p.spec().codec == Codec::Opus)
        .unwrap()
        .spec()
        .format;
    assert_eq!(a_opus.stereo, Some(true));

    // Rewritten SDP still connects.
    connect(&mut net, &[a, b]);
}

// ---------------------------------------------------------------------------
// Spike 4: H.264 profile matching.
// ---------------------------------------------------------------------------

struct H264Offer {
    net: Network,
    browser: PeerId,
    pending: str0m::change::SdpPendingOffer,
    answer: str0m::change::SdpAnswer,
}

/// A browser-like offerer sends H.264 entries in `profiles` order (mode 1)
/// to a Piik answerer whose only H.264 entry is 42e034.
fn offer_h264(profiles: &[(u8, u32)]) -> H264Offer {
    let mut offer_config = Rtc::builder().clear_codecs();
    for (pt, profile) in profiles {
        offer_config
            .codec_config()
            .add_h264((*pt).into(), Some((*pt + 1).into()), true, *profile);
    }
    let mut piik = Rtc::builder().set_rtp_mode(true).clear_codecs();
    piik.codec_config()
        .add_h264(96.into(), Some(97.into()), true, 0x42e034);

    let mut net = Network::new();
    let browser = net.add_peer(offer_config, addr("10.0.0.1:1000"));
    let sfu = net.add_peer(piik, addr("10.0.0.2:2000"));
    let mut api = net.peer_mut(browser).rtc.sdp_api();
    api.add_media(MediaKind::Video, Direction::SendOnly, None, None, None);
    let (offer, pending) = api.apply().unwrap();
    let answer = net.peer_mut(sfu).rtc.sdp_api().accept_offer(offer).unwrap();
    H264Offer {
        net,
        browser,
        pending,
        answer,
    }
}

/// The answered H.264 `(pt, fmtp)`, if any.
fn answer_h264(profiles: &[(u8, u32)]) -> Option<(u8, String)> {
    let answer = offer_h264(profiles).answer.to_sdp_string();
    let pt: u8 = answer.split("\r\n").find_map(|l| {
        let rest = l.strip_prefix("a=rtpmap:")?;
        let (pt, codec) = rest.split_once(' ')?;
        codec.starts_with("H264/").then(|| pt.parse().unwrap())
    })?;
    Some((pt, fmtp_line(&answer, pt).unwrap().to_string()))
}

const H264_42E01F: (u8, u32) = (102, 0x42e01f);
const H264_42C01F: (u8, u32) = (104, 0x42c01f);
const H264_640C1F: (u8, u32) = (106, 0x640c1f);
const H264_42E034: (u8, u32) = (108, 0x42e034);

/// Premise: str0m picks the Constrained Baseline PT closest in level. An
/// exact level wins; level ties go to the LAST tied entry in offer order
/// (`max_by_key`), and the answer echoes Piik's own level, not the offer's.
#[test]
fn spike4_h264_profile_match() {
    let chrome = [H264_42E01F, H264_42C01F, H264_640C1F, H264_42E034];
    let (pt, fmtp) = answer_h264(&chrome).unwrap();
    assert_eq!(pt, 108, "exact 42e034 wins");
    assert_eq!(
        fmtp,
        "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e034"
    );

    // Without 42e034, 42e01f and 42c01f tie (both CB level 3.1).
    let (pt, fmtp) = answer_h264(&[H264_42E01F, H264_42C01F, H264_640C1F]).unwrap();
    assert_eq!(pt, 104, "last tied entry wins");
    assert!(fmtp.ends_with("profile-level-id=42e034"), "{fmtp}");
    let (pt, _) = answer_h264(&[H264_42C01F, H264_42E01F]).unwrap();
    assert_eq!(pt, 102, "order decides the tie");

    // Constrained High alone has no compatible profile: no H.264 answered.
    assert_eq!(answer_h264(&[H264_640C1F]), None);
}

/// Finding: a str0m OFFERER with several entries of one H.264 profile panics
/// ("Pt locked multiple times") when the answer's single PT matches more than
/// one of them. Piik as offerer must advertise one entry per profile.
#[test]
fn spike4_str0m_offerer_panics_on_duplicate_profile_answer() {
    let H264Offer {
        mut net,
        browser,
        pending,
        answer,
    } = offer_h264(&[H264_42E01F, H264_42C01F, H264_42E034]);
    let accept = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        net.peer_mut(browser)
            .rtc
            .sdp_api()
            .accept_answer(pending, answer)
    }));
    assert!(
        accept.is_err(),
        "str0m 0.25.0 panics; remove once fixed upstream"
    );

    // One entry per profile is accepted.
    let H264Offer {
        mut net,
        browser,
        pending,
        answer,
    } = offer_h264(&[H264_42E034, H264_640C1F]);
    net.peer_mut(browser)
        .rtc
        .sdp_api()
        .accept_answer(pending, answer)
        .unwrap();
}

// ---------------------------------------------------------------------------
// Spikes 5 and 6: ICE-lite, peer-reflexive and shared-address demux.
// ---------------------------------------------------------------------------

fn strip_candidates(sdp: String) -> String {
    sdp.split("\r\n")
        .filter(|l| !l.starts_with("a=candidate:") && !l.starts_with("a=end-of-candidates"))
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn channel_text(net: &Network, peer: PeerId) -> Vec<String> {
    net.peer(peer)
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::ChannelData(d) => Some(String::from_utf8(d.data.clone()).unwrap()),
            _ => None,
        })
        .collect()
}

fn open_channel(net: &Network, peer: PeerId) -> Option<str0m::channel::ChannelId> {
    net.peer(peer).events.iter().find_map(|(_, e)| match e {
        Event::ChannelOpen(id, _) => Some(*id),
        _ => None,
    })
}

fn send_text(net: &mut Network, peer: PeerId, text: &str) {
    let id = open_channel(net, peer).expect("channel open");
    net.peer_mut(peer)
        .rtc
        .channel(id)
        .unwrap()
        .write(false, text.as_bytes())
        .unwrap();
}

/// Client behind a 1:1 NAT offers with no candidates at all; the ICE-lite SFU
/// with one host candidate learns it as peer-reflexive and both directions
/// carry data.
fn lite_session(
    net: &mut Network,
    sfu_addr: SocketAddr,
    private: &str,
    public: &str,
) -> (PeerId, PeerId) {
    let sfu = net.add_peer(rtp_mode().set_ice_lite(true), sfu_addr);
    let client = net.add_peer(Rtc::builder(), addr(private));
    net.add_nat(addr(private), addr(public));
    negotiate_with(
        net,
        client,
        sfu,
        |api| api.add_channel("piik".into()),
        |offer| {
            let stripped = strip_candidates(offer);
            assert!(!stripped.contains("a=candidate"));
            stripped
        },
        |answer| {
            assert!(answer.contains("a=ice-lite"));
            assert_eq!(answer.matches("a=candidate:").count(), 1);
            answer
        },
    )
    .unwrap();
    (sfu, client)
}

#[test]
fn spike5_ice_lite_with_peer_reflexive_client() {
    let mut net = Network::new();
    let (sfu, client) = lite_session(
        &mut net,
        addr("192.0.2.1:7000"),
        "10.0.0.2:5000",
        "203.0.113.7:40000",
    );
    connect(&mut net, &[sfu, client]);
    net.run_until(
        |n| open_channel(n, sfu).is_some() && open_channel(n, client).is_some(),
        Duration::from_secs(2),
    )
    .unwrap();
    send_text(&mut net, sfu, "down");
    send_text(&mut net, client, "up");
    net.run_for(Duration::from_millis(200)).unwrap();
    assert_eq!(channel_text(&net, client), ["down"]);
    assert_eq!(channel_text(&net, sfu), ["up"]);
    // The SFU only ever saw the public (reflexive) address.
    assert!(net.link_bytes(addr("192.0.2.1:7000"), addr("203.0.113.7:40000")) > 0);
    assert_eq!(net.stats().undelivered, 0);
}

/// mDNS: a browser offer whose only candidate is a `.local` host. str0m's
/// candidate grammar needs an IP, so the line falls through as an unknown
/// attribute: the SDP is accepted, no remote candidate is added, and the
/// connection still forms through peer-reflexive learning.
#[test]
fn spike5_mdns_candidate_is_ignored() {
    let mut net = Network::new();
    let sfu = net.add_peer(rtp_mode().set_ice_lite(true), addr("192.0.2.1:7000"));
    let client = net.add_peer(Rtc::builder(), addr("10.0.0.2:5000"));
    net.add_nat(addr("10.0.0.2:5000"), addr("203.0.113.7:40000"));
    let mdns = "a=candidate:1 1 udp 2122260223 4f1d2a3b-0c9e-4d1e-9a67-5b8e0f7c6d21.local 54321 typ host generation 0";
    negotiate_with(
        &mut net,
        client,
        sfu,
        |api| api.add_channel("piik".into()),
        |offer| strip_candidates(offer).replacen("a=mid:", &format!("{mdns}\r\na=mid:"), 1),
        |answer| answer,
    )
    .unwrap();
    connect(&mut net, &[sfu, client]);
}

/// Premise 6: two ICE-lite SFU peers bound to one address are demultiplexed
/// by `Rtc::accepts` with no misrouted or unclaimed datagrams.
#[test]
fn spike6_accepts_multiplexes_one_address() {
    let mut net = Network::new();
    let shared = addr("192.0.2.1:7000");
    let (sfu_a, client_a) = lite_session(&mut net, shared, "10.0.0.2:5000", "203.0.113.7:40000");
    let (sfu_b, client_b) = lite_session(&mut net, shared, "10.0.0.3:5000", "203.0.113.8:40000");
    let all = [sfu_a, client_a, sfu_b, client_b];
    connect(&mut net, &all);
    net.run_until(
        |n| all.iter().all(|p| open_channel(n, *p).is_some()),
        Duration::from_secs(2),
    )
    .unwrap();
    for (peer, text) in [
        (sfu_a, "to-a"),
        (sfu_b, "to-b"),
        (client_a, "from-a"),
        (client_b, "from-b"),
    ] {
        send_text(&mut net, peer, text);
    }
    net.run_for(Duration::from_millis(200)).unwrap();
    assert_eq!(channel_text(&net, client_a), ["to-a"]);
    assert_eq!(channel_text(&net, client_b), ["to-b"]);
    assert_eq!(channel_text(&net, sfu_a), ["from-a"]);
    assert_eq!(channel_text(&net, sfu_b), ["from-b"]);
    let stats = net.stats();
    assert_eq!(stats.ambiguous, 0, "{stats:?}");
    assert_eq!(stats.undelivered, 0, "{stats:?}");
}

// ---------------------------------------------------------------------------
// Spike 7: simulcast receive in RTP mode.
// ---------------------------------------------------------------------------

/// Premise 7: an RTP-mode SFU answering a 3-layer RID simulcast offer binds
/// every layer to its own receive stream, and a keyframe request on one
/// layer reaches the sender for exactly that RID.
#[test]
fn spike7_simulcast_rids_and_keyframe_per_rid() {
    const RIDS: [&str; 3] = ["q", "h", "f"];
    let mut net = Network::new();
    let browser = net.add_peer(Rtc::builder(), addr("10.0.0.1:1000"));
    let sfu = net.add_peer(rtp_mode(), addr("10.0.0.2:2000"));
    let mid = negotiate(&mut net, browser, sfu, |api| {
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
    connect(&mut net, &[browser, sfu]);

    let pt = vp8_pt(&net, browser).pt();
    for frame in 0..60u64 {
        let now = net.now();
        for (layer, rid) in RIDS.iter().enumerate() {
            net.peer_mut(browser)
                .rtc
                .writer(mid)
                .unwrap()
                .rid((*rid).into())
                .write(
                    pt,
                    now,
                    MediaTime::new(frame * 3000, Frequency::NINETY_KHZ),
                    vec![0x10 + layer as u8; 200 * (layer + 1)],
                )
                .unwrap();
        }
        net.run_for(Duration::from_millis(33)).unwrap();
    }

    let mut ssrc_of = Vec::new();
    for rid in RIDS {
        let stream = net
            .peer_mut(sfu)
            .rtc
            .direct_api()
            .stream_rx_by_mid(mid, Some(rid.into()))
            .map(|s| s.ssrc());
        ssrc_of.push(stream.unwrap_or_else(|| panic!("rid {rid} bound")));
    }
    let packets: Vec<Ssrc> = net
        .peer(sfu)
        .events
        .iter()
        .filter_map(|(_, e)| match e {
            Event::RtpPacket(p) => Some(p.header.ssrc),
            _ => None,
        })
        .collect();
    for (rid, ssrc) in RIDS.iter().zip(&ssrc_of) {
        assert!(
            packets.iter().filter(|s| *s == ssrc).count() >= 50,
            "rid {rid} forwarded as RTP"
        );
    }

    for rid in RIDS {
        let before = net.peer(browser).events.len();
        net.peer_mut(sfu)
            .rtc
            .direct_api()
            .stream_rx_by_mid(mid, Some(rid.into()))
            .unwrap()
            .request_keyframe(KeyframeRequestKind::Pli);
        net.run_for(Duration::from_millis(200)).unwrap();
        let requests: Vec<Option<Rid>> = net.peer(browser).events[before..]
            .iter()
            .filter_map(|(_, e)| match e {
                Event::KeyframeRequest(r) if r.mid == mid => Some(r.rid),
                _ => None,
            })
            .collect();
        assert_eq!(requests, [Some(rid.into())], "PLI for {rid}");
    }
}

// ---------------------------------------------------------------------------
// Spike 8: TWCC BWE and probing.
// ---------------------------------------------------------------------------

fn twcc_estimates(net: &Network, peer: PeerId) -> Vec<(Duration, Bitrate, bool)> {
    let start = net.clock().start();
    net.peer(peer)
        .events
        .iter()
        .filter_map(|(at, e)| match e {
            Event::EgressBitrateEstimate(BweKind::Twcc {
                estimate,
                can_probe,
            }) => Some((*at - start, *estimate, *can_probe)),
            _ => None,
        })
        .collect()
}

/// Premise 8: with TWCC, `EgressBitrateEstimate` events follow a 1 Mbps
/// bottleneck. After the bottleneck lifts, the estimate stays near the media
/// rate until `set_bitrate` raises `desired`, which probes for capacity.
#[test]
fn spike8_twcc_estimate_and_probing() {
    let mut net = Network::new();
    net.set_min_step(Duration::from_micros(200));
    let tx_addr = addr("10.0.0.1:1000");
    let rx_addr = addr("10.0.0.2:2000");
    let tx = net.add_peer(rtp_mode().enable_bwe(Some(Bitrate::kbps(300))), tx_addr);
    let rx = net.add_peer(rtp_mode(), rx_addr);
    connect_direct(&mut net, tx, rx);
    connect(&mut net, &[tx, rx]);

    let mid: Mid = "vid".into();
    {
        let mut api = net.peer_mut(tx).rtc.direct_api();
        api.declare_media(mid, MediaKind::Video);
        api.declare_stream_tx(MAIN.into(), Some(RTX.into()), mid, None);
    }
    {
        let mut api = net.peer_mut(rx).rtc.direct_api();
        api.declare_media(mid, MediaKind::Video);
        api.expect_stream_rx(MAIN.into(), Some(RTX.into()), mid, None);
        api.enable_twcc_feedback();
    }
    net.peer_mut(tx)
        .rtc
        .bwe()
        .set_bitrate(Bitrate::ZERO, Bitrate::mbps(5));
    net.set_link(
        tx_addr,
        rx_addr,
        NetemConfig::new()
            .latency(Duration::from_millis(20))
            .link(Bitrate::mbps(1), DataSize::kbytes(30)),
    );
    let pt = vp8_pt(&net, tx).pt();

    // 400 kbps of media: 1000-byte packets every 20 ms.
    let mut seq = 1u64;
    let mut send = |net: &mut Network, duration: Duration| {
        let end = net.now() + duration;
        while net.now() < end {
            let now = net.now();
            net.peer_mut(tx)
                .rtc
                .direct_api()
                .stream_tx(&MAIN.into())
                .unwrap()
                .write_rtp(
                    RtpWrite::new(pt, seq.into(), seq as u32 * 1800, now, vec![0u8; 1000])
                        .nackable(true),
                );
            seq += 1;
            net.run_for(Duration::from_millis(20)).unwrap();
        }
    };

    send(&mut net, Duration::from_secs(10));
    let bottleneck = twcc_estimates(&net, tx);
    assert!(!bottleneck.is_empty(), "TWCC estimates are emitted");
    assert!(bottleneck.iter().any(|e| e.2), "can_probe becomes true");
    let (_, last, _) = *bottleneck.last().unwrap();
    assert!(
        last > Bitrate::kbps(450) && last < Bitrate::kbps(1300),
        "estimate tracks the 1 Mbps bottleneck: {last}"
    );

    // Lifting the bottleneck alone: no new probe, the estimate only creeps up.
    net.set_link(
        tx_addr,
        rx_addr,
        NetemConfig::new().latency(Duration::from_millis(20)),
    );
    send(&mut net, Duration::from_secs(5));
    let (_, crept, _) = *twcc_estimates(&net, tx).last().unwrap();
    assert!(
        crept < Bitrate::mbps(1),
        "without a new probe request the estimate stays near the media rate: {crept}"
    );

    // Raising `desired` above the estimate requests an allocation probe.
    net.peer_mut(tx)
        .rtc
        .bwe()
        .set_bitrate(Bitrate::ZERO, Bitrate::mbps(10));
    let before = net.link_bytes(tx_addr, rx_addr);
    send(&mut net, Duration::from_secs(10));
    let sent = net.link_bytes(tx_addr, rx_addr) - before;
    let open = twcc_estimates(&net, tx);
    let (_, last, _) = *open.last().unwrap();
    assert!(
        last > Bitrate::mbps(2),
        "probing discovers capacity above the 400 kbps media: {last}"
    );
    // Media alone is 500 packets of ~1030 B on the wire; probes add more.
    assert!(sent > 530_000, "probe traffic on the wire: {sent}");
}
