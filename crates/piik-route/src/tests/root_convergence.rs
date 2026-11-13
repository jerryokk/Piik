//! Root convergence tests.

use super::*;
use crate::*;

#[test]
fn root_convergence_retains_committed_routes_until_exact_proof() {
    for capacity in [2, 3] {
        for child_count in 2..=capacity as usize {
            for outcome in [
                "commit",
                "failure",
                "timeout",
                "session-replaced",
                "availability-preempts",
            ] {
                let mut routes = controller(capacity, quality_opts());
                add_viewer(&mut routes, B, capacity, None);
                routes.hydrate_edge(B, peer_edge(HOST, "b_from_host"));
                let children = &[C, D, E][..child_count];
                for child in children {
                    add_viewer(&mut routes, child, 0, None);
                    routes.hydrate_edge(child, peer_edge(B, &format!("{child}_from_b")));
                }
                add_viewer(&mut routes, A, capacity, ms(10));
                routes.reconcile(10);
                commit_current(&mut routes, 11, "a_from_host");
                let op = must(routes.reconcile(13).operation);
                assert_eq!(op.reason, DemandReason::RootConvergence);
                let prepared = begin_operation(&mut routes, begin(14, "root_candidate", direct()));
                let revision = current(&prepared).revision;
                let guard = CandidateGuard {
                    child_peer_id: op.child_peer_id.clone(),
                    child_session_id: op.child_session_id.clone(),
                    revision,
                    connection_id: "root_candidate".to_owned(),
                };

                // Another child's report cannot authorize the prepared child,
                // even when it names the candidate revision and connection.
                let other = if children[0] == op.child_peer_id {
                    children[1]
                } else {
                    children[0]
                };
                let sample = sender_sample(
                    A,
                    other,
                    revision,
                    "root_candidate",
                    "candidate-sender",
                    SenderQualityState::Healthy,
                    15,
                );
                assert!(
                    !routes
                        .observe_sender_quality_evidence(sample, None)
                        .accepted
                );
                let ready = routes.candidate_ready(&guard, 16, None, CandidateProof::default());
                assert!(ready.accepted && !ready.committed);

                match outcome {
                    "commit" => {
                        let sample = sender_sample(
                            A,
                            &op.child_peer_id,
                            revision,
                            "root_candidate",
                            "candidate-sender",
                            SenderQualityState::Healthy,
                            17,
                        );
                        assert!(
                            routes
                                .observe_sender_quality_evidence(sample, None)
                                .committed
                        );
                    }
                    "failure" => assert!(routes.candidate_failed(&guard, 17).accepted),
                    "timeout" => assert!(routes.operation_expired(op.deadline_at_ms).accepted),
                    "session-replaced" => {
                        routes.upsert_participant(
                            viewer_input(A, "replacement_session", capacity),
                            ms(17),
                        );
                        assert!(
                            !routes
                                .candidate_ready(&guard, 18, None, CandidateProof::default())
                                .accepted
                        );
                    }
                    _ => {
                        add_viewer(&mut routes, F, 0, ms(17));
                        let next = must(routes.reconcile(18).operation);
                        assert_eq!(next.child_peer_id, F);
                        assert_eq!(next.reason, DemandReason::Join);
                        assert!(
                            !routes
                                .candidate_ready(&guard, 19, None, CandidateProof::default())
                                .accepted
                        );
                    }
                }
                for child in children {
                    let edge = edge_of(&routes, child);
                    if *child == op.child_peer_id && outcome == "commit" {
                        assert_eq!(edge.parent_peer_id, A);
                        assert_eq!(edge.connection_id, "root_candidate");
                    } else {
                        assert_eq!(edge.parent_peer_id, B, "{capacity}/{child_count}/{outcome}");
                        assert_eq!(edge.connection_id, format!("{child}_from_b"));
                    }
                    assert!(edge.usable && edge.physical_active);
                }
                routes.assert_graph();
            }
        }
    }
}
