//! Availability deadline tests.

use super::*;
use crate::*;

#[test]
fn connected_peer_deadline_preserves_untried_fallback() {
    for prediction in [false, true] {
        let mut routes = controller(
            2,
            Options {
                sfu_enabled: true,
                nat_prediction_enabled: prediction,
                operation_timeout_ms: 20_000,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 0, None);
        let operation = must(routes.reconcile(0).operation);
        let prepared =
            begin_operation(&mut routes, begin(1, "connected_without_picture", direct()));
        let guard = guard_for(A, current(&prepared).revision, "connected_without_picture");
        assert!(routes.candidate_transport_connected(&guard, 100).accepted);
        assert!(!routes.operation_expired(operation.wake_at_ms).accepted);
        let result = routes.operation_expired(operation.deadline_at_ms);
        assert!(result.accepted);
        assert!(result.failed_peer_ids.is_empty());
        no_operation(routes.snapshot().operation);
        // Roll back before a fresh bounded operation. An attempted Peer must
        // not consume or repeatedly jump ahead of an untried SFU route.
        let next = must(routes.reconcile(operation.deadline_at_ms + 1).operation);
        assert_eq!(next.candidates[next.cursor].tuple.kind, UpstreamKind::Sfu);
        assert!(
            !routes
                .candidate_ready(
                    &guard,
                    operation.deadline_at_ms + 2,
                    None,
                    CandidateProof::default()
                )
                .accepted
        );
        let fallback = begin_operation(
            &mut routes,
            begin_publication(
                operation.deadline_at_ms + 3,
                "fallback",
                "publication",
                "publisher",
                sfu_create("subscription", "publication"),
            ),
        );
        let fallback_guard = guard_for(A, current(&fallback).revision, "fallback");
        assert!(
            routes
                .candidate_ready(
                    &fallback_guard,
                    operation.deadline_at_ms + 4,
                    None,
                    CandidateProof::default()
                )
                .committed
        );
        assert_eq!(edge_of(&routes, A).kind, UpstreamKind::Sfu);
    }
}

#[test]
fn availability_deadlines_exhaust_only_their_bounded_opportunities() {
    for prediction in [false, true] {
        let mut routes = controller(
            2,
            Options {
                sfu_enabled: true,
                nat_prediction_enabled: prediction,
                operation_timeout_ms: 20_000,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 0, None);
        let (mut peer_attempts, mut sfu_attempts, mut now) = (0, 0, 0);
        for count in 0..6 {
            let Some(op) = routes.reconcile(now).operation else {
                break;
            };
            let connection = format!("attempt_{count}");
            let kind = op.candidates[op.cursor].tuple.kind;
            let input = if kind == UpstreamKind::Sfu {
                sfu_attempts += 1;
                begin_publication(
                    now + 1,
                    &connection,
                    "publication",
                    "publisher",
                    sfu_create("subscription", "publication"),
                )
            } else {
                peer_attempts += 1;
                begin(now + 1, &connection, direct())
            };
            let prepared = begin_operation(&mut routes, input);
            if kind == UpstreamKind::Peer {
                routes.candidate_transport_connected(
                    &guard_for(A, current(&prepared).revision, &connection),
                    now + 2,
                );
            }
            let result = routes.operation_expired(op.deadline_at_ms);
            if kind == UpstreamKind::Sfu {
                eq_labels(&result.released, &["subscription", "publication"]);
            }
            now = op.deadline_at_ms + 1;
        }
        assert_eq!(peer_attempts, if prediction { 4 } else { 1 });
        assert_eq!(sfu_attempts, 1);
        no_operation(routes.reconcile(now).operation);
    }
}
