//! Route quality evidence projection and convergence.

use super::*;

// TS: it.each([undefined, false, true]); the two representable outcomes come
// from a real quality-convergence candidate.
#[test]
fn router_projects_exact_endpoint_copy_context_from_current_snapshot() {
    let template = committed_edge_template();
    let edge = |parent_peer_id: &str, connection_id: &str| {
        let mut edge = template.clone();
        edge.kind = UpstreamKind::Peer;
        edge.child_session_id = "child-session".to_owned();
        edge.parent_peer_id = parent_peer_id.to_owned();
        edge.parent_session_id = "parent-session".to_owned();
        edge.transport = Transport::Direct;
        edge.connection_id = connection_id.to_owned();
        edge.usable = true;
        edge.physical_active = true;
        edge
    };
    let edges = |entries: &[(&str, &str, &str)]| {
        let mut upstream = Map::new();
        for (child, parent, connection) in entries {
            upstream.set((*child).to_owned(), edge(parent, connection));
        }
        upstream
    };
    let operation = |tuple: CandidateTuple, transition: EndpointTransition| OperationSnapshot {
        video_generation: String::new(),
        attach_connection_id: String::new(),
        child_peer_id: "candidate".to_owned(),
        child_session_id: "candidate-session".to_owned(),
        demand_peer_id: "candidate".to_owned(),
        reason: DemandReason::QualityConvergence,
        base_revision: 7,
        fact_version: 1,
        candidates: vec![CandidatePlan {
            tuple: tuple.clone(),
            endpoint_transition: transition,
        }],
        cursor: 0,
        deadline_at_ms: 20_000,
        wake_at_ms: 20_000,
        current: Some(CurrentAttempt {
            tuple,
            revision: 8,
            connection_id: "candidate-connection".to_owned(),
            connection_attempt: None,
        }),
    };
    let snapshot = |upstream: Map<String, CommittedEdge>,
                    operation: Option<OperationSnapshot>,
                    publication: Option<HostPublication>| RouteSnapshot {
        room_media: false,
        video_generation: String::new(),
        video_paused: false,
        revision: 7,
        paused: false,
        fact_version: 1,
        upstream_by_viewer: upstream,
        host_publication: publication,
        operation,
    };
    let project = |state: &RouteSnapshot, observed: &str, capacity: i64, sample: &QualitySample| {
        quality_copy_context(state, "host", observed, capacity, sample)
    };
    let overlap = EndpointTransition {
        kind: TransitionKind::Overlap,
        producer_peer_id: Some("host".to_owned()),
        retire: None,
    };
    let none = EndpointTransition {
        kind: TransitionKind::None,
        producer_peer_id: None,
        retire: None,
    };
    let host_edges = || edges(&[("a", "host", "a-connection"), ("b", "host", "b-connection")]);
    let peer_candidate = CandidateTuple {
        kind: UpstreamKind::Peer,
        parent_peer_id: "host".to_owned(),
        transport: Some(Transport::Direct),
        regenerate: false,
        publication: None,
    };
    let sample =
        |kind, child: &str, revision, connection: &str, publication: &str, host_session: &str| {
            QualitySample {
                kind,
                child_peer_id: child.to_owned(),
                route_revision: revision,
                connection_id: connection.to_owned(),
                publication_generation: publication.to_owned(),
                host_session_id: host_session.to_owned(),
            }
        };
    let sfu_sample = sample(UpstreamKind::Sfu, "", 8, "", "publication", "host-session");
    let publication = HostPublication {
        generation: "publication".to_owned(),
        host_session_id: "host-session".to_owned(),
        connection_id: "publication-connection".to_owned(),
        usable: true,
        physical_active: true,
        resource: Some(Arc::new(Resource::new(ResourceKind::SfuPublication))),
    };

    let got = project(
        &snapshot(
            host_edges(),
            Some(operation(peer_candidate, overlap.clone())),
            None,
        ),
        "host",
        2,
        &sample(
            UpstreamKind::Peer,
            "candidate",
            8,
            "candidate-connection",
            "",
            "",
        ),
    );
    assert_eq!(
        got,
        QualityCopyContext {
            committed_copies: 2,
            candidate_reserved_copies: 1,
            endpoint_capacity: 2,
            operation_reason: Some(DemandReason::QualityConvergence),
            candidate_transition: Some(TransitionKind::Overlap),
            sample_role: "candidate",
        }
    );

    let sfu_tuple = |plan| CandidateTuple {
        kind: UpstreamKind::Sfu,
        parent_peer_id: String::new(),
        transport: None,
        regenerate: false,
        publication: Some(plan),
    };
    let cases = [
        (
            snapshot(
                host_edges(),
                Some(operation(sfu_tuple(PublicationPlan::Create), overlap)),
                None,
            ),
            (2, 1, TransitionKind::Overlap),
        ),
        (
            snapshot(
                edges(&[("a", "host", "a-connection")]),
                Some(operation(sfu_tuple(PublicationPlan::Reuse), none)),
                Some(publication),
            ),
            (2, 0, TransitionKind::None),
        ),
    ];
    for (index, (state, (committed, reserved, transition))) in cases.iter().enumerate() {
        let got = project(state, "host", 2, &sfu_sample);
        assert_eq!(got.committed_copies, *committed, "sfu case {index}");
        assert_eq!(got.candidate_reserved_copies, *reserved, "sfu case {index}");
        assert_eq!(
            got.candidate_transition,
            Some(*transition),
            "sfu case {index}"
        );
        assert_eq!(got.sample_role, "candidate", "sfu case {index}");
    }

    let relay_edges = edges(&[("a", "relay", "relay-a"), ("b", "relay", "relay-b")]);
    let relay_sample = sample(UpstreamKind::Peer, "a", 7, "relay-a", "", "");
    let relay_state = snapshot(relay_edges, None, None);
    let got = project(&relay_state, "relay", 3, &relay_sample);
    assert_eq!(
        (
            got.committed_copies,
            got.candidate_reserved_copies,
            got.endpoint_capacity,
            got.sample_role
        ),
        (2, 0, 3, "active")
    );
    let stale = sample(UpstreamKind::Peer, "a", 6, "relay-a", "", "");
    assert_eq!(
        project(&relay_state, "relay", 3, &stale).sample_role,
        "unknown"
    );
}

#[test]
fn router_commits_p2p_quality_convergence_after_client_relative_approval() {
    let h = RouterHarness::new(RouterOptions {
        capacity: 2,
        debug: true,
        ..Default::default()
    });
    let c = prepare_quality_convergence(&h, true);
    h.locked(|core| {
        do_ready(core, &c.first, c.quality_prepare.revision);
        assert_eq!(
            do_must_edge(core, &c.created.room_id, &c.first.peer_id).upstream,
            peer(&c.host.peer_id),
            "upstream before approval"
        );
        do_ready_approved(core, &c.first, c.quality_prepare.revision);
        assert_eq!(
            do_must_edge(core, &c.created.room_id, &c.first.peer_id).upstream,
            peer(&c.second.peer_id),
            "upstream after approval"
        );
    });
}

#[test]
fn router_prepares_same_parent_connection_regeneration_after_persistent_degradation() {
    let h = RouterHarness::new(options(2));
    let created = h.create_room();
    let host = h.connect_host(&created, Some(QUALITY_POLICY), "", "");
    let viewer = h.connect_viewer(&created, "quality-regeneration");
    h.locked(|core| {
        do_complete(core, &host);
        do_complete(core, &viewer);
    });
    let initial = h.wait_prepared(&viewer.session_id);
    assert_eq!(initial.assignment.upstream, peer(&host.peer_id));
    let edge = h.locked(|core| {
        do_ready(core, &viewer, initial.revision);
        let edge = do_must_edge(core, &created.room_id, &viewer.peer_id);
        observe(
            core,
            &host,
            sender_evidence(
                &viewer.peer_id,
                &edge.connection_id,
                "old-sender-rtp",
                "old-track",
                100.0,
                edge.revision,
                "healthy",
            ),
        );
        for timestamp in [101.0, 102.0, 103.0] {
            observe(
                core,
                &host,
                sender_evidence(
                    &viewer.peer_id,
                    &edge.connection_id,
                    "old-sender-rtp",
                    "old-track",
                    timestamp,
                    edge.revision,
                    "degraded",
                ),
            );
        }
        edge
    });
    let regeneration = h.wait_prepared_above(&viewer.session_id, edge.revision);
    assert!(
        regeneration.transport() == "direct" && regeneration.candidate.quality_probe,
        "regeneration candidate"
    );
    assert_eq!(regeneration.assignment.upstream, peer(&host.peer_id));
    assert_eq!(
        h.prepared_for(&host.session_id)
            .map(|prepare| prepare.candidate.connection_id),
        Some(regeneration.candidate.connection_id.clone()),
        "the host must receive the same regeneration candidate"
    );
    let active = h.must_active_edge(&created.room_id, &viewer.peer_id);
    assert_eq!(active.connection_id, edge.connection_id);
    assert_eq!(active.upstream, peer(&host.peer_id));
}
