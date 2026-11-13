//! SFU voice tests on real 127.0.0.1 UDP: each room member
//! is a str0m peer answering the SFU's voice offers and sending one marked
//! Opus stream per publication it owns.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use common::*;
use piik_media_transport::str0m::format::Codec;
use piik_media_transport::str0m::media::{Frequency, MediaTime, Mid};
use piik_media_transport::str0m::{Event, Rtc};
use piik_sfu::runtime::{
    IceCandidateInit, PeerConnectionState, SdpType, SessionDescription, VoiceConnectionPlan,
    VoiceDescription, VoiceEventSink, VoiceMediaEvent, VoicePublication, VoiceRuntime, VoiceSignal,
    VoiceTrack,
};
use piik_sfu::{MAX_VOICE_PUBLICATIONS, Media};

fn publication(id: &str, peer: &str) -> VoicePublication {
    VoicePublication {
        id: id.into(),
        peer_id: peer.into(),
    }
}

fn plan(
    peer: &str,
    publish: &[&VoicePublication],
    subscribe: &[&VoicePublication],
) -> VoiceConnectionPlan {
    VoiceConnectionPlan {
        room_id: "1234".into(),
        peer_id: format!("relay_{peer}"),
        session_id: format!("session_{peer}"),
        connection_id: format!("connection_{peer}"),
        publish: publish.iter().map(|p| (*p).clone()).collect(),
        subscribe: subscribe.iter().map(|p| (*p).clone()).collect(),
    }
}

fn configure(
    media: &Media,
    plan: &VoiceConnectionPlan,
    emit: VoiceEventSink,
) -> Result<(), String> {
    let work = media
        .configure_voice(plan.clone(), emit)
        .map_err(|e| e.to_string())?;
    work.map_or(Ok(()), |work| work().map_err(|e| e.to_string()))
}

fn close(media: &Media, plan: &VoiceConnectionPlan) -> Result<(), String> {
    media
        .close_voice(
            &plan.room_id,
            &plan.peer_id,
            &plan.session_id,
            &plan.connection_id,
        )
        .map_or(Ok(()), |work| work().map_err(|e| e.to_string()))
}

fn signal(media: &Media, plan: &VoiceConnectionPlan, signal: VoiceSignal) -> Result<(), String> {
    media
        .signal_voice(
            &plan.room_id,
            &plan.peer_id,
            &plan.session_id,
            &plan.connection_id,
            signal,
        )
        .map_err(|e| e.to_string())
}

fn ignore() -> VoiceEventSink {
    Arc::new(|_| {})
}

/// One room member answering voice offers.
struct Member {
    peer: Peer,
    media: Media,
    plan: Mutex<VoiceConnectionPlan>,
    /// Publication id → (marker, active).
    sources: HashMap<String, (u8, Arc<AtomicBool>)>,
    /// Mid → marker this member sends there, from its latest answer.
    sending: Arc<Mutex<HashMap<Mid, u8>>>,
    hold: AtomicBool,
    held: Mutex<mpsc::Sender<VoiceDescription>>,
    errors: Mutex<Vec<String>>,
    revision: AtomicU64,
    offers: Mutex<Vec<VoiceDescription>>,
    connected: Mutex<Vec<VoiceMediaEvent>>,
    stop: Arc<AtomicBool>,
}

impl Member {
    fn new(
        media: &Media,
        plan: &VoiceConnectionPlan,
        markers: &[(&str, u8)],
    ) -> (Arc<Self>, mpsc::Receiver<VoiceDescription>) {
        let (held, receive) = mpsc::channel();
        let member = Arc::new(Self {
            peer: Peer::new(Rtc::builder()),
            media: media.clone(),
            plan: Mutex::new(plan.clone()),
            sources: markers
                .iter()
                .map(|(id, marker)| ((*id).to_owned(), (*marker, Arc::new(AtomicBool::new(true)))))
                .collect(),
            sending: Arc::default(),
            hold: AtomicBool::new(false),
            held: Mutex::new(held),
            errors: Mutex::default(),
            revision: AtomicU64::new(0),
            offers: Mutex::default(),
            connected: Mutex::default(),
            stop: Arc::new(AtomicBool::new(false)),
        });
        let writer = Arc::downgrade(&member);
        std::thread::spawn(move || write_loop(&writer));
        (member, receive)
    }

    fn sink(self: &Arc<Self>) -> VoiceEventSink {
        let member: Weak<Self> = Arc::downgrade(self);
        Arc::new(move |event: VoiceMediaEvent| {
            let Some(member) = member.upgrade() else {
                return;
            };
            if event.state == PeerConnectionState::Connected {
                member.connected.lock().unwrap().push(event.clone());
            }
            if let Some(description) = event.description {
                member.offers.lock().unwrap().push(description.clone());
                if member.hold.load(Ordering::SeqCst) {
                    let _ = member.held.lock().unwrap().send(description);
                } else if let Err(error) = member.answer(&description, false) {
                    member.errors.lock().unwrap().push(error);
                }
            }
        })
    }

    fn plan(&self) -> VoiceConnectionPlan {
        self.plan.lock().unwrap().clone()
    }

    fn configure(self: &Arc<Self>, plan: &VoiceConnectionPlan) {
        *self.plan.lock().unwrap() = plan.clone();
        configure(&self.media, plan, self.sink()).unwrap();
    }

    fn answer(&self, offer: &VoiceDescription, reject_spoof: bool) -> Result<(), String> {
        let answer = self.peer.answer(&offer.sdp, |sdp| sdp);
        let mut tracks = Vec::new();
        let mut sending = HashMap::new();
        for track in &offer.tracks {
            if let Some((marker, _)) = self.sources.get(&track.publication_id) {
                sending.insert(Mid::from(track.mid.as_str()), *marker);
                tracks.push(track.clone());
            }
        }
        let plan = self.plan();
        let description = VoiceDescription {
            revision: offer.revision,
            sdp: answer,
            tracks,
        };
        if reject_spoof {
            let mut spoof = description.clone();
            spoof.tracks = vec![VoiceTrack {
                mid: offer.tracks[0].mid.clone(),
                publication_id: "unapproved_publication".into(),
            }];
            if signal(
                &self.media,
                &plan,
                VoiceSignal {
                    description: Some(spoof),
                    ..Default::default()
                },
            )
            .is_ok()
            {
                return Err("unapproved publication was accepted".into());
            }
        }
        signal(
            &self.media,
            &plan,
            VoiceSignal {
                description: Some(description),
                ..Default::default()
            },
        )?;
        *self.sending.lock().unwrap() = sending;
        self.revision.store(offer.revision, Ordering::SeqCst);
        Ok(())
    }

    fn count(&self, marker: u8) -> usize {
        self.peer.count(
            |event| matches!(event, Event::MediaData(data) if data.data.first() == Some(&marker)),
        )
    }

    fn sections(&self) -> usize {
        self.offers
            .lock()
            .unwrap()
            .last()
            .map_or(0, |offer| offer.sdp.sdp.matches("m=audio").count())
    }

    fn check(&self) {
        let errors = self.errors.lock().unwrap();
        assert!(errors.is_empty(), "{errors:?}");
    }
}

impl Drop for Member {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn write_loop(member: &Weak<Member>) {
    let mut frame = 0u64;
    loop {
        let Some(member) = member.upgrade() else {
            return;
        };
        if member.stop.load(Ordering::Relaxed) {
            return;
        }
        let sending = member.sending.lock().unwrap().clone();
        member.peer.with(|rtc| {
            if !rtc.is_connected() {
                return;
            }
            for (mid, marker) in &sending {
                let active = member
                    .sources
                    .values()
                    .any(|(m, active)| m == marker && active.load(Ordering::Relaxed));
                if !active {
                    continue;
                }
                let Some(writer) = rtc.writer(*mid) else {
                    continue;
                };
                let Some(pt) = writer
                    .payload_params()
                    .find(|p| p.spec().codec == Codec::Opus)
                    .map(|p| p.pt())
                else {
                    continue;
                };
                let _ = writer.write(
                    pt,
                    Instant::now(),
                    MediaTime::new(frame * 480, Frequency::FORTY_EIGHT_KHZ),
                    vec![*marker, 0, 0],
                );
            }
        });
        drop(member);
        frame += 1;
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn recv(receive: &mpsc::Receiver<VoiceDescription>) -> VoiceDescription {
    receive.recv_timeout(WAIT).expect("held voice offer")
}

#[test]
fn junction_duplex_authorization_and_exact_retirement() {
    let f = fixture();
    let a1 = publication("voice_a1", "speaker_a");
    let a2 = publication("voice_a2", "speaker_c");
    let b1 = publication("voice_b1", "speaker_b");
    let a_plan = plan("a", &[&a1, &a2], &[&b1]);
    let b_plan = plan("b", &[&b1], &[&a1, &a2]);
    let (a, a_held) = Member::new(&f.media, &a_plan, &[(&a1.id, 0xa1), (&a2.id, 0xa2)]);
    let (b, b_held) = Member::new(&f.media, &b_plan, &[(&b1.id, 0xb1)]);
    let wait = |condition: &dyn Fn() -> bool, what: &str| {
        until(
            || {
                a.check();
                b.check();
                condition()
            },
            what,
        );
    };

    // Pause the first answer to prove a forged publication cannot consume it.
    a.hold.store(true, Ordering::SeqCst);
    a.configure(&a_plan);
    let first = recv(&a_held);
    a.hold.store(false, Ordering::SeqCst);
    a.answer(&first, true).unwrap();
    b.configure(&b_plan);
    wait(
        &|| a.count(0xb1) >= 3 && b.count(0xa1) >= 3 && b.count(0xa2) >= 3,
        "duplex voice",
    );
    let connected = a
        .connected
        .lock()
        .unwrap()
        .first()
        .cloned()
        .expect("connected event");
    let current = connected.is_current_state.clone().expect("state evidence");
    assert!(
        current(),
        "live voice transport lacked current connection evidence"
    );
    assert_eq!(
        a.count(0xa1),
        0,
        "voice was echoed to its incoming connection"
    );
    assert_eq!(
        b.count(0xb1),
        0,
        "voice was echoed to its incoming connection"
    );

    // A late close from a previous session cannot touch the current PC.
    let mut old = a_plan.clone();
    old.session_id = "old_session".into();
    close(&f.media, &old).unwrap();
    let before = b.count(0xa1);
    wait(
        &|| b.count(0xa1) >= before + 3,
        "old-session close kept media",
    );
    assert!(
        signal(
            &f.media,
            &a_plan,
            VoiceSignal {
                description: Some(VoiceDescription {
                    revision: first.revision,
                    sdp: SessionDescription {
                        sdp_type: SdpType::Answer,
                        sdp: String::new(),
                    },
                    tracks: Vec::new(),
                }),
                ..Default::default()
            }
        )
        .is_err(),
        "old answer was accepted after its negotiation ended"
    );

    // Removing one publication takes effect while its new answer is withheld.
    a.hold.store(true, Ordering::SeqCst);
    let revoked = plan("a", &[&a2], &[&b1]);
    a.configure(&revoked);
    let pending = recv(&a_held);
    let (before_a2, before_b) = (b.count(0xa2), a.count(0xb1));
    wait(
        &|| b.count(0xa2) >= before_a2 + 3 && a.count(0xb1) >= before_b + 3,
        "surviving source keeps flowing",
    );
    let stopped = b.count(0xa1);
    let before_a2 = b.count(0xa2);
    wait(&|| b.count(0xa2) >= before_a2 + 5, "surviving source");
    assert_eq!(
        b.count(0xa1),
        stopped,
        "revoked voice continued before SDP acknowledgement"
    );
    a.hold.store(false, Ordering::SeqCst);
    a.answer(&pending, false).unwrap();

    // Reusing a grant reuses its empty media section.
    for _ in 0..2 {
        a.configure(&a_plan);
        let before = b.count(0xa1);
        wait(&|| b.count(0xa1) >= before + 3, "regranted voice");
        a.configure(&revoked);
        let revision = pending.revision;
        wait(
            &|| a.revision.load(Ordering::SeqCst) >= revision + 2,
            "revoked grant renegotiated",
        );
    }
    assert_eq!(
        a.sections(),
        3,
        "repeated voice grants accumulated media sections"
    );

    // Revoke and regrant a subscription while another answer is pending; the
    // packet gate reopens when the retained desired set is applied.
    b.hold.store(true, Ordering::SeqCst);
    signal(
        &f.media,
        &b_plan,
        VoiceSignal {
            renegotiate: true,
            ..Default::default()
        },
    )
    .unwrap();
    let held = recv(&b_held);
    let without_a2 = plan("b", &[&b1], &[&a1]);
    b.configure(&without_a2);
    let before_b = a.count(0xb1);
    wait(&|| a.count(0xb1) >= before_b + 3, "b keeps publishing");
    let stopped = b.count(0xa2);
    let before_b = a.count(0xb1);
    wait(&|| a.count(0xb1) >= before_b + 3, "b keeps publishing");
    assert_eq!(
        b.count(0xa2),
        stopped,
        "revoked subscription kept receiving before SDP acknowledgement"
    );
    b.configure(&b_plan);
    b.hold.store(false, Ordering::SeqCst);
    b.answer(&held, false).unwrap();
    wait(&|| b.count(0xa2) >= stopped + 3, "regranted subscription");

    // Capture ending retires neither the relay connection nor the other source.
    a.sources[&a2.id].1.store(false, Ordering::Relaxed);
    let before_b = a.count(0xb1);
    wait(
        &|| a.count(0xb1) >= before_b + 3,
        "capture end kept the junction",
    );
    close(&f.media, &a_plan).unwrap();
    assert!(
        !current(),
        "queued connection evidence survived physical voice retirement"
    );
    assert!(
        signal(
            &f.media,
            &a_plan,
            VoiceSignal {
                renegotiate: true,
                ..Default::default()
            }
        )
        .is_err(),
        "closed connection accepted signaling"
    );
    assert_eq!(
        f.media.voice_connection_count("1234"),
        1,
        "exact close retired a sibling"
    );
    assert!(b.peer.connected(), "exact close retired a sibling");
    f.media.close().unwrap();
}

#[test]
fn subscription_reuses_slot_across_independent_packet_clocks() {
    let f = fixture();
    let first = publication("voice_first", "speaker_first");
    let next = publication("voice_next", "speaker_next");
    let send = plan("sender", &[&first, &next], &[]);
    let mut receive = plan("listener", &[], &[&first]);
    let (sender, _) = Member::new(&f.media, &send, &[(&first.id, 0xa1), (&next.id, 0xa2)]);
    let (listener, _) = Member::new(&f.media, &receive, &[]);
    sender.configure(&send);
    listener.configure(&receive);
    let wait = |marker: u8, target: usize| {
        until(
            || {
                sender.check();
                listener.check();
                listener.count(marker) >= target
            },
            &format!("source {marker:x} reaches the listener"),
        );
    };
    wait(0xa1, 3);
    let sections = listener.sections();
    receive.subscribe = vec![next.clone()];
    listener.configure(&receive);
    wait(0xa2, 3);
    let target = listener.count(0xa1) + 3;
    receive.subscribe = vec![first.clone()];
    listener.configure(&receive);
    wait(0xa1, target);
    assert_eq!(
        listener.sections(),
        sections,
        "source replacement grew the negotiated media sections"
    );
    f.media.close().unwrap();
}

#[test]
fn full_room_admits_both_publication_replacement_facets() {
    let f = fixture();
    let configure = |peer: &str, connection: &str| {
        configure(
            &f.media,
            &VoiceConnectionPlan {
                room_id: "1234".into(),
                peer_id: peer.into(),
                session_id: format!("session_{peer}"),
                connection_id: connection.into(),
                ..Default::default()
            },
            ignore(),
        )
    };
    configure("host", "committed_host").unwrap();
    for index in 0..MAX_VOICE_PUBLICATIONS - 1 {
        configure(&format!("viewer_{index}"), &format!("committed_{index}")).unwrap();
    }
    // Publication replacement keeps the full committed graph while the one
    // route operation connects a new Host junction and its first child.
    configure("host", "replacement_host").expect("replacement Host facet");
    configure("viewer_0", "replacement_child").expect("replacement child facet");
    assert!(
        configure("viewer_1", "excess_transition").is_err(),
        "room admitted a connection beyond the replacement allowance"
    );
    // Rooms are charged independently.
    let other = VoiceConnectionPlan {
        room_id: "5678".into(),
        peer_id: "host".into(),
        session_id: "session".into(),
        connection_id: "connection".into(),
        ..Default::default()
    };
    crate::configure(&f.media, &other, ignore()).expect("another room's capacity");
    f.media.close().unwrap();
}

#[test]
fn queued_candidate_failure_does_not_poison_negotiation() {
    let f = fixture();
    let plan = plan("peer", &[], &[]);
    let (peer, held) = Member::new(&f.media, &plan, &[]);
    peer.hold.store(true, Ordering::SeqCst);
    peer.configure(&plan);
    let first = recv(&held);
    let candidate = |text: String| VoiceSignal {
        candidate: Some(IceCandidateInit {
            candidate: text,
            ..Default::default()
        }),
        ..Default::default()
    };
    // One disposable candidate must not strand an applied answer in pending.
    signal(&f.media, &plan, candidate("candidate:malformed".into())).unwrap();
    // Trickle beyond the bounded queue is not a transport error.
    for index in 0..64 {
        signal(
            &f.media,
            &plan,
            candidate(format!(
                "candidate:queued{index} 1 udp 100 127.0.0.1 {} typ host",
                10_000 + index
            )),
        )
        .unwrap_or_else(|error| panic!("candidate {index} failed the transport: {error}"));
    }
    peer.answer(&first, false)
        .expect("a queued candidate rejected a valid answer");
    for text in [
        "candidate:malformed",
        "candidate:control 1 udp 2122260223 127.0.0.1 9999 typ host",
    ] {
        signal(&f.media, &plan, candidate(text.into())).expect("live candidate");
    }
    signal(
        &f.media,
        &plan,
        VoiceSignal {
            renegotiate: true,
            ..Default::default()
        },
    )
    .unwrap();
    let next = recv(&held);
    assert!(
        next.revision > first.revision,
        "renegotiation did not advance"
    );
    peer.answer(&next, false).unwrap();
    // Exactly one signal is required.
    assert!(signal(&f.media, &plan, VoiceSignal::default()).is_err());
    f.media.close().unwrap();
}

#[test]
fn plan_bounds_and_media_shutdown() {
    let f = fixture();
    let mut plan = plan("peer", &[], &[]);
    plan.publish = (0..=MAX_VOICE_PUBLICATIONS)
        .map(|index| publication(&format!("voice_{index}"), "speaker"))
        .collect();
    assert!(
        configure(&f.media, &plan, ignore()).is_err(),
        "an oversized plan allocated a connection"
    );
    plan.publish.truncate(1);
    plan.subscribe = plan.publish.clone();
    assert!(
        configure(&f.media, &plan, ignore()).is_err(),
        "a looping publication was admitted in both directions"
    );
    plan.publish.clear();
    plan.subscribe.clear();
    let mut bad = plan.clone();
    bad.session_id.clear();
    assert!(configure(&f.media, &bad, ignore()).is_err());

    // A callback may retire its own connection.
    let (closed, closes) = mpsc::channel();
    let media = f.media.clone();
    let own = plan.clone();
    configure(
        &f.media,
        &plan,
        Arc::new(move |event: VoiceMediaEvent| {
            if event.description.is_some() {
                let _ = closed.send(close(&media, &own));
            }
        }),
    )
    .unwrap();
    closes
        .recv_timeout(Duration::from_secs(3))
        .expect("callback retired its own connection")
        .unwrap();

    plan.connection_id = "shutdown_connection".into();
    configure(&f.media, &plan, ignore()).unwrap();
    f.media.close().unwrap();
    assert!(
        configure(&f.media, &plan, ignore()).is_err(),
        "closed media admitted voice"
    );
    assert_eq!(f.media.voice_connection_count("1234"), 0);
}

#[test]
fn silent_transport_accepts_late_publication_on_same_connection() {
    let f = fixture();
    let late = publication("late_voice", "speaker");
    let mut a_plan = plan("a", &[], &[]);
    let mut b_plan = plan("b", &[], &[]);
    let (a, _) = Member::new(&f.media, &a_plan, &[(&late.id, 0x71)]);
    let (b, _) = Member::new(&f.media, &b_plan, &[]);
    a.configure(&a_plan);
    b.configure(&b_plan);
    until(
        || {
            a.check();
            b.check();
            a.peer.connected() && b.peer.connected()
        },
        "silent transports connect",
    );
    assert_eq!(
        a.count(0x71) + b.count(0x71),
        0,
        "silent transport sent audio"
    );
    assert_eq!(
        a.sections(),
        1,
        "silent transport reserves exactly one slot"
    );
    assert_eq!(
        b.sections(),
        1,
        "silent transport reserves exactly one slot"
    );
    a_plan.publish = vec![late.clone()];
    b_plan.subscribe = vec![late.clone()];
    a.configure(&a_plan);
    b.configure(&b_plan);
    until(
        || {
            a.check();
            b.check();
            b.count(0x71) >= 3
        },
        "late publication",
    );
    assert_eq!(a.sections(), 1, "late microphone missed its idle slot");
    assert_eq!(f.media.voice_connection_count("1234"), 2);
    f.media.close().unwrap();
}

#[test]
fn deferred_work_uses_current_plan_and_cannot_revive_retired_connection() {
    let f = fixture();
    let base = VoiceConnectionPlan {
        room_id: "1234".into(),
        peer_id: "peer".into(),
        session_id: "session".into(),
        connection_id: "connection".into(),
        ..Default::default()
    };
    let offers = Arc::new(Mutex::new(Vec::<VoiceDescription>::new()));
    let sink: VoiceEventSink = {
        let offers = offers.clone();
        Arc::new(move |event: VoiceMediaEvent| {
            if let Some(description) = event.description {
                offers.lock().unwrap().push(description);
            }
        })
    };
    let old_work = f
        .media
        .configure_voice(base.clone(), sink.clone())
        .unwrap()
        .unwrap();
    let mut latest = base.clone();
    latest.publish = vec![publication("latest", "peer")];
    let new_work = f
        .media
        .configure_voice(latest.clone(), sink.clone())
        .unwrap()
        .unwrap();
    new_work().unwrap();
    old_work().unwrap();
    until(|| !offers.lock().unwrap().is_empty(), "offer");
    let tracks = offers.lock().unwrap()[0].tracks.clone();
    assert_eq!(tracks.len(), 1);
    assert_eq!(
        tracks[0].publication_id, "latest",
        "old work used a stale plan"
    );
    assert_eq!(offers.lock().unwrap().len(), 1, "old work offered again");

    let old_work = f
        .media
        .configure_voice(latest.clone(), sink.clone())
        .unwrap()
        .unwrap();
    let close_old = f
        .media
        .close_voice(
            &base.room_id,
            &base.peer_id,
            &base.session_id,
            &base.connection_id,
        )
        .unwrap();
    assert!(old_work().is_err(), "retired negotiation succeeded");
    assert_eq!(
        f.media.voice_connection_count("1234"),
        1,
        "retirement lost its physical capacity charge"
    );
    assert!(
        f.media
            .configure_voice(latest.clone(), sink.clone())
            .is_err(),
        "configuration reused a retiring physical key"
    );
    close_old().unwrap();
    let replacement = f.media.configure_voice(latest, sink).unwrap().unwrap();
    replacement().expect("old physical close retired its replacement");
    f.media.close().unwrap();
}

#[test]
fn retirement_keeps_capacity_charged_until_physical_close() {
    let f = fixture();
    let mut closes = Vec::new();
    for index in 0..MAX_VOICE_PUBLICATIONS + 2 {
        let plan = VoiceConnectionPlan {
            room_id: "room".into(),
            peer_id: "peer".into(),
            session_id: "session".into(),
            connection_id: format!("connection{index}"),
            ..Default::default()
        };
        f.media.configure_voice(plan.clone(), ignore()).unwrap();
        closes.push(
            f.media
                .close_voice(
                    &plan.room_id,
                    &plan.peer_id,
                    &plan.session_id,
                    &plan.connection_id,
                )
                .unwrap(),
        );
    }
    let extra = VoiceConnectionPlan {
        room_id: "room".into(),
        peer_id: "peer".into(),
        session_id: "session".into(),
        connection_id: "extra".into(),
        ..Default::default()
    };
    assert!(
        f.media.configure_voice(extra.clone(), ignore()).is_err(),
        "retiring physical connections escaped room capacity"
    );
    let first = closes.remove(0);
    first().unwrap();
    f.media
        .configure_voice(extra, ignore())
        .expect("completed close retained room capacity");
    for close in closes {
        close().unwrap();
    }
    f.media.close().unwrap();
}
