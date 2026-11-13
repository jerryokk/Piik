//! Participant departure tests.

use super::*;
use crate::*;

#[test]
fn confirmed_departure_retires_media_while_paused() {
    for transport in ["peer", "sfu"] {
        for relay in [false, true] {
            let mut routes = controller(2, opts());
            add_viewer(&mut routes, A, 1, None);
            if transport == "sfu" {
                routes.hydrate_host_publication("publication", res("publication"), "publisher");
                routes.hydrate_edge(A, sfu_edge("publication", "a_sfu", "subscription"));
            } else {
                routes.hydrate_edge(A, peer_edge(HOST, "a_peer"));
            }
            if relay {
                add_viewer(&mut routes, B, 0, None);
                routes.hydrate_edge(B, peer_edge(A, "b_peer"));
            }
            routes.set_paused(true, ms(1));
            routes.disconnect_session(A, &session_of(A));
            // Transient signaling loss retains media during membership grace.
            eq_labels(&routes.reconcile(2).released, &[]);
            assert!(edge_of(&routes, A).physical_active);

            routes.confirm_departure(A, ms(3));
            let before = routes.revision();
            let result = routes.reconcile(4);
            no_operation(result.operation);
            assert!(
                routes.revision() > before,
                "departure must publish a new active assignment"
            );
            if transport == "sfu" {
                eq_labels(&result.released, &["subscription", "publication"]);
            } else {
                eq_labels(&result.released, &[]);
            }
            if relay {
                let edge = edge_of(&routes, A);
                assert!(!edge.physical_active && !edge.usable);
                assert_eq!(edge_of(&routes, B).connection_id, "b_peer");
            } else {
                assert!(!has_edge(&routes, A));
            }
            routes.assert_graph();
            eq_labels(&routes.reconcile(5).released, &[]);
        }
    }
}

#[test]
fn departure_retires_media_before_pending_candidate_can_commit() {
    let mut routes = controller(2, opts());
    add_viewer(&mut routes, A, 0, None);
    routes.hydrate_edge(A, peer_edge(HOST, "a_peer"));
    add_viewer(&mut routes, B, 0, None);
    routes.reconcile(0);
    let pending = begin_operation(&mut routes, begin(1, "b_old", direct_overlap("overlap")));
    let stale = guard_for(B, current(&pending).revision, "b_old");

    routes.confirm_departure(A, ms(2));
    let result = routes.reconcile(3);
    eq_labels(&result.released, &["overlap"]);
    assert!(!has_edge(&routes, A));
    assert!(
        routes.revision() > stale.revision,
        "retirement must supersede the pending candidate revision"
    );
    assert!(
        !routes
            .candidate_ready(&stale, 4, None, CandidateProof::default())
            .accepted
    );
    let revision = routes.revision();
    commit_current(&mut routes, 5, "b_new");
    assert!(
        routes.revision() > revision,
        "replacement commit must not roll back the retirement revision"
    );
    assert_eq!(edge_of(&routes, B).connection_id, "b_new");
}

#[test]
fn departure_preserves_shared_publication_and_healthy_sibling() {
    let mut routes = controller(1, sfu_opts());
    routes.hydrate_host_publication("publication", res("publication"), "publisher");
    for peer_id in [A, B] {
        add_viewer(&mut routes, peer_id, 0, None);
        routes.hydrate_edge(
            peer_id,
            sfu_edge("publication", &format!("{peer_id}_sfu"), peer_id),
        );
    }
    add_viewer(&mut routes, C, 0, None);
    routes.reconcile(0);
    begin_operation(&mut routes, begin(1, "c_pending", sfu_reuse("pending")));
    routes.confirm_departure(A, ms(2));
    let result = routes.reconcile(3);
    eq_labels(&result.released, &["pending", A]);
    assert_eq!(
        routes.snapshot().host_publication.unwrap().connection_id,
        "publisher"
    );
    assert_eq!(edge_of(&routes, B).connection_id, format!("{B}_sfu"));
    assert!(edge_of(&routes, B).physical_active);
    routes.assert_graph();
}
