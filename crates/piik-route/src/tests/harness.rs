//! Route controller test harness. Every resource is a distinct `Arc` with a
//! string label; released lists compare labels in order.

use std::cell::RefCell;
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use piik_protocol::Role;

use crate::*;

pub(crate) const HOST: &str = "host_12345678";
pub(crate) const A: &str = "viewer_a_12345678";
pub(crate) const B: &str = "viewer_b_12345678";
pub(crate) const C: &str = "viewer_c_12345678";
pub(crate) const D: &str = "viewer_d_12345678";
pub(crate) const E: &str = "viewer_e_12345678";
pub(crate) const F: &str = "viewer_f_12345678";

thread_local! {
    // Labels by pointer; the resources stay alive so addresses never recycle.
    static LABELS: RefCell<(HashMap<usize, String>, Vec<ResourceRef>)> = RefCell::default();
}

pub(crate) fn ms(value: i64) -> Option<i64> {
    Some(value)
}

/// A fresh labelled resource (TS: a plain string).
pub(crate) fn res(label: &str) -> ResourceRef {
    let resource = Arc::new(Resource::new(ResourceKind::Overlap));
    LABELS.with(|labels| {
        let mut labels = labels.borrow_mut();
        labels
            .0
            .insert(Arc::as_ptr(&resource) as usize, label.to_owned());
        labels.1.push(resource.clone());
    });
    resource
}

pub(crate) fn labels(resources: &[ResourceRef]) -> Vec<String> {
    LABELS.with(|labels| {
        let labels = labels.borrow();
        resources
            .iter()
            .map(|resource| labels.0[&(Arc::as_ptr(resource) as usize)].clone())
            .collect()
    })
}

#[track_caller]
pub(crate) fn eq_labels(resources: &[ResourceRef], want: &[&str]) {
    assert_eq!(labels(resources), want);
}

#[track_caller]
pub(crate) fn contains_labels(resources: &[ResourceRef], want: &[&str]) {
    let have = labels(resources);
    for label in want {
        assert!(
            have.iter().any(|known| known == label),
            "released {have:?} does not contain {label:?}"
        );
    }
}

pub(crate) fn session_of(peer_id: &str) -> String {
    format!("{peer_id}_session")
}

pub(crate) fn parent_session(parent_peer_id: &str) -> String {
    if parent_peer_id == HOST {
        "host_session".to_owned()
    } else {
        session_of(parent_peer_id)
    }
}

/// Ports `controller(capacity, options)`: timeout defaults to 10 s and the
/// Host is upserted at the same capacity.
pub(crate) fn controller(capacity: i64, mut options: Options) -> Controller {
    options.host_peer_id = HOST.to_owned();
    options.endpoint_media_copy_capacity = capacity;
    if options.operation_timeout_ms == 0 {
        options.operation_timeout_ms = 10_000;
    }
    let mut routes = Controller::new(options);
    routes.upsert_participant(host_input("host_session", capacity), None);
    routes
}

pub(crate) fn opts() -> Options {
    Options::default()
}

pub(crate) fn sfu_opts() -> Options {
    Options {
        sfu_enabled: true,
        ..Options::default()
    }
}

pub(crate) fn quality_opts() -> Options {
    Options {
        quality_convergence_enabled: true,
        ..Options::default()
    }
}

pub(crate) fn add_viewer(
    routes: &mut Controller,
    peer_id: &str,
    capacity: i64,
    now_ms: Option<i64>,
) {
    routes.upsert_participant(
        viewer_input(peer_id, &session_of(peer_id), capacity),
        now_ms,
    );
}

pub(crate) fn viewer_input(peer_id: &str, session_id: &str, capacity: i64) -> ParticipantInput {
    ParticipantInput {
        peer_id: peer_id.to_owned(),
        role: Role::Viewer,
        session_id: session_id.to_owned(),
        effective_downstream_capacity: capacity,
        route_preference: None,
    }
}

pub(crate) fn host_input(session_id: &str, capacity: i64) -> ParticipantInput {
    ParticipantInput {
        peer_id: HOST.to_owned(),
        role: Role::Host,
        session_id: session_id.to_owned(),
        effective_downstream_capacity: capacity,
        route_preference: None,
    }
}

pub(crate) fn cursor_guard(operation: &OperationSnapshot) -> CandidateCursorGuard {
    CandidateCursorGuard {
        child_peer_id: operation.child_peer_id.clone(),
        child_session_id: operation.child_session_id.clone(),
        base_revision: operation.base_revision,
        fact_version: operation.fact_version,
        cursor: operation.cursor,
        plan: operation.candidates[operation.cursor].clone(),
    }
}

/// Ports the TS non-null assertion `operation!`.
#[track_caller]
pub(crate) fn must(operation: Option<OperationSnapshot>) -> OperationSnapshot {
    operation.expect("expected an operation")
}

#[track_caller]
pub(crate) fn no_operation(operation: Option<OperationSnapshot>) {
    if let Some(operation) = operation {
        panic!(
            "expected no operation, got child {:?} reason {:?}",
            operation.child_peer_id, operation.reason
        );
    }
}

#[track_caller]
pub(crate) fn expect_panic(want: &str, run: impl FnOnce()) {
    let Err(payload) = catch_unwind(AssertUnwindSafe(run)) else {
        panic!("expected a panic containing {want:?}");
    };
    let message = payload
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| {
            payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
        })
        .unwrap_or_default();
    assert!(
        message.contains(want),
        "panic {message:?} does not contain {want:?}"
    );
}
