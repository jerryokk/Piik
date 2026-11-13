//! Route lifecycle messages and room controller lifecycle.

use piik_protocol::{Role, ViewerRoutePreference};
use piik_route::{
    CandidateGuard, CandidateProof, Controller, EdgeGuard, OperationSnapshot, ParentEdgeGuard,
    ParticipantInput, PublicationGuard, UpstreamKind,
};

use crate::router::pump::RouteReady;
use crate::router::{
    DEFAULT_ROUTE_OPERATION_TIMEOUT_MS, RoomRuntime, RouteParticipant, route_debug,
    video_connection_id,
};
use crate::server::Core;

impl Core {
    pub(crate) fn handle_route_ready(
        &mut self,
        participant: &RouteParticipant,
        message: RouteReady,
    ) {
        if participant.role != Role::Viewer || !message.prepare {
            return;
        }
        let room_id = participant.room_id.clone();
        let revision = message.revision;
        route_debug!(self, &room_id, "route-ready-received",
            "participant" => self.debug_peer(&room_id, &participant.peer_id),
            "revision" => revision);
        let Some(operation) = self.controller(&room_id).and_then(Controller::operation) else {
            return;
        };
        let Some(current) = operation.current.as_ref() else {
            return;
        };
        if operation.child_peer_id != participant.peer_id
            || operation.child_session_id != participant.session_id
            || current.revision != revision
        {
            return;
        }
        let connection_id = current.connection_id.clone();
        let mut pending_matches = false;
        if let Some(runtime) = self.room_rt_mut(&room_id)
            && runtime.room_media
            && let Some(pending) = runtime.pending_media.as_mut()
            && pending.connection_id == connection_id
        {
            pending.video_ready = Some(message);
            pending_matches = true;
        }
        if pending_matches && !self.voice_candidate_ready(&room_id, &connection_id) {
            return;
        }
        let before = self.controller_revision(&room_id);
        let guard = CandidateGuard {
            child_peer_id: participant.peer_id.clone(),
            child_session_id: participant.session_id.clone(),
            revision,
            connection_id,
        };
        let now = self.now();
        let proof = CandidateProof {
            relative_quality_approved: message.quality_approved,
            video_generation: operation.video_generation.clone(),
        };
        let Some(settled) = self.with_commit(&room_id, |c, commit| {
            c.candidate_ready(&guard, now, commit, proof)
        }) else {
            return;
        };
        route_debug!(self, &room_id, "route-ready-settled",
            "participant" => self.debug_peer(&room_id, &participant.peer_id),
            "revision" => revision,
            "accepted" => settled.accepted,
            "committed" => settled.committed,
            "exhausted" => !settled.failed_peer_ids.is_empty());
        self.release_resources(settled.released);
        if settled.committed && settled.accepted {
            self.router.resource_waiters.delete(&room_id);
        }
        if self.controller_revision(&room_id) != before {
            self.broadcast_active(&room_id);
        }
        let revision = self.controller_revision(&room_id);
        self.send_route_failures(&room_id, &settled.failed_peer_ids, revision);
        self.request_pump(&room_id);
    }

    /// Either endpoint may report transport progress or local failure.
    fn owns_candidate_endpoint(
        &self,
        participant: &RouteParticipant,
        operation: &OperationSnapshot,
    ) -> bool {
        if participant.role == Role::Viewer
            && participant.peer_id == operation.child_peer_id
            && participant.session_id == operation.child_session_id
        {
            return true;
        }
        let Some(current) = operation.current.as_ref() else {
            return false;
        };
        if current.tuple.kind == UpstreamKind::Peer
            && participant.peer_id == current.tuple.parent_peer_id
        {
            return self
                .connected_peer(&participant.room_id, &participant.peer_id)
                .is_some_and(|peer| peer.session_id == participant.session_id);
        }
        false
    }

    pub(crate) fn handle_route_transport_connected(
        &mut self,
        participant: &RouteParticipant,
        revision: i64,
        connection_id: &str,
    ) {
        let room_id = participant.room_id.clone();
        let Some(operation) = self.controller(&room_id).and_then(Controller::operation) else {
            return;
        };
        let Some(current) = operation.current.as_ref() else {
            return;
        };
        if current.revision != revision
            || current.connection_id != connection_id
            || current.tuple.kind != UpstreamKind::Peer
            || !self.owns_candidate_endpoint(participant, &operation)
        {
            return;
        }
        let before = self.controller_revision(&room_id);
        let guard = CandidateGuard {
            child_peer_id: operation.child_peer_id.clone(),
            child_session_id: operation.child_session_id.clone(),
            revision,
            connection_id: connection_id.to_owned(),
        };
        let now = self.now();
        let progressed = self
            .ctl(&room_id, |c| c.candidate_transport_connected(&guard, now))
            .unwrap_or_default();
        self.release_resources(progressed.released);
        if self.controller_revision(&room_id) != before {
            self.broadcast_active(&room_id);
        }
        let revision = self.controller_revision(&room_id);
        self.send_route_failures(&room_id, &progressed.failed_peer_ids, revision);
        let after = self.controller(&room_id).and_then(Controller::operation);
        if !progressed.accepted
            || after.is_none_or(|after| after.wake_at_ms != operation.wake_at_ms)
        {
            self.request_pump(&room_id);
        }
    }

    pub(crate) fn handle_route_media_unavailable(
        &mut self,
        participant: &RouteParticipant,
        revision: i64,
    ) {
        if participant.role != Role::Viewer {
            return;
        }
        let room_id = participant.room_id.clone();
        route_debug!(self, &room_id, "sfu-media-unavailable",
            "participant" => self.debug_peer(&room_id, &participant.peer_id),
            "revision" => revision);
        let Some(snapshot) = self.snapshot(&room_id) else {
            return;
        };
        let Some(edge) = snapshot.upstream_by_viewer.get(&participant.peer_id) else {
            return;
        };
        if snapshot.paused
            || snapshot.revision != revision
            || edge.kind != UpstreamKind::Sfu
            || edge.child_session_id != participant.session_id
        {
            return;
        }
        let guard = EdgeGuard {
            child_peer_id: participant.peer_id.clone(),
            child_session_id: participant.session_id.clone(),
            route_revision: snapshot.revision,
            connection_id: edge.connection_id.clone(),
            parent_session_id: String::new(),
        };
        let now = self.now_some();
        self.ctl(&room_id, |c| c.invalidate_edge(guard, now));
        self.request_pump(&room_id);
    }

    pub(crate) fn handle_route_failed(
        &mut self,
        participant: &RouteParticipant,
        revision: i64,
        prepare: bool,
        connection_id: Option<&str>,
    ) {
        let connection_id = connection_id.unwrap_or_default().to_owned();
        let room_id = participant.room_id.clone();
        route_debug!(self, &room_id, "route-failed-received",
            "participant" => self.debug_peer(&room_id, &participant.peer_id),
            "role" => participant.role.as_str(),
            "phase" => if prepare { "prepare" } else { "active" },
            "revision" => revision,
            "hasConnectionId" => !connection_id.is_empty());
        let Some(snapshot) = self.snapshot(&room_id) else {
            return;
        };
        let now = self.now();
        if prepare {
            self.fail_prepared_candidate(participant, &snapshot, revision, &connection_id);
            return;
        }

        // A room revision can advance for another Viewer while this report is
        // in flight. Only an exact still-owned connection may outlive it;
        // identity-free reports retain their snapshot fence.
        if revision > snapshot.revision
            || (revision != snapshot.revision && connection_id.is_empty())
        {
            return;
        }
        if !connection_id.is_empty() {
            let mut logical = if snapshot.room_media {
                String::new()
            } else {
                connection_id.clone()
            };
            for (_, edge) in snapshot.upstream_by_viewer.iter() {
                if edge.kind == UpstreamKind::Peer
                    && edge.parent_peer_id == participant.peer_id
                    && video_connection_id(&snapshot, edge) == connection_id
                {
                    logical = edge.connection_id.clone();
                    break;
                }
            }
            if !logical.is_empty() {
                let guard = ParentEdgeGuard {
                    parent_peer_id: participant.peer_id.clone(),
                    parent_session_id: participant.session_id.clone(),
                    route_revision: snapshot.revision,
                    connection_id: logical,
                };
                if self
                    .ctl(&room_id, |c| {
                        c.invalidate_direct_edge_from_parent(guard, Some(now))
                    })
                    .unwrap_or(false)
                {
                    self.request_pump(&room_id);
                    return;
                }
            }
        }
        if participant.role == Role::Host {
            let Some(publication) = snapshot.host_publication.as_ref() else {
                return;
            };
            if publication.host_session_id != participant.session_id
                || (!connection_id.is_empty() && connection_id != publication.connection_id)
            {
                return;
            }
            let guard = PublicationGuard {
                host_session_id: participant.session_id.clone(),
                route_revision: snapshot.revision,
                generation: publication.generation.clone(),
                connection_id: publication.connection_id.clone(),
            };
            self.ctl(&room_id, |c| {
                c.invalidate_host_publication(guard.clone(), Some(now))
            });
            if !snapshot.room_media {
                let released = self
                    .ctl(&room_id, |c| {
                        let guard = PublicationGuard {
                            route_revision: c.revision(),
                            ..guard
                        };
                        c.retire_host_publication(guard)
                    })
                    .unwrap_or_default();
                self.release_resources(released);
            }
            self.broadcast_active(&room_id);
            self.request_pump(&room_id);
            return;
        }
        let Some(edge) = snapshot.upstream_by_viewer.get(&participant.peer_id) else {
            return;
        };
        if edge.child_session_id != participant.session_id
            || (!connection_id.is_empty() && connection_id != video_connection_id(&snapshot, edge))
        {
            return;
        }
        let guard = EdgeGuard {
            child_peer_id: participant.peer_id.clone(),
            child_session_id: participant.session_id.clone(),
            route_revision: snapshot.revision,
            connection_id: edge.connection_id.clone(),
            parent_session_id: if edge.kind == UpstreamKind::Peer {
                edge.parent_session_id.clone()
            } else {
                String::new()
            },
        };
        let invalidated = self
            .ctl(&room_id, |c| c.invalidate_edge(guard, Some(now)))
            .unwrap_or(false);
        if invalidated && edge.usable && snapshot.paused {
            self.hook_routes_changed(&room_id);
        }
        self.request_pump(&room_id);
    }

    fn fail_prepared_candidate(
        &mut self,
        participant: &RouteParticipant,
        snapshot: &piik_route::RouteSnapshot,
        revision: i64,
        connection_id: &str,
    ) {
        let room_id = participant.room_id.clone();
        let Some(operation) = snapshot.operation.as_ref() else {
            return;
        };
        let Some(current) = operation.current.as_ref() else {
            return;
        };
        if current.revision != revision {
            return;
        }
        let owns_endpoint = self.owns_candidate_endpoint(participant, operation);
        let owns_publication = current.tuple.kind == UpstreamKind::Sfu
            && participant.role == Role::Host
            && participant.peer_id == self.host_peer_of(&room_id)
            && self
                .store
                .get_connected_host(&room_id)
                .is_some_and(|host| host.session_id == participant.session_id);
        if !owns_endpoint && !owns_publication {
            return;
        }
        if !connection_id.is_empty() && connection_id != current.connection_id {
            return;
        }
        let guard = CandidateGuard {
            child_peer_id: operation.child_peer_id.clone(),
            child_session_id: operation.child_session_id.clone(),
            revision: current.revision,
            connection_id: current.connection_id.clone(),
        };
        let now = self.now();
        let settled = self
            .ctl(&room_id, |c| c.candidate_failed(&guard, now))
            .unwrap_or_default();
        self.release_resources(settled.released);
        if self.controller_revision(&room_id) != snapshot.revision {
            self.broadcast_active(&room_id);
        }
        let revision = self.controller_revision(&room_id);
        self.send_route_failures(&room_id, &settled.failed_peer_ids, revision);
        self.request_pump(&room_id);
    }

    pub(crate) fn refresh_sfu(&mut self, participant: &RouteParticipant, revision: i64) {
        if self
            .controller(&participant.room_id)
            .is_some_and(|c| revision <= c.revision())
        {
            self.send_fresh_sfu_config(participant);
        }
    }

    pub(crate) fn router_disconnect_participant(
        &mut self,
        room_id: &str,
        peer_id: &str,
        session_id: &str,
    ) {
        if self
            .ctl(room_id, |c| c.disconnect_session(peer_id, session_id))
            .unwrap_or(false)
        {
            self.request_pump(room_id);
        }
    }

    pub(crate) fn remove_viewer(&mut self, room_id: &str, peer_id: &str) {
        if let Some(runtime) = self.room_rt_mut(room_id) {
            runtime.viewer_settings.delete(&peer_id.to_owned());
        }
        let now = self.now_some();
        if self
            .ctl(room_id, |c| c.confirm_departure(peer_id, now))
            .unwrap_or(false)
        {
            self.request_pump(room_id);
        }
    }

    /// Preserves Viewer settings and the room's revision sequence across shares.
    pub(crate) fn stop_room(&mut self, room_id: &str) {
        let Some(runtime) = self.room_rt(room_id) else {
            return;
        };
        if runtime.room_media && runtime.controller.is_some() {
            self.set_video_publication(room_id, "", false);
            return;
        }
        let revisions = runtime.revisions.clone();
        let settings =
            std::mem::take(&mut self.room_rt_mut(room_id).expect("room").viewer_settings);
        self.clear_room(room_id);
        let id = self.router.next_id();
        let mut replacement = RoomRuntime::new(id, revisions);
        replacement.viewer_settings = settings;
        self.router.rooms.set(room_id.to_owned(), replacement);
    }

    pub(crate) fn delete_room(&mut self, room_id: &str) {
        self.clear_room(room_id);
    }

    /// Connects Viewers in store order.
    pub(crate) fn create_controller(&mut self, room_id: &str, host: &RouteParticipant) {
        let policy = host.route_policy;
        let operation_timeout_ms = match &self.router.sfu {
            Some(fallback) if fallback.prepare_timeout_ms != 0 => fallback.prepare_timeout_ms,
            _ => DEFAULT_ROUTE_OPERATION_TIMEOUT_MS,
        };
        let revisions = self.room_rt(room_id).expect("room").revisions.clone();
        let mut controller = Controller::new(piik_route::Options {
            host_peer_id: host.peer_id.clone(),
            debug_room_id: room_id.to_owned(),
            revisions: Some(revisions),
            debug_log: self.controller_debug_sink(),
            endpoint_media_copy_capacity: self.router.capacity,
            operation_timeout_ms,
            sfu_enabled: self.router.sfu.is_some() && !policy.is_some_and(|p| p.peer_only),
            sfu_only: self.router.sfu_only,
            quality_convergence_enabled: policy.is_some_and(|p| p.topology_optimization),
            nat_prediction_enabled: policy.is_some_and(|p| p.nat_prediction),
        });
        let now = self.now();
        let room_media = self.room_media(room_id);
        if room_media {
            let generation = self.video_generation(room_id);
            controller.set_video_publication(&generation, false, now);
        }
        controller.upsert_participant(
            ParticipantInput {
                peer_id: host.peer_id.clone(),
                role: Role::Host,
                session_id: host.session_id.clone(),
                effective_downstream_capacity: self.router.capacity,
                route_preference: None,
            },
            Some(now),
        );
        let id = self.router.next_id();
        {
            let runtime = self.room_rt_mut(room_id).expect("room");
            runtime.controller = Some(controller);
            runtime.controller_id = id;
        }
        for viewer in self.store.get_connected_viewers(room_id) {
            if room_media
                && self.video_generation(room_id).is_empty()
                && !self.voice_allowed(room_id, &viewer.peer_id)
            {
                continue;
            }
            let mut settings = self.viewer_settings(room_id, &viewer.peer_id);
            if !self.viewer_route_allowed(room_id, settings.preference) {
                settings.preference = ViewerRoutePreference::Auto;
                self.set_viewer_settings(room_id, &viewer.peer_id, settings.clone());
                self.send_viewer_route_preference(room_id, &viewer.peer_id, true);
            }
            self.ctl(room_id, |c| {
                c.upsert_participant(
                    ParticipantInput {
                        peer_id: viewer.peer_id.clone(),
                        role: Role::Viewer,
                        session_id: viewer.session_id.clone(),
                        effective_downstream_capacity: settings.capacity,
                        route_preference: Some(settings.preference),
                    },
                    Some(now),
                );
                c.set_relay_volunteer(
                    &viewer.peer_id,
                    &viewer.session_id,
                    settings.volunteer,
                    Some(now),
                );
            });
        }
    }

    /// Drains publications in admission order.
    pub(crate) fn clear_room(&mut self, room_id: &str) {
        if self.room_rt(room_id).is_none() {
            return;
        }
        self.clear_deadline(room_id);
        let controller = {
            let runtime = self.room_rt_mut(room_id).expect("room");
            runtime.requested = false;
            runtime.controller.take()
        };
        if let Some(mut controller) = controller {
            let released = controller.dispose();
            self.release_resources(released);
        }
        self.router.rooms.delete(&room_id.to_owned());
        self.router.resource_waiters.delete(&room_id.to_owned());
        let fences = match self.router.sfu.as_mut() {
            Some(fallback) => fallback.admission.begin_drain_room(room_id),
            None => Vec::new(),
        };
        for fence in fences {
            self.schedule_sfu_publication_drain(&fence);
        }
    }
}
