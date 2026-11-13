//! Participant lifecycle: joins, session changes, departures, capacity and
//! external fact changes.

use super::*;

impl Controller {
    /// Adds or updates a participant, aborting an operation that used a
    /// replaced session; a viewer without a usable route records a `join`
    /// demand when `now_ms` is set. Panics on a Host/role inconsistency.
    pub fn upsert_participant(
        &mut self,
        input: ParticipantInput,
        now_ms: Option<i64>,
    ) -> Vec<ResourceRef> {
        if (input.peer_id == self.host_peer_id) != (input.role == Role::Host) {
            panic!("Route Host identity is inconsistent");
        }
        let existing = self.participant(&input.peer_id).cloned();
        let volunteer = existing.as_ref().is_some_and(|current| {
            current.session_id == input.session_id && current.relay_volunteer
        });
        let capacity = self.effective_capacity(input.effective_downstream_capacity, volunteer);
        let mut released = Vec::new();
        if existing
            .as_ref()
            .is_some_and(|current| current.role != input.role)
        {
            panic!("Route role cannot change");
        }
        let peer_id = input.peer_id.clone();
        if let Some(current) = existing {
            let mut changed = false;
            let previous_session_id = current.session_id.clone();
            if previous_session_id != input.session_id {
                self.participant_mut(&peer_id).unwrap().relay_volunteer = false;
                self.clear_opportunities_for_session_change(&peer_id, input.role);
                self.clear_quality_for_participant(&peer_id);
                if self.operation_uses_participant_session(&peer_id) {
                    released.extend(self.abort_operation(now_ms, RejectionBucket::Stale));
                }
                self.direct_continuations.delete(&peer_id);
                self.clear_sfu_bootstrap_for_demand(&peer_id);
                self.participant_mut(&peer_id).unwrap().session_id = input.session_id.clone();
                self.rebind_committed_session(&peer_id, &input.session_id);
                changed = true;
            }
            let participant = self.participant_mut(&peer_id).unwrap();
            if participant.departure_confirmed {
                participant.departure_confirmed = false;
                changed = true;
            }
            participant.reported_downstream_capacity = input.effective_downstream_capacity;
            if participant.effective_downstream_capacity != capacity {
                participant.effective_downstream_capacity = capacity;
                self.record_capacity_reduction(&peer_id, now_ms);
                changed = true;
            }
            if changed {
                self.touch_facts();
                route_debug!(self, "participant-updated",
                    "participant" => self.debug_peer(&peer_id),
                    "capacity" => capacity,
                    "sessionChanged" => previous_session_id != input.session_id);
            }
            if input.role == Role::Viewer {
                if let Some(now) = now_ms
                    && !self.usable_route(&peer_id)
                {
                    self.record_demand(&peer_id, now, DemandReason::Join);
                }
                released.extend(self.set_viewer_route_preference(
                    &peer_id,
                    &input.session_id,
                    input.route_preference,
                    now_ms,
                ));
            }
            return released;
        }
        // New participants append to the scheduling order.
        self.participants.set(
            peer_id.clone(),
            Participant {
                peer_id: peer_id.clone(),
                role: input.role,
                session_id: input.session_id,
                departure_confirmed: false,
                join_order: self.next_join_order,
                effective_downstream_capacity: capacity,
                reported_downstream_capacity: input.effective_downstream_capacity,
                relay_volunteer: false,
                availability_exhausted: false,
                consumed_candidate_opportunities: Map::new(),
                bootstrap_failure_reported: false,
                sfu_first_at_next_route: false,
                route_preference: input.route_preference,
            },
        );
        self.next_join_order += 1;
        route_debug!(self, "participant-joined",
            "participant" => self.debug_peer(&peer_id),
            "role" => input.role,
            "capacity" => capacity);
        if input.role == Role::Viewer
            && let Some(now) = now_ms
        {
            self.record_demand(&peer_id, now, DemandReason::Join);
        }
        self.touch_facts();
        released
    }

    /// Changes eligibility, not physical edge health. The ordinary serial
    /// handoff keeps current media until commit or strict-policy exhaustion;
    /// selecting Auto again reopens only this Viewer's opportunities.
    pub fn set_viewer_route_preference(
        &mut self,
        peer_id: &str,
        session_id: &str,
        preference: impl Into<Option<ViewerRoutePreference>>,
        now_ms: Option<i64>,
    ) -> Vec<ResourceRef> {
        let preference = preference.into();
        let Some(current) = self.participant(peer_id) else {
            return Vec::new();
        };
        if current.role != Role::Viewer
            || !current.session_is(session_id)
            || current.route_preference == preference
        {
            return Vec::new();
        }
        let mut released = Vec::new();
        if self
            .operation
            .as_ref()
            .is_some_and(|op| op.child_peer_id == peer_id || op.demand_peer_id == peer_id)
        {
            released = self.abort_operation(now_ms, RejectionBucket::Aborted);
        }
        let current = self.participant_mut(peer_id).unwrap();
        current.route_preference = preference;
        current.consumed_candidate_opportunities.clear();
        current.availability_exhausted = false;
        current.bootstrap_failure_reported = false;
        self.direct_continuations.delete(&peer_id.to_owned());
        self.clear_sfu_bootstrap_for_demand(peer_id);
        self.touch_facts();
        route_debug!(self, "viewer-route-preference",
            "participant" => self.debug_peer(peer_id),
            "preference" => preference.map_or("", |preference| preference.as_str()));
        if let Some(now) = now_ms {
            let reason = self.route_demand_reason(peer_id);
            self.record_demand(peer_id, now, reason);
        }
        released
    }

    /// Moves the Host to a new peer ID (the same ID delegates to
    /// `upsert_participant`), tearing down the whole graph and returning every
    /// SFU subscription, the publication and the aborted reservation,
    /// deduplicated.
    pub fn rebind_host_identity(
        &mut self,
        peer_id: &str,
        session_id: &str,
        now_ms: Option<i64>,
    ) -> Vec<ResourceRef> {
        let previous_peer_id = self.host_peer_id.clone();
        if peer_id == previous_peer_id {
            return self.upsert_participant(
                ParticipantInput {
                    peer_id: peer_id.to_owned(),
                    role: Role::Host,
                    session_id: session_id.to_owned(),
                    effective_downstream_capacity: self.endpoint_media_copy_capacity,
                    route_preference: None,
                },
                now_ms,
            );
        }
        if self.participants.has(&peer_id.to_owned()) {
            panic!("Route Host identity collides with another participant");
        }
        if self
            .participant(&previous_peer_id)
            .is_none_or(|host| host.role != Role::Host)
        {
            panic!("Route Host identity is unavailable");
        }

        // Aborted reservation, then SFU edges in map order, then the
        // publication, deduplicated.
        let mut released = ResourceSet::default();
        if self.operation.is_some() {
            released.add(self.abort_operation(now_ms, RejectionBucket::Stale));
        }
        for (_, edge) in self.upstream_by_viewer.iter() {
            if edge.kind == UpstreamKind::Sfu && edge.physical_active {
                released.add(edge.resource.clone());
            }
        }
        if let Some(publication) = &self.host_publication
            && publication.physical_active
        {
            released.add(publication.resource.clone());
        }
        self.upstream_by_viewer.clear();
        self.host_publication = None;
        self.route_timings.clear();
        self.quality_observations.clear();
        self.sender_quality_observations.clear();
        self.sender_quality_baselines.clear();
        self.sender_quality_regeneration_blocks.clear();
        self.quality_baselines_pending.clear();
        self.sfu_publisher_quality_observation = None;
        self.direct_continuations.clear();
        self.retiring_publication_generations.clear();
        self.sfu_bootstrap_intent = None;
        self.consumed_sfu_bootstrap_opportunities.clear();
        self.root_convergence_root_peer_id.clear();
        // Demand is re-recorded in participants order.
        for key in self.participants.keys() {
            let current = self.participants.get_mut(&key).unwrap();
            current.availability_exhausted = false;
            current.bootstrap_failure_reported = false;
            current.sfu_first_at_next_route = false;
            current.consumed_candidate_opportunities.clear();
            let demand = current.role == Role::Viewer && !current.session_id.is_empty();
            if let (true, Some(now)) = (demand, now_ms) {
                self.record_demand(&key, now, DemandReason::EdgeUnavailable);
            }
        }

        // Delete + set moves the Host to the tail of the insertion order
        // while it keeps its join order.
        let mut host = self.participant(&previous_peer_id).unwrap().clone();
        self.participants.delete(&previous_peer_id);
        host.peer_id = peer_id.to_owned();
        host.session_id = session_id.to_owned();
        host.relay_volunteer = false;
        host.effective_downstream_capacity =
            self.effective_capacity(host.reported_downstream_capacity, false);
        host.departure_confirmed = false;
        self.participants.set(peer_id.to_owned(), host);
        self.host_peer_id = peer_id.to_owned();
        self.revision = self.allocate_revision();
        self.touch_facts();
        self.assert_graph();
        released.into_vec()
    }

    /// Marks the participant's exact session as gone (grace state, no
    /// revision change); false when the session does not match.
    pub fn disconnect_session(&mut self, peer_id: &str, session_id: &str) -> bool {
        let Some(current) = self.participant(peer_id) else {
            return false;
        };
        if !current.session_is(session_id) {
            return false;
        }
        let role = current.role;
        self.clear_opportunities_for_session_change(peer_id, role);
        self.participant_mut(peer_id).unwrap().session_id.clear();
        self.clear_quality_for_participant(peer_id);
        self.direct_continuations.delete(&peer_id.to_owned());
        self.clear_sfu_bootstrap_for_demand(peer_id);
        self.touch_facts();
        route_debug!(self, "participant-disconnected", "participant" => self.debug_peer(peer_id));
        true
    }

    /// Finalises a viewer's departure (capacity 0, demand recorded for its
    /// children when `now_ms` is set); false for the Host or an unknown peer;
    /// idempotent.
    pub fn confirm_departure(&mut self, peer_id: &str, now_ms: Option<i64>) -> bool {
        let Some(current) = self.participant(peer_id) else {
            return false;
        };
        if current.role == Role::Host {
            return false;
        }
        self.route_timings.remove(peer_id);
        let current = self.participant(peer_id).unwrap();
        if current.departure_confirmed
            && current.session_id.is_empty()
            && current.effective_downstream_capacity == 0
        {
            return true;
        }
        let role = current.role;
        self.clear_opportunities_for_session_change(peer_id, role);
        self.participant_mut(peer_id).unwrap().session_id.clear();
        self.clear_quality_for_participant(peer_id);
        let current = self.participant_mut(peer_id).unwrap();
        current.departure_confirmed = true;
        current.effective_downstream_capacity = 0;
        self.direct_continuations.delete(&peer_id.to_owned());
        self.clear_sfu_bootstrap_for_demand(peer_id);
        if let Some(now) = now_ms {
            for child_peer_id in self.children_of(peer_id) {
                self.record_demand(&child_peer_id, now, DemandReason::ParentDeparted);
            }
        }
        self.touch_facts();
        route_debug!(self, "participant-departed", "participant" => self.debug_peer(peer_id));
        true
    }

    /// Clamps and stores the downstream capacity for the matching session;
    /// true even when unchanged (facts are touched only on change).
    pub fn set_effective_capacity(
        &mut self,
        peer_id: &str,
        session_id: &str,
        value: i64,
        now_ms: Option<i64>,
    ) -> bool {
        let Some(current) = self.participant(peer_id) else {
            return false;
        };
        if !current.session_is(session_id) {
            return false;
        }
        let capacity = self.effective_capacity(value, current.relay_volunteer);
        let current = self.participant_mut(peer_id).unwrap();
        current.reported_downstream_capacity = value;
        if current.effective_downstream_capacity == capacity {
            return true;
        }
        current.effective_downstream_capacity = capacity;
        self.record_capacity_reduction(peer_id, now_ms);
        self.touch_facts();
        route_debug!(self, "capacity-updated",
            "participant" => self.debug_peer(peer_id),
            "capacity" => capacity);
        true
    }

    /// Reopens every SFU opportunity after an external fact change (for
    /// example SFU usage decrease) and bumps the fact version.
    pub fn touch_external_facts(&mut self) {
        self.clear_sfu_candidate_opportunities();
        self.sfu_bootstrap_intent = None;
        self.touch_facts();
    }
}
