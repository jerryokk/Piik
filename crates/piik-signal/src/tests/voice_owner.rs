//! Voice owner tests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use piik_protocol::{
    CodeEntryPolicy, DEFAULT_ROUTE_POLICY, MediaRouteUpstream, RoomAccessUpdateRequest,
    ServerMessage,
};
use serde_json::json;

use super::fake_media::{Barrier, FakeMedia, HookedMedia};
use super::harness::*;
use super::voice::*;
use super::wire::*;
use crate::runtime::lock;
use crate::voice::VoiceEdge;
use piik_sfu::runtime::{
    DeferredWork, MediaError, VoiceConnectionPlan, VoiceEventSink, VoiceRuntime, VoiceSignal,
};

const SHORT: Duration = Duration::from_millis(30);

fn voice_edge(id: &str, logical: &str, child: &str, session: &str) -> VoiceEdge {
    VoiceEdge {
        id: id.to_owned(),
        logical_id: logical.to_owned(),
        parent: String::new(),
        child: child.to_owned(),
        transport: "sfu",
        parent_session: String::new(),
        child_session: session.to_owned(),
        pending: false,
        revision: 0,
        send: HashMap::new(),
        connected: HashMap::new(),
        sfu_plan: None,
        sfu_work: None,
    }
}

#[test]
fn voice_failure_replaces_only_companion_of_healthy_picture() {
    let (h, host, viewer, _, viewer_id) = joined_voice_owner_room();
    start_sharing(&host, "voice_picture_123456", DEFAULT_ROUTE_POLICY);
    host.next("sharing-started");
    let prepare = next_prepared_route(&viewer);
    route_ready(&viewer, prepare.revision());
    next_active_route_revision(&viewer, prepare.revision());
    let before = next_voice_state(&viewer, |state| single_connection(state, false));
    let old_id = before.connections[0].connection_id.clone();
    viewer.send_json(
        json!({"type": "room-voice-transport", "connectionId": old_id, "state": "failed"}),
    );
    let after = next_voice_state(&viewer, |state| {
        state.connections.len() == 1 && state.connections[0].connection_id != old_id
    });
    assert!(
        !after.connections[0].pending,
        "voice repair started another route operation"
    );
    let room = h.room.room_id.clone();
    h.locked(|core| {
        let snapshot = core.snapshot(&room).expect("snapshot");
        let edge = snapshot
            .upstream_by_viewer
            .get(&viewer_id.peer_id())
            .cloned();
        assert!(
            edge.is_some_and(|edge| edge.usable && edge.video_ready)
                && snapshot.operation.is_none()
                && snapshot.revision == prepare.revision(),
            "voice failure invalidated the current picture route"
        );
    });
    report_connected(&[&host, &viewer], &after.connections[0].connection_id);
    next_voice_state(&viewer, |state| {
        state.members.iter().any(|member| {
            member.peer_id == viewer_id.peer_id()
                && matches!(member.upstream, MediaRouteUpstream::Peer { .. })
        })
    });
}

#[test]
fn voice_owner_must_open_before_viewers_can_join() {
    let h = Harness::start(HarnessOptions::default());
    let (host, viewer) = (h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "voice_closed_host_123456",
        -1,
        "",
        Presence::default(),
    );
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "voice_closed_viewer_123456",
        0,
        "",
        Presence::default(),
    );
    voice_command(&viewer, json!({"action": "join"}));
    let message = decode_server_message(&viewer.next("room-voice-error").raw);
    assert!(
        matches!(
            message,
            ServerMessage::RoomVoiceError {
                code: piik_protocol::RoomVoiceErrorCode::NotEnabled
            }
        ),
        "viewer could open voice without the Host"
    );
    voice_command(&host, json!({"action": "join"}));
    let state = next_voice_state(&viewer, |state| state.enabled);
    assert!(
        state.publications.is_empty() && state.connections.is_empty(),
        "opening room voice started a microphone or joined a Viewer"
    );
    voice_command(&viewer, json!({"action": "join"}));
    let pending = next_voice_state(&viewer, |state| single_connection(state, true));
    report_connected(&[&host, &viewer], &pending.connections[0].connection_id);
    next_voice_state(&viewer, |state| single_connection(state, false));
    voice_command(&viewer, json!({"action": "publish"}));
    next_voice_state(&host, |state| state.publications.len() == 1);
    voice_command(&host, json!({"action": "leave"}));
    let state = next_voice_state(&viewer, |state| !state.enabled);
    assert!(
        state.publications.is_empty() && state.connections.is_empty(),
        "closing room voice retained media"
    );
    assert!(
        state
            .members
            .iter()
            .all(|member| !member.joined && member.publication_id.is_none()),
        "closing room voice retained a joined participant"
    );
    voice_command(&host, json!({"action": "join"}));
    let state = next_voice_state(&viewer, |state| state.enabled);
    assert!(
        state.publications.is_empty() && state.connections.is_empty(),
        "reopening room voice restored former media"
    );
}

fn decode_server_message(raw: &str) -> ServerMessage {
    piik_protocol::decode_server_message(raw.as_bytes()).expect("server message")
}

#[test]
fn voice_owners_rebind_room_sessions_and_release_last_disconnect() {
    let (h, host, viewer, host_id, viewer_id) = joined_voice_owner_room();
    let room = h.room.room_id.clone();
    h.close_client(&viewer);
    let viewer = h.open_client();
    let rebound_viewer = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "owner_viewer_123456",
        1,
        "",
        Presence::default(),
    );
    assert_eq!(
        rebound_viewer.peer_id(),
        viewer_id.peer_id(),
        "viewer identity did not survive grace"
    );
    next_voice_state(&viewer, |state| state.connections.len() == 1);
    h.locked(|core| {
        let edge = core
            .snapshot(&room)
            .and_then(|s| s.upstream_by_viewer.get(&viewer_id.peer_id()).cloned());
        let peer = core
            .store
            .get_connected_viewer(&room, &viewer_id.peer_id())
            .expect("viewer");
        assert!(
            edge.is_some_and(|edge| edge.child_session_id == peer.session_id)
                && core.voice_allowed(&room, &viewer_id.peer_id()),
            "subscription did not rebind approved viewer route"
        );
    });
    h.close_client(&host);
    let host = h.open_client();
    let rebound_host = authenticate(
        &host,
        &h.room,
        "host",
        "owner_host_123456",
        -1,
        "",
        Presence::default(),
    );
    assert_eq!(
        rebound_host.peer_id(),
        host_id.peer_id(),
        "host identity changed on room reconnect"
    );
    next_voice_state(&host, |state| state.connections.len() == 1);
    h.locked(|core| {
        let edge = core
            .snapshot(&room)
            .and_then(|s| s.upstream_by_viewer.get(&viewer_id.peer_id()).cloned());
        let peer = core.store.get_connected_host(&room).expect("host");
        assert!(
            edge.is_some_and(|edge| edge.parent_session_id == peer.session_id),
            "subscription did not rebind room-only host route"
        );
    });
    h.close_client(&viewer);
    h.close_client(&host);
    h.locked(|core| {
        assert!(
            core.room_rt(&room)
                .is_none_or(|runtime| runtime.controller.is_none()),
            "last disconnected voice participant retained idle room resources"
        );
    });
}

#[test]
fn voice_owner_can_close_while_viewer_is_disconnected() {
    let (h, host, viewer, _, viewer_id) = joined_voice_owner_room();
    h.close_client(&viewer);
    next_voice_state(&host, |state| {
        state
            .members
            .iter()
            .any(|member| member.peer_id == viewer_id.peer_id() && !member.connected)
    });
    voice_command(&host, json!({"action": "leave"}));
    next_voice_state(&host, |state| {
        state
            .members
            .iter()
            .any(|member| member.peer_id == viewer_id.peer_id() && !state.enabled && !member.joined)
    });
    let viewer = h.open_client();
    authenticate(
        &viewer,
        &h.room,
        "viewer",
        "owner_viewer_123456",
        1,
        "",
        Presence::default(),
    );
    let state = next_voice_state(&viewer, any_state);
    for member in &state.members {
        assert!(
            member.peer_id != viewer_id.peer_id()
                || !(state.enabled || member.joined || member.publication_id.is_some()),
            "reconnection restored closed room voice"
        );
    }
}

#[test]
fn voice_owners_opt_in_preserves_paused_publication() {
    let h = Harness::start(HarnessOptions::default());
    let host = h.open_client();
    authenticate(
        &host,
        &h.room,
        "host",
        "owner_paused_host_123456",
        -1,
        "owner_paused_share_123456",
        Presence {
            sharing_paused: Some(true),
            ..Default::default()
        },
    );
    voice_command(&host, json!({"action": "join"}));
    next_voice_state(&host, |state| {
        state.enabled && state.members.iter().any(|member| member.joined)
    });
    let room = h.room.room_id.clone();
    h.locked(|core| {
        let snapshot = core.snapshot(&room).expect("snapshot");
        assert!(
            snapshot.room_media
                && snapshot.video_paused
                && snapshot.video_generation == "owner_paused_share_123456",
            "voice opt-in resumed a paused screen publication"
        );
    });
}

#[test]
fn voice_owners_grant_revocation_removes_permanent_member() {
    let (h, host, viewer, _, viewer_id) = joined_voice_owner_room();
    voice_command(&viewer, json!({"action": "publish"}));
    next_voice_state(&host, |state| state.publications.len() == 1);
    h.update_room_access(RoomAccessUpdateRequest::RevokeViewerGrant);
    viewer.next("viewer-grant-revoked");
    expect_close_code(&viewer, 4004);
    let state = next_voice_state(&host, |state| {
        state.members.len() == 1 && state.publications.is_empty()
    });
    // The revoked viewer left the roster, so only the Host remains.
    assert!(
        state.connections.is_empty(),
        "revoked member retained a voice connection"
    );
    let room = h.room.room_id.clone();
    h.locked(|core| {
        assert!(
            !core.voices[&room]
                .members
                .contains_key(&viewer_id.peer_id()),
            "permanently revoked identity retained voice permission"
        );
    });
}

#[test]
fn voice_owners_stop_drops_video_only_candidate_demand() {
    let (h, host, viewer, _, viewer_id) = joined_voice_owner_room();
    let legacy = h.open_client();
    let legacy_id = authenticate(
        &legacy,
        &h.room,
        "viewer",
        "owner_legacy_123456",
        0,
        "",
        Presence::default(),
    );
    start_sharing(&host, "owner_share_123456", DEFAULT_ROUTE_POLICY);
    host.next("sharing-started");
    let prepare = next_prepared_route(&viewer);
    route_ready(&viewer, prepare.revision());
    next_active_route_revision(&viewer, prepare.revision());
    next_prepared_route(&legacy);
    host.send_json(json!({"type": "stop-sharing", "shareGeneration": "owner_share_123456"}));
    viewer.next("sharing-stopped");
    let room = h.room.room_id.clone();
    assert!(
        wait_for(DEFAULT_WAIT, || h.locked(|core| {
            let Some(runtime) = core.room_rt(&room) else {
                return false;
            };
            let snapshot = core.snapshot(&room).expect("snapshot");
            let edge = snapshot.upstream_by_viewer.get(&viewer_id.peer_id());
            !runtime.pumping
                && snapshot.operation.is_none()
                && edge.is_some_and(|edge| edge.usable && !edge.video_ready)
                && snapshot
                    .upstream_by_viewer
                    .get(&legacy_id.peer_id())
                    .is_none()
        })),
        "video-only viewer retained voice candidate demand after share stop"
    );
}

// ---------------------------------------------------------------------------
// held voice effects
// ---------------------------------------------------------------------------

struct HeldVoiceMedia {
    negotiations: Mutex<mpsc::Sender<VoiceConnectionPlan>>,
    negotiate_release: Arc<Barrier>,
    latest_release: Arc<Barrier>,
    close_started: Arc<Barrier>,
    close_release: Arc<Barrier>,
}

impl VoiceRuntime for HeldVoiceMedia {
    fn configure_voice(
        &self,
        plan: VoiceConnectionPlan,
        _: VoiceEventSink,
    ) -> Result<Option<DeferredWork>, MediaError> {
        let negotiations = lock(&self.negotiations).clone();
        let (negotiate_release, latest_release) =
            (self.negotiate_release.clone(), self.latest_release.clone());
        Ok(Some(Box::new(move || {
            let empty = plan.publish.is_empty();
            let _ = negotiations.send(plan);
            if empty {
                negotiate_release.wait();
                return Err(MediaError::failed("held negotiation failed"));
            }
            latest_release.wait();
            Ok(())
        })))
    }
    fn signal_voice(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: VoiceSignal,
    ) -> Result<(), MediaError> {
        Ok(())
    }
    fn close_voice(&self, _: &str, _: &str, _: &str, _: &str) -> Option<DeferredWork> {
        let (started, release) = (self.close_started.clone(), self.close_release.clone());
        Some(Box::new(move || {
            started.open();
            release.wait();
            Ok(())
        }))
    }
}

fn hooked_voice(voice: Arc<dyn VoiceRuntime>) -> Arc<HookedMedia> {
    let mut hooked = HookedMedia::new(FakeMedia::new());
    hooked.voice = Some(voice);
    Arc::new(hooked)
}

/// `CreateRoom` on another thread; `true` once it returned without error.
fn create_room_promptly(h: &Harness) -> bool {
    let (server, runtime) = (h.server.clone(), h.runtime.handle().clone());
    let (done, created) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(
            runtime
                .block_on(server.create_room(CodeEntryPolicy::Open, None, None))
                .is_ok(),
        );
    });
    created.recv_timeout(DEFAULT_WAIT).unwrap_or(false)
}

#[test]
fn voice_effects_preserve_synchronous_authority_and_shutdown_joins_work() {
    let (negotiations, planned) = mpsc::channel();
    let media = Arc::new(HeldVoiceMedia {
        negotiations: Mutex::new(negotiations),
        negotiate_release: Barrier::new(),
        latest_release: Barrier::new(),
        close_started: Barrier::new(),
        close_release: Barrier::new(),
    });
    let h = Harness::start(HarnessOptions {
        sfu: Some(sfu_with(hooked_voice(media.clone()), 2, 20)),
        ..Default::default()
    });
    let room = h.room.room_id.clone();
    let configure = |publications: Vec<piik_protocol::VoicePublication>, send: Option<&str>| {
        let core = h.server.core.clone();
        let room = room.clone();
        let send = send.map(str::to_owned);
        let (done, finished) = mpsc::channel();
        std::thread::spawn(move || {
            crate::server::locked_with_io(&core, |core| {
                core.voice_room(&room);
                let mut voice = core.voices.remove(&room).expect("voice");
                let edge = voice
                    .connections
                    .entry("logical".to_owned())
                    .or_insert_with(|| voice_edge("physical", "logical", "peer", "session"));
                if let Some(id) = send {
                    edge.send.insert("peer".to_owned(), vec![id]);
                }
                core.configure_voice_junction(&room, &mut voice, "logical", &publications);
                core.voices.insert(room.clone(), voice);
            });
            let _ = done.send(());
        });
        assert!(
            finished.recv_timeout(DEFAULT_WAIT).is_ok(),
            "synchronous voice projection waited for SDP work"
        );
    };
    let work_identity = || {
        h.locked(|core| {
            core.voices[&room].connections["logical"]
                .sfu_work
                .as_ref()
                .map(|work| Arc::as_ptr(&work.done) as usize)
        })
    };
    configure(Vec::new(), None);
    planned
        .recv_timeout(DEFAULT_WAIT)
        .expect("negotiation did not start");
    assert!(
        create_room_promptly(&h),
        "voice negotiation blocked another room"
    );
    let operation = work_identity();
    assert!(operation.is_some(), "negotiation worker missing");
    for id in ["superseded", "latest"] {
        configure(
            vec![piik_protocol::VoicePublication {
                id: id.to_owned(),
                peer_id: "peer".to_owned(),
            }],
            Some(id),
        );
        assert_eq!(
            work_identity(),
            operation,
            "plan update started an overlapping negotiation worker"
        );
    }
    let replacement_plan = h.locked(|core| {
        core.voices[&room].connections["logical"]
            .sfu_plan
            .as_ref()
            .map(|plan| plan.0)
    });
    media.negotiate_release.open();
    let plan = planned
        .recv_timeout(DEFAULT_WAIT)
        .expect("latest negotiation did not start");
    assert!(
        plan.publish.len() == 1 && plan.publish[0].id == "latest",
        "queued work replayed a superseded publication: {:?}",
        plan.publish
    );
    assert_eq!(
        h.locked(|core| core.voices[&room].connections["logical"]
            .sfu_plan
            .as_ref()
            .map(|plan| plan.0)),
        replacement_plan,
        "stale negotiation failure overwrote replacement plan"
    );
    h.locked(|core| core.close_room_voice(&room));
    assert!(
        media.close_started.wait_timeout(DEFAULT_WAIT),
        "physical retirement did not start"
    );
    assert!(
        create_room_promptly(&h),
        "voice retirement blocked another room"
    );
    let (server, runtime) = (h.server.clone(), h.runtime.handle().clone());
    let (done, closed) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = done.send(runtime.block_on(server.close(DEFAULT_WAIT)));
    });
    assert!(
        closed.recv_timeout(SHORT).is_err(),
        "shutdown escaped held retirement"
    );
    media.close_release.open();
    assert!(
        closed.recv_timeout(SHORT).is_err(),
        "shutdown escaped in-flight negotiation"
    );
    media.latest_release.open();
    closed
        .recv_timeout(DEFAULT_WAIT)
        .expect("shutdown did not join retirement")
        .expect("shutdown");
}

struct CapacityHeldVoiceMedia {
    busy: Arc<Mutex<bool>>,
    plans: Mutex<mpsc::Sender<VoiceConnectionPlan>>,
    release: Arc<Barrier>,
}

impl VoiceRuntime for CapacityHeldVoiceMedia {
    fn configure_voice(
        &self,
        plan: VoiceConnectionPlan,
        _: VoiceEventSink,
    ) -> Result<Option<DeferredWork>, MediaError> {
        if *lock(&self.busy) {
            return Err(MediaError::failed("voice connection capacity is exhausted"));
        }
        let _ = lock(&self.plans).send(plan);
        Ok(None)
    }
    fn signal_voice(
        &self,
        _: &str,
        _: &str,
        _: &str,
        _: &str,
        _: VoiceSignal,
    ) -> Result<(), MediaError> {
        Ok(())
    }
    fn close_voice(&self, _: &str, _: &str, _: &str, _: &str) -> Option<DeferredWork> {
        let (busy, release) = (self.busy.clone(), self.release.clone());
        Some(Box::new(move || {
            release.wait();
            *lock(&busy) = false;
            Ok(())
        }))
    }
}

#[test]
fn voice_retirement_refreshes_current_plan_after_capacity_release() {
    let (plans, planned) = mpsc::channel();
    let media = Arc::new(CapacityHeldVoiceMedia {
        busy: Arc::new(Mutex::new(true)),
        plans: Mutex::new(plans),
        release: Barrier::new(),
    });
    let h = Harness::start(HarnessOptions {
        sfu_only: true,
        sfu: Some(sfu_with(hooked_voice(media.clone()), 2, 20)),
        ..Default::default()
    });
    let (host, viewer) = (h.open_client(), h.open_client());
    authenticate(
        &host,
        &h.room,
        "host",
        "capacity-host",
        -1,
        "",
        Presence::default(),
    );
    let viewer_id = authenticate(
        &viewer,
        &h.room,
        "viewer",
        "capacity-viewer",
        0,
        "",
        Presence::default(),
    );
    voice_command(&host, json!({"action": "join"}));
    next_voice_state(&host, |state| state.enabled);
    voice_command(&viewer, json!({"action": "join"}));
    next_voice_state(&viewer, |state| !state.connections.is_empty());
    // Keep the actual controller's current facets. Capacity is held by an older
    // retiring facet, and publication intent changes before its close completes.
    let room = h.room.room_id.clone();
    h.locked(|core| {
        for edge in core
            .voices
            .get_mut(&room)
            .expect("voice")
            .connections
            .values_mut()
        {
            edge.pending = false;
            let (parent, child) = (edge.parent.clone(), edge.child.clone());
            edge.connected.insert(parent, true);
            edge.connected.insert(child, true);
        }
        core.commit_voice_candidate(&room);
        core.sync_room_voice(&room);
        let mut retiring = voice_edge(
            "retiring-capacity",
            "retiring-capacity",
            "old-peer",
            "old-session",
        );
        core.close_voice_edge(&room, &mut retiring);
    });
    voice_command(&viewer, json!({"action": "publish"}));
    let state = next_voice_state(&viewer, |state| state.publications.len() == 1);
    let expected = state.publications[0].id.clone();
    media.release.open();
    let deadline = std::time::Instant::now() + DEFAULT_WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let plan = planned
            .recv_timeout(remaining)
            .expect("completed retirement did not resume current publication intent");
        if plan.peer_id == viewer_id.peer_id()
            && plan
                .publish
                .iter()
                .any(|publication| publication.id == expected)
        {
            return;
        }
    }
}
