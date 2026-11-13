//! Route assignments, active-topology broadcast, physical paths and the
//! router's signaling hooks.

use std::collections::{HashMap, HashSet};

use piik_ordered::Map;
use piik_protocol::{
    ErrorCode, MediaRouteUpstream, ParticipantRouteAssignment, RouteStatus, ServerMessage,
};
use piik_room::ConnectedPeer;
use piik_route::{CandidateTuple, CommittedEdge, PublicationPlan, RouteSnapshot, UpstreamKind};
use serde_json::{Value, json};

use crate::server::Core;

/// The value of the `edges` map of [`Core::assignments`].
#[derive(Clone)]
struct AssignmentEdge {
    kind: UpstreamKind,
    parent_peer_id: String,
    publication_generation: String,
}

pub(crate) fn empty_assignment() -> ParticipantRouteAssignment {
    ParticipantRouteAssignment {
        upstream: MediaRouteUpstream::None,
        child_peer_ids: Vec::new(),
        sfu_publication_generation: None,
    }
}

pub(crate) fn assignment_for(
    assignments: &HashMap<String, ParticipantRouteAssignment>,
    peer_id: &str,
) -> ParticipantRouteAssignment {
    assignments
        .get(peer_id)
        .cloned()
        .unwrap_or_else(empty_assignment)
}

pub(crate) fn video_connection_id(snapshot: &RouteSnapshot, edge: &CommittedEdge) -> String {
    if snapshot.room_media {
        edge.video_connection_id.clone()
    } else {
        edge.connection_id.clone()
    }
}

fn video_attached(snapshot: &RouteSnapshot, edge: &CommittedEdge) -> bool {
    edge.video_ready
        && edge.video_generation == snapshot.video_generation
        && !snapshot.video_generation.is_empty()
}

/// Returns an empty string when neither peer is the other's child.
pub(crate) fn child_for_pair(snapshot: &RouteSnapshot, first: &str, second: &str) -> String {
    let is_child = |child: &str, parent: &str| {
        snapshot
            .upstream_by_viewer
            .get(&child.to_owned())
            .is_some_and(|edge| edge.kind == UpstreamKind::Peer && edge.parent_peer_id == parent)
    };
    if is_child(first, second) {
        first.to_owned()
    } else if is_child(second, first) {
        second.to_owned()
    } else {
        String::new()
    }
}

impl Core {
    /// Overlays the current attempt when `candidate` is set and otherwise uses
    /// only the committed graph. Peers are the Host first, then the store's
    /// Viewer order; edges keep the snapshot's insertion order, which is the
    /// `childPeerIds` order on the wire.
    pub(crate) fn assignments(
        &self,
        room_id: &str,
        snapshot: &RouteSnapshot,
        candidate: Option<&CandidateTuple>,
        candidate_child_peer_id: &str,
        candidate_publication_generation: &str,
    ) -> HashMap<String, ParticipantRouteAssignment> {
        let mut assignments = HashMap::new();
        let host_peer_id = self.host_peer_of(room_id);
        if host_peer_id.is_empty() {
            return assignments;
        }
        let mut peer_ids = vec![host_peer_id.clone()];
        let mut seen = HashSet::from([host_peer_id.clone()]);
        for peer_id in self.store.get_viewer_peer_ids(room_id) {
            if seen.insert(peer_id.clone()) {
                peer_ids.push(peer_id);
            }
        }
        let mut edges: Map<String, AssignmentEdge> = Map::new();
        for (child, edge) in snapshot.upstream_by_viewer.iter() {
            if !edge.physical_active || (snapshot.room_media && !video_attached(snapshot, edge)) {
                continue;
            }
            edges.set(
                child.clone(),
                AssignmentEdge {
                    kind: edge.kind,
                    parent_peer_id: if edge.kind == UpstreamKind::Peer {
                        edge.parent_peer_id.clone()
                    } else {
                        String::new()
                    },
                    publication_generation: if edge.kind == UpstreamKind::Sfu {
                        edge.publication_generation.clone()
                    } else {
                        String::new()
                    },
                },
            );
        }
        let mut publication_generation = snapshot
            .host_publication
            .as_ref()
            .filter(|p| p.physical_active)
            .map(|p| p.generation.clone())
            .unwrap_or_default();
        if let Some(candidate) = candidate.filter(|_| !candidate_child_peer_id.is_empty()) {
            // The replacement candidate follows the surviving committed children.
            let child = candidate_child_peer_id.to_owned();
            edges.delete(&child);
            if candidate.kind == UpstreamKind::Peer {
                edges.set(
                    child.clone(),
                    AssignmentEdge {
                        kind: UpstreamKind::Peer,
                        parent_peer_id: candidate.parent_peer_id.clone(),
                        publication_generation: String::new(),
                    },
                );
                if snapshot
                    .upstream_by_viewer
                    .get(&child)
                    .is_some_and(|old| old.kind == UpstreamKind::Sfu)
                    && !edges.iter().any(|(_, edge)| edge.kind == UpstreamKind::Sfu)
                {
                    publication_generation.clear();
                }
            } else {
                if candidate.publication != Some(PublicationPlan::Reuse) {
                    let sfu_children: Vec<String> = edges
                        .iter()
                        .filter(|(_, edge)| edge.kind == UpstreamKind::Sfu)
                        .map(|(peer, _)| peer.clone())
                        .collect();
                    for peer in sfu_children {
                        edges.delete(&peer);
                    }
                    publication_generation = candidate_publication_generation.to_owned();
                }
                if !publication_generation.is_empty() {
                    edges.set(
                        child,
                        AssignmentEdge {
                            kind: UpstreamKind::Sfu,
                            parent_peer_id: String::new(),
                            publication_generation: publication_generation.clone(),
                        },
                    );
                }
            }
        }
        let source_reachable = |peer_id: &str| {
            let mut visited = HashSet::new();
            let mut current = peer_id.to_owned();
            loop {
                if current == host_peer_id {
                    return true;
                }
                if !visited.insert(current.clone()) {
                    return false;
                }
                let Some(edge) = edges.get(&current) else {
                    return false;
                };
                if edge.kind == UpstreamKind::Sfu {
                    return publication_generation == edge.publication_generation;
                }
                current = edge.parent_peer_id.clone();
            }
        };
        for peer_id in &peer_ids {
            let edge = edges.get(peer_id);
            let reachable = source_reachable(peer_id);
            let child_peer_ids: Vec<String> = edges
                .iter()
                .filter(|(_, child)| {
                    child.kind == UpstreamKind::Peer && &child.parent_peer_id == peer_id
                })
                .map(|(child, _)| child.clone())
                .collect();
            let upstream = match edge {
                _ if *peer_id == host_peer_id => MediaRouteUpstream::None,
                None => MediaRouteUpstream::None,
                Some(edge) if edge.kind == UpstreamKind::Peer => {
                    MediaRouteUpstream::peer(&edge.parent_peer_id)
                }
                Some(_) if reachable => MediaRouteUpstream::Sfu,
                Some(_) => MediaRouteUpstream::None,
            };
            let sfu_publication_generation = if *peer_id == host_peer_id {
                (!publication_generation.is_empty()).then(|| publication_generation.clone())
            } else {
                edge.filter(|edge| reachable && edge.kind == UpstreamKind::Sfu)
                    .map(|edge| edge.publication_generation.clone())
            };
            assignments.insert(
                peer_id.clone(),
                ParticipantRouteAssignment {
                    upstream,
                    child_peer_ids,
                    sfu_publication_generation,
                },
            );
        }
        assignments
    }

    /// Sends to the Host first, then Viewers in store order, with one
    /// routes-changed call at the end.
    pub(crate) fn broadcast_active(&mut self, room_id: &str) {
        if self.room_media(room_id) && self.video_generation(room_id).is_empty() {
            self.hook_routes_changed(room_id);
            return;
        }
        let Some(snapshot) = self.snapshot(room_id) else {
            return;
        };
        if self.debug_enabled() {
            let routes: Vec<Value> = snapshot
                .upstream_by_viewer
                .iter()
                .map(|(child, edge)| {
                    let parent = if edge.kind == UpstreamKind::Peer {
                        self.debug_peer(room_id, &edge.parent_peer_id)
                    } else {
                        "sfu".to_owned()
                    };
                    json!({
                        "child": self.debug_peer(room_id, child),
                        "parent": parent,
                        "usable": edge.usable,
                        "physicalActive": edge.physical_active,
                    })
                })
                .collect();
            self.emit_debug(
                room_id,
                "active-topology",
                vec![
                    ("revision", json!(snapshot.revision)),
                    ("paused", json!(snapshot.paused)),
                    (
                        "committedHostPublication",
                        json!(
                            snapshot
                                .host_publication
                                .as_ref()
                                .is_some_and(|p| p.physical_active)
                        ),
                    ),
                    ("routes", Value::Array(routes)),
                ],
            );
        }
        let assignments = self.assignments(room_id, &snapshot, None, "", "");
        let mut participants: Vec<ConnectedPeer> = Vec::new();
        participants.extend(self.store.get_connected_host(room_id));
        participants.extend(self.store.get_connected_viewers(room_id));
        for participant in participants {
            self.router_send(
                &participant.session_id,
                ServerMessage::RouteUpdateActive {
                    revision: snapshot.revision,
                    assignment: assignment_for(&assignments, &participant.peer_id),
                },
            );
        }
        self.hook_routes_changed(room_id);
    }

    pub(crate) fn path_is_physical(
        &self,
        room_id: &str,
        snapshot: &RouteSnapshot,
        child: &str,
    ) -> bool {
        let host_peer_id = self.host_peer_of(room_id);
        if host_peer_id.is_empty() {
            return false;
        }
        let mut seen = HashSet::new();
        let mut current = child.to_owned();
        while current != host_peer_id {
            if !seen.insert(current.clone()) {
                return false;
            }
            let Some(edge) = snapshot.upstream_by_viewer.get(&current) else {
                return false;
            };
            if !edge.physical_active || !edge.usable {
                return false;
            }
            if snapshot.room_media && !video_attached(snapshot, edge) {
                return false;
            }
            if edge.kind == UpstreamKind::Sfu {
                return snapshot.host_publication.as_ref().is_some_and(|p| {
                    p.physical_active && p.generation == edge.publication_generation
                });
            }
            if edge.parent_peer_id == host_peer_id {
                return true;
            }
            current = edge.parent_peer_id.clone();
        }
        true
    }

    pub(crate) fn connected_peer(&self, room_id: &str, peer_id: &str) -> Option<ConnectedPeer> {
        if let Some(host) = self.store.get_connected_host(room_id)
            && host.peer_id == peer_id
        {
            return Some(host);
        }
        self.store.get_connected_viewer(room_id, peer_id)
    }

    pub(crate) fn debug_peer(&self, room_id: &str, peer_id: &str) -> String {
        match self.room_rt(room_id) {
            Some(runtime) if peer_id == runtime.host_peer_id => "host".to_owned(),
            Some(runtime) => runtime.controller.as_ref().map_or_else(
                || "viewer-unknown".to_owned(),
                |c| c.diagnostic_participant_label(peer_id),
            ),
            None => "viewer-unknown".to_owned(),
        }
    }

    pub(crate) fn debug_tuple(&self, room_id: &str, tuple: &CandidateTuple) -> String {
        if tuple.kind == UpstreamKind::Peer {
            let mut label = format!("p2p:{}", self.debug_peer(room_id, &tuple.parent_peer_id));
            if tuple.regenerate {
                label.push_str(":regenerate");
            }
            return label;
        }
        format!("sfu:{}", tuple.publication.map_or("", |plan| plan.as_str()))
    }

    /// Notifies only the connected Host.
    pub(crate) fn send_room_error(&mut self, room_id: &str, code: ErrorCode, message: &str) {
        if let Some(host) = self.store.get_connected_host(room_id) {
            self.router_send(
                &host.session_id,
                ServerMessage::Error {
                    code,
                    message: message.to_owned(),
                },
            );
        }
    }

    pub(crate) fn send_viewer_route_status(
        &mut self,
        room_id: &str,
        viewer: &str,
        revision: i64,
        status: RouteStatus,
    ) {
        if self.room_media(room_id)
            && let Some(snapshot) = self.snapshot(room_id)
            && (snapshot.video_generation.is_empty() || snapshot.video_paused)
        {
            if status == RouteStatus::FailedRouteExhausted {
                self.voice_route_failed(room_id, viewer);
            }
            return;
        }
        if let Some(peer) = self.store.get_connected_viewer(room_id, viewer) {
            self.router_send(
                &peer.session_id,
                ServerMessage::RouteStatus { revision, status },
            );
        }
    }

    /// Deduplicates recipients; the first occurrence wins.
    pub(crate) fn send_route_failures(&mut self, room_id: &str, failed: &[String], revision: i64) {
        let mut seen = HashSet::new();
        for peer_id in failed {
            if seen.insert(peer_id.clone()) {
                self.send_viewer_route_status(
                    room_id,
                    peer_id,
                    revision,
                    RouteStatus::FailedRouteExhausted,
                );
            }
        }
    }
}
