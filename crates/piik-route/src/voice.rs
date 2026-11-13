//! Room-voice listener permission over the same graph.

use std::collections::HashSet;

use piik_protocol::Role;

use crate::{Controller, DemandReason, UpstreamKind};

impl Controller {
    /// Supplies current room permission, not a second topology. A permission
    /// change opens the existing bounded availability opportunities only for
    /// listeners whose route needs repair. Healthy video stays committed while
    /// the ordinary serial operation proves a permitted replacement.
    pub fn set_voice_listeners<S: AsRef<str>>(&mut self, peer_ids: &[S], now_ms: i64) -> bool {
        if !self.room_media {
            return false;
        }
        let next: HashSet<String> = peer_ids.iter().map(|id| id.as_ref().to_owned()).collect();
        if self.voice_listeners == next {
            return false;
        }
        let previous = std::mem::replace(&mut self.voice_listeners, next);
        for peer_id in self.participants.keys() {
            let current = self.participant(&peer_id).unwrap();
            if current.role != Role::Viewer
                || (previous.contains(&peer_id) == self.voice_listeners.contains(&peer_id)
                    && !self.voice_route_missing(&peer_id))
            {
                continue;
            }
            let current = self.participant_mut(&peer_id).unwrap();
            current.consumed_candidate_opportunities.clear();
            current.availability_exhausted = false;
            current.bootstrap_failure_reported = false;
            self.direct_continuations.delete(&peer_id);
            self.clear_sfu_bootstrap_for_demand(&peer_id);
            if self.voice_route_missing(&peer_id) {
                self.record_demand(&peer_id, now_ms, DemandReason::EdgeUnavailable);
            }
        }
        self.touch_facts();
        true
    }

    pub(crate) fn voice_parent_allowed(&self, child_peer_id: &str, parent_peer_id: &str) -> bool {
        !self.voice_listeners.contains(child_peer_id) || self.voice_source_usable(parent_peer_id)
    }

    pub(crate) fn voice_route_missing(&self, peer_id: &str) -> bool {
        self.voice_listeners.contains(peer_id) && !self.voice_source_usable(peer_id)
    }

    /// The permitted path ends at the listening Host or the admitted SFU
    /// junction. Checking every hop prevents an approved relay with an
    /// unapproved upstream from becoming a candidate that can never receive
    /// the room's voice publications.
    fn voice_source_usable(&self, peer_id: &str) -> bool {
        let mut seen = HashSet::new();
        let mut peer_id = peer_id.to_owned();
        loop {
            if !self.voice_listeners.contains(&peer_id) || !seen.insert(peer_id.clone()) {
                return false;
            }
            let Some(participant) = self.participant(&peer_id) else {
                return false;
            };
            if participant.session_id.is_empty() || participant.departure_confirmed {
                return false;
            }
            if peer_id == self.host_peer_id {
                return true;
            }
            let Some(edge) = self.edge(&peer_id) else {
                return false;
            };
            if !edge.usable || !edge.physical_active {
                return false;
            }
            if edge.kind == UpstreamKind::Sfu {
                return self.publication_serves(edge);
            }
            peer_id = edge.parent_peer_id.clone();
        }
    }
}
