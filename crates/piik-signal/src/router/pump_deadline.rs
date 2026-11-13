//! Pump operation deadlines: scheduling, invalidation and expiry handling.

use std::time::Duration;

use piik_route::OperationSnapshot;

use crate::router::route_debug;
use crate::server::Core;

impl Core {
    pub(crate) fn schedule_deadline(&mut self, room_id: &str, operation: &OperationSnapshot) {
        self.clear_deadline(room_id);
        let wake_in_ms = (operation.wake_at_ms - self.now()).max(0);
        if self.debug_enabled() {
            let current = operation
                .current
                .as_ref()
                .map(|c| self.debug_tuple(room_id, &c.tuple));
            route_debug!(self, room_id, "deadline-scheduled",
                "child" => self.debug_peer(room_id, &operation.child_peer_id),
                "wakeInMs" => wake_in_ms,
                "cursor" => operation.cursor,
                "candidateCount" => operation.candidates.len(),
                "current" => current);
        }
        let Some(rm) = self.room_rt(room_id) else {
            return;
        };
        let generation = rm.deadline_generation;
        let room_identity = rm.id;
        let child_peer_id = operation.child_peer_id.clone();
        let locker = self.locker.clone();
        let room = room_id.to_owned();
        let stop = (self.after_func)(
            Duration::from_millis(wake_in_ms.unsigned_abs()),
            Box::new(move || {
                locker.with(move |core| {
                    core.deadline_fired(&room, room_identity, generation, &child_peer_id)
                });
            }),
        );
        if let Some(rm) = self.room_rt_mut(room_id) {
            rm.deadline_stop = Some(stop);
        }
    }

    fn deadline_fired(
        &mut self,
        room_id: &str,
        room_identity: u64,
        generation: u64,
        child_peer_id: &str,
    ) {
        let closing = self.router.closing;
        let Some(rm) = self.room_rt_mut(room_id) else {
            return;
        };
        if rm.id != room_identity
            || rm.deadline_generation != generation
            || rm.controller.is_none()
            || closing
        {
            return;
        }
        rm.deadline_stop = None;
        let before = self.controller_revision(room_id);
        let now = self.now();
        let expired = self
            .ctl(room_id, |c| c.operation_expired(now))
            .unwrap_or_default();
        route_debug!(self, room_id, "deadline-fired",
            "child" => self.debug_peer(room_id, child_peer_id),
            "accepted" => expired.accepted,
            "exhausted" => !expired.failed_peer_ids.is_empty());
        self.release_resources(expired.released);
        if self.controller_revision(room_id) != before {
            self.broadcast_active(room_id);
        }
        let revision = self.controller_revision(room_id);
        self.send_route_failures(room_id, &expired.failed_peer_ids, revision);
        self.request_pump(room_id);
    }

    /// Bumps the generation to retire a callback that already lost the stop race.
    pub(crate) fn clear_deadline(&mut self, room_id: &str) {
        if let Some(rm) = self.room_rt_mut(room_id) {
            if let Some(stop) = rm.deadline_stop.take() {
                stop();
            }
            rm.deadline_generation += 1;
        }
    }
}
