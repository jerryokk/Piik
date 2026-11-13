//! SFU-only policy tests.

use super::*;
use crate::*;

#[test]
fn sfu_only_uses_one_publication_without_peer_convergence() {
    let mut routes = controller(
        1,
        Options {
            sfu_enabled: true,
            sfu_only: true,
            quality_convergence_enabled: true,
            nat_prediction_enabled: true,
            ..opts()
        },
    );
    for (index, viewer) in [A, B, C].into_iter().enumerate() {
        let now = index as i64 * 10;
        add_viewer(&mut routes, viewer, 1, Some(now));
        let op = must(routes.reconcile(now).operation);
        assert_eq!(op.candidates.len(), 1);
        let connection = format!("{viewer}_sfu");
        let (publication, input) = if index == 0 {
            (
                PublicationPlan::Create,
                begin_publication(
                    now + 1,
                    &connection,
                    "publication",
                    "publication_connection",
                    sfu_create(viewer, "publication_resource"),
                ),
            )
        } else {
            (
                PublicationPlan::Reuse,
                begin(now + 1, &connection, sfu_reuse(viewer)),
            )
        };
        assert_eq!(op.candidates[0].tuple, sfu_tuple(publication));
        let prepared = begin_operation(&mut routes, input);
        // Availability still commits only on the exact child's media-ready proof.
        assert!(!has_edge(&routes, viewer));
        let guard = guard_for(viewer, current(&prepared).revision, &connection);
        assert!(
            routes
                .candidate_ready(&guard, now + 2, None, CandidateProof::default())
                .accepted
        );
        assert_eq!(edge_of(&routes, viewer).kind, UpstreamKind::Sfu);
        no_operation(routes.reconcile(now + 3).operation);
    }
    assert_eq!(routes.physical_copies(HOST), 1);
    assert_eq!(routes.direct_continuations.len(), 0);
    // A failed publication is replaced through the same bounded operation.
    let publication = routes.publication().unwrap();
    let guard = PublicationGuard {
        host_session_id: "host_session".to_owned(),
        route_revision: routes.revision(),
        generation: publication.generation,
        connection_id: publication.connection_id,
    };
    assert!(routes.invalidate_host_publication(guard, ms(40)));
    let op = must(routes.reconcile(41).operation);
    assert_eq!(op.candidates.len(), 1);
    assert_eq!(op.candidates[0].tuple, sfu_tuple(PublicationPlan::Replace));
}

#[test]
fn sfu_only_failure_does_not_escape_to_peers() {
    for admission_denied in [false, true] {
        let mut routes = controller(
            2,
            Options {
                sfu_enabled: true,
                sfu_only: true,
                ..opts()
            },
        );
        add_viewer(&mut routes, A, 2, None);
        let op = must(routes.reconcile(0).operation);
        if admission_denied {
            routes.skip_current_candidate(&cursor_guard(&op), 1, RejectionBucket::SfuAdmission);
        } else {
            let prepared = begin_operation(
                &mut routes,
                begin_publication(
                    1,
                    "sfu_connection",
                    "publication",
                    "publication_connection",
                    sfu_create("edge", "publication"),
                ),
            );
            routes.candidate_failed(
                &guard_for(A, current(&prepared).revision, "sfu_connection"),
                2,
            );
        }
        no_operation(routes.reconcile(3).operation);
        assert!(!has_edge(&routes, A));
        // Resource release may retry SFU; it must not reopen a Peer opportunity.
        routes.touch_external_facts();
        let retry = must(routes.reconcile(4).operation);
        assert_eq!(retry.candidates.len(), 1);
        assert_eq!(retry.candidates[0].tuple.kind, UpstreamKind::Sfu);
    }
}
