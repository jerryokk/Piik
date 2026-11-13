//! Peer signal authorization and Viewer route preference and relay capacity.

use piik_protocol::{
    CandidateSignalOrigin, MediaRouteUpstream, ServerMessage, ViewerRoutePreference,
};
use piik_route::{AdoptDirectConnectionInput, EdgeGuard, Transport, UpstreamKind};
use serde_json::json;

use crate::router::graph::{assignment_for, child_for_pair, video_connection_id};
use crate::router::{
    ActiveViewerMediaEdge, CANDIDATE_CONNECTION_ID_PREFIX, PeerSignalInput, RouteParticipant,
    SignalAuthorization, route_debug,
};
use crate::server::Core;

impl Core {
    pub(crate) fn is_active_peer_parent_of(
        &self,
        room_id: &str,
        parent: &str,
        child: &str,
    ) -> bool {
        self.resolve_active_viewer_media_edge(room_id, child)
            .is_some_and(|edge| edge.upstream == MediaRouteUpstream::peer(parent))
    }

    /// A retained child connection can restart while its parent repairs its
    /// own ingress; this inbound edge must still own the exact endpoint
    /// sessions and connection.
    pub(crate) fn peer_restart_authorized(&mut self, input: &PeerSignalInput) -> bool {
        let Some(snapshot) = self.snapshot(&input.room_id) else {
            return false;
        };
        snapshot
            .upstream_by_viewer
            .get(&input.source_peer_id)
            .is_some_and(|edge| {
                edge.kind == UpstreamKind::Peer && edge.parent_peer_id == input.target_peer_id
            })
            && self.peer_signal_authorization(input) == SignalAuthorization::Allowed
    }

    pub(crate) fn resolve_active_viewer_media_edge(
        &self,
        room_id: &str,
        child_peer_id: &str,
    ) -> Option<ActiveViewerMediaEdge> {
        let snapshot = self.snapshot(room_id)?;
        let edge = snapshot.upstream_by_viewer.get(&child_peer_id.to_owned())?;
        if !self.path_is_physical(room_id, &snapshot, child_peer_id) {
            return None;
        }
        let upstream = if edge.kind == UpstreamKind::Peer {
            MediaRouteUpstream::peer(&edge.parent_peer_id)
        } else {
            MediaRouteUpstream::Sfu
        };
        Some(ActiveViewerMediaEdge {
            revision: snapshot.revision,
            connection_id: video_connection_id(&snapshot, edge),
            upstream,
        })
    }

    pub(crate) fn get_viewer_route_upstream(
        &self,
        room_id: &str,
        viewer: &str,
    ) -> MediaRouteUpstream {
        match self.snapshot(room_id) {
            Some(snapshot) => {
                assignment_for(&self.assignments(room_id, &snapshot, None, "", ""), viewer).upstream
            }
            None => MediaRouteUpstream::None,
        }
    }

    /// Resolves candidate and retained media by connection, since both may
    /// belong to the same parent/child pair during overlap.
    pub(crate) fn peer_signal_authorization(
        &mut self,
        input: &PeerSignalInput,
    ) -> SignalAuthorization {
        let preparation = input.signal_kind == "punch" || input.punched;
        let Some(snapshot) = self.snapshot(&input.room_id) else {
            return if preparation {
                SignalAuthorization::Denied
            } else {
                SignalAuthorization::Unassigned
            };
        };
        if let Some(op) = snapshot.operation.as_ref()
            && let Some(current) = op.current.as_ref()
            && current.tuple.kind == UpstreamKind::Peer
        {
            let parent = self.connected_peer(&input.room_id, &current.tuple.parent_peer_id);
            let child = self
                .store
                .get_connected_viewer(&input.room_id, &op.child_peer_id);
            let parent_to_child = input.source_peer_id == current.tuple.parent_peer_id
                && input.target_peer_id == op.child_peer_id;
            let child_to_parent = input.source_peer_id == op.child_peer_id
                && input.target_peer_id == current.tuple.parent_peer_id;
            if (parent_to_child || child_to_parent) && input.connection_id == current.connection_id
            {
                let (parent_session, child_session) = if parent_to_child {
                    (&input.source_session_id, &input.target_session_id)
                } else {
                    (&input.target_session_id, &input.source_session_id)
                };
                if parent.is_some_and(|p| &p.session_id == parent_session)
                    && child.is_some_and(|c| &c.session_id == child_session)
                {
                    if preparation {
                        if current.connection_attempt.is_none_or(|a| a.current <= 1) {
                            return SignalAuthorization::Denied;
                        }
                        if (input.punched && parent_to_child && input.description_type == "offer")
                            || (parent_to_child && matches!(input.punch_phase, "prepare" | "start"))
                            || (child_to_parent
                                && matches!(input.punch_phase, "ready" | "complete"))
                        {
                            return SignalAuthorization::Probe;
                        }
                        return SignalAuthorization::Denied;
                    }
                    if input.signal_kind == "candidate"
                        || (parent_to_child && input.description_type == "offer")
                        || (child_to_parent && input.description_type == "answer")
                    {
                        return SignalAuthorization::Probe;
                    }
                }
                return SignalAuthorization::Denied;
            }
        }
        // Preparation is never an active-edge capability or an unassigned signal.
        if preparation {
            return SignalAuthorization::Denied;
        }
        let child_peer_id = child_for_pair(&snapshot, &input.source_peer_id, &input.target_peer_id);
        if child_peer_id.is_empty() {
            return SignalAuthorization::Unassigned;
        }
        let Some(edge) = snapshot.upstream_by_viewer.get(&child_peer_id) else {
            return SignalAuthorization::Unassigned;
        };
        if edge.kind != UpstreamKind::Peer || !edge.physical_active {
            return SignalAuthorization::Unassigned;
        }
        let parent_to_child = input.source_peer_id == edge.parent_peer_id;
        let sessions_match = (parent_to_child
            && input.source_session_id == edge.parent_session_id
            && input.target_session_id == edge.child_session_id)
            || (!parent_to_child
                && input.source_session_id == edge.child_session_id
                && input.target_session_id == edge.parent_session_id);
        if !sessions_match {
            return SignalAuthorization::Denied;
        }
        let active_connection_id = video_connection_id(&snapshot, edge);
        if self.room_media(&input.room_id)
            && (!edge.video_ready
                || edge.video_generation != snapshot.video_generation
                || snapshot.video_generation.is_empty())
        {
            return SignalAuthorization::Denied;
        }
        if edge.transport == Transport::Direct
            && parent_to_child
            && input.signal_kind == "description"
            && input.description_type == "offer"
            && input.connection_id != active_connection_id
        {
            if input
                .connection_id
                .starts_with(CANDIDATE_CONNECTION_ID_PREFIX)
            {
                return SignalAuthorization::Denied;
            }
            let now = self.now();
            let guard = EdgeGuard {
                child_peer_id: child_peer_id.clone(),
                child_session_id: edge.child_session_id.clone(),
                parent_session_id: edge.parent_session_id.clone(),
                route_revision: snapshot.revision,
                connection_id: edge.connection_id.clone(),
            };
            let adopted = self
                .ctl(&input.room_id, |c| {
                    c.adopt_direct_connection(
                        AdoptDirectConnectionInput {
                            edge: guard,
                            new_connection_id: input.connection_id.clone(),
                        },
                        now,
                    )
                })
                .unwrap_or_default();
            if adopted.accepted {
                self.release_resources(adopted.released);
                if adopted.active_revision != snapshot.revision {
                    self.broadcast_active(&input.room_id);
                    self.request_pump(&input.room_id);
                }
                return SignalAuthorization::Allowed;
            }
            return SignalAuthorization::Denied;
        }
        if input.connection_id == active_connection_id {
            SignalAuthorization::Allowed
        } else {
            SignalAuthorization::Denied
        }
    }

    pub(crate) fn debug_peer_signal(
        &self,
        input: &PeerSignalInput,
        candidate_origin: Option<CandidateSignalOrigin>,
        authorization: SignalAuthorization,
    ) {
        if !self.debug_enabled() {
            return;
        }
        let authorization = match authorization {
            SignalAuthorization::Unassigned => json!("assignment"),
            SignalAuthorization::Denied => json!(false),
            SignalAuthorization::Allowed => json!(true),
            SignalAuthorization::Probe => json!("probe"),
        };
        let description = (!input.description_type.is_empty()).then_some(input.description_type);
        self.emit_debug(
            &input.room_id,
            "peer-signal",
            vec![
                (
                    "source",
                    json!(self.debug_peer(&input.room_id, &input.source_peer_id)),
                ),
                (
                    "target",
                    json!(self.debug_peer(&input.room_id, &input.target_peer_id)),
                ),
                ("signalKind", json!(input.signal_kind)),
                (
                    "candidateOrigin",
                    json!(candidate_origin.map(|o| o.as_str())),
                ),
                ("descriptionType", json!(description)),
                ("authorization", authorization),
            ],
        );
    }

    pub(crate) fn viewer_route_allowed(
        &self,
        room_id: &str,
        preference: ViewerRoutePreference,
    ) -> bool {
        if let Some(controller) = self.controller(room_id) {
            return controller.allows_viewer_route_preference(preference);
        }
        (preference != ViewerRoutePreference::Sfu || self.router.sfu.is_some())
            && (preference != ViewerRoutePreference::Peer || !self.router.sfu_only)
    }

    pub(crate) fn set_viewer_route_preference(
        &mut self,
        participant: &RouteParticipant,
        preference: ViewerRoutePreference,
    ) {
        let room_id = &participant.room_id;
        self.ensure_room(room_id);
        let mut settings = self.viewer_settings(room_id, &participant.peer_id);
        let accepted = self.viewer_route_allowed(room_id, preference);
        settings.preference = if accepted {
            preference
        } else {
            ViewerRoutePreference::Auto
        };
        self.set_viewer_settings(room_id, &participant.peer_id, settings);
        if accepted && self.has_controller(room_id) {
            let before = self.controller_revision(room_id);
            let now = self.now_some();
            let released = self
                .ctl(room_id, |c| {
                    c.set_viewer_route_preference(
                        &participant.peer_id,
                        &participant.session_id,
                        preference,
                        now,
                    )
                })
                .unwrap_or_default();
            self.release_resources(released);
            if self.controller_revision(room_id) != before {
                self.broadcast_active(room_id);
            }
        }
        self.send_viewer_route_preference(room_id, &participant.peer_id, accepted);
        if accepted {
            self.request_pump(room_id);
        }
    }

    pub(crate) fn send_viewer_route_preference(
        &mut self,
        room_id: &str,
        peer_id: &str,
        accepted: bool,
    ) {
        if self.room_rt(room_id).is_none() {
            return;
        }
        let preference = self.viewer_settings(room_id, peer_id).preference;
        if let Some(viewer) = self.store.get_connected_viewer(room_id, peer_id) {
            self.router_send(
                &viewer.session_id,
                ServerMessage::ViewerRoutePreference {
                    preference,
                    accepted,
                },
            );
        }
    }

    pub(crate) fn set_viewer_relay_capacity(&mut self, participant: &RouteParticipant, edges: i64) {
        let room_id = &participant.room_id;
        self.ensure_room(room_id);
        route_debug!(self, room_id, "relay-capacity-received",
            "participant" => self.debug_peer(room_id, &participant.peer_id),
            "downstreamEdges" => edges);
        let mut settings = self.viewer_settings(room_id, &participant.peer_id);
        settings.capacity = edges;
        self.set_viewer_settings(room_id, &participant.peer_id, settings);
        let now = self.now_some();
        if self
            .ctl(room_id, |c| {
                c.set_effective_capacity(&participant.peer_id, &participant.session_id, edges, now)
            })
            .unwrap_or(false)
        {
            self.request_pump(room_id);
        }
    }
}
