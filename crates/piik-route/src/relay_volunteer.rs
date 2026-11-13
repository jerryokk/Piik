//! Session-scoped relay volunteering.

use piik_protocol::{
    EndpointMediaCopyPhase, MAX_VIEWERS_PER_ROOM_LIMIT, endpoint_media_copy_limit,
};

use crate::controller::Participant;
use crate::{Controller, DemandReason};

/// The steady fanout of a willing ordinary peer. The existing single
/// operation may reserve one additional physical copy.
pub const MAX_RELAY_VOLUNTEER_CHILDREN: i64 = MAX_VIEWERS_PER_ROOM_LIMIT as i64;

impl Controller {
    /// Changes policy for this exact authenticated session. The endpoint's
    /// reported capacity remains the physical capability ceiling.
    pub fn set_relay_volunteer(
        &mut self,
        peer_id: &str,
        session_id: &str,
        enabled: bool,
        now_ms: Option<i64>,
    ) -> bool {
        let Some(current) = self.participant(peer_id) else {
            return false;
        };
        if !current.session_is(session_id) {
            return false;
        }
        if current.relay_volunteer == enabled {
            return true;
        }
        let capacity = self.effective_capacity(current.reported_downstream_capacity, enabled);
        let current = self.participant_mut(peer_id).unwrap();
        current.relay_volunteer = enabled;
        current.effective_downstream_capacity = capacity;
        self.record_capacity_reduction(peer_id, now_ms);
        self.touch_facts();
        true
    }

    pub(crate) fn record_capacity_reduction(&mut self, peer_id: &str, now_ms: Option<i64>) {
        if let Some(now) = now_ms {
            for child_peer_id in self.overflow_peer_children(peer_id) {
                self.record_demand(&child_peer_id, now, DemandReason::CapacityReduction);
            }
        }
    }

    pub(crate) fn transition_copy_limit(&self, producer: &Participant) -> i64 {
        if producer.relay_volunteer
            && producer.effective_downstream_capacity > self.endpoint_media_copy_capacity
        {
            if producer.effective_downstream_capacity > 3 {
                return producer.effective_downstream_capacity + 1;
            }
            return 3;
        }
        endpoint_media_copy_limit(
            self.endpoint_media_copy_capacity,
            EndpointMediaCopyPhase::Transition,
        )
        .expect("constructor validated the endpoint copy capacity")
    }
}
