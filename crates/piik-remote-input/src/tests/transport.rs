// Relay-backed manager tests: the attended grant lifecycle, a late old epoch
// that cannot hide a relayed grant from branch loss, and input retirement
// ordering already-read packets. Fake data channels replace SCTP; the relay,
// encryption and grant owner are the production code.

use piik_input_route::{Delivery, Receive, Relay};

use super::*;
use crate::cipher::FROM_HOST;

/// A Viewer endpoint: its key, epoch and the channel it sends on.
struct Viewer {
    peer: RemoteInputPeer,
    cipher: InputCipher,
    sequence: u64,
    epoch: String,
    edge: String,
    channel: Arc<FakeChannel>,
}

#[derive(Debug, serde::Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Reply {
    state: String,
    #[serde(default)]
    grant: String,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    co_play: bool,
    input_failed: Option<bool>,
}

impl Viewer {
    fn new(name: &str, endpoint: char, secret: char, edge: &str) -> Self {
        let peer = peer(name, endpoint, secret);
        Self {
            cipher: InputCipher::new(&peer.secret).unwrap(),
            epoch: peer.endpoint_id.clone(),
            peer,
            sequence: 0,
            edge: edge.into(),
            channel: FakeChannel::open(),
        }
    }

    fn send(&mut self, session: &TestSession, event: &InputEvent) {
        self.sequence += 1;
        let packet = viewer_packet(
            &self.cipher,
            self.sequence,
            &self.epoch,
            &self.peer.endpoint_id,
            event,
        );
        let text = serde_json::to_vec(&packet).unwrap();
        session
            .route()
            .down_message(&self.edge, &self.channel, &text, true);
    }

    /// Decrypts this endpoint's Host replies, discarding others' packets.
    fn replies(&self) -> Vec<Reply> {
        self.channel
            .take()
            .into_iter()
            .filter(|packet| packet.id == self.peer.endpoint_id && !packet.data.is_empty())
            .filter_map(|packet| {
                self.cipher
                    .open_with(FROM_HOST, &packet.data, &packet.epoch)
            })
            .map(|plain| serde_json::from_slice(&plain).unwrap())
            .collect()
    }

    fn last_reply(&self) -> Reply {
        self.replies().pop().expect("input state reply missing")
    }
}

fn find(fixture: &Fixture, session: &TestSession, viewer: &Viewer) -> Key {
    key_of(session, &viewer.peer)
        .clone()
        .tap(|key| assert!(fixture.inner(|inner| inner.connections.contains_key(key))))
}

trait Tap: Sized {
    fn tap(self, f: impl FnOnce(&Self)) -> Self {
        f(&self);
        self
    }
}

impl<T> Tap for T {}

fn request(fixture: &Fixture, session: &TestSession, viewer: &mut Viewer) -> Key {
    let mut intent = [0u8; 16];
    crate::cipher::random(&mut intent);
    viewer.send(
        session,
        &InputEvent {
            intent: hex::encode(intent),
            ..InputEvent::of("request")
        },
    );
    let key = find(fixture, session, viewer);
    assert!(
        fixture.conn(&key, |c| c.requested),
        "request was not received"
    );
    key
}

#[test]
fn attended_grant_lifecycle() {
    let fixture = Fixture::with(
        RecordedInput::focused().tap(|sink| {
            sink.with(|r| {
                r.geometry = Bounds {
                    width: 1920,
                    height: 1080,
                    ..Bounds::default()
                }
            })
        }),
        Options::default(),
    );
    let session = fixture.session();
    set_policy(&session, "ask", "").unwrap();
    let mut first = Viewer::new("first-viewer", '1', 'a', "first");
    let mut second = Viewer::new("second-viewer", '2', 'b', "second");
    session
        .set_peers(&[first.peer.clone(), second.peer.clone()])
        .unwrap();
    let lose_first = session.attach("first", first.channel.clone());
    let lose_second = session.attach("second", second.channel.clone());
    let _ = &lose_first;
    let key = |c: &Key, grant: &str| {
        fixture.receive(
            c,
            &format!(r#"{{"type":"key","code":"KeyA","down":true,"grant":"{grant}"}}"#)
                .into_bytes(),
        )
    };
    let decide = |c: &Key, allow: bool| {
        let id = fixture.conn(c, |c| c.request_id.clone());
        session.decide(&id, allow).unwrap();
        fixture.grant(c)
    };
    // Drain setup notifications; the next one must be a request.
    while session.take_change() {}
    let a = request(&fixture, &session, &mut first);
    let other = fixture.manager.new_session::<FakeChannel>().unwrap();
    other.set_target(display("2"));
    other
        .set_peers(std::slice::from_ref(&first.peer))
        .expect("endpoint identity leaked across independent rooms");
    let id = fixture.conn(&a, |c| c.request_id.clone());
    assert!(
        other.state().requests.is_empty()
            && other.decide(&id, true).is_err()
            && other.decide(&id, false).is_err(),
        "another Host session can view or decide this share's request"
    );
    assert!(session.take_change(), "request did not notify its Host");
    let views = session.state().requests;
    assert!(
        views.len() == 1 && views[0].peer_id == "first-viewer" && views[0].state == "pending",
        "request not reflected in its Host snapshot"
    );
    key(&a, "");
    assert_eq!(fixture.sink.events(), 0, "unapproved input accepted");
    let grant = decide(&a, true);
    assert!(!grant.is_empty() && session.state().requests[0].state == "active");
    key(&a, &grant);
    assert_eq!(fixture.sink.events(), 1, "approved input missing");
    let b = request(&fixture, &session, &mut second);
    let grant2 = decide(&b, true);
    key(&a, &grant);
    assert!(
        fixture.sink.events() == 1 && fixture.sink.resets() == 1,
        "takeover did not retire old input"
    );
    key(&b, &grant2);
    session.pause(true);
    key(&b, &grant2);
    assert!(
        fixture.sink.events() == 2 && fixture.sink.resets() == 2,
        "pause did not release input"
    );
    assert_eq!(second.last_reply().state, "unavailable");
    session.pause(false);
    assert_eq!(second.last_reply().state, "ready");
    let b = request(&fixture, &session, &mut second);
    let grant2 = decide(&b, true);
    lose_second();
    key(&b, &grant2);
    assert!(
        fixture.sink.events() == 2 && fixture.sink.resets() == 3,
        "disconnect kept a grant"
    );
    // ICE may recover on the same channel: a fresh request is accepted, but
    // neither the old grant nor a pending approval revives.
    let b = request(&fixture, &session, &mut second);
    let stale = fixture.conn(&b, |c| c.request_id.clone());
    lose_second();
    assert!(
        session.decide(&stale, true).is_err() && session.state().requests.is_empty(),
        "transport loss kept a pending approval"
    );
    let b = request(&fixture, &session, &mut second);
    key(&b, &grant2);
    assert_eq!(
        fixture.sink.events(),
        2,
        "new request restored the disconnected grant"
    );
    decide(&b, false);
    let a = request(&fixture, &session, &mut first);
    let grant = decide(&a, true);
    fixture.manager.revoke();
    key(&a, &grant);
    assert!(
        fixture.sink.events() == 2 && fixture.sink.resets() == 4,
        "local stop kept a grant"
    );
    let a = request(&fixture, &session, &mut first);
    let grant = decide(&a, true);
    session.set_target(CaptureTarget::default());
    key(&a, &grant);
    assert!(
        fixture.sink.events() == 2
            && fixture.sink.resets() == 5
            && !fixture.inner(|inner| inner.available(session.id())),
        "source replacement kept input authority"
    );
    let next_display = Bounds {
        left: 1920,
        width: 1280,
        height: 720,
        ..Bounds::default()
    };
    fixture.sink.with(|r| r.geometry = next_display);
    session.set_target(window("3"));
    set_policy(&session, "ask", "").unwrap();
    let a = request(&fixture, &session, &mut first);
    let (old_request, old_intent) = fixture.conn(&a, |c| (c.request_id.clone(), c.intent.clone()));
    let release = format!(r#"{{"type":"release","intent":"{old_intent}"}}"#).into_bytes();
    fixture.receive(&a, &release);
    let a = request(&fixture, &session, &mut first);
    assert!(
        session.decide(&old_request, true).is_err(),
        "late approval authorized a replacement request"
    );
    let next_grant = decide(&a, true);
    fixture.receive(&a, &release);
    key(&a, &grant);
    key(&a, &next_grant);
    assert!(
        fixture.sink.events() == 3
            && next_grant != grant
            && fixture.sink.with(|r| r.target) == next_display,
        "replacement did not require fresh approval on the existing channel"
    );
    // A held key is released when the Host leaves the shared window, even
    // when the Viewer sends nothing. Focus recovery retains the grant.
    fixture.sink.set_focused(false);
    fixture.refresh();
    fixture.refresh();
    key(&a, &next_grant);
    assert!(
        fixture.conn(&a, |c| c.suspended)
            && session.state().requests[0].state == "suspended"
            && fixture.sink.events() == 3
            && fixture.sink.resets() == 6,
        "window focus loss did not suspend and release held input once"
    );
    fixture.sink.set_focused(true);
    let moved = Bounds {
        left: 100,
        top: 200,
        width: 640,
        height: 480,
    };
    fixture.sink.with(|r| r.geometry = moved);
    key(&a, &next_grant);
    assert!(
        !fixture.conn(&a, |c| c.suspended)
            && fixture.sink.events() == 4
            && fixture.sink.with(|r| r.target) == moved,
        "window movement/focus recovery did not use the current target"
    );
    // Every peer of one build receives the same source facts; no negotiation
    // state exists per connection.
    first.replies();
    second.replies();
    first.send(&session, &InputEvent::of("sync"));
    let reply = first.last_reply();
    assert!(
        !reply.co_play && reply.capabilities == ["coPlay", "inputFeedback", "text"],
        "single-controller facts were missing from a sync reply: {reply:?}"
    );
    second.send(&session, &InputEvent::of("sync"));
    assert_eq!(
        second.last_reply().capabilities,
        ["coPlay", "inputFeedback", "text"],
        "one Guest changed another Guest's reply"
    );
    let mv = |x: f64, grant: &str| InputEvent {
        x,
        y: 0.3,
        grant: grant.into(),
        ..InputEvent::of("move")
    };
    fixture
        .sink
        .set_apply_error(Some("execution temporarily refused"));
    first.send(&session, &mv(0.4, &next_grant));
    assert!(fixture.conn(&a, |c| c.input_failed));
    let reply = first.last_reply();
    assert!(
        reply.input_failed == Some(true) && reply.grant == next_grant && reply.state == "active",
        "failure feedback did not retain the grant"
    );
    fixture.sink.set_apply_error(None);
    first.send(&session, &mv(0.2, &next_grant));
    assert!(!fixture.conn(&a, |c| c.input_failed));
    let reply = first.last_reply();
    assert!(
        reply.input_failed.is_none() && reply.grant == next_grant && reply.state == "active",
        "fresh input did not clear failure within the same grant"
    );
    // A new epoch for this endpoint retires its grant; runtime facts stay.
    first.epoch = "a".repeat(32);
    first.send(&session, &InputEvent::of("sync"));
    let reply = first.last_reply();
    assert!(
        reply.input_failed.is_none()
            && reply.grant.is_empty()
            && !reply.co_play
            && reply.capabilities == ["coPlay", "inputFeedback", "text"],
        "new epoch inherited a grant or lost its source facts"
    );
    let a = request(&fixture, &session, &mut first);
    let next_grant = decide(&a, true);
    first.replies();
    fixture
        .sink
        .set_apply_error(Some("new epoch execution refused"));
    first.send(&session, &mv(0.4, &next_grant));
    let reply = first.last_reply();
    assert!(
        reply.input_failed == Some(true) && reply.grant == next_grant && reply.state == "active",
        "new epoch feedback did not follow its own grant"
    );
    fixture.sink.set_apply_error(None);
    let (events, resets) = (fixture.sink.events(), fixture.sink.resets());
    let request_id = fixture.conn(&a, |c| c.request_id.clone());
    session.close();
    assert!(
        fixture.sink.events() == events
            && fixture.sink.resets() == resets + 1
            && session.state().requests.is_empty()
            && session.decide(&request_id, true).is_err()
            && fixture.inner(|inner| !inner.connections.contains_key(&a)),
        "source retirement kept a grant"
    );
}

#[test]
fn late_old_epoch_cannot_hide_relayed_grant_from_branch_loss() {
    let fixture = Fixture::new();
    let session = fixture.session();
    set_policy(&session, "ask", "").unwrap();
    let viewer = peer("viewer-route-test", 'a', 'b');
    session.set_peers(std::slice::from_ref(&viewer)).unwrap();
    let key = key_of(&session, &viewer);
    // Root leg from the SFU relay into the Host.
    let root = FakeChannel::open();
    let _ = session.attach("sfu-root", root.clone());
    let relay: Relay<FakeChannel> = Relay::new(None);
    let up = FakeChannel::open();
    relay.attach_up(up.clone());
    relay.set_up_available(true);
    let branch = FakeChannel::open();
    let lose_branch = relay.attach_down("shared-branch", branch.clone());
    // Carries both directions of the relay-to-root leg until it is idle.
    let pump = || {
        loop {
            let upward = up.take();
            let downward = root.take();
            if upward.is_empty() && downward.is_empty() {
                break;
            }
            for packet in upward {
                let text = serde_json::to_vec(&packet).unwrap();
                session.route().down_message("sfu-root", &root, &text, true);
            }
            for packet in downward {
                let text = serde_json::to_vec(&packet).unwrap();
                relay.up_message(&up, &text, true);
            }
        }
    };
    pump();
    let cipher = InputCipher::new(&viewer.secret).unwrap();
    let send = |sequence: u64, epoch: &str, event: InputEvent| {
        let packet = viewer_packet(&cipher, sequence, epoch, &viewer.endpoint_id, &event);
        relay.down_message(
            "shared-branch",
            &branch,
            &serde_json::to_vec(&packet).unwrap(),
            true,
        );
        pump();
    };
    let approve = || {
        let views = session.state().requests;
        assert!(
            views.len() == 1 && views[0].state == "pending",
            "request state: {views:?}"
        );
        session.decide(&views[0].id, true).unwrap();
        fixture.grant(&key)
    };
    let (old_epoch, new_epoch) = ("c".repeat(32), "d".repeat(32));
    send(
        1,
        &old_epoch,
        InputEvent {
            intent: old_epoch.clone(),
            ..InputEvent::of("request")
        },
    );
    let old_grant = approve();
    // Packet 2 was sent on the old longer branch before the route migrated;
    // the new shorter path overtakes it. Its lower sequence is rejected.
    let late = viewer_packet(
        &cipher,
        2,
        &old_epoch,
        &viewer.endpoint_id,
        &InputEvent {
            grant: old_grant,
            ..InputEvent::key("KeyA", true)
        },
    );
    send(
        3,
        &new_epoch,
        InputEvent {
            intent: new_epoch.clone(),
            ..InputEvent::of("request")
        },
    );
    let new_grant = approve();
    send(
        4,
        &new_epoch,
        InputEvent {
            grant: new_grant.clone(),
            ..InputEvent::key("KeyW", true)
        },
    );
    relay.down_message(
        "shared-branch",
        &branch,
        &serde_json::to_vec(&late).unwrap(),
        true,
    );
    pump();
    assert!(
        fixture.grant(&key) == new_grant
            && fixture.conn(&key, |c| !c.keys.contains("KeyA")
                && c.keys.contains("KeyW")),
        "late old packet changed authority"
    );
    // The shared upstream branch disconnects while the root SFU leg stays up.
    lose_branch();
    pump();
    // A later sync may report availability but cannot restore the grant.
    send(5, &new_epoch, InputEvent::of("sync"));
    assert!(
        fixture.grant(&key).is_empty() && fixture.conn(&key, |c| !c.keys.contains("KeyW")),
        "branch loss retained current grant and held key after a late old epoch"
    );
}

/// The production handoff after the relay unlocks: retirement must
/// invalidate a captured delivery before the Manager accepts it.
#[test]
fn input_retirement_orders_already_read_packets() {
    for retired in ["channel", "ice", "explicit-close"] {
        let fixture = Fixture::new();
        let session = fixture.session();
        set_policy(&session, "room", "").unwrap();
        let viewer = peer("viewer-route-test", 'a', 'b');
        session.set_peers(std::slice::from_ref(&viewer)).unwrap();
        type Held = Arc<Mutex<Vec<(String, Packet, Option<Delivery<FakeChannel>>)>>>;
        let held: Held = Arc::default();
        let capture = held.clone();
        let receive: Receive<FakeChannel> = Box::new(move |edge: &str, packet, delivery| {
            lock(&capture).push((edge.to_owned(), packet, delivery));
        });
        let relay = Relay::new(Some(receive));
        let root = FakeChannel::open();
        let _ = relay.attach_down("root-edge", root.clone());
        let deliver = || {
            for (edge, packet, delivery) in lock(&held).drain(..) {
                let current = delivery.map(|delivery| move || delivery.is_current());
                fixture.manager.shared().receive_packet(
                    session.id(),
                    &edge,
                    packet,
                    current.as_ref().map(|f| f as &dyn Fn() -> bool),
                );
            }
        };
        let cipher = InputCipher::new(&viewer.secret).unwrap();
        let epoch = "c".repeat(32);
        let send = |sequence, event: InputEvent| {
            let packet = viewer_packet(&cipher, sequence, &epoch, &viewer.endpoint_id, &event);
            relay.down_message(
                "root-edge",
                &root,
                &serde_json::to_vec(&packet).unwrap(),
                true,
            );
        };
        let request = |intent: char| InputEvent {
            intent: intent.to_string().repeat(32),
            ..InputEvent::of("request")
        };
        if retired == "explicit-close" {
            send(1, request('d'));
        } else {
            send(1, InputEvent::of("sync"));
        }
        deliver();
        if retired == "explicit-close" {
            assert_eq!(
                session.state().requests.len(),
                1,
                "request was not approved"
            );
            let closed = Packet {
                id: viewer.endpoint_id.clone(),
                epoch: epoch.clone(),
                closed: true,
                ..Packet::default()
            };
            relay.down_message(
                "root-edge",
                &root,
                &serde_json::to_vec(&closed).unwrap(),
                true,
            );
        } else {
            send(2, request('d'));
        }
        // The packet has been read; now the channel or ICE retires.
        if retired == "ice" {
            let mut inner = fixture.manager.shared().lock();
            if relay.reset_down("root-edge", &root) {
                inner.revoke_edge(session.id(), "root-edge");
            }
        } else {
            root.close();
        }
        deliver();
        assert!(
            session.state().requests.is_empty(),
            "{retired}: already-read packet retained or recreated authorization after retirement"
        );
        if retired == "ice" {
            send(3, request('e'));
            deliver();
            let views = session.state().requests;
            assert!(
                views.len() == 1 && views[0].state == "active",
                "fresh request on recovered channel: {views:?}"
            );
        }
    }
}
