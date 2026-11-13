// Input relay tests against a fake channel.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use super::*;

#[derive(Default)]
struct FakeChannel {
    label: String,
    closed: AtomicBool,
    buffered: AtomicUsize,
    sent: Mutex<Vec<String>>,
}

impl FakeChannel {
    fn open() -> Arc<Self> {
        Arc::new(Self {
            label: LABEL.into(),
            ..Self::default()
        })
    }

    fn sent(&self) -> Vec<Packet> {
        self.sent
            .lock()
            .unwrap()
            .drain(..)
            .map(|text| serde_json::from_str(&text).unwrap())
            .collect()
    }
}

impl InputChannel for FakeChannel {
    fn label(&self) -> &str {
        &self.label
    }
    fn is_open(&self) -> bool {
        !self.closed.load(Ordering::SeqCst)
    }
    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
    fn buffered_amount(&self) -> usize {
        self.buffered.load(Ordering::SeqCst)
    }
    fn send_text(&self, text: &str) {
        self.sent.lock().unwrap().push(text.to_owned());
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

type Received = Arc<Mutex<Vec<(Packet, Option<Delivery<FakeChannel>>)>>>;

fn root() -> (Relay<FakeChannel>, Received) {
    let received: Received = Arc::default();
    let sink = received.clone();
    let relay = Relay::new(Some(Box::new(move |_: &str, packet, delivery| {
        sink.lock().unwrap().push((packet, delivery));
    }) as Receive<FakeChannel>));
    (relay, received)
}

fn take(received: &Received) -> Vec<Packet> {
    received.lock().unwrap().drain(..).map(|(p, _)| p).collect()
}

fn send(relay: &Relay<FakeChannel>, edge: &str, channel: &Arc<FakeChannel>, packet: &Packet) {
    let text = serde_json::to_vec(packet).unwrap();
    relay.down_message(edge, channel, &text, true);
}

fn input(id: &str, epoch: &str, data: &str) -> Packet {
    Packet {
        id: id.into(),
        epoch: epoch.into(),
        data: data.into(),
        ..Packet::default()
    }
}

fn closed(packet: &Packet) -> Packet {
    Packet::retirement(&packet.id, &packet.epoch)
}

fn hex32(value: usize) -> String {
    format!("{value:032x}")
}

#[test]
fn closed_downstream_binding_allows_replacement() {
    let (relay, received) = root();
    let old_channel = FakeChannel::open();
    let old_loss = relay.attach_down("same-edge", old_channel.clone());
    let old_packet = input(&"a".repeat(32), &"b".repeat(32), "old");
    send(&relay, "same-edge", &old_channel, &old_packet);
    assert_eq!(take(&received), std::slice::from_ref(&old_packet));

    // The channel reaches Closed before its close notification arrives.
    old_channel.close();
    let new_channel = FakeChannel::open();
    let _ = relay.attach_down("same-edge", new_channel.clone());
    assert_eq!(
        take(&received),
        [closed(&old_packet)],
        "old endpoint was not retired"
    );
    let packet = input(&"c".repeat(32), &"d".repeat(32), "new");
    let check_replacement = || {
        send(&relay, "same-edge", &new_channel, &packet);
        assert_eq!(take(&received), std::slice::from_ref(&packet));
    };
    check_replacement();
    relay.down_closed("same-edge", &old_channel);
    check_replacement(); // Late cleanup must not detach the replacement.
    old_loss();
    check_replacement(); // A queued ICE loss also belongs only to the old channel.
    let duplicate = FakeChannel::open();
    let _ = relay.attach_down("same-edge", duplicate.clone());
    assert!(duplicate.is_closed(), "live duplicate was not rejected");
    check_replacement();
    relay.close();
}

#[test]
fn branch_loss_retires_every_unclosed_epoch() {
    for old_closed in [false, true] {
        let (relay, received) = root();
        let branch = FakeChannel::open();
        let lose_branch = relay.attach_down("shared-branch", branch.clone());
        let old = input(&"a".repeat(32), &"b".repeat(32), "old");
        let current = input(&old.id, &"c".repeat(32), "current");
        // Two routes may converge on this leg. The old longer route's last
        // packet can follow a newer epoch without becoming current at Host.
        for packet in [&old, &current, &old] {
            send(&relay, "shared-branch", &branch, packet);
            assert_eq!(take(&received), std::slice::from_ref(packet));
        }
        if old_closed {
            send(&relay, "shared-branch", &branch, &closed(&old));
            assert_eq!(take(&received), [closed(&old)]);
        }
        lose_branch();
        let retired: HashSet<String> = received
            .lock()
            .unwrap()
            .drain(..)
            .map(|(packet, delivery)| {
                assert!(packet.id == current.id && packet.closed && delivery.is_none());
                packet.epoch
            })
            .collect();
        assert!(retired.contains(&current.epoch));
        assert_eq!(retired.contains(&old.epoch), !old_closed);
        // Transient loss keeps the binding; new input still flows.
        send(&relay, "shared-branch", &branch, &current);
        assert_eq!(take(&received), std::slice::from_ref(&current));
    }
}

#[test]
fn epoch_tracking_limit_retires_input_and_closes_only_the_channel() {
    let (relay, received) = root();
    let branch = FakeChannel::open();
    let _ = relay.attach_down("branch", branch.clone());
    let mut tracked = HashMap::new();
    for index in 0..MAX_TRACKED_EPOCHS {
        let packet = input(&hex32(index % 20), &hex32(index), "input");
        send(&relay, "branch", &branch, &packet);
        assert_eq!(take(&received), std::slice::from_ref(&packet));
        tracked.insert(packet.epoch.clone(), packet);
    }
    // At capacity, an existing epoch still carries input. A Closed packet frees
    // its exact record without retiring another epoch of the same endpoint.
    let mut packet = tracked[&hex32(0)].clone();
    send(&relay, "branch", &branch, &packet);
    assert_eq!(take(&received), std::slice::from_ref(&packet));
    send(&relay, "branch", &branch, &closed(&packet));
    assert_eq!(take(&received), [closed(&packet)]);
    tracked.remove(&packet.epoch);
    packet.epoch = hex32(MAX_TRACKED_EPOCHS);
    send(&relay, "branch", &branch, &packet);
    assert_eq!(take(&received), std::slice::from_ref(&packet));
    tracked.insert(packet.epoch.clone(), packet.clone());
    packet.epoch = hex32(MAX_TRACKED_EPOCHS + 1);
    send(&relay, "branch", &branch, &packet);
    let retired = take(&received);
    assert_eq!(retired.len(), MAX_TRACKED_EPOCHS);
    for closure in retired {
        let known = tracked
            .remove(&closure.epoch)
            .expect("overflow forwarded new input");
        assert!(closure.closed && closure.id == known.id);
    }
    assert!(
        branch.is_closed(),
        "overflow did not close the input channel"
    );
}

#[test]
fn new_endpoints_beyond_room_size_are_ignored() {
    let (relay, received) = root();
    let branch = FakeChannel::open();
    let _ = relay.attach_down("branch", branch.clone());
    for index in 0..MAX_TRACKED_ENDPOINTS {
        send(
            &relay,
            "branch",
            &branch,
            &input(&hex32(index), &hex32(0), "x"),
        );
    }
    assert_eq!(take(&received).len(), MAX_TRACKED_ENDPOINTS);
    send(
        &relay,
        "branch",
        &branch,
        &input(&hex32(99), &hex32(0), "x"),
    );
    assert!(take(&received).is_empty());
    assert!(branch.is_open(), "endpoint limit is not a channel failure");
}

#[test]
fn reset_down_invalidates_read_binding_across_same_epoch() {
    let (relay, received) = root();
    let viewer = FakeChannel::open();
    let branch = FakeChannel::open();
    let _ = relay.attach_down("branch", branch.clone());
    let packet = input(&"a".repeat(32), &"b".repeat(32), "input");
    let read_permit = || {
        send(&relay, "branch", &branch, &packet);
        received.lock().unwrap().pop().unwrap().1.unwrap()
    };
    let old = read_permit();
    assert!(old.is_current() && relay.reset_down("branch", &branch));
    let current = read_permit(); // The same epoch can send again after ICE recovery.
    assert!(!old.is_current() && current.is_current());
    assert!(
        !relay.reset_down("branch", &viewer) && current.is_current(),
        "another physical channel retired the current binding"
    );
    branch.close();
    assert!(
        !current.is_current(),
        "a closed channel kept its delivery binding"
    );
}

#[test]
fn rejects_invalid_messages() {
    let (relay, received) = root();
    let branch = FakeChannel::open();
    let _ = relay.attach_down("branch", branch.clone());
    let valid = input(&"a".repeat(32), &"b".repeat(32), "x");
    let text = serde_json::to_vec(&valid).unwrap();
    relay.down_message("branch", &branch, &text, false);
    relay.down_message("branch", &branch, b"{", true);
    let mut large = valid.clone();
    large.data = "x".repeat(MAX_PACKET);
    send(&relay, "branch", &branch, &large);
    send(
        &relay,
        "branch",
        &branch,
        &input(&"a".repeat(31), &"b".repeat(32), "x"),
    );
    send(
        &relay,
        "branch",
        &branch,
        &input(&"a".repeat(32), &"b".repeat(32), ""),
    );
    send(&relay, "branch", &relay_channel_other(), &valid);
    assert!(take(&received).is_empty());
    send(&relay, "branch", &branch, &valid);
    assert_eq!(take(&received), [valid]);
}

fn relay_channel_other() -> Arc<FakeChannel> {
    FakeChannel::open()
}

#[test]
fn intermediate_relay_forwards_and_follows_upstream_readiness() {
    let relay: Relay<FakeChannel> = Relay::new(None);
    let up = FakeChannel::open();
    assert!(relay.attach_up(up.clone()));
    let second = FakeChannel::open();
    assert!(!relay.attach_up(second.clone()) && second.is_closed());
    relay.up_opened(&up);
    assert_eq!(up.sent(), [Packet::ready(true)]);

    let child = FakeChannel::open();
    let lose = relay.attach_down("child", child.clone());
    relay.down_opened("child", &child);
    assert_eq!(child.sent(), [Packet::ready(false)]);
    let packet = input(&"a".repeat(32), &"b".repeat(32), "x");
    send(&relay, "child", &child, &packet);
    assert!(
        up.sent().is_empty(),
        "input crossed a relay whose root is not ready"
    );

    relay.up_message(&up, br#"{"ready":true}"#, true);
    assert_eq!(child.sent(), [Packet::ready(true)]);
    send(&relay, "child", &child, &Packet::ready(true));
    assert_eq!(child.sent(), [Packet::ready(true)]);
    send(&relay, "child", &child, &packet);
    assert_eq!(up.sent(), std::slice::from_ref(&packet));
    // Host replies addressed to an endpoint do not change readiness.
    let reply = Packet {
        id: packet.id.clone(),
        ready: Some(false),
        ..Packet::default()
    };
    relay.up_message(&up, &serde_json::to_vec(&reply).unwrap(), true);
    assert_eq!(child.sent(), [reply]);
    send(&relay, "child", &child, &packet);
    assert_eq!(up.sent(), std::slice::from_ref(&packet));

    lose();
    assert_eq!(up.sent(), [closed(&packet)]);
    relay.set_up_available(false);
    assert_eq!(child.sent(), [Packet::ready(false)]);
    relay.set_up_available(true);
    assert_eq!(up.sent(), [Packet::ready(true)]);
    relay.close();
    assert!(up.is_closed() && child.is_closed());
    relay.reply(&packet);
    assert!(!relay.attach_up(FakeChannel::open()));
}

#[test]
fn congested_channel_is_closed_instead_of_buffered() {
    let (relay, _) = root();
    let child = FakeChannel::open();
    let _ = relay.attach_down("child", child.clone());
    child
        .buffered
        .store(MAX_BUFFERED_AMOUNT + 1, Ordering::SeqCst);
    relay.reply(&Packet::ready(true));
    assert!(child.sent().is_empty() && child.is_closed());
}

#[test]
fn packet_json_omits_empty_fields() {
    assert_eq!(
        serde_json::to_string(&Packet::ready(false)).unwrap(),
        r#"{"ready":false}"#
    );
    assert_eq!(
        serde_json::to_string(&Packet::retirement("i", "e")).unwrap(),
        r#"{"id":"i","epoch":"e","closed":true}"#
    );
}
