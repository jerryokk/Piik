use std::panic::{AssertUnwindSafe, catch_unwind};

use super::*;

fn publication_fence(room_id: &str, publication_generation: &str) -> ResourceFence {
    ResourceFence {
        room_id: room_id.into(),
        share_generation: format!("share_{room_id}"),
        publication_generation: publication_generation.into(),
    }
}

fn subscription_fence(fence: &ResourceFence, viewer_peer_id: &str) -> SubscriptionFence {
    SubscriptionFence {
        resource: fence.clone(),
        viewer_peer_id: viewer_peer_id.into(),
        connection_id: String::new(),
    }
}

fn admission(ingress_capacity: usize, egress_capacity: usize) -> Admission {
    Admission::new(AdmissionOptions {
        ingress_capacity,
        egress_capacity,
    })
}

fn expect_panic(want: &str, run: impl FnOnce()) {
    let error = catch_unwind(AssertUnwindSafe(run)).expect_err("expected a panic");
    let message = error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(message.contains(want), "panic = {message:?}, want {want:?}");
}

fn expect_usage(admission: &Admission, ingress: usize, egress: usize) {
    assert_eq!(admission.usage(), Usage { ingress, egress });
}

fn expect_drained(drained: Option<Vec<ResourceFence>>, want: &[&ResourceFence]) {
    let drained = drained.expect("commit was refused, want it accepted");
    let want: Vec<ResourceFence> = want.iter().map(|f| (*f).clone()).collect();
    assert_eq!(drained, want);
}

#[test]
fn subscription_drain_releases_only_exact_connection() {
    let mut admission = admission(1, 2);
    let publication = ResourceFence {
        room_id: "1234".into(),
        share_generation: "share_12345678".into(),
        publication_generation: "publication_12345678".into(),
    };
    let old = SubscriptionFence {
        resource: publication.clone(),
        viewer_peer_id: "viewer_12345678".into(),
        connection_id: "old_12345678".into(),
    };
    let current = SubscriptionFence {
        connection_id: "new_12345678".into(),
        ..old.clone()
    };
    assert!(admission.reserve_publication(&publication) && admission.reserve_subscription(&old));
    assert!(admission.commit_publication(&publication).is_some());
    assert!(
        admission.reserve_subscription(&current),
        "candidate connection was not charged independently"
    );
    assert_eq!(admission.usage().egress, 2);
    assert!(
        admission.begin_subscription_drain(&old) && admission.complete_subscription_drain(&old)
    );
    assert!(
        !admission.complete_subscription_drain(&old)
            && admission.has_subscription(&current)
            && admission.usage().egress == 1,
        "stale retirement changed the candidate"
    );
}

#[test]
fn requires_explicit_capacities() {
    // The TypeScript 1.5 case cannot be expressed by an integer.
    for ingress in [0, MAX_SAFE_INTEGER as usize + 1] {
        expect_panic("positive safe integer", || {
            admission(ingress, 1);
        });
    }
    expect_panic("positive safe integer", || {
        admission(1, 0);
    });
}

#[test]
fn enforces_ingress_and_egress_independently() {
    let mut admission = admission(2, 2);
    let first = publication_fence("1", "publication_first");
    let second = publication_fence("2", "publication_second");
    assert!(admission.reserve_publication(&first) && admission.reserve_publication(&second));
    assert!(!admission.reserve_publication(&publication_fence("3", "publication_third")));
    assert!(
        admission.reserve_subscription(&subscription_fence(&first, "viewer_a"))
            && admission.reserve_subscription(&subscription_fence(&first, "viewer_b"))
    );
    assert!(!admission.reserve_subscription(&subscription_fence(&second, "viewer_c")));
    expect_usage(&admission, 2, 2);
}

#[test]
fn keeps_exact_operations_idempotent() {
    let mut admission = admission(1, 1);
    let active = publication_fence("1", "publication_active");
    let viewer = subscription_fence(&active, "viewer_a");
    assert!(admission.reserve_publication(&active) && admission.reserve_publication(&active));
    assert!(admission.reserve_subscription(&viewer) && admission.reserve_subscription(&viewer));
    assert!(
        !admission.commit_subscription(&viewer),
        "a subscription must not commit before its publication"
    );
    expect_drained(admission.commit_publication(&active), &[]);
    expect_drained(admission.commit_publication(&active), &[]);
    assert!(admission.commit_subscription(&viewer));
    expect_usage(&admission, 1, 1);

    let stale = publication_fence("1", "publication_stale");
    assert!(!admission.reserve_subscription(&subscription_fence(&stale, "viewer_a")));
    assert!(admission.commit_publication(&stale).is_none());
    assert!(!admission.commit_subscription(&subscription_fence(&active, "viewer_b")));
    assert!(!admission.begin_subscription_drain(&subscription_fence(&active, "viewer_b")));
    assert!(!admission.begin_drain(&stale));
}

#[test]
fn does_not_commit_without_first_subscription() {
    let mut admission = admission(1, 1);
    let pending = publication_fence("1", "publication_pending");
    assert!(admission.reserve_publication(&pending));
    assert!(admission.commit_publication(&pending).is_none());
    assert!(admission.begin_drain(&pending) && admission.complete_drain(&pending));
    expect_usage(&admission, 0, 0);
}

#[test]
fn commits_new_generation_before_draining_the_old() {
    let mut admission = admission(2, 3);
    let first = publication_fence("1", "publication_first");
    let second = publication_fence("1", "publication_second");
    let first_viewer = subscription_fence(&first, "viewer_a");
    assert!(admission.reserve_publication(&first) && admission.reserve_subscription(&first_viewer));
    expect_drained(admission.commit_publication(&first), &[]);

    assert!(
        admission.reserve_publication(&second)
            && admission.reserve_subscription(&subscription_fence(&second, "viewer_b"))
            && admission.reserve_subscription(&subscription_fence(&second, "viewer_c"))
    );
    expect_usage(&admission, 2, 3);

    expect_drained(admission.commit_publication(&second), &[&first]);
    assert!(admission.commit_subscription(&subscription_fence(&second, "viewer_b")));
    assert!(!admission.reserve_publication(&first));
    assert!(!admission.reserve_subscription(&first_viewer));
    assert!(admission.complete_drain(&first));
    assert!(!admission.complete_drain(&first));
    expect_usage(&admission, 1, 2);
}

#[test]
fn keeps_released_reserved_subscription_charged() {
    let mut admission = admission(1, 2);
    let active = publication_fence("1", "publication_active");
    let first = subscription_fence(&active, "viewer_a");
    let candidate = subscription_fence(&active, "viewer_b");
    assert!(admission.reserve_publication(&active) && admission.reserve_subscription(&first));
    expect_drained(admission.commit_publication(&active), &[]);
    assert!(admission.reserve_subscription(&candidate));
    expect_usage(&admission, 1, 2);
    assert!(
        admission.begin_subscription_drain(&candidate)
            && admission.begin_subscription_drain(&candidate)
    );
    assert!(!admission.commit_subscription(&candidate));
    expect_usage(&admission, 1, 2);
    assert!(!admission.reserve_subscription(&subscription_fence(&active, "viewer_c")));
    assert!(admission.begin_drain(&active) && admission.complete_drain(&active));
    expect_usage(&admission, 0, 0);
}

#[test]
fn does_not_revive_a_connection_during_physical_close() {
    let mut admission = admission(1, 1);
    let active = publication_fence("1", "publication_active");
    let mut viewer = subscription_fence(&active, "viewer_a");
    assert!(admission.reserve_publication(&active) && admission.reserve_subscription(&viewer));
    expect_drained(admission.commit_publication(&active), &[]);
    assert!(admission.begin_subscription_drain(&viewer));
    expect_usage(&admission, 1, 1);
    assert!(
        !admission.reserve_subscription(&viewer),
        "a draining connection cannot be revived"
    );
    expect_usage(&admission, 1, 1);
    assert!(!admission.reserve_subscription(&subscription_fence(&active, "viewer_b")));
    assert!(admission.complete_subscription_drain(&viewer));
    viewer.connection_id = "replacement_12345678".into();
    assert!(admission.reserve_subscription(&viewer) && admission.commit_subscription(&viewer));
    expect_usage(&admission, 1, 1);
}

#[test]
fn keeps_departed_subscription_charged_until_drain_proof() {
    let mut admission = admission(1, 1);
    let active = publication_fence("1", "publication_active");
    let viewer = subscription_fence(&active, "viewer_a");
    assert!(admission.reserve_publication(&active) && admission.reserve_subscription(&viewer));
    expect_drained(admission.commit_publication(&active), &[]);
    assert!(admission.begin_subscription_drain(&viewer));
    assert!(!admission.reserve_subscription(&subscription_fence(&active, "viewer_b")));
    assert!(admission.begin_drain(&active) && admission.begin_drain(&active));
    expect_usage(&admission, 1, 1);
    assert!(admission.complete_drain(&active));
    expect_usage(&admission, 0, 0);
}

#[test]
fn drains_every_generation_exactly_once() {
    let mut admission = admission(3, 4);
    let active = publication_fence("1", "publication_active");
    let replacement = publication_fence("1", "publication_replacement");
    let other = publication_fence("2", "publication_other");
    assert!(
        admission.reserve_publication(&active)
            && admission.reserve_subscription(&subscription_fence(&active, "viewer_a"))
    );
    expect_drained(admission.commit_publication(&active), &[]);
    assert!(
        admission.reserve_publication(&replacement)
            && admission.reserve_subscription(&subscription_fence(&replacement, "viewer_b"))
            && admission.reserve_publication(&other)
            && admission.reserve_subscription(&subscription_fence(&other, "viewer_c"))
    );

    // Insertion order, not map order: the drain tasks are created from it.
    let first_room = admission.begin_drain_room("1");
    assert_eq!(first_room, [active.clone(), replacement.clone()]);
    assert_eq!(admission.begin_drain_room("1"), first_room);
    assert_eq!(
        admission.begin_drain_all(),
        [active.clone(), replacement.clone(), other.clone()]
    );
    expect_usage(&admission, 3, 3);

    for fence in [&active, &replacement, &other] {
        assert!(admission.complete_drain(fence), "{fence:?} must complete");
        assert!(
            !admission.complete_drain(fence),
            "{fence:?} completed twice"
        );
    }
    expect_usage(&admission, 0, 0);
}

#[test]
fn rejects_invalid_fences() {
    let mut admission = admission(1, 1);
    let valid = publication_fence("1", "publication_active");
    expect_panic("SFU resource fence is invalid", || {
        admission.reserve_publication(&ResourceFence {
            room_id: String::new(),
            share_generation: "s".into(),
            publication_generation: "p".into(),
        });
    });
    expect_panic("SFU resource fence is invalid", || {
        admission.begin_drain(&ResourceFence {
            room_id: "1".into(),
            share_generation: String::new(),
            publication_generation: "p".into(),
        });
    });
    expect_panic("SFU subscription fence is invalid", || {
        admission.reserve_subscription(&subscription_fence(&valid, ""));
    });
    expect_panic("SFU resource room ID is invalid", || {
        admission.begin_drain_room("");
    });
}

/// Reaches the accounting assertion the public state machine cannot reach,
/// by corrupting the egress counter first.
#[test]
fn asserts_no_underflow() {
    let mut admission = admission(1, 1);
    let active = publication_fence("1", "publication_active");
    assert!(
        admission.reserve_publication(&active)
            && admission.reserve_subscription(&subscription_fence(&active, "viewer_a"))
            && admission.begin_drain(&active)
    );
    admission.egress_in_use = 0;
    expect_panic("SFU resource accounting underflow", || {
        admission.complete_drain(&active);
    });
}

#[test]
fn validates_wire_fences() {
    assert!(publication_fence("1234", "publication_12345678").is_valid());
    for room_id in ["", "0", "01", "1234567890123", "12a"] {
        assert!(!publication_fence(room_id, "publication_12345678").is_valid());
    }
    assert!(!publication_fence("1234", "short").is_valid());
}
