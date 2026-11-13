//! SFU admission commits, exact resource release and physical drains.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use piik_protocol::{Role, ServerMessage, SfuSignalKind, SfuSignalMessage};
use piik_route::{
    CandidateReservation, DemandReason, ReservationKind, ResourceKind, ResourceRef, UpstreamKind,
};

use crate::router::RouteParticipant;
use crate::router::pump::{resource_fence, subscription_fence};
use crate::runtime::{Completion, opaque_id, spawn_blocking};
use crate::server::Core;
use piik_sfu::runtime::{Admission, DeferredWork, ResourceFence, SubscriptionFence};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DrainKind {
    Publication,
    Subscription,
    VideoPublication,
    Voice,
}

/// One asynchronous exact physical close. `id` identifies the task when a
/// settled drain checks it is still the registered one.
pub(crate) struct SfuDrainTask {
    pub id: u64,
    pub kind: DrainKind,
    pub fence: SubscriptionFence,
    pub operation: Option<Arc<Completion>>,
    pub close_physical: Option<DeferredWork>,
}

/// Commits the reservation's admission; publication commits return the
/// generations to drain in admission order.
pub(crate) fn commit_reservation(
    admission: Option<&RefCell<&mut Admission>>,
    drains: &RefCell<Vec<ResourceFence>>,
    reservation: &CandidateReservation,
) -> bool {
    match reservation.kind {
        Some(ReservationKind::Direct) => return true,
        Some(ReservationKind::SfuReuse) => {
            return reservation
                .edge
                .as_ref()
                .is_some_and(|edge| edge.kind == ResourceKind::SfuSubscription)
                && admission.is_some_and(|admission| {
                    admission
                        .borrow_mut()
                        .commit_subscription(&subscription_fence(
                            reservation.edge.as_ref().expect("edge"),
                        ))
                });
        }
        _ => {}
    }
    let Some(publication) = reservation
        .publication
        .as_ref()
        .filter(|p| p.kind == ResourceKind::SfuPublication)
    else {
        return false;
    };
    let Some(admission) = admission else {
        return false;
    };
    let Some(draining) = admission
        .borrow_mut()
        .commit_publication(&resource_fence(publication))
    else {
        return false;
    };
    drains.borrow_mut().extend(draining);
    true
}

/// A borrowed reuse edge belongs to another holder and is not released.
pub(crate) fn reservation_resources(reservation: &CandidateReservation) -> Vec<ResourceRef> {
    let mut resources = Vec::new();
    if let Some(edge) = &reservation.edge
        && (reservation.kind != Some(ReservationKind::SfuReuse) || !reservation.borrowed)
    {
        resources.push(edge.clone());
    }
    resources.extend(reservation.publication.clone());
    resources.extend(reservation.overlap.clone());
    resources
}

impl Core {
    pub(crate) fn release_reservation(&mut self, reservation: &CandidateReservation) {
        self.release_resources(reservation_resources(reservation));
    }

    /// Retires publications before their exact subscriptions.
    pub(crate) fn release_resources(&mut self, resources: Vec<ResourceRef>) {
        let (publications, others): (Vec<_>, Vec<_>) = resources
            .into_iter()
            .partition(|resource| resource.kind == ResourceKind::SfuPublication);
        for resource in publications.into_iter().chain(others) {
            self.release_resource(&resource);
        }
    }

    /// Idempotent through `released`.
    fn release_resource(&mut self, resource: &ResourceRef) {
        if resource.released.swap(true, Ordering::SeqCst) {
            return;
        }
        match resource.kind {
            ResourceKind::Overlap => {}
            ResourceKind::SfuSubscription => {
                self.release_subscription(&subscription_fence(resource));
            }
            ResourceKind::SfuPublication => {
                let fence = resource_fence(resource);
                if self
                    .router
                    .sfu
                    .as_mut()
                    .is_some_and(|fallback| fallback.admission.begin_drain(&fence))
                {
                    self.schedule_sfu_publication_drain(&fence);
                }
            }
        }
    }

    fn release_subscription(&mut self, fence: &SubscriptionFence) -> bool {
        let accepted = self
            .router
            .sfu
            .as_mut()
            .is_some_and(|fallback| fallback.admission.begin_subscription_drain(fence));
        if accepted {
            self.schedule_sfu_subscription_drain(fence);
        }
        accepted
    }

    /// Preserves the waiters' snapshot order.
    pub(crate) fn wake_resource_waiters(&mut self) {
        let room_ids = self.router.resource_waiters.keys();
        self.router.resource_waiters.clear();
        for room_id in room_ids {
            if self.ctl(&room_id, |c| c.touch_external_facts()).is_some() {
                self.request_pump(&room_id);
            }
        }
    }

    /// Reauthentication describes the existing physical connection without
    /// replacing it.
    pub(crate) fn send_fresh_sfu_config(&mut self, participant: &RouteParticipant) {
        if self.router.sfu.is_none() {
            return;
        }
        let Some(snapshot) = self.snapshot(&participant.room_id) else {
            return;
        };
        let room_media = self.room_media(&participant.room_id);
        if room_media && snapshot.video_generation.is_empty() {
            return;
        }
        let Some(publication) = snapshot
            .host_publication
            .as_ref()
            .filter(|p| p.physical_active)
        else {
            return;
        };
        if publication
            .resource
            .as_ref()
            .is_some_and(|resource| self.sfu_video_retiring(&resource_fence(resource)))
        {
            return;
        }
        let mut connection_id = publication.connection_id.clone();
        let mut revision = snapshot.revision;
        if participant.role == Role::Viewer {
            let Some(edge) = snapshot
                .upstream_by_viewer
                .get(&participant.peer_id)
                .filter(|edge| edge.kind == UpstreamKind::Sfu && edge.physical_active)
            else {
                return;
            };
            connection_id = edge.connection_id.clone();
            if room_media {
                let attaching = snapshot.operation.as_ref().filter(|op| {
                    op.current
                        .as_ref()
                        .is_some_and(|c| c.tuple.kind == UpstreamKind::Sfu)
                        && op.reason == DemandReason::PublicationAttach
                        && op.child_peer_id == participant.peer_id
                        && op.child_session_id == participant.session_id
                        && op.attach_connection_id == edge.connection_id
                });
                if let Some(op) = attaching {
                    connection_id = op.current.as_ref().expect("current").connection_id.clone();
                } else if edge.video_ready && !edge.video_connection_id.is_empty() {
                    connection_id = edge.video_connection_id.clone();
                } else {
                    return;
                }
            }
        }
        if room_media
            && let Some(op) = snapshot.operation.as_ref()
            && let Some(current) = op.current.as_ref()
            && current.tuple.kind == UpstreamKind::Sfu
            && (participant.role == Role::Host || op.child_peer_id == participant.peer_id)
        {
            revision = current.revision;
        }
        self.router_send(
            &participant.session_id,
            ServerMessage::SfuConfig {
                revision,
                publication_generation: publication.generation.clone(),
                connection_id: connection_id.clone(),
            },
        );
        if participant.role == Role::Host
            && let Some(resource) = publication.resource.as_ref()
        {
            let media = self
                .router
                .sfu
                .as_ref()
                .expect("SFU fallback")
                .media
                .clone();
            if let Ok(active) = media.publication_demand(&resource_fence(resource), &connection_id)
            {
                self.router_send(
                    &participant.session_id,
                    ServerMessage::SfuSignal(Box::new(SfuSignalMessage {
                        revision: snapshot.revision,
                        publication_generation: publication.generation.clone(),
                        connection_id,
                        kind: SfuSignalKind::Layers,
                        description: None,
                        candidate: None,
                        media: None,
                        active_count: Some(active),
                    })),
                );
            }
        }
    }

    /// Attaches video to an already admitted room edge. The pump has begun
    /// this exact operation and installed the current Host video connection.
    pub(crate) fn prepare_sfu_video_attachment(
        &mut self,
        room_id: &str,
        operation: Option<&piik_route::OperationSnapshot>,
    ) {
        let Some(operation) = operation else {
            return;
        };
        let Some(op_current) = operation.current.as_ref() else {
            return;
        };
        if self.router.sfu.is_none()
            || !self.room_media(room_id)
            || operation.reason != DemandReason::PublicationAttach
            || op_current.tuple.kind != UpstreamKind::Sfu
        {
            return;
        }
        let Some(snapshot) = self.snapshot(room_id) else {
            return;
        };
        let Some(publication) = snapshot.host_publication.as_ref() else {
            return;
        };
        let Some(resource) = publication.resource.as_ref() else {
            return;
        };
        if snapshot
            .operation
            .as_ref()
            .and_then(|op| op.current.as_ref())
            .is_none_or(|current| current.connection_id != op_current.connection_id)
            || publication.connection_id.is_empty()
        {
            return;
        }
        let host = self.store.get_connected_host(room_id);
        let child = self
            .store
            .get_connected_viewer(room_id, &operation.child_peer_id);
        let (Some(host), Some(child)) = (host, child) else {
            return;
        };
        if host.session_id != publication.host_session_id
            || child.session_id != operation.child_session_id
        {
            return;
        }
        let fence = resource_fence(resource);
        if self.sfu_video_retiring(&fence) {
            self.router.resource_waiters.set(room_id.to_owned(), ());
            return;
        }
        let config = |connection_id: &str| ServerMessage::SfuConfig {
            revision: op_current.revision,
            publication_generation: publication.generation.clone(),
            connection_id: connection_id.to_owned(),
        };
        self.router_send(&host.session_id, config(&publication.connection_id));
        self.router_send(&child.session_id, config(&op_current.connection_id));
        let media = self
            .router
            .sfu
            .as_ref()
            .expect("SFU fallback")
            .media
            .clone();
        if media.has_publication(&fence, &publication.connection_id) {
            self.prepare_sfu_subscriber(room_id, operation, &publication.generation);
        }
    }

    /// Retains room admission and the independent voice transports.
    pub(crate) fn retire_room_video(&mut self, room_id: &str) {
        if self.router.sfu.is_none() || !self.room_media(room_id) {
            return;
        }
        let Some(publication) = self.controller(room_id).and_then(|c| c.publication()) else {
            return;
        };
        let Some(resource) = publication.resource.as_ref() else {
            return;
        };
        let fence = resource_fence(resource);
        if fence.room_id == room_id {
            self.retire_sfu_video_publication(&fence, &publication.connection_id);
        }
    }

    /// Moves the room-media controller to one video generation.
    pub(crate) fn set_video_publication(&mut self, room_id: &str, generation: &str, paused: bool) {
        if !self.room_media(room_id) || !self.has_controller(room_id) {
            return;
        }
        let previous = self
            .snapshot(room_id)
            .map(|snapshot| snapshot.video_generation)
            .unwrap_or_default();
        if previous != generation {
            self.retire_room_video(room_id);
        }
        let now = self.now();
        let released = self
            .controller_mut(room_id)
            .map(|c| c.set_video_publication(generation, paused, now))
            .unwrap_or_default();
        self.release_resources(released);
        if previous != generation
            && !generation.is_empty()
            && let Some(controller) = self.controller_mut(room_id)
        {
            controller.set_host_video_connection_id(&opaque_id());
        }
        if !generation.is_empty() {
            for viewer in self.store.get_connected_viewers(room_id) {
                self.connect_participant(&RouteParticipant::new(
                    room_id,
                    Role::Viewer,
                    &viewer.peer_id,
                    &viewer.session_id,
                ));
            }
        }
        if generation.is_empty() && self.detached.is_none() {
            self.sync_voice_demand(room_id);
        }
        if !self.has_controller(room_id) {
            self.hook_routes_changed(room_id);
            return;
        }
        self.broadcast_active(room_id);
        self.request_pump(room_id);
    }

    pub(crate) fn retire_sfu_video_publication(
        &mut self,
        fence: &ResourceFence,
        connection_id: &str,
    ) {
        let Some(fallback) = self.router.sfu.as_ref() else {
            return;
        };
        let media = fallback.media.clone();
        let close_video: Option<DeferredWork> = if media.supports_retire_publication() {
            media.retire_publication(fence, connection_id)
        } else {
            // Legacy runtimes cannot detach a reusable room-generation handle;
            // their video-only publication fences are unique for each share.
            if self.room_media(&fence.room_id) {
                return;
            }
            if media.has_publication(fence, connection_id) {
                let fence = fence.clone();
                Some(Box::new(move || media.close_publication(&fence)))
            } else {
                None
            }
        };
        let Some(close_video) = close_video else {
            return;
        };
        let key = SubscriptionFence {
            resource: fence.clone(),
            viewer_peer_id: String::new(),
            connection_id: connection_id.to_owned(),
        };
        self.register_drain_task(DrainKind::VideoPublication, key.clone(), Some(close_video));
        self.run_sfu_drain(&key);
    }

    pub(crate) fn register_drain_task(
        &mut self,
        kind: DrainKind,
        key: SubscriptionFence,
        close_physical: Option<DeferredWork>,
    ) {
        let id = self.router.next_id();
        self.router.sfu_drain_tasks.set(
            key.clone(),
            SfuDrainTask {
                id,
                kind,
                fence: key,
                operation: None,
                close_physical,
            },
        );
    }

    /// The logical room reservation also owns its retiring picture facet; no
    /// replacement is issued until its exact physical close settles.
    pub(crate) fn sfu_video_retiring(&self, fence: &ResourceFence) -> bool {
        self.router.sfu_drain_tasks.iter().any(|(_, task)| {
            task.kind == DrainKind::VideoPublication && task.fence.resource == *fence
        })
    }

    /// Deletes sibling subscription tasks before draining the publication.
    pub(crate) fn schedule_sfu_publication_drain(&mut self, fence: &ResourceFence) {
        let siblings: Vec<SubscriptionFence> = self
            .router
            .sfu_drain_tasks
            .iter()
            .filter(|(_, task)| {
                task.kind == DrainKind::Subscription && task.fence.resource == *fence
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in siblings {
            self.router.sfu_drain_tasks.delete(&key);
        }
        self.schedule_sfu_drain(
            DrainKind::Publication,
            SubscriptionFence {
                resource: fence.clone(),
                ..SubscriptionFence::default()
            },
        );
    }

    /// Skips work when a pending publication drain already removes every
    /// participant.
    fn schedule_sfu_subscription_drain(&mut self, fence: &SubscriptionFence) {
        let publication_key = SubscriptionFence {
            resource: fence.resource.clone(),
            ..SubscriptionFence::default()
        };
        if self.router.sfu_drain_tasks.has(&publication_key) {
            return;
        }
        self.schedule_sfu_drain(DrainKind::Subscription, fence.clone());
    }

    fn schedule_sfu_drain(&mut self, kind: DrainKind, fence: SubscriptionFence) {
        if self.router.sfu.is_none() {
            return;
        }
        if !self.router.sfu_drain_tasks.has(&fence) {
            self.register_drain_task(kind, fence.clone(), None);
        }
        if self
            .router
            .sfu_drain_tasks
            .get(&fence)
            .is_some_and(|task| task.operation.is_none())
        {
            self.run_sfu_drain(&fence);
        }
    }

    /// Performs physical close outside the authority lock.
    pub(crate) fn run_sfu_drain(&mut self, key: &SubscriptionFence) -> Arc<Completion> {
        let Some(task) = self.router.sfu_drain_tasks.get_mut(key) else {
            let done = Completion::new();
            done.complete(Ok(()));
            return done;
        };
        if let Some(operation) = &task.operation {
            return operation.clone();
        }
        let operation = Completion::new();
        let Some(fallback) = self.router.sfu.as_ref() else {
            operation.complete(Ok(()));
            return operation;
        };
        let media = fallback.media.clone();
        task.operation = Some(operation.clone());
        let (task_id, kind, fence) = (task.id, task.kind, task.fence.clone());
        let close_physical = task.close_physical.take();
        let mut video_drains = Vec::new();
        if kind == DrainKind::Publication {
            let siblings: Vec<SubscriptionFence> = self
                .router
                .sfu_drain_tasks
                .iter()
                .filter(|(_, sibling)| {
                    sibling.kind == DrainKind::VideoPublication
                        && sibling.fence.resource == fence.resource
                })
                .map(|(sibling_key, _)| sibling_key.clone())
                .collect();
            for sibling in siblings {
                video_drains.push(self.run_sfu_drain(&sibling));
            }
        }
        self.router.inflight += 1;
        let locker = self.locker.clone();
        let key = key.clone();
        let completion = operation.clone();
        spawn_blocking(move || {
            let mut errors: Vec<String> = Vec::new();
            let closed = match kind {
                DrainKind::VideoPublication | DrainKind::Voice => {
                    close_physical.map_or(Ok(()), |close| close())
                }
                DrainKind::Publication => media.close_publication(&fence.resource),
                DrainKind::Subscription => media.close_subscription(&fence, &fence.connection_id),
            };
            if let Err(error) = closed {
                errors.push(error.to_string());
            }
            for retired in video_drains {
                if let Err(error) = retired.wait() {
                    errors.push(error);
                }
            }
            let error = (!errors.is_empty()).then(|| errors.join("\n"));
            let settled = locker.with(|core| {
                core.router.inflight -= 1;
                let result = core.settle_sfu_drain(&key, task_id, error);
                if let Some(task) = core.router.sfu_drain_tasks.get_mut(&key)
                    && task
                        .operation
                        .as_ref()
                        .is_some_and(|op| Arc::ptr_eq(op, &completion))
                {
                    task.operation = None;
                }
                completion.complete(result.clone());
                result
            });
            if settled.is_none() {
                completion.complete(Err("signaling server dropped".to_owned()));
            }
        });
        operation
    }

    /// A failed physical close remains charged until shutdown completes it.
    fn settle_sfu_drain(
        &mut self,
        key: &SubscriptionFence,
        task_id: u64,
        error: Option<String>,
    ) -> Result<(), String> {
        if let Some(error) = error {
            return Err(error);
        }
        let Some(task) = self
            .router
            .sfu_drain_tasks
            .get(key)
            .filter(|task| task.id == task_id)
        else {
            return Ok(());
        };
        let (kind, fence) = (task.kind, task.fence.clone());
        let fallback = self.router.sfu.as_mut().expect("SFU fallback");
        if kind == DrainKind::Publication && !fallback.admission.complete_drain(&fence.resource) {
            return Err("SFU drain has no matching resource generation".to_owned());
        }
        if kind == DrainKind::Subscription {
            fallback.admission.complete_subscription_drain(&fence);
        }
        self.router.sfu_drain_tasks.delete(key);
        if kind != DrainKind::Voice {
            self.wake_resource_waiters();
            if kind == DrainKind::VideoPublication && !self.router.closing {
                let operation = self
                    .controller(&fence.resource.room_id)
                    .and_then(|c| c.operation());
                self.prepare_sfu_video_attachment(&fence.resource.room_id, operation.as_ref());
            }
        } else if !self.router.closing {
            self.hook_routes_changed(&fence.resource.room_id);
        }
        Ok(())
    }

    /// The synchronous closing prefix of shutdown: retire rooms and start
    /// every drain. The caller waits for the returned tasks unlocked.
    pub(crate) fn begin_router_close(
        &mut self,
    ) -> Vec<(SubscriptionFence, u64, Option<Arc<Completion>>)> {
        self.router.closing = true;
        for room_id in self.router.rooms.keys() {
            self.clear_room(&room_id);
        }
        self.router.resource_waiters.clear();
        let Some(fallback) = self.router.sfu.as_mut() else {
            return Vec::new();
        };
        for fence in fallback.admission.begin_drain_all() {
            self.schedule_sfu_publication_drain(&fence);
        }
        self.router
            .sfu_drain_tasks
            .iter()
            .map(|(key, task)| (key.clone(), task.id, task.operation.clone()))
            .collect()
    }

    /// Re-runs a still registered task after its earlier operation settled.
    pub(crate) fn rerun_registered_drain(
        &mut self,
        key: &SubscriptionFence,
        task_id: u64,
    ) -> Option<Arc<Completion>> {
        self.router
            .sfu_drain_tasks
            .get(key)
            .is_some_and(|task| task.id == task_id)
            .then(|| self.run_sfu_drain(key))
    }
}
