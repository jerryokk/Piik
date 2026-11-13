//! The router harness: a detached [`Core`] with a memory store, a real
//! admission ledger, [`FakeMedia`], recorded sends and the same authority
//! mutex every router call and assertion takes.
//!
//! Timing model. Each [`RouterHarness::locked`] call is one critical section
//! under the authority mutex; deferred router work re-enters the lock and
//! `settle` waits until `router.inflight` reaches zero. With fake timers
//! `wait_for` ticks the clock 50 ms before each of 20 checks, `advance` fires
//! due timers in due order and settles between them, and `set` shifts pending
//! timers with the jump.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use piik_protocol::{
    CodeEntryPolicy, IceConfig, MediaRouteUpstream, NatTraversalPath, Nullable,
    ParticipantRouteAssignment, PreparedRouteCandidate, Role, RouteDiagnosticSnapshot, RouteStatus,
    RouteTransport, SenderQualityDiagnostics, SenderQualityEvidenceMessage, SenderQualityReason,
    SenderQualityState, ServerMessage,
};
use piik_room::{ConnectParticipantInput, CreatedRoom};
use serde_json::Value;
use url::Url;

use super::fake_media::FakeMedia;
use crate::router::pump::RouteReady;
use crate::router::{ActiveViewerMediaEdge, HybridState, RouteParticipant, SfuFallback};
use crate::runtime::{AfterFunc, TimerStop, lock};
use crate::server::{
    Core, DetachedHooks, Options, RouteDebugSink, build_core, finish_router_close, locked_with_io,
};
use piik_sfu::runtime::{Admission, AdmissionOptions, Runtime, SubscriptionFence, Usage};

pub(crate) const TEST_SHARE_GENERATION: &str = "share_generation_12345678";

#[derive(Default, Clone, Copy)]
pub(crate) struct RouterOptions {
    pub capacity: i64,
    pub with_sfu: bool,
    pub prepare_timeout_ms: i64,
    pub sfu_ingress_capacity: usize,
    pub fake_timers: bool,
    /// Records route debug events.
    pub debug: bool,
}

struct ManualTimer {
    id: u64,
    due_ms: i64,
    callback: Box<dyn FnOnce() + Send>,
}

#[derive(Default)]
struct ClockState {
    now_ms: i64,
    next_id: u64,
    timers: Vec<ManualTimer>,
}

/// `vi.useFakeTimers` over a sorted timer queue.
#[derive(Default)]
pub(crate) struct ManualClock {
    state: Mutex<ClockState>,
}

impl ManualClock {
    pub(crate) fn now(&self) -> i64 {
        lock(&self.state).now_ms
    }

    fn after_func(self: &Arc<Self>) -> AfterFunc {
        let clock = self.clone();
        Arc::new(move |duration: Duration, callback| -> TimerStop {
            let mut state = lock(&clock.state);
            let id = state.next_id;
            state.next_id += 1;
            let due_ms = state.now_ms + i64::try_from(duration.as_millis()).unwrap_or(i64::MAX);
            state.timers.push(ManualTimer {
                id,
                due_ms,
                callback,
            });
            let clock = clock.clone();
            Box::new(move || {
                let mut state = lock(&clock.state);
                let before = state.timers.len();
                state.timers.retain(|timer| timer.id != id);
                state.timers.len() != before
            })
        })
    }

    /// The next due timer at or before `target`, in due then creation order.
    fn take_due(&self, target: i64) -> Option<Box<dyn FnOnce() + Send>> {
        let mut state = lock(&self.state);
        let index = state
            .timers
            .iter()
            .enumerate()
            .filter(|(_, timer)| timer.due_ms <= target)
            .min_by_key(|(_, timer)| (timer.due_ms, timer.id))
            .map(|(index, _)| index)?;
        let timer = state.timers.remove(index);
        state.now_ms = state.now_ms.max(timer.due_ms);
        Some(timer.callback)
    }
}

pub(crate) type DebugRecord = (String, String, Vec<(String, Value)>);

/// Records the sanitized route events.
#[derive(Default)]
pub(crate) struct DebugRecorder {
    pub records: Mutex<Vec<DebugRecord>>,
}

impl DebugRecorder {
    /// `expect(debug).toHaveBeenCalledWith(room, event, objectContaining(expected))`.
    pub(crate) fn called_with(
        &self,
        room_id: &str,
        event: &str,
        expected: &[(&str, Value)],
    ) -> bool {
        lock(&self.records).iter().any(|(room, name, details)| {
            room == room_id
                && name == event
                && expected.iter().all(|(key, want)| {
                    details
                        .iter()
                        .any(|(name, value)| name == key && value == want)
                })
        })
    }
}

pub(crate) struct RouterHarness {
    pub core: Arc<Mutex<Core>>,
    pub media: Option<Arc<FakeMedia>>,
    pub clock: Option<Arc<ManualClock>>,
    pub debug: Arc<DebugRecorder>,
    closed: bool,
}

/// A `route-update` prepare message.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Prepare {
    pub revision: i64,
    pub assignment: ParticipantRouteAssignment,
    pub candidate: PreparedRouteCandidate,
}

impl Prepare {
    pub(crate) fn transport(&self) -> &'static str {
        transport_name(self.candidate.transport)
    }
}

pub(crate) fn transport_name(transport: RouteTransport) -> &'static str {
    match transport {
        RouteTransport::Direct => "direct",
        RouteTransport::Sfu => "sfu",
    }
}

pub(crate) fn as_prepare(message: &ServerMessage) -> Option<Prepare> {
    match message {
        ServerMessage::RouteUpdatePrepare {
            revision,
            assignment,
            candidate,
        } => Some(Prepare {
            revision: *revision,
            assignment: assignment.clone(),
            candidate: candidate.clone(),
        }),
        _ => None,
    }
}

pub(crate) fn last_prepare(messages: &[ServerMessage], child_peer_id: &str) -> Option<Prepare> {
    messages
        .iter()
        .rev()
        .filter_map(as_prepare)
        .find(|prepare| {
            child_peer_id.is_empty() || prepare.candidate.child_peer_id == child_peer_id
        })
}

pub(crate) fn sfu_config_count(messages: &[ServerMessage]) -> usize {
    messages
        .iter()
        .filter(|message| matches!(message, ServerMessage::SfuConfig { .. }))
        .count()
}

pub(crate) fn peer(peer_id: &str) -> MediaRouteUpstream {
    MediaRouteUpstream::peer(peer_id)
}

pub(crate) fn has_viewer(fences: &[SubscriptionFence], viewer_peer_id: &str) -> bool {
    fences
        .iter()
        .any(|fence| fence.viewer_peer_id == viewer_peer_id)
}

/// `senderEvidenceMessage`.
pub(crate) fn sender_evidence(
    child_peer_id: &str,
    connection_id: &str,
    rtp_stats_id: &str,
    track_identifier: &str,
    sample_timestamp_ms: f64,
    route_revision: i64,
    state: &str,
) -> SenderQualityEvidenceMessage {
    let (state, reason) = match state {
        "healthy" => (SenderQualityState::Healthy, Some(SenderQualityReason::None)),
        "degraded" => (
            SenderQualityState::Degraded,
            Some(SenderQualityReason::Bandwidth),
        ),
        _ => (SenderQualityState::Unknown, None),
    };
    SenderQualityEvidenceMessage {
        child_peer_id: child_peer_id.to_owned(),
        connection_id: connection_id.to_owned(),
        rtp_stats_id: Some(rtp_stats_id.to_owned()),
        track_identifier: Some(track_identifier.to_owned()),
        sample_timestamp_ms: Some(sample_timestamp_ms),
        route_revision,
        state,
        diagnostics: SenderQualityDiagnostics {
            nat_traversal_path: NatTraversalPath::Unknown,
            reason,
            frames_per_second: None,
            bitrate_kbps: None,
            capture_frames_per_second: Nullable::Absent,
            media_source_frames_per_second: Nullable::Absent,
            width: Nullable::Absent,
            height: Nullable::Absent,
            video_encoding_count: Nullable::Absent,
            active_video_encoding_count: Nullable::Absent,
            available_outgoing_kbps: Nullable::Absent,
            rtt_ms: Nullable::Absent,
            packet_loss_percent: Nullable::Absent,
        },
    }
}

/// A memory store with the harness limits.
pub(crate) fn memory_store(max_rooms: usize, max_viewers_per_room: usize) -> piik_room::Store {
    piik_room::Store::new(piik_room::Options {
        max_rooms,
        max_viewers_per_room,
        ..Default::default()
    })
    .expect("store")
}

pub(crate) fn base_url() -> Url {
    Url::parse("https://piik.test/").expect("url")
}

/// Options with every optional seam at its default.
pub(crate) fn base_options(store: piik_room::Store, capacity: i64) -> Options {
    Options {
        store,
        endpoint_media_copy_capacity: capacity,
        sfu_fallback: None,
        sfu_only: false,
        ice: IceConfig::default(),
        nat_prediction_enabled: true,
        allowed_origins: HashSet::new(),
        site_access_at_upgrade: None,
        public_base_url: base_url(),
        now: None,
        authentication_timeout_ms: 0,
        viewer_disconnect_grace_ms: 0,
        heartbeat_interval_ms: 0,
        room_empty_timeout_ms: 0,
        max_connections: 0,
        max_unauthenticated_connections: 0,
        after_func: None,
        route_debug: None,
    }
}

pub(crate) fn recording_sink(recorder: &Arc<DebugRecorder>) -> RouteDebugSink {
    let recorder = recorder.clone();
    Arc::new(
        move |room_id: &str, event: &str, details: &[(&'static str, Value)]| {
            let details = details
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect();
            lock(&recorder.records).push((room_id.to_owned(), event.to_owned(), details));
        },
    )
}

impl RouterHarness {
    pub(crate) fn new(options: RouterOptions) -> Self {
        let media = options.with_sfu.then(FakeMedia::new);
        Self::with_media(
            options,
            media.clone().map(|media| media as Arc<dyn Runtime>),
            media,
        )
    }

    /// A harness whose SFU runtime is `runtime` (recorded through `media`).
    pub(crate) fn with_media(
        options: RouterOptions,
        runtime: Option<Arc<dyn Runtime>>,
        media: Option<Arc<FakeMedia>>,
    ) -> Self {
        let clock = options
            .fake_timers
            .then(|| Arc::new(ManualClock::default()));
        let debug = Arc::new(DebugRecorder::default());
        let mut core_options = base_options(memory_store(4, 20), options.capacity);
        core_options.sfu_fallback = runtime.map(|runtime| SfuFallback {
            media: runtime,
            admission: Admission::new(AdmissionOptions {
                ingress_capacity: if options.sfu_ingress_capacity == 0 {
                    2
                } else {
                    options.sfu_ingress_capacity
                },
                egress_capacity: 20,
            }),
            prepare_timeout_ms: options.prepare_timeout_ms,
        });
        if let Some(clock) = &clock {
            let now = clock.clone();
            core_options.now = Some(Arc::new(move || now.now()));
            core_options.after_func = Some(clock.after_func());
        }
        if options.debug {
            core_options.route_debug = Some(recording_sink(&debug));
        }
        let core = build_core(
            core_options,
            2_048,
            256,
            Some(DetachedHooks {
                share_generation: TEST_SHARE_GENERATION.to_owned(),
                ..Default::default()
            }),
        );
        Self {
            core,
            media,
            clock,
            debug,
            closed: false,
        }
    }

    pub(crate) fn media(&self) -> &FakeMedia {
        self.media.as_deref().expect("SFU media")
    }

    pub(crate) fn clock(&self) -> &ManualClock {
        self.clock.as_deref().expect("fake timers")
    }

    /// Runs `f` as one synchronous segment.
    pub(crate) fn locked<R>(&self, f: impl FnOnce(&mut Core) -> R) -> R {
        locked_with_io(&self.core, f)
    }

    /// Waits for the router's pending continuations to finish.
    pub(crate) fn settle(&self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.locked(|core| core.router.inflight == 0) {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the router to settle"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// `await router.close()`: the locked closing prefix, then drain waits.
    pub(crate) fn close(&mut self) -> Result<(), String> {
        self.closed = true;
        let drains = self.locked(Core::begin_router_close);
        let errors =
            finish_router_close(&self.core, drains, Instant::now() + Duration::from_secs(5));
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "SFU drain failed during shutdown: {}",
                errors.join("\n")
            ))
        }
    }

    pub(crate) fn advance(&self, ms: i64) {
        self.settle();
        let target = self.clock().now() + ms;
        while let Some(callback) = self.clock().take_due(target) {
            callback();
            self.settle();
        }
        lock(&self.clock().state).now_ms = target;
        self.settle();
    }

    /// `setSystemTime`: shifts every pending timer by the jump.
    pub(crate) fn set_time(&self, ms: i64) {
        let mut state = lock(&self.clock().state);
        let difference = ms - state.now_ms;
        state.now_ms = ms;
        for timer in &mut state.timers {
            timer.due_ms += difference;
        }
    }

    pub(crate) fn create_room(&self) -> CreatedRoom {
        self.locked(|core| {
            core.store.begin_create_room().expect("begin create room");
            core.store
                .create_room(CodeEntryPolicy::Open, Ok(None), None)
                .expect("create room")
        })
    }

    pub(crate) fn connect_host(
        &self,
        created: &CreatedRoom,
        policy: Option<piik_protocol::RoutePolicy>,
        client_id: &str,
        session_id: &str,
    ) -> RouteParticipant {
        let client_id = if client_id.is_empty() {
            "host_client_12345678"
        } else {
            client_id
        };
        let session_id = if session_id.is_empty() {
            "host_session_12345678"
        } else {
            session_id
        };
        let connected = self.locked(|core| {
            core.store.connect_participant(&ConnectParticipantInput {
                room_id: created.room_id.clone(),
                role: Role::Host,
                token: created.host_token.clone(),
                viewer_grant: String::new(),
                client_id: client_id.to_owned(),
                session_id: session_id.to_owned(),
            })
        });
        let connected = connected.expect("connect host");
        let mut participant =
            RouteParticipant::new(&created.room_id, Role::Host, &connected.peer_id, session_id);
        participant.route_policy = policy;
        participant
    }

    pub(crate) fn connect_viewer(&self, created: &CreatedRoom, suffix: &str) -> RouteParticipant {
        let session_id = format!("viewer_session_{suffix}_12345678");
        let connected = self.locked(|core| {
            core.store.connect_participant(&ConnectParticipantInput {
                room_id: created.room_id.clone(),
                role: Role::Viewer,
                token: String::new(),
                viewer_grant: created.viewer_grant.clone(),
                client_id: format!("viewer_client_{suffix}_12345678"),
                session_id: session_id.clone(),
            })
        });
        let connected =
            connected.unwrap_or_else(|error| panic!("connect viewer {suffix}: {error}"));
        RouteParticipant::new(
            &created.room_id,
            Role::Viewer,
            &connected.peer_id,
            &session_id,
        )
    }

    pub(crate) fn complete(&self, participant: &RouteParticipant) -> HybridState {
        self.locked(|core| do_complete(core, participant))
    }

    pub(crate) fn route_ready(&self, participant: &RouteParticipant, revision: i64) {
        self.locked(|core| do_ready(core, participant, revision));
    }

    pub(crate) fn route_failed(
        &self,
        participant: &RouteParticipant,
        revision: i64,
        phase: &str,
        connection_id: &str,
    ) {
        self.locked(|core| do_failed(core, participant, revision, phase, Some(connection_id)));
    }

    pub(crate) fn active_edge(
        &self,
        room_id: &str,
        peer_id: &str,
    ) -> Option<ActiveViewerMediaEdge> {
        self.locked(|core| core.resolve_active_viewer_media_edge(room_id, peer_id))
    }

    pub(crate) fn must_active_edge(&self, room_id: &str, peer_id: &str) -> ActiveViewerMediaEdge {
        self.active_edge(room_id, peer_id)
            .expect("expected an active viewer media edge")
    }

    pub(crate) fn messages(&self, session_id: &str) -> Vec<ServerMessage> {
        self.locked(|core| sent(core, session_id))
    }

    pub(crate) fn all_messages(&self) -> Vec<ServerMessage> {
        self.locked(|core| {
            core.detached
                .as_ref()
                .map(|hooks| hooks.sent.values().flatten().cloned().collect())
                .unwrap_or_default()
        })
    }

    pub(crate) fn routes_changed(&self) -> Vec<String> {
        self.locked(|core| {
            core.detached
                .as_ref()
                .map(|hooks| hooks.routes_changed.clone())
                .unwrap_or_default()
        })
    }

    pub(crate) fn prepared_for(&self, session_id: &str) -> Option<Prepare> {
        last_prepare(&self.messages(session_id), "")
    }

    pub(crate) fn own_prepare(&self, participant: &RouteParticipant) -> Option<Prepare> {
        last_prepare(
            &self.messages(&participant.session_id),
            &participant.peer_id,
        )
    }

    /// The last active update above `revision`.
    pub(crate) fn active_after(
        &self,
        session_id: &str,
        revision: i64,
    ) -> Option<(i64, ParticipantRouteAssignment)> {
        self.messages(session_id)
            .iter()
            .rev()
            .find_map(|message| match message {
                ServerMessage::RouteUpdateActive {
                    revision: active,
                    assignment,
                } if *active > revision => Some((*active, assignment.clone())),
                _ => None,
            })
    }

    pub(crate) fn last_route_status(&self, session_id: &str) -> Option<RouteStatus> {
        self.messages(session_id)
            .iter()
            .rev()
            .find_map(|message| match message {
                ServerMessage::RouteStatus { status, .. } => Some(*status),
                _ => None,
            })
    }

    pub(crate) fn count_route_status(&self, session_id: &str, status: RouteStatus) -> usize {
        self.messages(session_id)
            .iter()
            .filter(|message| matches!(message, ServerMessage::RouteStatus { status: s, .. } if *s == status))
            .count()
    }

    pub(crate) fn has_failed_status(&self, session_id: &str) -> bool {
        self.count_route_status(session_id, RouteStatus::FailedRouteExhausted) > 0
    }

    /// `vi.waitFor`: fake timers tick 50 ms before each of 20 checks.
    pub(crate) fn wait_for(&self, description: &str, mut condition: impl FnMut() -> bool) {
        if self.clock.is_some() {
            for _ in 0..20 {
                self.advance(50);
                if condition() {
                    return;
                }
            }
            panic!("timed out waiting for {description}");
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub(crate) fn wait_prepared_matching(
        &self,
        session_id: &str,
        description: &str,
        matches: impl Fn(&Prepare) -> bool,
    ) -> Prepare {
        let mut found = None;
        self.wait_for(&format!("{description} for {session_id}"), || {
            found = self.prepared_for(session_id).filter(&matches);
            found.is_some()
        });
        found.expect("prepare")
    }

    pub(crate) fn wait_prepared(&self, session_id: &str) -> Prepare {
        self.wait_prepared_matching(session_id, "a prepare", |_| true)
    }

    pub(crate) fn wait_prepared_transport(&self, session_id: &str, transport: &str) -> Prepare {
        self.wait_prepared_matching(session_id, &format!("{transport} prepare"), |prepare| {
            prepare.transport() == transport
        })
    }

    pub(crate) fn wait_prepared_upstream(
        &self,
        session_id: &str,
        upstream: &MediaRouteUpstream,
    ) -> Prepare {
        self.wait_prepared_matching(session_id, "a prepare with upstream", |prepare| {
            prepare.assignment.upstream == *upstream
        })
    }

    pub(crate) fn wait_prepared_above(&self, session_id: &str, revision: i64) -> Prepare {
        self.wait_prepared_matching(session_id, "a newer prepare", |prepare| {
            prepare.revision > revision
        })
    }

    pub(crate) fn wait_failed(&self, session_id: &str) {
        self.wait_for(&format!("route-exhausted for {session_id}"), || {
            self.last_route_status(session_id) == Some(RouteStatus::FailedRouteExhausted)
        });
    }

    pub(crate) fn wait_usage(&self, want: Usage) {
        self.wait_for("SFU usage to reach the expected value", || {
            self.usage() == want
        });
    }

    pub(crate) fn diagnostic(&self, room_id: &str) -> RouteDiagnosticSnapshot {
        self.locked(|core| core.route_diagnostic_snapshot(room_id))
    }

    pub(crate) fn usage(&self) -> Usage {
        self.locked(|core| admission(core).usage())
    }

    /// A Host, a first Viewer moved to the SFU and a second Viewer joining it.
    pub(crate) fn establish_sfu_room(
        &self,
        created: &CreatedRoom,
    ) -> (RouteParticipant, RouteParticipant, RouteParticipant) {
        let host = self.connect_host(created, None, "", "");
        let first = self.connect_viewer(created, &format!("sfu-root-{}", created.room_id));
        self.locked(|core| {
            do_complete(core, &host);
            do_complete(core, &first);
        });
        let first_direct = self.wait_prepared(&first.session_id);
        let second = self.connect_viewer(created, &format!("sfu-leaf-{}", created.room_id));
        self.locked(|core| {
            do_ready(core, &first, first_direct.revision);
            do_complete(core, &second);
        });
        let first_sfu = self.wait_prepared_transport(&first.session_id, "sfu");
        self.route_ready(&first, first_sfu.revision);
        let second_sfu = self.wait_prepared_transport(&second.session_id, "sfu");
        self.route_ready(&second, second_sfu.revision);
        (host, first, second)
    }
}

impl Drop for RouterHarness {
    fn drop(&mut self) {
        if !self.closed && !std::thread::panicking() {
            let _ = self.close();
        }
    }
}

pub(crate) fn admission(core: &mut Core) -> &mut Admission {
    &mut core.router.sfu.as_mut().expect("SFU fallback").admission
}

pub(crate) fn sent(core: &Core, session_id: &str) -> Vec<ServerMessage> {
    core.detached
        .as_ref()
        .and_then(|hooks| hooks.sent.get(session_id).cloned())
        .unwrap_or_default()
}

pub(crate) fn clear_sent(core: &mut Core, session_id: &str) {
    if let Some(hooks) = core.detached.as_mut() {
        hooks.sent.remove(session_id);
    }
}

pub(crate) fn do_complete(core: &mut Core, participant: &RouteParticipant) -> HybridState {
    let state = core.connect_participant(participant);
    core.router_complete_authentication(participant);
    state
}

pub(crate) fn do_ready(core: &mut Core, participant: &RouteParticipant, revision: i64) {
    core.handle_route_ready(
        participant,
        RouteReady {
            revision,
            prepare: true,
            quality_approved: false,
        },
    );
}

pub(crate) fn do_ready_approved(core: &mut Core, participant: &RouteParticipant, revision: i64) {
    core.handle_route_ready(
        participant,
        RouteReady {
            revision,
            prepare: true,
            quality_approved: true,
        },
    );
}

pub(crate) fn do_failed(
    core: &mut Core,
    participant: &RouteParticipant,
    revision: i64,
    phase: &str,
    connection_id: Option<&str>,
) {
    core.handle_route_failed(participant, revision, phase == "prepare", connection_id);
}

pub(crate) fn do_relay(core: &mut Core, participant: &RouteParticipant, edges: i64) {
    core.set_viewer_relay_capacity(participant, edges);
}

/// `store.disconnectParticipant` plus `router.disconnectParticipant`.
pub(crate) fn do_disconnect(core: &mut Core, participant: &RouteParticipant) {
    core.store
        .disconnect_participant(
            &participant.room_id,
            &participant.peer_id,
            &participant.session_id,
        )
        .expect("disconnect participant");
    core.router_disconnect_participant(
        &participant.room_id,
        &participant.peer_id,
        &participant.session_id,
    );
}

pub(crate) fn do_edge(core: &Core, room_id: &str, peer_id: &str) -> Option<ActiveViewerMediaEdge> {
    core.resolve_active_viewer_media_edge(room_id, peer_id)
}

pub(crate) fn do_must_edge(core: &Core, room_id: &str, peer_id: &str) -> ActiveViewerMediaEdge {
    do_edge(core, room_id, peer_id).expect("expected an active viewer media edge")
}
