//! Carries opaque attended-input packets over the existing media tree. It owns
//! no PeerConnections, route selection or permissions.
//!
//! The data channel is abstracted behind [`InputChannel`]; the transport owner
//! forwards each channel's open, message and close notifications to the
//! matching `*_opened`, `*_message` and `*_closed` methods of [`Relay`].

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard};

use serde::{Deserialize, Serialize};

pub const LABEL: &str = "piik-remote-input-prototype-v1";

/// 2 KiB of text, JSON escaping, authenticated encryption and the relay
/// envelope.
pub const MAX_PACKET: usize = 6144;

/// Allows route overlap for the room's 20 endpoints while bounding retirement
/// metadata. Epochs remain opaque here; only the Host knows which is
/// authorized.
pub const MAX_TRACKED_EPOCHS: usize = 128;

const MAX_TRACKED_ENDPOINTS: usize = 20;
const MAX_BUFFERED_AMOUNT: usize = 8192;
const ID_LENGTH: usize = 32;

/// Packet data is authenticated and encrypted between the Viewer and Host App.
/// Relays only learn endpoint IDs. Small Host state replies fan out along the
/// existing tree; only the named endpoint can read them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Packet {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub id: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub epoch: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub data: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub closed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ready: Option<bool>,
}

impl Packet {
    fn ready(ready: bool) -> Self {
        Self {
            ready: Some(ready),
            ..Self::default()
        }
    }

    fn retirement(id: &str, epoch: &str) -> Self {
        Self {
            id: id.to_owned(),
            epoch: epoch.to_owned(),
            closed: true,
            ..Self::default()
        }
    }
}

/// One physical input data channel. Identity is the `Arc` allocation.
///
/// [`InputChannel::close`] must not synchronously call back into the relay;
/// the relay invokes it outside its lock, but a transport normally reports the
/// resulting closure through [`Relay::down_closed`] or [`Relay::up_closed`]
/// later.
pub trait InputChannel: Send + Sync {
    fn label(&self) -> &str;
    /// The channel is open and may send.
    fn is_open(&self) -> bool;
    /// The channel reached its final closed state, even if its close
    /// notification has not been delivered yet.
    fn is_closed(&self) -> bool;
    fn buffered_amount(&self) -> usize;
    /// Sends one text message; failures are ignored like a lost packet.
    fn send_text(&self, text: &str);
    fn close(&self);
}

/// Receives each input packet at the Host root. Retirement packets created by
/// branch loss carry no delivery binding.
pub type Receive<C> = Box<dyn Fn(&str, Packet, Option<Delivery<C>>) + Send + Sync>;

/// Proves whether a delivered packet still belongs to the current binding of
/// its exact input channel. The root checks it while holding its permission
/// lock, so retirement can invalidate an already-read packet.
pub struct Delivery<C> {
    shared: Arc<Shared<C>>,
    edge_id: String,
    leg: u64,
    channel: Arc<C>,
}

impl<C: InputChannel> Delivery<C> {
    pub fn is_current(&self) -> bool {
        let state = self.shared.lock();
        !state.closed
            && state
                .down
                .get(&self.edge_id)
                .is_some_and(|leg| leg.id == self.leg)
            && self.channel.is_open()
    }
}

struct Leg<C> {
    id: u64,
    channel: Arc<C>,
    endpoints: HashMap<String, HashSet<String>>,
}

struct State<C> {
    up: Option<Arc<C>>,
    down: BTreeMap<String, Leg<C>>,
    ready: bool,
    closed: bool,
    next_leg: u64,
}

impl<C> State<C> {
    fn leg(&mut self, channel: Arc<C>) -> Leg<C> {
        self.next_leg += 1;
        Leg {
            id: self.next_leg,
            channel,
            endpoints: HashMap::new(),
        }
    }
}

struct Shared<C> {
    state: Mutex<State<C>>,
    receive: Option<Receive<C>>,
}

impl<C> Shared<C> {
    fn lock(&self) -> MutexGuard<'_, State<C>> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }
}

/// Channels to close once the relay lock is released.
struct Closes<C>(Vec<Arc<C>>);

impl<C: InputChannel> Closes<C> {
    fn new() -> Self {
        Self(Vec::new())
    }

    fn send(&mut self, channel: Option<&Arc<C>>, packet: &Packet) {
        let Some(channel) = channel.filter(|channel| channel.is_open()) else {
            return;
        };
        if channel.buffered_amount() > MAX_BUFFERED_AMOUNT {
            self.0.push(channel.clone());
            return;
        }
        let text = serde_json::to_string(packet).expect("packet serializes");
        channel.send_text(&text);
    }

    fn run(self) {
        for channel in self.0 {
            channel.close();
        }
    }
}

fn decode(data: &[u8], is_string: bool) -> Option<Packet> {
    if !is_string || data.len() > MAX_PACKET {
        return None;
    }
    let packet: Packet = serde_json::from_slice(data).ok()?;
    let valid = packet.ready.is_some()
        || packet.id.len() == ID_LENGTH
            && packet.epoch.len() == ID_LENGTH
            && (packet.closed || !packet.data.is_empty());
    valid.then_some(packet)
}

pub struct Relay<C: InputChannel> {
    shared: Arc<Shared<C>>,
}

impl<C: InputChannel + 'static> Relay<C> {
    /// A root relay passes `Some(receive)`; an intermediate relay forwards
    /// child input upstream.
    pub fn new(receive: Option<Receive<C>>) -> Self {
        let ready = receive.is_some();
        Self {
            shared: Arc::new(Shared {
                state: Mutex::new(State {
                    up: None,
                    down: BTreeMap::new(),
                    ready,
                    closed: false,
                    next_leg: 0,
                }),
                receive,
            }),
        }
    }

    /// Binds the upstream channel. A second, mislabeled or post-close channel
    /// is closed and reported as rejected.
    pub fn attach_up(&self, channel: Arc<C>) -> bool {
        let mut state = self.shared.lock();
        if state.closed || state.up.is_some() || channel.label() != LABEL {
            drop(state);
            channel.close();
            return false;
        }
        state.up = Some(channel);
        true
    }

    pub fn up_opened(&self, channel: &Arc<C>) {
        let mut closes = Closes::new();
        closes.send(Some(channel), &Packet::ready(true));
        closes.run();
    }

    pub fn up_message(&self, channel: &Arc<C>, data: &[u8], is_string: bool) {
        let Some(packet) = decode(data, is_string) else {
            return;
        };
        let mut closes = Closes::new();
        {
            let mut state = self.shared.lock();
            if state.closed || !state.up.as_ref().is_some_and(|up| Arc::ptr_eq(up, channel)) {
                return;
            }
            if let Some(ready) = packet.ready
                && packet.id.is_empty()
            {
                state.ready = ready;
            }
            for child in state.down.values() {
                closes.send(Some(&child.channel), &packet);
            }
        }
        closes.run();
    }

    pub fn up_closed(&self, _channel: &Arc<C>) {
        self.set_up_available(false);
    }

    /// ICE loss releases all descendant input even if SCTP has not closed yet.
    pub fn set_up_available(&self, available: bool) {
        let mut closes = Closes::new();
        {
            let mut state = self.shared.lock();
            if available {
                closes.send(state.up.as_ref(), &Packet::ready(true));
            } else {
                state.ready = false;
                for child in state.down.values() {
                    closes.send(Some(&child.channel), &Packet::ready(false));
                }
            }
        }
        closes.run();
    }

    /// Binds a downstream channel and returns transient-loss cleanup tied to
    /// this exact physical channel.
    pub fn attach_down(&self, edge_id: &str, channel: Arc<C>) -> Box<dyn Fn() + Send + Sync> {
        // SCTP can close before its close notification retires the old binding.
        let stale = {
            let state = self.shared.lock();
            state
                .down
                .get(edge_id)
                .filter(|old| !state.closed && old.channel.is_closed())
                .map(|old| old.channel.clone())
        };
        if let Some(old) = stale {
            self.lose_down(edge_id, &old, true);
        }
        let mut state = self.shared.lock();
        if state.closed || state.down.contains_key(edge_id) {
            drop(state);
            channel.close();
            return Box::new(|| {});
        }
        let leg = state.leg(channel.clone());
        state.down.insert(edge_id.to_owned(), leg);
        drop(state);
        let shared = self.shared.clone();
        let edge_id = edge_id.to_owned();
        Box::new(move || {
            Relay {
                shared: shared.clone(),
            }
            .lose_down(&edge_id, &channel, false)
        })
    }

    pub fn down_opened(&self, _edge_id: &str, channel: &Arc<C>) {
        let mut closes = Closes::new();
        {
            let state = self.shared.lock();
            closes.send(Some(channel), &Packet::ready(state.ready));
        }
        closes.run();
    }

    pub fn down_message(&self, edge_id: &str, channel: &Arc<C>, data: &[u8], is_string: bool) {
        let Some(packet) = decode(data, is_string) else {
            return;
        };
        let mut closes = Closes::new();
        let mut state = self.shared.lock();
        if state.closed {
            return;
        }
        let ready = state.ready;
        let up = state.up.clone();
        let Some(child) = state
            .down
            .get_mut(edge_id)
            .filter(|child| Arc::ptr_eq(&child.channel, channel))
        else {
            return;
        };
        let leg = child.id;
        if packet.ready.is_some() {
            closes.send(Some(channel), &Packet::ready(ready));
            drop(state);
            closes.run();
            return;
        }
        if !ready {
            return;
        }
        if packet.closed {
            if let Some(epochs) = child.endpoints.get_mut(&packet.id) {
                epochs.remove(&packet.epoch);
                if epochs.is_empty() {
                    child.endpoints.remove(&packet.id);
                }
            }
        } else {
            let known_endpoint = child.endpoints.contains_key(&packet.id);
            if !known_endpoint && child.endpoints.len() >= MAX_TRACKED_ENDPOINTS {
                return;
            }
            let known_epoch = child
                .endpoints
                .get(&packet.id)
                .is_some_and(|epochs| epochs.contains(&packet.epoch));
            if !known_epoch {
                let count: usize = child.endpoints.values().map(HashSet::len).sum();
                if count >= MAX_TRACKED_EPOCHS {
                    drop(state);
                    self.lose_down(edge_id, channel, true);
                    channel.close();
                    return;
                }
                // A late packet from an old branch must not erase another epoch
                // whose grant the Host may still need to retire on branch loss.
                child
                    .endpoints
                    .entry(packet.id.clone())
                    .or_default()
                    .insert(packet.epoch.clone());
            }
        }
        let Some(receive) = &self.shared.receive else {
            closes.send(up.as_ref(), &packet);
            drop(state);
            closes.run();
            return;
        };
        drop(state);
        receive(
            edge_id,
            packet,
            Some(Delivery {
                shared: self.shared.clone(),
                edge_id: edge_id.to_owned(),
                leg,
                channel: channel.clone(),
            }),
        );
    }

    pub fn down_closed(&self, edge_id: &str, channel: &Arc<C>) {
        self.lose_down(edge_id, channel, true);
    }

    /// Invalidates in-flight deliveries from this exact input channel. The
    /// permission owner calls this under its own lock and retires its grants in
    /// the same transaction; this method never calls back into that owner.
    pub fn reset_down(&self, edge_id: &str, channel: &Arc<C>) -> bool {
        let mut state = self.shared.lock();
        if state.closed
            || !state
                .down
                .get(edge_id)
                .is_some_and(|child| Arc::ptr_eq(&child.channel, channel))
        {
            return false;
        }
        let leg = state.leg(channel.clone());
        state.down.insert(edge_id.to_owned(), leg);
        true
    }

    fn lose_down(&self, edge_id: &str, channel: &Arc<C>, remove: bool) {
        let mut closes = Closes::new();
        let packets = {
            let mut state = self.shared.lock();
            let Some(child) = state
                .down
                .get(edge_id)
                .filter(|child| Arc::ptr_eq(&child.channel, channel))
            else {
                return;
            };
            let packets: Vec<Packet> = child
                .endpoints
                .iter()
                .flat_map(|(id, epochs)| epochs.iter().map(|epoch| Packet::retirement(id, epoch)))
                .collect();
            if remove {
                state.down.remove(edge_id);
            } else {
                let leg = state.leg(channel.clone());
                state.down.insert(edge_id.to_owned(), leg);
            }
            if self.shared.receive.is_none() {
                for packet in &packets {
                    closes.send(state.up.as_ref(), packet);
                }
            }
            packets
        };
        closes.run();
        if let Some(receive) = &self.shared.receive {
            for packet in packets {
                receive(edge_id, packet, None);
            }
        }
    }

    /// Fans a Host state reply out to every child.
    pub fn reply(&self, packet: &Packet) {
        let mut closes = Closes::new();
        {
            let state = self.shared.lock();
            if state.closed {
                return;
            }
            for child in state.down.values() {
                closes.send(Some(&child.channel), packet);
            }
        }
        closes.run();
    }

    pub fn close(&self) {
        let channels: Vec<Arc<C>> = {
            let mut state = self.shared.lock();
            if state.closed {
                return;
            }
            state.closed = true;
            state
                .up
                .iter()
                .cloned()
                .chain(state.down.values().map(|child| child.channel.clone()))
                .collect()
        };
        for channel in channels {
            channel.close();
        }
    }
}

#[cfg(test)]
mod tests;
