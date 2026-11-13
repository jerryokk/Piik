//! The signaling owner: one authority lock over sessions, rooms, the router
//! and the route controllers.
//!
//! # Locking
//!
//! [`Core`] is the whole effect layer and lives behind one `std` mutex. Every
//! handler, timer callback and deferred job takes it and never awaits while
//! holding it. Password derivation and media transport work run unlocked, after
//! which current session, room and operation identities are revalidated.

pub(crate) mod close;
pub(crate) mod rooms;

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use piik_ordered::Map;
use piik_protocol::{CoPlayGrant, IceConfig, QualitySettings, RoutePolicy, ServerMessage};
use piik_room::Store;
use serde_json::Value;
use url::Url;

use crate::router::{Router, SfuFallback};
use crate::runtime::{AfterFunc, Clock, Locker, TimerStop, default_after_func, lock, wall_clock};
use crate::session::Session;
use crate::voice::RoomVoice;

/// Unsent data bound per connection; exceeding it terminates a stalled reader.
pub(crate) const MAX_BUFFERED_SIGNAL_BYTES: usize = 256 * 1024;
/// Admission ceilings that bound abuse, not capacity.
pub(crate) const DEFAULT_MAX_SIGNAL_CONNECTIONS: usize = 2_048;
pub(crate) const DEFAULT_MAX_UNAUTHENTICATED_CONNECTIONS: usize = 256;
/// Bounds liveness challenge frequency.
pub(crate) const MIN_SIGNALING_CHALLENGE_INTERVAL_MS: i64 = 1_000;
/// How long a peer has to acknowledge close 1012 before termination.
pub(crate) const SERVICE_RESTART_CLOSE_GRACE: Duration = Duration::from_millis(1_000);
pub(crate) const DEFAULT_AUTHENTICATION_TIMEOUT_MS: u64 = 5_000;
/// Covers the Browser's nominal first five reconnect backoffs.
pub(crate) const DEFAULT_VIEWER_DISCONNECT_GRACE_MS: u64 = 20_000;
pub(crate) const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 30_000;

/// Receives sanitized route debug events: `(roomId, event, details)`.
pub type RouteDebugSink = Arc<dyn Fn(&str, &str, &[(&'static str, Value)]) + Send + Sync>;

/// Decides site access from the upgrade request's headers.
pub type SiteAccessCheck = Arc<dyn Fn(&axum::http::HeaderMap) -> bool + Send + Sync>;

/// Room authority, runtime capabilities and signaling limits.
pub struct Options {
    pub store: Store,
    pub endpoint_media_copy_capacity: i64,
    /// `None` when embedded SFU media is disabled.
    pub sfu_fallback: Option<SfuFallback>,
    /// Restricts the controller to server media candidates.
    pub sfu_only: bool,
    /// The derived wire ICE configuration ([`piik_config::ice_config`]).
    pub ice: IceConfig,
    /// The server capability that clamps a Host's requested NAT prediction.
    pub nat_prediction_enabled: bool,
    pub allowed_origins: HashSet<String>,
    /// `None` treats every upgrade as site-access authenticated.
    pub site_access_at_upgrade: Option<SiteAccessCheck>,
    pub public_base_url: Url,
    /// Unix milliseconds; `None` uses the wall clock.
    pub now: Option<Clock>,
    /// Zero selects the defaults.
    pub authentication_timeout_ms: u64,
    pub viewer_disconnect_grace_ms: u64,
    pub heartbeat_interval_ms: u64,
    /// Zero retains room authority indefinitely; otherwise empty rooms expire
    /// and allocation pressure may reclaim the oldest empty room early.
    pub room_empty_timeout_ms: i64,
    pub max_connections: usize,
    pub max_unauthenticated_connections: usize,
    /// The timer factory; `None` uses Tokio timers.
    pub after_func: Option<AfterFunc>,
    /// Route debug events; `None` enables `tracing` output only when
    /// `PIIK_DEBUG` names the `route` section or debug logging is enabled.
    pub route_debug: Option<RouteDebugSink>,
}

/// A signaling setup failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Error(pub String);

/// Gates rejecting stale or repeated evidence for the current edge and
/// presentation epoch; `upstream_peer_id` is empty for SFU.
#[derive(Debug, Clone)]
pub(crate) struct ViewerQualityEvidenceGate {
    pub viewer_session_id: String,
    pub upstream_kind: &'static str,
    pub upstream_peer_id: String,
    pub connection_id: String,
    pub presentation_epoch: i64,
    pub sequence: i64,
}

/// Scopes the evidence rate window to a committed connection.
#[derive(Debug, Clone)]
pub(crate) struct ViewerQualityEvidenceAttempt {
    pub connection_id: String,
    pub at_ms: i64,
}

/// One room's Host publication intent. An empty generation means no intent.
#[derive(Debug, Clone, Default)]
pub(crate) struct RoomShare {
    /// Survives `stop_sharing`; only `close_room` drops the whole record.
    pub generation: String,
    pub quality_settings: Option<QualitySettings>,
    pub route_policy: Option<RoutePolicy>,
    /// The generation the Host paused, empty if none.
    pub paused_generation: String,
    pub audio_available: Option<bool>,
    pub co_play_grants: Vec<CoPlayGrant>,
}

pub(crate) struct SenderQualityRateWindow {
    pub started_at_ms: i64,
    pub count: usize,
}

/// A grace timer; callbacks compare `id` so replacement invalidates them.
pub(crate) struct GraceTimer {
    pub id: u64,
    pub stop: Option<TimerStop>,
}

/// Work queued under the lock to run after it is released, in order, before
/// the session reads its next frame.
pub(crate) type IoJob = Box<dyn FnOnce() + Send>;

/// A router harness without a signaling server: records sends and route
/// changes and supplies a fixed share generation.
#[derive(Default)]
pub(crate) struct DetachedHooks {
    pub sent: HashMap<String, Vec<ServerMessage>>,
    pub routes_changed: Vec<String>,
    pub share_generation: String,
}

/// Everything the authority lock guards.
pub(crate) struct Core {
    pub store: Store,
    pub endpoint_media_copy_capacity: i64,
    pub ice: IceConfig,
    pub nat_prediction_enabled: bool,
    pub allowed_origins: HashSet<String>,
    pub public_base_url: Url,
    pub now: Clock,
    pub after_func: AfterFunc,
    pub authentication_timeout_ms: u64,
    pub viewer_disconnect_grace_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub room_empty_timeout_ms: i64,
    pub max_connections: usize,
    pub max_unauthenticated_connections: usize,

    pub router: Router,

    /// Heartbeat walks sessions in insertion order; `sessions_by_id` indexes them.
    pub sessions: Map<u64, Session>,
    pub sessions_by_id: HashMap<String, u64>,
    pub next_session_key: u64,
    /// Upgrades between the capacity check and acceptance.
    pub pending_connections: usize,
    pub unauthenticated_connections: usize,

    pub viewer_grace_timers: HashMap<String, GraceTimer>,
    pub viewer_quality_evidence_gates: HashMap<String, ViewerQualityEvidenceGate>,
    pub viewer_quality_evidence_attempts: HashMap<String, ViewerQualityEvidenceAttempt>,
    pub sender_quality_rate_by_session: HashMap<String, SenderQualityRateWindow>,
    pub shares: HashMap<String, RoomShare>,
    pub voices: HashMap<String, RoomVoice>,
    pub deferred_viewer_presence_rooms: HashSet<String>,

    pub heartbeat_stop: Option<TimerStop>,
    pub closing: bool,

    pub locker: Locker,
    pub route_debug: Option<RouteDebugSink>,
    pub detached: Option<DetachedHooks>,
    pub io_jobs: Vec<IoJob>,
}

/// Route debugging is enabled by `PIIK_DEBUG=route` or debug logging.
pub(crate) fn route_debug_flag(value: &str) -> bool {
    value.split(',').any(|section| section.trim() == "route")
}

/// `piik_debug` is the `PIIK_DEBUG` value; an active debug-level subscriber
/// also enables route events.
pub(crate) fn route_debug_enabled(piik_debug: Option<&str>) -> bool {
    piik_debug.is_some_and(route_debug_flag) || tracing::enabled!(tracing::Level::DEBUG)
}

fn default_route_debug() -> Option<RouteDebugSink> {
    let enabled = route_debug_enabled(std::env::var("PIIK_DEBUG").ok().as_deref());
    enabled.then(|| -> RouteDebugSink {
        Arc::new(|room_id, event, details| {
            let details: serde_json::Map<String, Value> = details
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone()))
                .collect();
            // A structured, redacted object; diagnostics read its fields.
            let details = piik_diagnostics::Json(Value::Object(details));
            tracing::info!(event, room_id, details = ?details, "piik-route");
        })
    })
}

fn or_default(value: u64, fallback: u64) -> u64 {
    if value == 0 { fallback } else { value }
}

impl Core {
    pub(crate) fn debug_enabled(&self) -> bool {
        self.route_debug.is_some()
    }

    /// Emits one sanitized event; never pass raw peer, session or connection IDs.
    pub(crate) fn emit_debug(
        &self,
        room_id: &str,
        event: &str,
        details: Vec<(&'static str, Value)>,
    ) {
        if let Some(sink) = &self.route_debug {
            sink(room_id, event, &details);
        }
    }

    /// The controller's own sanitized events go to the same sink.
    pub(crate) fn controller_debug_sink(&self) -> Option<piik_route::DebugSink> {
        let sink = self.route_debug.clone()?;
        Some(Arc::new(
            move |_message: &str, fields: &[(&'static str, String)]| {
                let field = |key: &str| {
                    fields
                        .iter()
                        .find(|(name, _)| *name == key)
                        .map(|(_, value)| value.clone())
                        .unwrap_or_default()
                };
                let details: Vec<(&'static str, Value)> = fields
                    .iter()
                    .filter(|(key, _)| *key != "event" && *key != "roomId")
                    .map(|(key, value)| (*key, Value::String(value.clone())))
                    .collect();
                sink(&field("roomId"), &field("event"), &details);
            },
        ))
    }

    /// Router sends: recorded in detached mode, otherwise queued to the session.
    pub(crate) fn router_send(&mut self, session_id: &str, message: ServerMessage) {
        if let Some(detached) = self.detached.as_mut() {
            detached
                .sent
                .entry(session_id.to_owned())
                .or_default()
                .push(message);
            return;
        }
        self.send_to_session(session_id, &message);
    }

    pub(crate) fn hook_routes_changed(&mut self, room_id: &str) {
        if let Some(detached) = self.detached.as_mut() {
            detached.routes_changed.push(room_id.to_owned());
            return;
        }
        self.send_viewer_presence(room_id);
        self.sync_room_voice(room_id);
    }

    pub(crate) fn share_generation(&self, room_id: &str) -> String {
        if let Some(detached) = &self.detached {
            return detached.share_generation.clone();
        }
        self.shares
            .get(room_id)
            .map(|share| share.generation.clone())
            .unwrap_or_default()
    }

    pub(crate) fn video_generation(&self, room_id: &str) -> String {
        if self.detached.is_some() {
            return self.share_generation(room_id);
        }
        self.active_host_share_generation(room_id)
    }

    /// Queues work to run unlocked after the current handler returns.
    pub(crate) fn defer_io(&mut self, job: impl FnOnce() + Send + 'static) {
        self.io_jobs.push(Box::new(job));
    }

    pub(crate) fn share(&self, room_id: &str) -> RoomShare {
        self.shares.get(room_id).cloned().unwrap_or_default()
    }

    pub(crate) fn has_connection_capacity(&self) -> bool {
        !self.closing
            && self.sessions.len() + self.pending_connections < self.max_connections
            && self.unauthenticated_connections + self.pending_connections
                < self.max_unauthenticated_connections
    }

    /// Requires a present, parseable origin equal to its own serialized origin
    /// (so `http://x/path` is refused) and configured.
    pub(crate) fn is_allowed_origin(&self, origin: &str) -> bool {
        if origin.is_empty() {
            return false;
        }
        let Ok(parsed) = Url::parse(origin) else {
            return false;
        };
        if parsed.host_str().is_none_or(str::is_empty) {
            return false;
        }
        self.allowed_origins.contains(origin) && piik_config::origin(&parsed) == origin
    }
}

/// Room signaling and its serialized effects.
pub struct Server {
    pub(crate) core: Arc<Mutex<Core>>,
    pub(crate) site_access_at_upgrade: Option<SiteAccessCheck>,
}

/// Locks, runs `f`, then runs the I/O it queued with the lock released.
pub(crate) fn locked_with_io<R>(core: &Mutex<Core>, f: impl FnOnce(&mut Core) -> R) -> R {
    let (out, jobs) = {
        let mut guard = lock(core);
        let out = f(&mut guard);
        (out, std::mem::take(&mut guard.io_jobs))
    };
    for job in jobs {
        job();
    }
    out
}

impl Server {
    /// Creates the signaling owner and starts its heartbeat timer.
    pub fn new(options: Options) -> Result<Arc<Self>, Error> {
        piik_protocol::assert_endpoint_media_copy_capacity(options.endpoint_media_copy_capacity)
            .map_err(|error| Error(error.to_string()))?;
        if options.sfu_only && options.sfu_fallback.is_none() {
            return Err(Error(
                "SFU-only routing requires SFU media and resource admission".to_owned(),
            ));
        }
        let max_connections = if options.max_connections == 0 {
            DEFAULT_MAX_SIGNAL_CONNECTIONS
        } else {
            options.max_connections
        };
        let max_unauthenticated = if options.max_unauthenticated_connections == 0 {
            DEFAULT_MAX_UNAUTHENTICATED_CONNECTIONS
        } else {
            options.max_unauthenticated_connections
        };
        if max_unauthenticated > max_connections {
            return Err(Error("WebSocket connection limits are invalid".to_owned()));
        }
        let site_access_at_upgrade = options.site_access_at_upgrade.clone();
        let core = build_core(options, max_connections, max_unauthenticated, None);
        lock(&core).arm_heartbeat();
        Ok(Arc::new(Self {
            core,
            site_access_at_upgrade,
        }))
    }
}

/// Builds the authority state; `detached` selects the router-only harness.
pub(crate) fn build_core(
    options: Options,
    max_connections: usize,
    max_unauthenticated: usize,
    detached: Option<DetachedHooks>,
) -> Arc<Mutex<Core>> {
    {
        let router = Router::new(
            options.endpoint_media_copy_capacity,
            options.sfu_fallback,
            options.sfu_only,
        );
        let route_debug = options.route_debug.or_else(default_route_debug);
        Arc::new_cyclic(|weak: &std::sync::Weak<Mutex<Core>>| {
            Mutex::new(Core {
                store: options.store,
                endpoint_media_copy_capacity: options.endpoint_media_copy_capacity,
                ice: options.ice,
                nat_prediction_enabled: options.nat_prediction_enabled,
                allowed_origins: options.allowed_origins,
                public_base_url: options.public_base_url,
                now: options.now.unwrap_or_else(wall_clock),
                after_func: options.after_func.unwrap_or_else(default_after_func),
                authentication_timeout_ms: or_default(
                    options.authentication_timeout_ms,
                    DEFAULT_AUTHENTICATION_TIMEOUT_MS,
                ),
                viewer_disconnect_grace_ms: or_default(
                    options.viewer_disconnect_grace_ms,
                    DEFAULT_VIEWER_DISCONNECT_GRACE_MS,
                ),
                heartbeat_interval_ms: or_default(
                    options.heartbeat_interval_ms,
                    DEFAULT_HEARTBEAT_INTERVAL_MS,
                ),
                room_empty_timeout_ms: options.room_empty_timeout_ms,
                max_connections,
                max_unauthenticated_connections: max_unauthenticated,
                router,
                sessions: Map::new(),
                sessions_by_id: HashMap::new(),
                next_session_key: 0,
                pending_connections: 0,
                unauthenticated_connections: 0,
                viewer_grace_timers: HashMap::new(),
                viewer_quality_evidence_gates: HashMap::new(),
                viewer_quality_evidence_attempts: HashMap::new(),
                sender_quality_rate_by_session: HashMap::new(),
                shares: HashMap::new(),
                voices: HashMap::new(),
                deferred_viewer_presence_rooms: HashSet::new(),
                heartbeat_stop: None,
                closing: false,
                locker: Locker::from_weak(weak.clone()),
                route_debug,
                detached,
                io_jobs: Vec::new(),
            })
        })
    }
}

impl Server {
    /// Delivers one SFU media callback; it identifies a physical handle and
    /// signaling reconnect does not retire it.
    pub fn handle_sfu_media_event(&self, event: piik_sfu::runtime::MediaEvent) {
        locked_with_io(&self.core, |core| core.handle_sfu_media_event(event));
    }

    /// Closes the room store. Call after HTTP draining.
    pub fn close_storage(&self) -> Result<(), piik_room::Error> {
        lock(&self.core).store.close()
    }

    /// Closes every room in store order.
    pub fn end_all_rooms(&self) -> Result<(), piik_room::Error> {
        locked_with_io(&self.core, |core| {
            for closed in core.store.abandon_all_rooms()? {
                core.close_room(&closed);
            }
            Ok(())
        })
    }

    /// The invitation URL; the optional Viewer grant stays in the fragment.
    pub fn invite_url(public_base_url: &Url, room_id: &str, viewer_grant: &str) -> String {
        invite_url(public_base_url, room_id, viewer_grant)
    }
}

#[cfg(test)]
pub(crate) use crate::server::close::finish_router_close;
pub use crate::server::close::invite_url;
pub(crate) use crate::server::close::room_limit;
