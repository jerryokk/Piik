//! Committed-graph mutation, retirement and invariants.

use std::collections::HashSet;

use piik_protocol::Role;

use super::super::{CommittedEdge, Controller, RejectionBucket, ResourceRef, UpstreamKind};
use crate::relay_volunteer::MAX_RELAY_VOLUNTEER_CHILDREN;

impl Controller {
    /// A confirmed departure ends media authority even while acquisition is
    /// paused or descendants still need the old graph anchor.
    pub(crate) fn retire_departed_edges(&mut self, now_ms: i64) -> Vec<ResourceRef> {
        let mut released = Vec::new();
        let mut changed = false;
        for peer_id in self.upstream_by_viewer.keys() {
            let Some(edge) = self.edge(&peer_id) else {
                continue;
            };
            let departed = self
                .participant(&peer_id)
                .is_some_and(|current| current.departure_confirmed);
            if !departed || !edge.physical_active {
                continue;
            }
            if !changed {
                // Active and prepared assignments share one revision sequence.
                // Retire the candidate before publishing a newer active graph;
                // its late ACK must not commit an older revision or restore
                // the departed route.
                released.extend(self.abort_operation(Some(now_ms), RejectionBucket::Aborted));
            }
            changed = true;
            if self.edge(&peer_id).unwrap().kind == UpstreamKind::Sfu {
                let retain = self.retains_anchor(&peer_id);
                self.retire_sfu_edge(&peer_id, &mut released, retain);
            } else {
                let edge = self.edge_mut(&peer_id).unwrap();
                edge.physical_active = false;
                edge.usable = false;
            }
        }
        if changed {
            if !self.has_sfu_subscribers() && self.host_publication.is_some() {
                self.clear_sfu_quality();
                let publication = self.host_publication.take().unwrap();
                if publication.physical_active {
                    released.extend(publication.resource);
                }
            }
            self.prune_retiring_sfu_anchors();
            self.revision = self.allocate_revision();
            self.touch_facts();
        }
        released
    }

    /// Removes departed viewers without children to a fixpoint, in
    /// participants order, and drops an orphaned publication.
    pub(crate) fn prune_departed_leaves(&mut self, released: &mut Vec<ResourceRef>) -> Vec<String> {
        let mut removed = Vec::new();
        let mut changed = true;
        while changed {
            changed = false;
            for peer_id in self.participants.keys() {
                // Live iteration: an entry deleted earlier in this pass is skipped.
                let Some(current) = self.participant(&peer_id) else {
                    continue;
                };
                if current.role != Role::Viewer
                    || !current.departure_confirmed
                    || !self.children_of(&peer_id).is_empty()
                {
                    continue;
                }
                if self
                    .edge(&peer_id)
                    .is_some_and(|edge| edge.kind == UpstreamKind::Sfu)
                {
                    self.retire_sfu_edge(&peer_id, released, false);
                } else {
                    self.upstream_by_viewer.delete(&peer_id);
                }
                self.clear_quality_for_participant(&peer_id);
                self.participants.delete(&peer_id);
                self.route_timings.remove(&peer_id);
                self.direct_continuations.delete(&peer_id);
                self.clear_sfu_bootstrap_for_demand(&peer_id);
                removed.push(peer_id);
                changed = true;
            }
        }
        if !removed.is_empty() {
            if !self.has_sfu_subscribers() && self.host_publication.is_some() {
                self.clear_sfu_quality();
                let publication = self.host_publication.take().unwrap();
                if publication.physical_active {
                    released.extend(publication.resource);
                }
            }
            self.revision = self.allocate_revision();
            self.touch_facts();
            self.prune_retiring_sfu_anchors();
        }
        removed
    }

    pub(crate) fn retire_invalid_operation_edge(
        &mut self,
        child_peer_id: &str,
        released: &mut Vec<ResourceRef>,
    ) -> bool {
        let Some(edge) = self.edge(child_peer_id) else {
            return false;
        };
        if !self.edge_requires_move(child_peer_id, edge) {
            return false;
        }
        let kind = edge.kind;
        if kind == UpstreamKind::Sfu {
            let retain = self.retains_anchor(child_peer_id);
            self.retire_sfu_edge(child_peer_id, released, retain);
            self.prune_retiring_sfu_anchors();
        } else {
            let edge = self.edge_mut(child_peer_id).unwrap();
            edge.physical_active = false;
            edge.usable = false;
        }
        if kind == UpstreamKind::Sfu
            && !self.has_sfu_subscribers()
            && self.host_publication.is_some()
        {
            let publication = self.host_publication.take().unwrap();
            if publication.physical_active {
                released.extend(publication.resource);
            }
        }
        true
    }

    pub(crate) fn edge_requires_move(&self, child_peer_id: &str, edge: &CommittedEdge) -> bool {
        if !edge.usable
            || !edge.physical_active
            || !self.allows_incoming(child_peer_id, edge.kind)
            || self.parent_route_retired(edge)
        {
            return true;
        }
        if edge.kind == UpstreamKind::Sfu {
            return self.host_publication.as_ref().is_none_or(|publication| {
                !publication.usable
                    || !publication.physical_active
                    || publication.generation != edge.publication_generation
            });
        }
        if self
            .participant(&edge.parent_peer_id)
            .is_some_and(|parent| parent.departure_confirmed)
        {
            return true;
        }
        self.overflow_peer_children(&edge.parent_peer_id)
            .iter()
            .any(|overflowing| overflowing == child_peer_id)
    }

    /// Keeps a relay's subtree while it repairs its ingress. Once that ingress
    /// is actually retired, its direct children need their own handoff.
    pub(crate) fn parent_route_retired(&self, edge: &CommittedEdge) -> bool {
        if edge.kind != UpstreamKind::Peer || edge.parent_peer_id == self.host_peer_id {
            return false;
        }
        self.edge(&edge.parent_peer_id)
            .is_some_and(|parent_edge| !parent_edge.physical_active)
    }

    /// Preserves the reconnecting peer's edges and publication, updating them
    /// to its new session ID.
    pub(crate) fn rebind_committed_session(&mut self, peer_id: &str, session_id: &str) {
        let host_generation = self
            .host_publication
            .as_ref()
            .map(|publication| publication.generation.clone());
        let is_host = peer_id == self.host_peer_id;
        for child_peer_id in self.upstream_by_viewer.keys() {
            let edge = self.upstream_by_viewer.get_mut(&child_peer_id).unwrap();
            let mut involved = false;
            if child_peer_id == peer_id {
                edge.child_session_id = session_id.to_owned();
                involved = true;
            }
            if edge.kind == UpstreamKind::Peer && edge.parent_peer_id == peer_id {
                edge.parent_session_id = session_id.to_owned();
                involved = true;
            }
            // A failed optional attachment belongs to its endpoint sessions.
            // Proven video and the retained room transport survive
            // reauthentication.
            if edge.kind == UpstreamKind::Sfu
                && is_host
                && host_generation.as_deref() == Some(edge.publication_generation.as_str())
            {
                involved = true;
            }
            if involved && !edge.video_generation.is_empty() && !edge.video_ready {
                edge.video_generation.clear();
            }
        }
        if is_host && let Some(publication) = self.host_publication.as_mut() {
            publication.host_session_id = session_id.to_owned();
        }
    }

    pub(crate) fn remove_publication_generation(&mut self, generation: &str) -> Vec<ResourceRef> {
        let mut released = Vec::new();
        for viewer_peer_id in self.upstream_by_viewer.keys() {
            let Some(edge) = self.edge(&viewer_peer_id) else {
                continue;
            };
            if edge.kind != UpstreamKind::Sfu || edge.publication_generation != generation {
                continue;
            }
            let retain = self.retains_anchor(&viewer_peer_id);
            self.retire_sfu_edge(&viewer_peer_id, &mut released, retain);
        }
        if self
            .host_publication
            .as_ref()
            .is_some_and(|publication| publication.generation == generation)
        {
            let publication = self.host_publication.take().unwrap();
            if publication.physical_active {
                released.extend(publication.resource);
            }
        }
        self.prune_retiring_sfu_anchors();
        released
    }

    /// Keeps a retiring SFU edge while it still has peer children.
    pub(crate) fn retains_anchor(&self, viewer_peer_id: &str) -> bool {
        !self.children_of(viewer_peer_id).is_empty()
    }

    /// Releases the subscription only while it is still physically active,
    /// then keeps the edge as an anchor for attached peer descendants or
    /// deletes it.
    pub(crate) fn retire_sfu_edge(
        &mut self,
        viewer_peer_id: &str,
        released: &mut Vec<ResourceRef>,
        retain_anchor: bool,
    ) {
        let edge = self.edge(viewer_peer_id).unwrap();
        if edge.physical_active {
            released.extend(edge.resource.clone());
        }
        self.clear_quality_for_participant(viewer_peer_id);
        if retain_anchor {
            let edge = self.edge_mut(viewer_peer_id).unwrap();
            edge.usable = false;
            edge.physical_active = false;
            let generation = edge.publication_generation.clone();
            self.retiring_publication_generations.set(generation, ());
        } else {
            self.upstream_by_viewer.delete(&viewer_peer_id.to_owned());
        }
    }

    /// Reports whether an inactive SFU edge still references the generation.
    pub(crate) fn has_retiring_anchor(&self, generation: &str) -> bool {
        self.upstream_by_viewer.iter().any(|(_, edge)| {
            edge.kind == UpstreamKind::Sfu
                && !edge.physical_active
                && edge.publication_generation == generation
        })
    }

    pub(crate) fn prune_retiring_sfu_anchors(&mut self) {
        let mut removed = true;
        while removed {
            removed = false;
            for viewer_peer_id in self.upstream_by_viewer.keys() {
                let Some(edge) = self.edge(&viewer_peer_id) else {
                    continue;
                };
                if edge.kind == UpstreamKind::Sfu
                    && !edge.physical_active
                    && self
                        .retiring_publication_generations
                        .has(&edge.publication_generation)
                    && self.children_of(&viewer_peer_id).is_empty()
                {
                    self.upstream_by_viewer.delete(&viewer_peer_id);
                    removed = true;
                }
            }
        }
        for generation in self.retiring_publication_generations.keys() {
            if !self.has_retiring_anchor(&generation) {
                self.retiring_publication_generations.delete(&generation);
            }
        }
    }

    /// Panics on programming errors. Edge insertion order and participant
    /// order determine which violation is reported first.
    pub(crate) fn assert_graph(&self) {
        if self
            .participant(&self.host_peer_id)
            .is_none_or(|host| host.role != Role::Host)
        {
            panic!("Route Host is missing");
        }
        let publication_generation = self
            .host_publication
            .as_ref()
            .map(|publication| publication.generation.as_str());
        for (child, edge) in self.upstream_by_viewer.iter() {
            self.assert_viewer(child);
            if edge.kind == UpstreamKind::Peer && !self.participants.has(&edge.parent_peer_id) {
                panic!("Route parent is missing");
            }
            if edge.kind == UpstreamKind::Sfu
                && edge.physical_active
                && publication_generation != Some(edge.publication_generation.as_str())
            {
                panic!("SFU publication is stale");
            }
            if edge.kind == UpstreamKind::Sfu
                && !edge.physical_active
                && !self
                    .retiring_publication_generations
                    .has(&edge.publication_generation)
            {
                panic!("Retiring SFU publication is unknown");
            }
            let mut seen = HashSet::new();
            let mut current = child.clone();
            while current != self.host_peer_id {
                if !seen.insert(current.clone()) {
                    panic!("Peer route contains a cycle");
                }
                let Some(current_edge) = self.edge(&current) else {
                    panic!("Route is not source-reachable");
                };
                if current_edge.kind == UpstreamKind::Sfu {
                    let unknown = if current_edge.physical_active {
                        publication_generation != Some(current_edge.publication_generation.as_str())
                    } else {
                        !self
                            .retiring_publication_generations
                            .has(&current_edge.publication_generation)
                    };
                    if unknown {
                        panic!("Route terminates at an unknown SFU publication");
                    }
                    break;
                }
                current = current_edge.parent_peer_id.clone();
            }
        }
        for (peer_id, _) in self.participants.iter() {
            // Admission uses the participant's effective policy. Existing
            // resources drain through serial handoffs after withdrawal or
            // capacity reduction.
            if self.used_slots(peer_id) > MAX_RELAY_VOLUNTEER_CHILDREN {
                panic!("Committed route exceeds endpoint capacity");
            }
        }
        for (generation, _) in self.retiring_publication_generations.iter() {
            if !self.has_retiring_anchor(generation) {
                panic!("Retiring SFU publication has no anchor");
            }
        }
    }

    pub(crate) fn assert_viewer(&self, peer_id: &str) {
        if self
            .participant(peer_id)
            .is_none_or(|current| current.role != Role::Viewer)
        {
            panic!("Route child must be a Viewer");
        }
    }

    pub(crate) fn has_sfu_subscribers(&self) -> bool {
        self.sfu_subscriber_count() > 0
    }

    pub(crate) fn sfu_subscriber_count(&self) -> usize {
        self.upstream_by_viewer
            .iter()
            .filter(|(_, edge)| edge.kind == UpstreamKind::Sfu && edge.physical_active)
            .count()
    }

    /// SFU subscriptions in map order, then the publication.
    pub(crate) fn committed_resources(&self) -> Vec<ResourceRef> {
        let mut resources: Vec<ResourceRef> = self
            .upstream_by_viewer
            .iter()
            .filter(|(_, edge)| edge.kind == UpstreamKind::Sfu && edge.physical_active)
            .filter_map(|(_, edge)| edge.resource.clone())
            .collect();
        if let Some(publication) = &self.host_publication
            && publication.physical_active
        {
            resources.extend(publication.resource.clone());
        }
        resources
    }
}
