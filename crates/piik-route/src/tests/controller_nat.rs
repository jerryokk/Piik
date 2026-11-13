//! Controller tests: bounded NAT candidate acquisition.

use super::*;
use crate::*;

#[test]
fn runs_four_real_generations_then_exhausts_until_new_session() {
    for failure in ["failed", "timeout"] {
        let mut routes = controller(
            2,
            Options {
                nat_prediction_enabled: true,
                operation_timeout_ms: 20_000,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 0, None);
        let mut now = 0;
        let mut previous: Option<CandidateGuard> = None;
        for attempt in 1..=4 {
            let (operation, prepared, guard) =
                prepare_peer(&mut routes, now, &format!("nat_{attempt}"));
            assert_eq!(
                current(&prepared).connection_attempt,
                Some(ConnectionAttemptProgress {
                    current: attempt,
                    total: 4
                })
            );
            assert_eq!(operation.deadline_at_ms - now, 20_000);
            assert_eq!(prepared.wake_at_ms, operation.deadline_at_ms);
            if let Some(previous) = &previous {
                assert!(!routes.candidate_failed(previous, now).accepted);
            }
            assert_eq!(
                current(&must(routes.snapshot().operation)).connection_id,
                format!("nat_{attempt}")
            );
            let result = if failure == "timeout" {
                now = operation.deadline_at_ms;
                routes.operation_expired(now)
            } else {
                now += 1;
                routes.candidate_failed(&guard, now)
            };
            if attempt == 4 {
                assert_eq!(result.failed_peer_ids, [A]);
            } else {
                assert!(result.failed_peer_ids.is_empty(), "{failure}/{attempt}");
            }
            assert!(!routes.candidate_failed(&guard, now).accepted);
            previous = Some(guard);
            now += 1;
        }
        no_operation(routes.reconcile(now).operation);
        routes.upsert_participant(viewer_input(A, "replacement_session", 0), ms(now));
        let (_, prepared, guard) = prepare_peer(&mut routes, now + 1, "new_session");
        assert_eq!(attempt_current(&prepared), 1);
        assert!(
            routes
                .candidate_ready(&guard, now + 2, None, CandidateProof::default())
                .committed
        );
    }
}

// TS 5067
#[test]
fn gives_waiting_viewers_first_opportunity_before_another_viewer_retries() {
    let mut routes = controller(
        2,
        Options {
            nat_prediction_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 0, None);
    add_viewer(&mut routes, B, 0, None);
    for (index, child) in [A, B, A, B, A, B, A, B].into_iter().enumerate() {
        let index = index as i64;
        let (_, prepared, guard) = prepare_peer(&mut routes, index * 2, &format!("fair_{index}"));
        assert_eq!(prepared.child_peer_id, child);
        assert_eq!(attempt_current(&prepared), index / 2 + 1);
        routes.candidate_failed(&guard, index * 2 + 1);
    }
    no_operation(routes.reconcile(20).operation);
}

// TS 5080
#[test]
fn tries_newly_usable_parent_before_spending_another_attempt_on_old_parent() {
    let mut routes = controller(
        2,
        Options {
            nat_prediction_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, B, 2, None);
    routes.hydrate_edge(B, peer_edge(HOST, "existing_b"));
    add_viewer(&mut routes, A, 0, None);
    let (_, first, first_guard) = prepare_peer(&mut routes, 0, "first_host");
    assert_eq!(current(&first).tuple.parent_peer_id, HOST);
    routes.candidate_failed(&first_guard, 1);
    let (_, second, second_guard) = prepare_peer(&mut routes, 2, "first_b");
    assert_eq!(current(&second).tuple.parent_peer_id, B);
    routes.candidate_failed(&second_guard, 3);
    add_viewer(&mut routes, C, 2, ms(4));
    routes.hydrate_edge(C, peer_edge(HOST, "existing_c"));
    let (_, third, _) = prepare_peer(&mut routes, 5, "first_c");
    assert_eq!(current(&third).tuple.parent_peer_id, C);
    assert_eq!(attempt_current(&third), 1);
}

// TS 5098
#[test]
fn keeps_sfu_foreground_timing_and_shares_attempt_budget_with_direct_continuations() {
    let mut routes = controller(
        2,
        Options {
            nat_prediction_enabled: true,
            sfu_enabled: true,
            operation_timeout_ms: 20_000,
            ..opts()
        },
    );
    let mut now = 0;
    for child in [A, B] {
        add_viewer(&mut routes, child, 0, ms(now));
        let (_, direct_prepared, _) = prepare_peer(&mut routes, now, &format!("{child}_first"));
        assert_eq!(direct_prepared.wake_at_ms, now + 5_000);
        now += 5_000;
        assert!(routes.operation_expired(now).failed_peer_ids.is_empty());
        let reservation = if child == A {
            sfu_create("subscription_a", "publication")
        } else {
            sfu_reuse("subscription_b")
        };
        let connection = format!("{child}_sfu");
        let sfu = begin_operation(
            &mut routes,
            begin_publication(
                now,
                &connection,
                "sfu_generation",
                "sfu_publisher",
                reservation,
            ),
        );
        assert!(
            current(&sfu).connection_attempt.is_none(),
            "SFU attempts carry no NAT connection attempt"
        );
        now += 1;
        let guard = CandidateGuard {
            child_peer_id: child.to_owned(),
            child_session_id: sfu.child_session_id.clone(),
            revision: current(&sfu).revision,
            connection_id: connection,
        };
        routes.candidate_ready(&guard, now, None, CandidateProof::default());
        now += 1;
    }
    for (index, child) in [A, B, A, B, A, B].into_iter().enumerate() {
        let (_, retry, guard) = prepare_peer(&mut routes, now, &format!("background_{index}"));
        assert_eq!(retry.child_peer_id, child);
        assert_eq!(retry.reason, DemandReason::DirectConvergence);
        assert_eq!(
            current(&retry).connection_attempt,
            Some(ConnectionAttemptProgress {
                current: index as i64 / 2 + 2,
                total: 4
            })
        );
        assert_eq!(retry.wake_at_ms - now, 20_000);
        now += 1;
        assert!(
            routes
                .candidate_failed(&guard, now)
                .failed_peer_ids
                .is_empty()
        );
        let edge = edge_of(&routes, child);
        assert!(edge.kind == UpstreamKind::Sfu && edge.usable);
        now += 1;
    }
    no_operation(routes.reconcile(now).operation);
}

// TS 5139
#[test]
fn does_not_retry_without_nat_policy_or_when_no_candidate_was_created() {
    let mut ordinary = controller(2, opts());
    add_viewer(&mut ordinary, A, 0, None);
    let (_, first, guard) = prepare_peer(&mut ordinary, 0, "ordinary");
    assert!(current(&first).connection_attempt.is_none());
    assert_eq!(ordinary.candidate_failed(&guard, 1).failed_peer_ids, [A]);
    no_operation(ordinary.reconcile(2).operation);

    let mut routes = controller(
        2,
        Options {
            nat_prediction_enabled: true,
            ..opts()
        },
    );
    add_viewer(&mut routes, A, 0, None);
    let operation = must(routes.reconcile(0).operation);
    routes.skip_current_candidate(
        &cursor_guard(&operation),
        1,
        RejectionBucket::CandidateFailed,
    );
    no_operation(routes.reconcile(2).operation);
}
