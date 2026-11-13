//! Session opportunity tests (part one).

use super::*;
use crate::*;

fn invalidate_current(routes: &mut Controller, child: &str, revision: i64, now: i64) -> bool {
    let edge = edge_of(routes, child);
    routes.invalidate_edge(
        EdgeGuard {
            child_peer_id: child.to_owned(),
            child_session_id: edge.child_session_id,
            parent_session_id: edge.parent_session_id,
            route_revision: revision,
            connection_id: edge.connection_id,
        },
        ms(now),
    )
}

#[test]
fn retained_edge_failure_preserves_new_session_opportunity() {
    for upstream in [UpstreamKind::Peer, UpstreamKind::Sfu] {
        for changed in ["none", "child", "host"] {
            let mut routes = controller(2, sfu_opts());
            add_viewer(&mut routes, A, 0, None);
            if upstream == UpstreamKind::Sfu {
                routes.hydrate_host_publication("publication", res("publication_resource"), "");
                routes.hydrate_edge(
                    A,
                    sfu_edge("publication", "retained_connection", "subscription"),
                );
            } else {
                routes.hydrate_edge(A, peer_edge(HOST, "retained_connection"));
            }
            match changed {
                "child" => drop(
                    routes
                        .upsert_participant(viewer_input(A, "replacement_child_session", 0), None),
                ),
                "host" => {
                    drop(routes.upsert_participant(host_input("replacement_host_session", 2), None))
                }
                _ => {}
            }
            // Reauthentication keeps healthy media and its physical identity.
            let edge = edge_of(&routes, A);
            assert!(edge.usable);
            assert_eq!(edge.connection_id, "retained_connection");
            if upstream == UpstreamKind::Sfu {
                assert_eq!(
                    must(routes.reconcile(0).operation).reason,
                    DemandReason::DirectConvergence
                );
            } else {
                no_operation(routes.reconcile(0).operation);
            }
            let revision = routes.revision();
            assert!(invalidate_current(&mut routes, A, revision, 1));
            let operation = must(routes.reconcile(1).operation);
            assert_eq!(
                has_tuple(&operation, |tuple| tuple.kind == upstream),
                changed != "none",
                "{upstream:?}/{changed}"
            );
        }
    }
}

#[test]
fn session_rebind_restores_bounded_direct_attempts_behind_sfu() {
    for changed in ["child", "host", "parent"] {
        let mut routes = controller(
            3,
            Options {
                sfu_enabled: true,
                nat_prediction_enabled: true,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 0, None);
        routes.hydrate_host_publication("publication", res("publication"), "publisher");
        routes.hydrate_edge(A, sfu_edge("publication", "retained_sfu", "subscription"));
        let mut parent = HOST;
        let mut rebound = host_input("new_host_session", 3);
        if changed == "child" {
            rebound = viewer_input(A, "new_child_session", 0);
        } else if changed == "parent" {
            parent = B;
            add_viewer(&mut routes, B, 2, None);
            routes.hydrate_edge(B, peer_edge(HOST, "host_b"));
            rebound = viewer_input(B, "new_parent_session", 2);
        }
        // The previous sessions exhausted direct opportunities, not SFU media.
        for plan in routes.build_candidates(A, false) {
            if plan.tuple.kind == UpstreamKind::Peer {
                routes.consume_candidate_opportunity(A, &plan);
            }
        }
        no_operation(routes.reconcile(0).operation);
        let previous = routes
            .participant(&rebound.peer_id)
            .unwrap()
            .session_id
            .clone();
        routes.disconnect_session(&rebound.peer_id, &previous);
        routes.reconcile(1);
        routes.upsert_participant(rebound.clone(), ms(2));
        for attempt in 1..=4 {
            let now = 3 + attempt * 2;
            let op = must(routes.reconcile(now).operation);
            assert_eq!(op.reason, DemandReason::DirectConvergence);
            assert_eq!(op.child_peer_id, A);
            assert_eq!(op.candidates[0].tuple, peer_tuple(parent));
            let prepared = begin_operation(
                &mut routes,
                begin(now, &format!("retry_{attempt}"), direct()),
            );
            assert_eq!(attempt_current(&prepared), attempt);
            let guard = CandidateGuard {
                child_peer_id: A.to_owned(),
                child_session_id: prepared.child_session_id.clone(),
                revision: current(&prepared).revision,
                connection_id: current(&prepared).connection_id.clone(),
            };
            routes.candidate_failed(&guard, now + 1);
            assert_eq!(edge_of(&routes, A).connection_id, "retained_sfu");
            assert!(edge_of(&routes, A).usable);
        }
        no_operation(routes.reconcile(20).operation);
        // Duplicate session updates cannot reopen the exhausted budget.
        routes.upsert_participant(rebound, ms(21));
        no_operation(routes.reconcile(22).operation);
    }
}

#[test]
fn rebuilt_direct_connection_consumes_current_session_opportunity() {
    let mut routes = controller(2, sfu_opts());
    add_viewer(&mut routes, A, 0, ms(0));
    routes.reconcile(0);
    commit_current(&mut routes, 1, "original_connection");
    routes.upsert_participant(viewer_input(A, "replacement_child_session", 0), None);
    let edge = edge_of(&routes, A);
    let result = routes.adopt_direct_connection(
        AdoptDirectConnectionInput {
            edge: EdgeGuard {
                child_peer_id: A.to_owned(),
                child_session_id: edge.child_session_id.clone(),
                parent_session_id: edge.parent_session_id.clone(),
                route_revision: routes.revision(),
                connection_id: edge.connection_id.clone(),
            },
            new_connection_id: "rebuilt_connection".to_owned(),
        },
        3,
    );
    assert!(result.accepted);
    let guard = EdgeGuard {
        child_peer_id: A.to_owned(),
        child_session_id: edge.child_session_id,
        parent_session_id: edge.parent_session_id,
        route_revision: result.active_revision,
        connection_id: "rebuilt_connection".to_owned(),
    };
    assert!(routes.invalidate_edge(guard, ms(4)));
    assert!(!has_tuple(&must(routes.reconcile(4).operation), is_peer));
}

#[test]
fn regenerated_edge_failure_consumes_only_its_acquired_session_opportunity() {
    for prediction in [false, true] {
        for changed in ["none", "child", "parent"] {
            let mut routes = controller(
                2,
                Options {
                    sfu_enabled: true,
                    quality_convergence_enabled: true,
                    nat_prediction_enabled: prediction,
                    ..opts()
                },
            );
            add_viewer(&mut routes, A, 0, None);
            routes.reconcile(0);
            commit_current(&mut routes, 1, "original");
            observe_persistent_degraded(&mut routes, A, "original", 10);
            let op = must(routes.reconcile(14).operation);
            assert_eq!(op.reason, DemandReason::QualityConvergence);
            assert_eq!(op.candidates[0].tuple, regenerate_tuple(HOST));
            let prepared = begin_operation(&mut routes, begin(15, "regenerated", direct()));
            let revision = current(&prepared).revision;
            let guard = guard_for(A, revision, "regenerated");
            let approved = CandidateProof {
                relative_quality_approved: true,
                ..CandidateProof::default()
            };
            assert!(!routes.candidate_ready(&guard, 16, None, approved).committed);
            let sample = sender_sample(
                HOST,
                A,
                revision,
                "regenerated",
                "regenerated-sender",
                SenderQualityState::Healthy,
                17,
            );
            assert!(
                routes
                    .observe_sender_quality_evidence(sample, None)
                    .committed
            );

            match changed {
                "child" => {
                    drop(routes.upsert_participant(viewer_input(A, "replacement-child", 0), ms(18)))
                }
                "parent" => {
                    drop(routes.upsert_participant(host_input("replacement-parent", 2), ms(18)))
                }
                _ => {}
            }
            let revision = routes.revision();
            assert!(invalidate_current(&mut routes, A, revision, 19));
            let repair = must(routes.reconcile(20).operation);
            assert_eq!(
                has_tuple(&repair, is_peer),
                changed != "none",
                "nat={prediction}/{changed}"
            );
            assert!(has_tuple(&repair, is_sfu));
        }
    }
}
