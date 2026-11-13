//! Fixture edges, hydration and snapshot accessors.

use super::harness::res;
use crate::*;

pub(crate) struct EdgeSeed {
    kind: UpstreamKind,
    parent_peer_id: String,
    publication_generation: String,
    transport: Transport,
    connection_id: String,
    resource: Option<ResourceRef>,
}

pub(crate) fn peer_edge(parent_peer_id: &str, connection_id: &str) -> EdgeSeed {
    EdgeSeed {
        kind: UpstreamKind::Peer,
        parent_peer_id: parent_peer_id.to_owned(),
        publication_generation: String::new(),
        transport: Transport::Direct,
        connection_id: connection_id.to_owned(),
        resource: None,
    }
}

pub(crate) fn sfu_edge(generation: &str, connection_id: &str, resource: &str) -> EdgeSeed {
    EdgeSeed {
        kind: UpstreamKind::Sfu,
        parent_peer_id: String::new(),
        publication_generation: generation.to_owned(),
        transport: Transport::Sfu,
        connection_id: connection_id.to_owned(),
        resource: Some(res(resource)),
    }
}

impl Controller {
    /// Installs a committed edge directly (test fixture).
    pub(crate) fn hydrate_edge(&mut self, child_peer_id: &str, edge: EdgeSeed) {
        assert!(
            self.operation.is_none(),
            "Cannot hydrate while an operation is active"
        );
        self.assert_viewer(child_peer_id);
        let child_session_id = self.participant(child_peer_id).unwrap().session_id.clone();
        assert!(
            !child_session_id.is_empty(),
            "Route child has no current session"
        );
        let opportunity_key = self.candidate_opportunity_base(
            child_peer_id,
            &CandidatePlan::bare(CandidateTuple::bare(edge.kind, &edge.parent_peer_id)),
        );
        let parent_session_id = if edge.kind == UpstreamKind::Peer {
            self.participant(&edge.parent_peer_id)
                .map(|parent| parent.session_id.clone())
                .unwrap_or_default()
        } else {
            String::new()
        };
        self.upstream_by_viewer.set(
            child_peer_id.to_owned(),
            CommittedEdge {
                kind: edge.kind,
                child_session_id,
                parent_peer_id: edge.parent_peer_id,
                parent_session_id,
                publication_generation: edge.publication_generation,
                transport: edge.transport,
                connection_id: edge.connection_id,
                usable: true,
                physical_active: true,
                video_generation: String::new(),
                video_connection_id: String::new(),
                video_ready: false,
                resource: edge.resource,
                opportunity_key,
            },
        );
        self.require_sender_quality_baseline(child_peer_id, false);
        self.assert_graph();
    }

    /// Installs the Host publication directly (test fixture).
    pub(crate) fn hydrate_host_publication(
        &mut self,
        generation: &str,
        resource: ResourceRef,
        connection_id: &str,
    ) {
        assert!(
            self.operation.is_none(),
            "Cannot hydrate while an operation is active"
        );
        let host_session_id = self.host_session_id();
        assert!(
            !host_session_id.is_empty(),
            "Route Host has no current session"
        );
        let connection_id = if connection_id.is_empty() {
            format!("publication:{generation}")
        } else {
            connection_id.to_owned()
        };
        self.host_publication = Some(HostPublication {
            generation: generation.to_owned(),
            host_session_id,
            connection_id,
            usable: true,
            physical_active: true,
            resource: Some(resource),
        });
        self.assert_graph();
    }

    /// Mutable edge access for fixtures that mark existing video evidence.
    pub(crate) fn edge_for_test(&mut self, child_peer_id: &str) -> &mut CommittedEdge {
        self.edge_mut(child_peer_id).unwrap()
    }
}

pub(crate) fn peer_tuple(parent_peer_id: &str) -> CandidateTuple {
    CandidateTuple::peer(parent_peer_id)
}

pub(crate) fn regenerate_tuple(parent_peer_id: &str) -> CandidateTuple {
    CandidateTuple {
        regenerate: true,
        ..CandidateTuple::peer(parent_peer_id)
    }
}

pub(crate) fn sfu_tuple(publication: PublicationPlan) -> CandidateTuple {
    CandidateTuple::sfu(publication)
}

pub(crate) fn tuples(operation: &OperationSnapshot) -> Vec<CandidateTuple> {
    tuples_from(operation, 0)
}

pub(crate) fn tuples_from(operation: &OperationSnapshot, from: usize) -> Vec<CandidateTuple> {
    operation.candidates[from..]
        .iter()
        .map(|candidate| candidate.tuple.clone())
        .collect()
}

pub(crate) fn tuple_kinds(operation: &OperationSnapshot) -> Vec<UpstreamKind> {
    operation
        .candidates
        .iter()
        .map(|candidate| candidate.tuple.kind)
        .collect()
}

pub(crate) fn has_tuple(
    operation: &OperationSnapshot,
    matches: impl Fn(&CandidateTuple) -> bool,
) -> bool {
    tuples(operation).iter().any(matches)
}

pub(crate) fn is_sfu(tuple: &CandidateTuple) -> bool {
    tuple.kind == UpstreamKind::Sfu
}

pub(crate) fn is_peer(tuple: &CandidateTuple) -> bool {
    tuple.kind == UpstreamKind::Peer
}

#[track_caller]
pub(crate) fn edge_of(routes: &Controller, peer_id: &str) -> CommittedEdge {
    routes
        .snapshot()
        .upstream_by_viewer
        .get(&peer_id.to_owned())
        .cloned()
        .unwrap_or_else(|| panic!("expected an upstream edge for {peer_id:?}"))
}

pub(crate) fn has_edge(routes: &Controller, peer_id: &str) -> bool {
    routes
        .snapshot()
        .upstream_by_viewer
        .has(&peer_id.to_owned())
}

/// Ports `toEqual` on an endpoint transition without retirement; an empty
/// producer means the TS object omitted it.
#[track_caller]
pub(crate) fn expect_transition(got: &EndpointTransition, kind: TransitionKind, producer: &str) {
    assert_eq!(got.kind, kind);
    if producer.is_empty() {
        assert_eq!(got.producer_peer_id, None);
    } else {
        assert_eq!(got.producer_peer_id.as_deref(), Some(producer));
    }
    if kind != TransitionKind::BoundedGap {
        assert!(
            got.retire.is_none(),
            "expected no retirement for {kind:?} transition"
        );
    }
}
