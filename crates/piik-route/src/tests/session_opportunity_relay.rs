//! Session opportunity tests (part two).

use super::publication::commit_room_peer;
use super::*;
use crate::*;

#[test]
fn fresh_relay_opportunity_behind_healthy_sfu() {
    for voice_only in [false, true] {
        for nat in [false, true] {
            let mut routes = controller(
                1,
                Options {
                    sfu_enabled: true,
                    nat_prediction_enabled: nat,
                    ..opts()
                },
            );
            add_viewer(&mut routes, A, 0, None);
            add_viewer(&mut routes, B, 0, None);
            routes.hydrate_host_publication("publication", res("publication"), "publisher");
            routes.hydrate_edge(A, sfu_edge("publication", "retained_sfu", "subscription_a"));
            routes.hydrate_edge(B, sfu_edge("publication", "parent_sfu", "subscription_b"));
            if voice_only {
                routes.set_video_publication("", false, 0);
                routes.set_voice_listeners(&[HOST, A, B], 0);
            }
            no_operation(routes.reconcile(0).operation);
            let attempts = if nat { 4 } else { 1 };
            for attempt in 1..=attempts {
                let now = attempt * 3;
                routes.set_effective_capacity(B, &session_of(B), 1, ms(now));
                let op = must(routes.reconcile(now).operation);
                assert_eq!(op.reason, DemandReason::DirectConvergence);
                assert_eq!(op.child_peer_id, A);
                assert_eq!(op.candidates[0].tuple, peer_tuple(B));
                let prepared = begin_operation(
                    &mut routes,
                    begin(now, &format!("fresh_{attempt}"), direct()),
                );
                let attempt_current = current(&prepared);
                routes.candidate_failed(
                    &guard_for(A, attempt_current.revision, &attempt_current.connection_id),
                    now + 1,
                );
                assert_eq!(edge_of(&routes, A).connection_id, "retained_sfu");
                assert!(edge_of(&routes, A).usable);
            }
            routes.set_effective_capacity(B, &session_of(B), 2, ms(20));
            no_operation(routes.reconcile(20).operation);
            routes.upsert_participant(viewer_input(B, "fresh_parent_session", 2), ms(21));
            let op = must(routes.reconcile(22).operation);
            assert_eq!(op.child_peer_id, A);
            assert_eq!(op.candidates[0].tuple, peer_tuple(B));
            if voice_only {
                commit_room_peer(&mut routes, 23, "new_peer");
            } else {
                commit_current(&mut routes, 23, "new_peer");
            }
            routes.set_effective_capacity(B, "fresh_parent_session", 3, ms(26));
            routes.reconcile(26);
            assert_eq!(edge_of(&routes, A).connection_id, "new_peer");
        }
    }
}
