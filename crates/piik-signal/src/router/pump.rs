//! Room authority prepares one candidate at a time; media signaling follows
//! admission.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;

use piik_protocol::{
    ErrorCode, PreparedRouteCandidate, RouteStatus, RouteTransport, ServerMessage,
};
use piik_room::ConnectedPeer;
use piik_route::{
    BeginInput, CandidateCursorGuard, CandidatePlan, CandidateReservation, Controller,
    DemandReason, OperationSnapshot, PublicationPlan, RejectionBucket, ReservationKind, Resource,
    ResourceKind, ResourceRef, TransitionKind, Transport, UpstreamKind,
};

use crate::router::graph::assignment_for;
use crate::router::{CANDIDATE_CONNECTION_ID_PREFIX, route_debug};
use crate::runtime::{opaque_id, spawn_blocking};
use crate::server::Core;
use piik_sfu::runtime::{ResourceFence, SubscriptionFence};

/// A Viewer's `route-ready` report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RouteReady {
    pub revision: i64,
    pub prepare: bool,
    pub quality_approved: bool,
}

/// One `sfu-config` message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SfuConfig {
    pub revision: i64,
    pub publication_generation: String,
    pub connection_id: String,
}

impl SfuConfig {
    pub(crate) fn message(&self) -> ServerMessage {
        ServerMessage::SfuConfig {
            revision: self.revision,
            publication_generation: self.publication_generation.clone(),
            connection_id: self.connection_id.clone(),
        }
    }
}

/// The allocations and signaling needed to begin an attempt.
#[derive(Debug, Clone, Default)]
pub(crate) struct PreparedCandidate {
    pub reservation: CandidateReservation,
    pub connection_id: String,
    pub publication_generation: String,
    pub publication_connection_id: String,
    pub host_session_id: String,
    pub host_sfu_config: Option<SfuConfig>,
    pub viewer_sfu_config: Option<SfuConfig>,
    pub video_ready: Option<RouteReady>,
}

/// One admitted candidate or a typed rejection.
#[derive(Debug, Clone)]
#[allow(
    clippy::large_enum_variant,
    reason = "built and moved once per pump turn"
)]
pub(crate) enum PrepareResult {
    Denied(RejectionBucket),
    Prepared(PreparedCandidate),
}

/// Keeps the admitted result across the pump driver's queued turn.
#[derive(Debug, Clone)]
pub(crate) struct PumpCandidate {
    operation: OperationSnapshot,
    plan: CandidatePlan,
    guard: CandidateCursorGuard,
    revision: i64,
    pub(crate) result: PrepareResult,
}

/// Room and controller identity captured by `request_pump`.
#[derive(Clone, Copy)]
pub(crate) struct PumpIdentity {
    room: u64,
    controller: u64,
}

impl Core {
    pub(crate) fn pump_identity(&self, room_id: &str) -> Option<PumpIdentity> {
        self.room_rt(room_id).map(|rm| PumpIdentity {
            room: rm.id,
            controller: if rm.controller.is_some() {
                rm.controller_id
            } else {
                0
            },
        })
    }

    /// Starts the single-flight driver. Reconcile, candidate selection and
    /// admission reservations run synchronously so later calls in the handler
    /// see the result and rooms reserve SFU capacity in wake order; the driver
    /// continues the remaining work on its own turn. `pumping` is set before
    /// that prefix, so re-entrant requests only set `requested`.
    pub(crate) fn request_pump(&mut self, room_id: &str) {
        if self.router.closing || !self.has_controller(room_id) {
            return;
        }
        let rm = self.room_rt_mut(room_id).expect("room runtime exists");
        rm.requested = true;
        if rm.pumping {
            return;
        }
        rm.pumping = true;
        rm.requested = false;
        let identity = self.pump_identity(room_id).expect("room runtime exists");
        let mut broadcast_revision = self.controller_revision(room_id);
        let mut pending = None;
        let failed = self.recover_pump(|core| {
            pending = core.pump_room(room_id, &mut broadcast_revision, None, true);
        });
        self.router.inflight += 1;
        let locker = self.locker.clone();
        let room_id = room_id.to_owned();
        spawn_blocking(move || {
            locker.with(move |core| {
                core.drive_pump(&room_id, identity, broadcast_revision, pending, failed)
            });
        });
    }

    /// Reports a failed pump segment, including controller assertion panics.
    pub(crate) fn recover_pump(&mut self, segment: impl FnOnce(&mut Self)) -> bool {
        let failed = catch_unwind(AssertUnwindSafe(|| segment(self))).is_err();
        if failed {
            tracing::error!("Media route reconciliation failed");
        }
        failed
    }

    /// The deferred remainder of `request_pump`: the suspended first
    /// iteration, the `requested` loop, re-arming and the Host failure report.
    pub(crate) fn drive_pump(
        &mut self,
        room_id: &str,
        identity: PumpIdentity,
        mut broadcast_revision: i64,
        pending: Option<PumpCandidate>,
        mut failed: bool,
    ) {
        self.router.inflight -= 1;
        let current = self.pump_identity(room_id);
        if current.is_none_or(|current| {
            current.room != identity.room || current.controller != identity.controller
        }) {
            // Preparation yielded before this share retired. Its remainder owns
            // only the reservation, never the replacement room's waiters.
            if let Some(PumpCandidate {
                result: PrepareResult::Prepared(prepared),
                ..
            }) = pending
            {
                self.release_reservation(&prepared.reservation);
            }
            if current.is_some_and(|current| current.room == identity.room) {
                let closing = self.router.closing;
                let rm = self.room_rt_mut(room_id).expect("room runtime exists");
                rm.pumping = false;
                if rm.controller.is_some() && rm.requested && !closing {
                    self.request_pump(room_id);
                }
            }
            return;
        }
        if !failed {
            failed = self.recover_pump(|core| {
                if let Some(pending) = pending {
                    core.pump_room(room_id, &mut broadcast_revision, Some(pending), false);
                }
                while !core.router.closing && core.room_rt(room_id).is_some_and(|rm| rm.requested) {
                    core.room_rt_mut(room_id).expect("room").requested = false;
                    if !core.has_controller(room_id) {
                        continue;
                    }
                    let mut revision = core.controller_revision(room_id);
                    core.pump_room(room_id, &mut revision, None, false);
                }
            });
        }
        let closing = self.router.closing;
        if let Some(rm) = self.room_rt_mut(room_id) {
            rm.pumping = false;
            if rm.requested && !closing {
                self.request_pump(room_id);
            }
        }
        if failed {
            self.send_room_error(room_id, ErrorCode::ServerError, "Media routing failed");
        }
    }

    /// Keeps admission ordered while allowing the driver to yield before signaling.
    pub(crate) fn pump_room(
        &mut self,
        room_id: &str,
        broadcast_revision: &mut i64,
        mut resume: Option<PumpCandidate>,
        yield_at_prepare: bool,
    ) -> Option<PumpCandidate> {
        loop {
            if !self.has_controller(room_id) {
                return None;
            }
            let candidate = match resume.take() {
                Some(candidate) => candidate,
                None => {
                    let mut candidate = self.select_candidate(room_id, broadcast_revision)?;
                    candidate.result = self.prepare_candidate(
                        room_id,
                        &candidate.operation,
                        &candidate.plan,
                        candidate.revision,
                    );
                    if yield_at_prepare {
                        return Some(candidate);
                    }
                    candidate
                }
            };
            let PumpCandidate {
                operation,
                plan,
                guard,
                result,
                ..
            } = candidate;
            let prepared = match result {
                PrepareResult::Denied(bucket) => {
                    route_debug!(self, room_id, "candidate-preparation-denied",
                        "child" => self.debug_peer(room_id, &operation.child_peer_id),
                        "route" => self.debug_tuple(room_id, &plan.tuple),
                        "rejectionBucket" => bucket.as_str(),
                        "cursor" => operation.cursor);
                    let now = self.now();
                    if matches!(
                        operation.reason,
                        DemandReason::QualityConvergence | DemandReason::RootConvergence
                    ) {
                        let skipped = self
                            .ctl(room_id, |c| c.skip_current_candidate(&guard, now, bucket))
                            .unwrap_or_default();
                        self.release_resources(skipped.released);
                        continue;
                    }
                    if bucket != RejectionBucket::SfuAdmission {
                        self.router.resource_waiters.delete(&room_id.to_owned());
                    } else {
                        if !self
                            .ctl(room_id, |c| {
                                c.note_current_candidate_rejection(
                                    &guard,
                                    RejectionBucket::SfuAdmission,
                                )
                            })
                            .unwrap_or(false)
                        {
                            continue;
                        }
                        self.router.resource_waiters.set(room_id.to_owned(), ());
                        self.send_viewer_route_status(
                            room_id,
                            &operation.demand_peer_id,
                            operation.base_revision,
                            RouteStatus::WaitingSfuAdmission,
                        );
                        self.schedule_deadline(room_id, &operation);
                        return None;
                    }
                    let skipped = self
                        .ctl(room_id, |c| c.skip_current_candidate(&guard, now, bucket))
                        .unwrap_or_default();
                    self.release_resources(skipped.released);
                    let revision = self.controller_revision(room_id);
                    self.send_route_failures(room_id, &skipped.failed_peer_ids, revision);
                    continue;
                }
                PrepareResult::Prepared(prepared) => prepared,
            };
            self.router.resource_waiters.delete(&room_id.to_owned());
            route_debug!(self, room_id, "candidate-prepared",
                "child" => self.debug_peer(room_id, &operation.child_peer_id),
                "route" => self.debug_tuple(room_id, &plan.tuple),
                "cursor" => operation.cursor);
            let mut begin_guard = guard.clone();
            if plan.endpoint_transition.kind == TransitionKind::BoundedGap {
                let now = self.now();
                let gap = self
                    .ctl(room_id, |c| {
                        c.retire_current_candidate_producer(&guard, now)
                    })
                    .unwrap_or_default();
                self.release_resources(gap.released);
                let revision = self.controller_revision(room_id);
                self.send_route_failures(room_id, &gap.failed_peer_ids, revision);
                if !gap.accepted {
                    self.release_reservation(&prepared.reservation);
                    continue;
                }
                self.broadcast_revision_change(room_id, broadcast_revision);
                let Some(current) = self
                    .controller(room_id)
                    .and_then(Controller::operation)
                    .filter(|op| op.cursor < op.candidates.len())
                else {
                    self.release_reservation(&prepared.reservation);
                    continue;
                };
                // The guard is re-derived: the retirement moved the revision.
                begin_guard = cursor_guard(&current, &current.candidates[current.cursor]);
            }
            let now = self.now();
            let begun = self
                .ctl(room_id, |c| {
                    c.begin_current_candidate(BeginInput {
                        guard: begin_guard,
                        now_ms: now,
                        connection_id: prepared.connection_id.clone(),
                        reservation: prepared.reservation.clone(),
                        publication_generation: prepared.publication_generation.clone(),
                        publication_connection_id: prepared.publication_connection_id.clone(),
                        host_session_id: prepared.host_session_id.clone(),
                    })
                })
                .unwrap_or_default();
            route_debug!(self, room_id, "candidate-begin-settled",
                "child" => self.debug_peer(room_id, &operation.child_peer_id),
                "route" => self.debug_tuple(room_id, &plan.tuple),
                "accepted" => begun.accepted,
                "exhausted" => !begun.failed_peer_ids.is_empty());
            // A rejected begin returns the reservation inside `released`.
            self.release_resources(begun.released);
            let Some(begun_operation) = begun
                .operation
                .filter(|op| begun.accepted && op.current.is_some())
            else {
                let revision = self.controller_revision(room_id);
                self.send_route_failures(room_id, &begun.failed_peer_ids, revision);
                continue;
            };
            if let Some(rm) = self.room_rt_mut(room_id) {
                rm.pending_media = Some(prepared.clone());
            }
            self.send_prepare_messages(room_id, &begun_operation, &prepared);
            route_debug!(self, room_id, "candidate-prepare-sent",
                "child" => self.debug_peer(room_id, &operation.child_peer_id),
                "route" => self.debug_tuple(room_id, &plan.tuple),
                "revision" => begun_operation.current.as_ref().map_or(0, |c| c.revision));
            self.schedule_deadline(room_id, &begun_operation);
            return None;
        }
    }

    fn broadcast_revision_change(&mut self, room_id: &str, broadcast_revision: &mut i64) {
        let revision = self.controller_revision(room_id);
        if revision == *broadcast_revision {
            return;
        }
        self.broadcast_active(room_id);
        *broadcast_revision = revision;
    }

    /// Reconciles, then either finishes the iteration (`None`) or picks the
    /// cursor's plan and its guard.
    fn select_candidate(
        &mut self,
        room_id: &str,
        broadcast_revision: &mut i64,
    ) -> Option<PumpCandidate> {
        let now = self.now();
        let reconciled = self.ctl(room_id, |c| c.reconcile(now))?;
        self.release_resources(reconciled.released);
        self.broadcast_revision_change(room_id, broadcast_revision);
        let revision = self.controller_revision(room_id);
        self.send_route_failures(room_id, &reconciled.failed_peer_ids, revision);
        let operation = reconciled
            .operation
            .or_else(|| self.controller(room_id).and_then(Controller::operation));
        let Some(operation) = operation else {
            self.clear_deadline(room_id);
            self.router.resource_waiters.delete(&room_id.to_owned());
            return None;
        };
        self.router.resource_waiters.delete(&room_id.to_owned());
        if operation.current.is_some() {
            self.schedule_deadline(room_id, &operation);
            return None;
        }
        let plan = operation.candidates.get(operation.cursor)?.clone();
        let guard = cursor_guard(&operation, &plan);
        let revision = operation.base_revision
            + if plan.endpoint_transition.kind == TransitionKind::BoundedGap {
                2
            } else {
                1
            };
        Some(PumpCandidate {
            operation,
            plan,
            guard,
            revision,
            result: PrepareResult::Denied(RejectionBucket::None),
        })
    }

    /// Reserves the exact connection before any client can signal it.
    fn prepare_candidate(
        &mut self,
        room_id: &str,
        operation: &OperationSnapshot,
        plan: &CandidatePlan,
        revision: i64,
    ) -> PrepareResult {
        let Some(snapshot) = self.snapshot(room_id) else {
            return PrepareResult::Denied(RejectionBucket::Stale);
        };
        let child = self
            .store
            .get_connected_viewer(room_id, &operation.child_peer_id);
        let share_generation = if self.room_media(room_id) {
            self.room_rt(room_id)
                .map(|rm| rm.media_generation.clone())
                .unwrap_or_default()
        } else {
            self.share_generation(room_id)
        };
        if child.is_none_or(|child| child.session_id != operation.child_session_id)
            || share_generation.is_empty()
        {
            return PrepareResult::Denied(RejectionBucket::Stale);
        }
        let connection_id = format!("{CANDIDATE_CONNECTION_ID_PREFIX}{}", opaque_id());
        if !operation.attach_connection_id.is_empty() {
            return PrepareResult::Prepared(PreparedCandidate {
                connection_id,
                ..PreparedCandidate::default()
            });
        }
        let overlap = (plan.endpoint_transition.kind == TransitionKind::Overlap).then(|| {
            overlap_resource(
                plan.endpoint_transition
                    .producer_peer_id
                    .as_deref()
                    .unwrap_or_default(),
            )
        });
        if plan.tuple.kind == UpstreamKind::Peer {
            return PrepareResult::Prepared(PreparedCandidate {
                connection_id,
                reservation: CandidateReservation {
                    kind: Some(ReservationKind::Direct),
                    overlap,
                    ..CandidateReservation::default()
                },
                ..PreparedCandidate::default()
            });
        }
        let host = self.store.get_connected_host(room_id);
        let host_peer_id = self.host_peer_of(room_id);
        let Some(host) =
            host.filter(|host| self.router.sfu.is_some() && host.peer_id == host_peer_id)
        else {
            return PrepareResult::Denied(RejectionBucket::Stale);
        };
        if plan.tuple.publication == Some(PublicationPlan::Reuse) {
            let Some(publication) = snapshot.host_publication.as_ref().filter(|p| {
                p.resource
                    .as_ref()
                    .is_some_and(|r| r.kind == ResourceKind::SfuPublication)
            }) else {
                return PrepareResult::Denied(RejectionBucket::Stale);
            };
            let resource = publication.resource.as_ref().expect("filtered above");
            let fence = SubscriptionFence {
                resource: resource_fence(resource),
                viewer_peer_id: operation.child_peer_id.clone(),
                connection_id: connection_id.clone(),
            };
            let admission = &mut self.router.sfu.as_mut().expect("SFU fallback").admission;
            if !admission.reserve_subscription(&fence) {
                return PrepareResult::Denied(RejectionBucket::SfuAdmission);
            }
            return PrepareResult::Prepared(PreparedCandidate {
                connection_id: connection_id.clone(),
                host_session_id: host.session_id,
                reservation: CandidateReservation {
                    kind: Some(ReservationKind::SfuReuse),
                    edge: Some(subscription_resource(&fence)),
                    overlap,
                    ..CandidateReservation::default()
                },
                publication_generation: publication.generation.clone(),
                viewer_sfu_config: Some(SfuConfig {
                    revision,
                    publication_generation: publication.generation.clone(),
                    connection_id,
                }),
                ..PreparedCandidate::default()
            });
        }
        self.prepare_new_publication(
            room_id,
            operation,
            revision,
            connection_id,
            share_generation,
            host,
            overlap,
        )
    }

    /// One candidate reserves both publication and first subscription atomically.
    #[allow(clippy::too_many_arguments)]
    fn prepare_new_publication(
        &mut self,
        room_id: &str,
        operation: &OperationSnapshot,
        revision: i64,
        connection_id: String,
        share_generation: String,
        host: ConnectedPeer,
        overlap: Option<ResourceRef>,
    ) -> PrepareResult {
        let publication_generation = opaque_id();
        let publication_fence = ResourceFence {
            room_id: room_id.to_owned(),
            share_generation,
            publication_generation: publication_generation.clone(),
        };
        let subscription_fence = SubscriptionFence {
            resource: publication_fence.clone(),
            viewer_peer_id: operation.child_peer_id.clone(),
            connection_id: connection_id.clone(),
        };
        let admission = &mut self.router.sfu.as_mut().expect("SFU fallback").admission;
        if !admission.reserve_publication(&publication_fence) {
            return PrepareResult::Denied(RejectionBucket::SfuAdmission);
        }
        if !admission.reserve_subscription(&subscription_fence) {
            admission.begin_drain(&publication_fence);
            self.schedule_sfu_publication_drain(&publication_fence);
            return PrepareResult::Denied(RejectionBucket::SfuAdmission);
        }
        let publication_connection_id = publication_generation.clone();
        PrepareResult::Prepared(PreparedCandidate {
            connection_id: connection_id.clone(),
            host_session_id: host.session_id,
            publication_generation: publication_generation.clone(),
            publication_connection_id: publication_connection_id.clone(),
            reservation: CandidateReservation {
                kind: Some(ReservationKind::SfuCreate),
                edge: Some(subscription_resource(&subscription_fence)),
                publication: Some(publication_resource(&publication_fence)),
                overlap,
                borrowed: false,
            },
            host_sfu_config: Some(SfuConfig {
                revision,
                publication_generation: publication_generation.clone(),
                connection_id: publication_connection_id,
            }),
            viewer_sfu_config: Some(SfuConfig {
                revision,
                publication_generation,
                connection_id,
            }),
            video_ready: None,
        })
    }

    /// Preserves recipient order. It sends nothing when the child is stale or
    /// the peer parent vanished; the deadline then resolves the operation.
    fn send_prepare_messages(
        &mut self,
        room_id: &str,
        operation: &OperationSnapshot,
        prepared: &PreparedCandidate,
    ) {
        if self.room_media(room_id) {
            self.hook_routes_changed(room_id);
            if operation.video_generation.is_empty() {
                return;
            }
        }
        let Some(current) = operation.current.as_ref() else {
            return;
        };
        let Some(snapshot) = self.snapshot(room_id) else {
            return;
        };
        let candidate = prepared_route_candidate(operation, &current.tuple, &current.connection_id);
        let assignments = self.assignments(
            room_id,
            &snapshot,
            Some(&current.tuple),
            &operation.child_peer_id,
            &prepared.publication_generation,
        );
        let Some(child) = self
            .store
            .get_connected_viewer(room_id, &operation.child_peer_id)
            .filter(|child| child.session_id == operation.child_session_id)
        else {
            return;
        };
        let prepare = |peer_id: &str| ServerMessage::RouteUpdatePrepare {
            revision: current.revision,
            assignment: assignment_for(&assignments, peer_id),
            candidate: candidate.clone(),
        };
        let child_update = prepare(&operation.child_peer_id);
        if current.tuple.kind == UpstreamKind::Peer {
            let Some(parent) = self.connected_peer(room_id, &current.tuple.parent_peer_id) else {
                return;
            };
            self.router_send(&child.session_id, child_update);
            self.router_send(&parent.session_id, prepare(&parent.peer_id));
            return;
        }
        if let Some(config) = &prepared.host_sfu_config
            && let Some(host) = self
                .store
                .get_connected_host(room_id)
                .filter(|host| host.session_id == prepared.host_session_id)
        {
            self.router_send(&host.session_id, prepare(&host.peer_id));
            self.router_send(&host.session_id, config.message());
        }
        self.router_send(&child.session_id, child_update);
        if !operation.attach_connection_id.is_empty() {
            self.prepare_sfu_video_attachment(room_id, Some(operation));
            return;
        }
        if let Some(config) = &prepared.viewer_sfu_config {
            self.router_send(&child.session_id, config.message());
            if prepared.host_sfu_config.is_none() {
                self.prepare_sfu_subscriber(room_id, operation, &prepared.publication_generation);
            }
        }
    }
}

pub(crate) fn cursor_guard(
    operation: &OperationSnapshot,
    plan: &CandidatePlan,
) -> CandidateCursorGuard {
    CandidateCursorGuard {
        child_peer_id: operation.child_peer_id.clone(),
        child_session_id: operation.child_session_id.clone(),
        base_revision: operation.base_revision,
        fact_version: operation.fact_version,
        cursor: operation.cursor,
        plan: plan.clone(),
    }
}

/// Includes `connectionAttempt` only when the current attempt carries one.
fn prepared_route_candidate(
    operation: &OperationSnapshot,
    tuple: &piik_route::CandidateTuple,
    connection_id: &str,
) -> PreparedRouteCandidate {
    let transport = if tuple.kind == UpstreamKind::Peer && tuple.transport != Some(Transport::Sfu) {
        RouteTransport::Direct
    } else {
        RouteTransport::Sfu
    };
    PreparedRouteCandidate {
        child_peer_id: operation.child_peer_id.clone(),
        connection_id: connection_id.to_owned(),
        transport,
        quality_probe: operation.reason == DemandReason::QualityConvergence,
        connection_attempt: operation
            .current
            .as_ref()
            .and_then(|c| c.connection_attempt)
            .map(|attempt| piik_protocol::ConnectionAttempt {
                current: attempt.current,
                total: attempt.total,
            }),
    }
}

fn overlap_resource(endpoint_peer_id: &str) -> ResourceRef {
    let mut resource = Resource::new(ResourceKind::Overlap);
    resource.endpoint_peer_id = endpoint_peer_id.to_owned();
    Arc::new(resource)
}

pub(crate) fn subscription_resource(fence: &SubscriptionFence) -> ResourceRef {
    let mut resource = Resource::new(ResourceKind::SfuSubscription);
    resource.room_id = fence.resource.room_id.clone();
    resource.share_generation = fence.resource.share_generation.clone();
    resource.publication_generation = fence.resource.publication_generation.clone();
    resource.viewer_peer_id = fence.viewer_peer_id.clone();
    resource.connection_id = fence.connection_id.clone();
    Arc::new(resource)
}

pub(crate) fn publication_resource(fence: &ResourceFence) -> ResourceRef {
    let mut resource = Resource::new(ResourceKind::SfuPublication);
    resource.room_id = fence.room_id.clone();
    resource.share_generation = fence.share_generation.clone();
    resource.publication_generation = fence.publication_generation.clone();
    Arc::new(resource)
}

pub(crate) fn resource_fence(resource: &Resource) -> ResourceFence {
    ResourceFence {
        room_id: resource.room_id.clone(),
        share_generation: resource.share_generation.clone(),
        publication_generation: resource.publication_generation.clone(),
    }
}

pub(crate) fn subscription_fence(resource: &Resource) -> SubscriptionFence {
    SubscriptionFence {
        resource: resource_fence(resource),
        viewer_peer_id: resource.viewer_peer_id.clone(),
        connection_id: resource.connection_id.clone(),
    }
}
