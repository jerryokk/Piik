//! Applies route controller decisions through room signaling and media
//! resources.
//!
//! The router owns no lock: [`Core`] is guarded by the server's authority
//! mutex and every method here runs with it held. Work the router defers
//! (pump drivers, media prepares, drains and timer callbacks) re-acquires the
//! lock itself and revalidates the owning room, operation and resource before
//! committing effects.

pub(crate) mod graph;
pub(crate) mod ops;
pub(crate) mod pump;
pub(crate) mod pump_deadline;
pub(crate) mod quality;
pub(crate) mod sfu;
pub(crate) mod sfu_signal;
pub(crate) mod signal;

use std::cell::RefCell;
use std::sync::Arc;

use piik_ordered::Map;
use piik_protocol::{
    MediaRouteUpstream, ParticipantRouteAssignment, Role, RouteDiagnosticSnapshot, RoutePolicy,
    ViewerRoutePreference, packet_loss_percent_from_deltas,
};
use piik_route::{
    CandidateReservation, CommitReservation, Controller, ParticipantInput, RevisionSequence,
    RouteQualityEvidenceInput, RouteQualityEvidenceResult, RouteSnapshot, UpstreamKind,
};
use serde_json::json;

pub(crate) use crate::router::graph::{assignment_for, empty_assignment, video_connection_id};
use crate::router::pump::PreparedCandidate;
use crate::router::sfu::{SfuDrainTask, commit_reservation};
use crate::runtime::TimerStop;
use crate::server::Core;
use piik_sfu::runtime::{Admission, Runtime, SubscriptionFence};

pub(crate) const DEFAULT_ROUTE_OPERATION_TIMEOUT_MS: i64 = 20_000;

/// A retired server candidate must never be mistaken for a parent-initiated
/// replacement. Its ID remains opaque to clients.
pub(crate) const CANDIDATE_CONNECTION_ID_PREFIX: &str = "route_";

/// Embedded media and its admission ledger; a zero prepare timeout selects
/// the router default.
pub struct SfuFallback {
    pub media: Arc<dyn Runtime>,
    pub admission: Admission,
    pub prepare_timeout_ms: i64,
}

/// Admitted identity and optional Host policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RouteParticipant {
    pub room_id: String,
    pub role: Role,
    pub peer_id: String,
    pub session_id: String,
    pub route_policy: Option<RoutePolicy>,
    pub route_preference: Option<ViewerRoutePreference>,
}

impl RouteParticipant {
    pub(crate) fn new(room_id: &str, role: Role, peer_id: &str, session_id: &str) -> Self {
        Self {
            room_id: room_id.to_owned(),
            role,
            peer_id: peer_id.to_owned(),
            session_id: session_id.to_owned(),
            route_policy: None,
            route_preference: None,
        }
    }
}

/// The joining session's route assignment.
#[derive(Debug, Clone)]
pub(crate) struct HybridState {
    pub route_revision: i64,
    pub route_assignment: ParticipantRouteAssignment,
}

impl HybridState {
    pub(crate) fn empty() -> Self {
        Self {
            route_revision: 0,
            route_assignment: empty_assignment(),
        }
    }
}

/// One Viewer's active Peer or SFU upstream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveViewerMediaEdge {
    pub revision: i64,
    pub connection_id: String,
    pub upstream: MediaRouteUpstream,
}

/// Unassigned makes the signaling server check the committed-edge assignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SignalAuthorization {
    Unassigned,
    Denied,
    Allowed,
    Probe,
}

/// Session/connection identity for signal authorization. Kinds, phases and
/// description types are their wire spellings; empty means absent.
#[derive(Debug, Clone, Default)]
pub(crate) struct PeerSignalInput {
    pub room_id: String,
    pub source_peer_id: String,
    pub source_session_id: String,
    pub target_peer_id: String,
    pub target_session_id: String,
    pub connection_id: String,
    pub signal_kind: &'static str,
    pub description_type: &'static str,
    pub punch_phase: &'static str,
    pub punched: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct ViewerRouteSettings {
    pub capacity: i64,
    pub session_id: String,
    pub volunteer: bool,
    pub preference: ViewerRoutePreference,
}

impl Default for ViewerRouteSettings {
    fn default() -> Self {
        Self {
            capacity: 0,
            session_id: String::new(),
            volunteer: false,
            preference: ViewerRoutePreference::Auto,
        }
    }
}

/// One room's controller. `pumping` prevents concurrent drivers;
/// `deadline_generation` retires a deadline callback that lost the stop race.
/// `id` and `controller_id` identify this runtime instance across unlocked work.
pub(crate) struct RoomRuntime {
    pub id: u64,
    pub host_peer_id: String,
    pub controller: Option<Controller>,
    pub controller_id: u64,
    pub revisions: Arc<RevisionSequence>,
    pub room_media: bool,
    pub media_generation: String,
    pub pending_media: Option<PreparedCandidate>,
    /// Viewer choices survive signaling recovery and share replacement.
    pub viewer_settings: Map<String, ViewerRouteSettings>,
    pub requested: bool,
    pub pumping: bool,
    pub deadline_stop: Option<TimerStop>,
    pub deadline_generation: u64,
}

impl RoomRuntime {
    pub(crate) fn new(id: u64, revisions: Arc<RevisionSequence>) -> Self {
        Self {
            id,
            host_peer_id: String::new(),
            controller: None,
            controller_id: 0,
            revisions,
            room_media: false,
            media_generation: String::new(),
            pending_media: None,
            viewer_settings: Map::new(),
            requested: false,
            pumping: false,
            deadline_stop: None,
            deadline_generation: 0,
        }
    }
}

/// Router state inside [`Core`].
pub(crate) struct Router {
    pub capacity: i64,
    pub sfu: Option<SfuFallback>,
    pub sfu_only: bool,
    /// Rooms, drain tasks and waiters are walked in insertion order.
    pub rooms: Map<String, RoomRuntime>,
    pub resource_waiters: Map<String, ()>,
    pub sfu_drain_tasks: Map<SubscriptionFence, SfuDrainTask>,
    /// Counts the deferred jobs that will re-acquire the lock: pump drivers,
    /// subscriber prepares, voice negotiations and drains.
    pub inflight: usize,
    pub closing: bool,
    pub next_id: u64,
}

impl Router {
    /// # Panics
    /// When the endpoint media copy capacity is invalid.
    pub(crate) fn new(capacity: i64, sfu: Option<SfuFallback>, sfu_only: bool) -> Self {
        if let Err(error) = piik_protocol::assert_endpoint_media_copy_capacity(capacity) {
            panic!("{error}");
        }
        Self {
            capacity,
            sfu,
            sfu_only,
            rooms: Map::new(),
            resource_waiters: Map::new(),
            sfu_drain_tasks: Map::new(),
            inflight: 0,
            closing: false,
            next_id: 0,
        }
    }

    pub(crate) fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

/// Emits one debug event when route debugging is enabled.
macro_rules! route_debug {
    ($core:expr, $room:expr, $event:expr $(, $key:literal => $value:expr)* $(,)?) => {
        if $core.debug_enabled() {
            let details: Vec<(&'static str, ::serde_json::Value)> =
                vec![$(($key, ::serde_json::json!($value))),*];
            $core.emit_debug($room, $event, details);
        }
    };
}
pub(crate) use route_debug;

impl Core {
    pub(crate) fn now(&self) -> i64 {
        (self.now)()
    }

    pub(crate) fn now_some(&self) -> Option<i64> {
        Some(self.now())
    }

    pub(crate) fn room_rt(&self, room_id: &str) -> Option<&RoomRuntime> {
        self.router.rooms.get(&room_id.to_owned())
    }

    pub(crate) fn room_rt_mut(&mut self, room_id: &str) -> Option<&mut RoomRuntime> {
        self.router.rooms.get_mut(&room_id.to_owned())
    }

    pub(crate) fn controller(&self, room_id: &str) -> Option<&Controller> {
        self.room_rt(room_id)?.controller.as_ref()
    }

    pub(crate) fn controller_mut(&mut self, room_id: &str) -> Option<&mut Controller> {
        self.room_rt_mut(room_id)?.controller.as_mut()
    }

    pub(crate) fn has_controller(&self, room_id: &str) -> bool {
        self.controller(room_id).is_some()
    }

    pub(crate) fn snapshot(&self, room_id: &str) -> Option<RouteSnapshot> {
        self.controller(room_id).map(Controller::snapshot)
    }

    pub(crate) fn controller_revision(&self, room_id: &str) -> i64 {
        self.controller(room_id).map_or(0, Controller::revision)
    }

    pub(crate) fn host_peer_of(&self, room_id: &str) -> String {
        self.room_rt(room_id)
            .map(|rm| rm.host_peer_id.clone())
            .unwrap_or_default()
    }

    pub(crate) fn room_media(&self, room_id: &str) -> bool {
        self.room_rt(room_id).is_some_and(|rm| rm.room_media)
    }

    /// `r.room`: the runtime, created on first use.
    pub(crate) fn ensure_room(&mut self, room_id: &str) -> &mut RoomRuntime {
        if !self.router.rooms.has(&room_id.to_owned()) {
            let id = self.router.next_id();
            self.router.rooms.set(
                room_id.to_owned(),
                RoomRuntime::new(id, Arc::new(RevisionSequence::default())),
            );
        }
        self.router
            .rooms
            .get_mut(&room_id.to_owned())
            .expect("room runtime was just inserted")
    }

    /// Runs `f` with the room controller and the router's admission commit.
    /// The commit's publication drains are scheduled once `f` returns.
    pub(crate) fn with_commit<R>(
        &mut self,
        room_id: &str,
        f: impl FnOnce(&mut Controller, CommitReservation<'_>) -> R,
    ) -> Option<R> {
        let mut controller = self.room_rt_mut(room_id)?.controller.take()?;
        let drains = RefCell::new(Vec::new());
        let out = {
            let admission = self
                .router
                .sfu
                .as_mut()
                .map(|fallback| RefCell::new(&mut fallback.admission));
            let commit = |reservation: &CandidateReservation| {
                commit_reservation(admission.as_ref(), &drains, reservation)
            };
            f(&mut controller, Some(&commit))
        };
        if let Some(rm) = self.room_rt_mut(room_id) {
            rm.controller = Some(controller);
        }
        for fence in drains.into_inner() {
            self.schedule_sfu_publication_drain(&fence);
        }
        Some(out)
    }

    pub(crate) fn connect_participant(&mut self, input: &RouteParticipant) -> HybridState {
        let room_id = input.room_id.clone();
        let now = self.now_some();
        self.ensure_room(&room_id);
        if input.role == Role::Host {
            let rebind = {
                let rm = self.room_rt(&room_id).expect("room runtime exists");
                rm.controller.is_some() && rm.host_peer_id != input.peer_id
            };
            if rebind {
                self.clear_deadline(&room_id);
                let released = self
                    .controller_mut(&room_id)
                    .map(|c| c.rebind_host_identity(&input.peer_id, &input.session_id, now))
                    .unwrap_or_default();
                self.release_resources(released);
            }
            self.room_rt_mut(&room_id)
                .expect("room runtime exists")
                .host_peer_id = input.peer_id.clone();
            if !self.has_controller(&room_id) {
                self.create_controller(&room_id, input);
            }
        } else {
            let mut settings = self.viewer_settings(&room_id, &input.peer_id);
            if settings.session_id != input.session_id {
                settings.session_id = input.session_id.clone();
                settings.preference = ViewerRoutePreference::Auto;
                settings.volunteer = false;
            }
            if let Some(preference) = input.route_preference {
                settings.preference = preference;
                if !self.viewer_route_allowed(&room_id, preference) {
                    settings.preference = ViewerRoutePreference::Auto;
                }
            }
            self.set_viewer_settings(&room_id, &input.peer_id, settings);
        }
        if self.has_controller(&room_id) {
            if self.room_media(&room_id)
                && input.role == Role::Viewer
                && self.video_generation(&room_id).is_empty()
                && !self.voice_allowed(&room_id, &input.peer_id)
            {
                return HybridState::empty();
            }
            let (capacity, preference) = if input.role == Role::Host {
                (self.router.capacity, None)
            } else {
                let settings = self.viewer_settings(&room_id, &input.peer_id);
                (settings.capacity, Some(settings.preference))
            };
            let released = self
                .controller_mut(&room_id)
                .map(|c| {
                    c.upsert_participant(
                        ParticipantInput {
                            peer_id: input.peer_id.clone(),
                            role: input.role,
                            session_id: input.session_id.clone(),
                            effective_downstream_capacity: capacity,
                            route_preference: preference,
                        },
                        now,
                    )
                })
                .unwrap_or_default();
            self.release_resources(released);
            if input.role == Role::Viewer {
                let volunteer = self.viewer_settings(&room_id, &input.peer_id).volunteer;
                if let Some(controller) = self.controller_mut(&room_id) {
                    controller.set_relay_volunteer(
                        &input.peer_id,
                        &input.session_id,
                        volunteer,
                        now,
                    );
                }
            }
        }
        match self.snapshot(&room_id) {
            Some(snapshot) => {
                let assignments = self.assignments(&room_id, &snapshot, None, "", "");
                HybridState {
                    route_revision: snapshot.revision,
                    route_assignment: assignment_for(&assignments, &input.peer_id),
                }
            }
            None => HybridState::empty(),
        }
    }

    pub(crate) fn router_complete_authentication(&mut self, participant: &RouteParticipant) {
        if !self.has_controller(&participant.room_id) {
            return;
        }
        self.broadcast_active(&participant.room_id);
        self.send_fresh_sfu_config(participant);
        self.request_pump(&participant.room_id);
    }

    pub(crate) fn router_set_paused(&mut self, room_id: &str, paused: bool) {
        if !self.has_controller(room_id) {
            return;
        }
        let before = self.controller_revision(room_id);
        let now = self.now_some();
        let released = self
            .controller_mut(room_id)
            .map(|c| c.set_paused(paused, now))
            .unwrap_or_default();
        self.release_resources(released);
        if self.controller_revision(room_id) != before {
            self.broadcast_active(room_id);
        }
        if !paused || self.room_media(room_id) {
            self.request_pump(room_id);
        }
    }

    pub(crate) fn route_diagnostic_snapshot(&self, room_id: &str) -> RouteDiagnosticSnapshot {
        match self.controller(room_id) {
            Some(controller) => controller.route_diagnostic_snapshot(self.now()),
            None => RouteDiagnosticSnapshot {
                children: Vec::new(),
                operation: None,
            },
        }
    }

    /// Runs `f` on the room controller.
    pub(crate) fn ctl<R>(
        &mut self,
        room_id: &str,
        f: impl FnOnce(&mut Controller) -> R,
    ) -> Option<R> {
        self.controller_mut(room_id).map(f)
    }

    pub(crate) fn viewer_settings(&self, room_id: &str, peer_id: &str) -> ViewerRouteSettings {
        self.room_rt(room_id)
            .and_then(|rm| rm.viewer_settings.get(&peer_id.to_owned()).cloned())
            .unwrap_or_default()
    }

    pub(crate) fn set_viewer_settings(
        &mut self,
        room_id: &str,
        peer_id: &str,
        settings: ViewerRouteSettings,
    ) {
        if let Some(rm) = self.room_rt_mut(room_id) {
            rm.viewer_settings.set(peer_id.to_owned(), settings);
        }
    }

    pub(crate) fn router_observe_quality_evidence(
        &mut self,
        room_id: &str,
        evidence: RouteQualityEvidenceInput,
    ) -> RouteQualityEvidenceResult {
        let debug = self.debug_enabled().then(|| evidence.clone());
        let result = self
            .controller_mut(room_id)
            .map_or(RouteQualityEvidenceResult::Rejected, |c| {
                c.observe_quality_evidence(evidence)
            });
        if let Some(evidence) = debug.filter(|_| result != RouteQualityEvidenceResult::Rejected) {
            let upstream = if evidence.upstream.kind == UpstreamKind::Peer {
                format!(
                    "p2p:{}",
                    self.debug_peer(room_id, &evidence.upstream.peer_id)
                )
            } else {
                "sfu".to_owned()
            };
            let m = &evidence.metrics;
            let packet_loss = packet_loss_percent_from_deltas(
                m.packets_received_delta.map(|v| v as f64),
                m.packets_lost_delta.map(|v| v as f64),
            );
            self.emit_debug(
                room_id,
                "viewer-quality-evidence",
                vec![
                    (
                        "viewer",
                        json!(self.debug_peer(room_id, &evidence.child_peer_id)),
                    ),
                    ("upstream", json!(upstream)),
                    ("natTraversalPath", json!(m.nat_traversal_path.as_str())),
                    ("framesPerSecond", json!(m.frames_per_second)),
                    ("bitrateKbps", json!(m.bitrate_kbps)),
                    ("width", json!(m.width)),
                    ("height", json!(m.height)),
                    ("framesDecoded", json!(m.frames_decoded_delta)),
                    ("freezes", json!(m.freeze_count_delta)),
                    ("pauses", json!(m.pause_count_delta)),
                    ("packetsReceivedDelta", json!(m.packets_received_delta)),
                    ("packetsLostDelta", json!(m.packets_lost_delta)),
                    ("packetLossPercent", json!(packet_loss)),
                    ("rttMs", json!(m.rtt_ms)),
                    ("jitterMs", json!(m.jitter_ms)),
                ],
            );
        }
        result
    }
}
#[cfg(test)]
pub(crate) use crate::router::quality::{QualityCopyContext, QualitySample, quality_copy_context};
