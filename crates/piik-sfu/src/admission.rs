//! Reserved, committed and draining SFU capacity.
//!
//! The signaling owner serializes access; capacity stays charged until
//! physical retirement is confirmed through [`Admission::complete_drain`] or
//! [`Admission::complete_subscription_drain`].

use std::collections::HashMap;

use piik_ordered::Map;
use piik_protocol::{MAX_SAFE_INTEGER, valid_opaque_id};

/// Identifies one publication generation within a room share.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct ResourceFence {
    pub room_id: String,
    pub share_generation: String,
    pub publication_generation: String,
}

impl ResourceFence {
    /// Reports whether the fence carries a wire-valid room ID and opaque
    /// generations, as untrusted media requests must.
    pub fn is_valid(&self) -> bool {
        valid_room_id(&self.room_id)
            && valid_opaque_id(&self.share_generation)
            && valid_opaque_id(&self.publication_generation)
    }

    fn key(&self) -> String {
        format!("{}\0{}", self.share_generation, self.publication_generation)
    }
}

/// Identifies one Viewer's connection to a publication.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct SubscriptionFence {
    pub resource: ResourceFence,
    pub viewer_peer_id: String,
    pub connection_id: String,
}

impl SubscriptionFence {
    fn key(&self) -> (String, String) {
        (self.viewer_peer_id.clone(), self.connection_id.clone())
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    pub ingress: usize,
    pub egress: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionOptions {
    pub ingress_capacity: usize,
    pub egress_capacity: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Reserved,
    Committed,
    Draining,
}

#[derive(Debug, Clone)]
struct SubscriptionEntry {
    fence: SubscriptionFence,
    state: State,
}

#[derive(Debug, Clone)]
struct PublicationEntry {
    fence: ResourceFence,
    state: State,
    // Counted and looked up by exact connection; iteration order does not
    // affect drain order.
    subscriptions: HashMap<(String, String), SubscriptionEntry>,
}

impl PublicationEntry {
    fn mark_draining(&mut self) {
        self.state = State::Draining;
        for subscription in self.subscriptions.values_mut() {
            subscription.state = State::Draining;
        }
    }
}

/// Publication insertion order determines the router's physical drain order.
type RoomPublications = Map<String, PublicationEntry>;

#[derive(Debug)]
pub struct Admission {
    // Room insertion order is preserved by `begin_drain_all`.
    rooms: Map<String, RoomPublications>,
    ingress_capacity: usize,
    egress_capacity: usize,
    ingress_in_use: usize,
    egress_in_use: usize,
}

impl Admission {
    /// Requires capacities from validated configuration.
    ///
    /// # Panics
    /// When either capacity is not a positive safe integer.
    pub fn new(options: AdmissionOptions) -> Self {
        assert_positive_safe_integer(options.ingress_capacity, "SFU ingress capacity");
        assert_positive_safe_integer(options.egress_capacity, "SFU egress capacity");
        Self {
            rooms: Map::new(),
            ingress_capacity: options.ingress_capacity,
            egress_capacity: options.egress_capacity,
            ingress_in_use: 0,
            egress_in_use: 0,
        }
    }

    pub fn reserve_publication(&mut self, fence: &ResourceFence) -> bool {
        assert_fence(fence);
        let key = fence.key();
        let room = self.rooms.get(&fence.room_id);
        if let Some(existing) = room.and_then(|room| room.get(&key)) {
            return existing.state != State::Draining && existing.fence == *fence;
        }
        if self.ingress_in_use >= self.ingress_capacity {
            return false;
        }
        if room.is_some_and(|room| room.iter().any(|(_, p)| p.state == State::Reserved)) {
            return false;
        }
        let entry = PublicationEntry {
            fence: fence.clone(),
            state: State::Reserved,
            subscriptions: HashMap::new(),
        };
        match self.rooms.get_mut(&fence.room_id) {
            Some(room) => room.set(key, entry),
            None => {
                let mut room = RoomPublications::new();
                room.set(key, entry);
                self.rooms.set(fence.room_id.clone(), room);
            }
        }
        self.ingress_in_use += 1;
        true
    }

    pub fn reserve_subscription(&mut self, fence: &SubscriptionFence) -> bool {
        assert_subscription_fence(fence);
        let egress_full = self.egress_in_use >= self.egress_capacity;
        let Some(publication) = self.publication_mut(&fence.resource) else {
            return false;
        };
        if publication.state == State::Draining {
            return false;
        }
        let key = fence.key();
        if let Some(existing) = publication.subscriptions.get(&key) {
            return existing.fence == *fence && existing.state != State::Draining;
        }
        if egress_full {
            return false;
        }
        publication.subscriptions.insert(
            key,
            SubscriptionEntry {
                fence: fence.clone(),
                state: State::Reserved,
            },
        );
        self.egress_in_use += 1;
        true
    }

    /// Promotes the reserved publication and subscriptions and returns older
    /// committed publications for physical retirement, in insertion order.
    /// `None` reports refusal; a repeated commit succeeds without scheduling
    /// another drain.
    pub fn commit_publication(&mut self, fence: &ResourceFence) -> Option<Vec<ResourceFence>> {
        assert_fence(fence);
        let key = fence.key();
        let room = self.rooms.get_mut(&fence.room_id)?;
        let publication = room.get(&key).filter(|p| p.fence == *fence)?;
        if publication.state == State::Committed {
            return Some(Vec::new());
        }
        if publication.state != State::Reserved
            || !publication
                .subscriptions
                .values()
                .any(|s| s.state == State::Reserved)
        {
            return None;
        }

        let mut draining = Vec::new();
        for other_key in room.keys() {
            if other_key == key {
                continue;
            }
            if let Some(other) = room.get_mut(&other_key)
                && other.state == State::Committed
            {
                other.mark_draining();
                draining.push(other.fence.clone());
            }
        }
        let publication = room.get_mut(&key)?;
        publication.state = State::Committed;
        for subscription in publication.subscriptions.values_mut() {
            if subscription.state == State::Reserved {
                subscription.state = State::Committed;
            }
        }
        Some(draining)
    }

    pub fn commit_subscription(&mut self, fence: &SubscriptionFence) -> bool {
        assert_subscription_fence(fence);
        let Some(publication) = self.publication_mut(&fence.resource) else {
            return false;
        };
        if publication.state != State::Committed {
            return false;
        }
        let Some(subscription) = publication
            .subscriptions
            .get_mut(&fence.key())
            .filter(|s| s.fence == *fence)
        else {
            return false;
        };
        match subscription.state {
            State::Committed => true,
            State::Reserved => {
                subscription.state = State::Committed;
                true
            }
            State::Draining => false,
        }
    }

    pub fn begin_subscription_drain(&mut self, fence: &SubscriptionFence) -> bool {
        assert_subscription_fence(fence);
        let Some(subscription) = self
            .publication_mut(&fence.resource)
            .and_then(|p| p.subscriptions.get_mut(&fence.key()))
            .filter(|s| s.fence == *fence)
        else {
            return false;
        };
        subscription.state = State::Draining;
        true
    }

    pub fn begin_drain(&mut self, fence: &ResourceFence) -> bool {
        assert_fence(fence);
        let Some(publication) = self.publication_mut(fence) else {
            return false;
        };
        publication.mark_draining();
        true
    }

    pub fn has_publication(&self, fence: &ResourceFence) -> bool {
        self.publication(fence)
            .is_some_and(|p| p.state != State::Draining)
    }

    pub fn has_subscription(&self, fence: &SubscriptionFence) -> bool {
        let Some(publication) = self
            .publication(&fence.resource)
            .filter(|p| p.state != State::Draining)
        else {
            return false;
        };
        publication
            .subscriptions
            .get(&fence.key())
            .is_some_and(|s| s.fence == *fence && s.state != State::Draining)
    }

    /// In-process closure revokes this exact connection; no reusable token
    /// remains.
    pub fn complete_subscription_drain(&mut self, fence: &SubscriptionFence) -> bool {
        let key = fence.key();
        let Some(publication) = self.publication_mut(&fence.resource) else {
            return false;
        };
        if !publication
            .subscriptions
            .get(&key)
            .is_some_and(|s| s.fence == *fence && s.state == State::Draining)
        {
            return false;
        }
        publication.subscriptions.remove(&key);
        self.subtract_egress(1);
        true
    }

    pub fn complete_drain(&mut self, fence: &ResourceFence) -> bool {
        assert_fence(fence);
        let key = fence.key();
        let Some(room) = self.rooms.get_mut(&fence.room_id) else {
            return false;
        };
        let Some(subscriptions) = room
            .get(&key)
            .filter(|p| p.state == State::Draining && p.fence == *fence)
            .map(|p| p.subscriptions.len())
        else {
            return false;
        };
        room.delete(&key);
        let empty = room.is_empty();
        self.ingress_in_use = self
            .ingress_in_use
            .checked_sub(1)
            .expect("SFU resource accounting underflow");
        self.subtract_egress(subscriptions);
        if empty {
            self.rooms.delete(&fence.room_id);
        }
        true
    }

    /// Marks every publication of the room draining and returns them in
    /// insertion order, including those already draining.
    ///
    /// # Panics
    /// When `room_id` is empty.
    pub fn begin_drain_room(&mut self, room_id: &str) -> Vec<ResourceFence> {
        assert!(!room_id.is_empty(), "SFU resource room ID is invalid");
        let Some(room) = self.rooms.get_mut(&room_id.to_owned()) else {
            return Vec::new();
        };
        let mut fences = Vec::with_capacity(room.len());
        for key in room.keys() {
            if let Some(publication) = room.get_mut(&key) {
                publication.mark_draining();
                fences.push(publication.fence.clone());
            }
        }
        fences
    }

    pub fn begin_drain_all(&mut self) -> Vec<ResourceFence> {
        let mut fences = Vec::new();
        for room_id in self.rooms.keys() {
            fences.extend(self.begin_drain_room(&room_id));
        }
        fences
    }

    pub fn usage(&self) -> Usage {
        Usage {
            ingress: self.ingress_in_use,
            egress: self.egress_in_use,
        }
    }

    fn publication(&self, fence: &ResourceFence) -> Option<&PublicationEntry> {
        self.rooms
            .get(&fence.room_id)?
            .get(&fence.key())
            .filter(|p| p.fence == *fence)
    }

    fn publication_mut(&mut self, fence: &ResourceFence) -> Option<&mut PublicationEntry> {
        self.rooms
            .get_mut(&fence.room_id)?
            .get_mut(&fence.key())
            .filter(|p| p.fence == *fence)
    }

    fn subtract_egress(&mut self, count: usize) {
        self.egress_in_use = self
            .egress_in_use
            .checked_sub(count)
            .expect("SFU resource accounting underflow");
    }
}

fn assert_fence(fence: &ResourceFence) {
    assert!(
        !fence.room_id.is_empty()
            && !fence.share_generation.is_empty()
            && !fence.publication_generation.is_empty(),
        "SFU resource fence is invalid"
    );
}

fn assert_subscription_fence(fence: &SubscriptionFence) {
    assert_fence(&fence.resource);
    assert!(
        !fence.viewer_peer_id.is_empty(),
        "SFU subscription fence is invalid"
    );
}

fn assert_positive_safe_integer(value: usize, name: &str) {
    assert!(
        value > 0 && u64::try_from(value).is_ok_and(|v| v <= MAX_SAFE_INTEGER as u64),
        "{name} must be a positive safe integer"
    );
}

/// `^[1-9][0-9]{0,11}$`.
fn valid_room_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=12).contains(&bytes.len()) && bytes[0] != b'0' && bytes.iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests;
