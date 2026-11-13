//! One shared HTTP, room-authority and media service lifecycle.
//!
//! # Locking
//!
//! `piik_signal::Server` owns the one authority lock over the room store, the
//! media router and every session; nothing here takes it directly. `State`
//! below guards only this server's own lifecycle bookkeeping and is never
//! held across an await or while signaling runs. The site-access check handed
//! to signaling reads only the immutable [`AccessPolicy`].

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use piik_config::{Config, Environment};
use piik_room::Store;
use piik_sfu::runtime::{Admission, AdmissionOptions};
use piik_signal::{AfterFunc, Clock, SfuFallback};
use piik_tls::ServerTls;
use tokio::sync::{OnceCell, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::access::{AccessPolicy, SiteAccessGate, SiteAccessOptions, wall_clock};
use crate::assets::{Assets, Frontend};

mod http;
mod sfu;

use crate::media::{EmbeddedSfu, SfuMedia, SfuPublicAddress, resolve_sfu_public_ip};
pub(crate) use http::{accept_loop, remaining};
#[cfg(test)]
pub(crate) use sfu::refresh_sfu_public_ip;

/// Bounds reading one request body.
pub(crate) const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Bounds reading request headers (and a TLS handshake). hyper also applies
/// it while a keep-alive connection waits for its next request.
pub(crate) const HEADERS_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a DNS-named `SFU_PUBLIC_IP` is resolved again.
const SFU_DNS_REFRESH: Duration = Duration::from_secs(60);

/// A server lifecycle failure. Cloneable so every shutdown caller receives
/// the first operation's result.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// Invalid composition: missing TLS identity, SFU runtime mismatch.
    #[error("{0}")]
    Config(String),
    #[error("Piik server startup was already requested")]
    AlreadyStarted,
    #[error("Piik server is closing")]
    Closing,
    /// The HTTP listener could not bind; nothing else was opened.
    #[error("{0}")]
    Bind(Arc<io::Error>),
    #[error("Piik STUN listener failed: {0}")]
    Stun(Arc<io::Error>),
    /// `SFU_PUBLIC_IP` did not resolve.
    #[error("{0}")]
    Sfu(String),
    /// Room authority could not be restored, or signaling could not start.
    #[error("{0}")]
    Startup(String),
    /// A permanent accept failure on a serving listener.
    #[error("Piik HTTP listener stopped unexpectedly: {0}")]
    Listener(Arc<io::Error>),
    #[error("Piik server shutdown failed: {0}")]
    Shutdown(String),
}

/// One prebound HTTP or HTTPS socket. Ownership transfers to [`Server::new`].
pub struct Listener {
    socket: std::net::TcpListener,
    tls: Option<ServerTls>,
}

impl Listener {
    pub fn plain(socket: std::net::TcpListener) -> Self {
        Self { socket, tls: None }
    }

    pub fn tls(socket: std::net::TcpListener, identity: ServerTls) -> Self {
        Self {
            socket,
            tls: Some(identity),
        }
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.socket.local_addr()
    }
}

/// Service configuration and optional lifecycle dependencies.
#[derive(Default)]
pub struct Options {
    /// The loaded configuration; callers own `piik_config::load` / `local`.
    pub config: Config,
    /// Prebound sockets. Empty binds `listen_host:port` at [`Server::listen`].
    pub listeners: Vec<Listener>,
    /// The identity for the self-bound listener when `config.tls` is set
    /// (`piik_tls::direct`). Prebound listeners carry their own.
    pub tls: Option<ServerTls>,
    /// The built Browser UI; `None` serves the API only.
    pub assets: Option<Arc<dyn Assets>>,
    /// Replaces the embedded SFU that `config.sfu` binds at
    /// [`Server::listen`]; only valid with `config.sfu`.
    pub sfu: Option<SfuMedia>,
    /// Unix milliseconds; `None` uses the wall clock.
    pub now: Option<Clock>,
    pub authentication_timeout_ms: u64,
    pub viewer_disconnect_grace_ms: u64,
    pub heartbeat_interval_ms: u64,
    pub max_signal_connections: usize,
    pub max_unauthenticated_signal_connections: usize,
    /// Zero selects the default 24-hour site-access lifetime.
    pub site_access_ttl_seconds: i64,
    /// The signaling timer factory; `None` uses Tokio timers.
    pub after_func: Option<AfterFunc>,
    /// Replaces the store built from `config`.
    pub room_store: Option<Store>,
}

/// Owns one shared HTTP, room-authority and media service lifecycle.
pub struct Server {
    inner: Arc<Inner>,
}

/// Signaling inputs held until startup publishes room authority.
struct Pending {
    store: Store,
    sfu: Option<SfuMedia>,
    now: Option<Clock>,
    after_func: Option<AfterFunc>,
    authentication_timeout_ms: u64,
    viewer_disconnect_grace_ms: u64,
    heartbeat_interval_ms: u64,
    max_connections: usize,
    max_unauthenticated_connections: usize,
}

#[derive(Default)]
struct State {
    closing: bool,
    startup_requested: bool,
    listeners: Vec<Listener>,
    tls: Option<ServerTls>,
    pending: Option<Pending>,
    stun: Option<piik_stun::Server>,
    /// The server-owned SFU while its UDP listener is open.
    sfu: Option<Arc<EmbeddedSfu>>,
    accept_tasks: Vec<JoinHandle<()>>,
    dns_refresh: Option<JoinHandle<()>>,
    serve_failure: Option<Error>,
    #[cfg(test)]
    listener_faults: Vec<CancellationToken>,
}

pub(crate) struct Inner {
    pub(crate) config: Config,
    pub(crate) access: Arc<AccessPolicy>,
    pub(crate) frontend: Option<Frontend>,
    pub(crate) sfu_enabled: bool,
    pub(crate) sfu_only: bool,
    sfu_address: Option<SfuPublicAddress>,
    /// Read by requests without the state lock; written together under it.
    accepting: AtomicBool,
    signaling: RwLock<Option<Arc<piik_signal::Server>>>,
    state: Mutex<State>,
    /// Stops accept loops and asks open connections to finish.
    stop: CancellationToken,
    connections: TaskTracker,
    startup_done: watch::Sender<bool>,
    failure: watch::Sender<Option<Error>>,
    shutdown: OnceCell<Result<(), Error>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl Server {
    /// Composes the service without binding sockets or opening the room
    /// database.
    pub fn new(options: Options) -> Result<Self, Error> {
        let config = options.config;
        if config.sfu.is_none() && options.sfu.is_some() {
            return Err(Error::Config(
                "SFU media requires an SFU configuration".to_owned(),
            ));
        }
        if config.tls.is_some() && options.tls.is_none() && options.listeners.is_empty() {
            return Err(Error::Config(
                "direct TLS requires its server certificate".to_owned(),
            ));
        }
        let now = options.now.clone().unwrap_or_else(wall_clock);
        let store = match options.room_store {
            Some(store) => store,
            None => build_store(&config, options.now.clone())?,
        };
        let gate = SiteAccessGate::new(SiteAccessOptions {
            password: config.site_access_password.clone(),
            secure: config.env == Environment::Production
                && config.public_base_url.scheme() == "https",
            now,
            ttl_seconds: options.site_access_ttl_seconds,
        })
        .map_err(Error::Config)?;
        let public_host =
            config.public_base_url[url::Position::BeforeHost..url::Position::AfterPort].to_owned();
        let access = Arc::new(AccessPolicy::new(
            gate,
            config.allowed_origins.clone(),
            public_host,
        ));
        let sfu_only = config.sfu.as_ref().is_some_and(|sfu| sfu.only);
        let sfu_address = config.sfu.as_ref().map(|_| {
            options
                .sfu
                .as_ref()
                .map(|sfu| sfu.public_address.clone())
                .unwrap_or_default()
        });
        let state = State {
            listeners: options.listeners,
            tls: options.tls,
            pending: Some(Pending {
                store,
                sfu: options.sfu,
                now: options.now,
                after_func: options.after_func,
                authentication_timeout_ms: options.authentication_timeout_ms,
                viewer_disconnect_grace_ms: options.viewer_disconnect_grace_ms,
                heartbeat_interval_ms: options.heartbeat_interval_ms,
                max_connections: options.max_signal_connections,
                max_unauthenticated_connections: options.max_unauthenticated_signal_connections,
            }),
            ..State::default()
        };
        Ok(Self {
            inner: Arc::new(Inner {
                sfu_enabled: config.sfu.is_some(),
                sfu_only,
                sfu_address,
                config,
                access,
                frontend: options.assets.map(Frontend::new),
                accepting: AtomicBool::new(false),
                signaling: RwLock::new(None),
                state: Mutex::new(state),
                stop: CancellationToken::new(),
                connections: TaskTracker::new(),
                startup_done: watch::Sender::new(false),
                failure: watch::Sender::new(None),
                shutdown: OnceCell::new(),
            }),
        })
    }

    /// Binds application sockets, restores room authority and starts
    /// accepting traffic. Returns the first HTTP listener's port.
    pub async fn listen(&self) -> Result<u16, Error> {
        {
            let mut state = lock(&self.inner.state);
            if state.startup_requested {
                return Err(Error::AlreadyStarted);
            }
            if state.closing {
                return Err(Error::Closing);
            }
            state.startup_requested = true;
        }
        // Release the startup latch however start ends, including a
        // cancelled or panicking future, so shutdown never waits forever.
        struct Settled<'a>(&'a watch::Sender<bool>);
        impl Drop for Settled<'_> {
            fn drop(&mut self) {
                self.0.send_replace(true);
            }
        }
        let _settled = Settled(&self.inner.startup_done);
        self.inner.start().await
    }

    /// Resolves with the first unexpected HTTP listener failure; normal
    /// [`Server::close`] / [`Server::end`] never resolve it.
    pub async fn failed(&self) -> Error {
        let mut failure = self.inner.failure.subscribe();
        loop {
            if let Some(error) = failure.borrow_and_update().clone() {
                return error;
            }
            if failure.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
    }

    /// Stops traffic, closes signaling, the HTTP/STUN listeners and the room
    /// store, and leaves the rooms in place. Hosted restarts use it.
    /// `timeout` bounds signaling close and the HTTP drain.
    pub async fn close(&self, timeout: Duration) -> Result<(), Error> {
        self.shutdown(timeout, false).await
    }

    /// [`Server::close`] preceded by ending every room. The App uses it,
    /// because its Local authority dies with the process.
    pub async fn end(&self, timeout: Duration) -> Result<(), Error> {
        self.shutdown(timeout, true).await
    }

    /// Every shutdown caller joins the first operation and its result.
    async fn shutdown(&self, timeout: Duration, end_rooms: bool) -> Result<(), Error> {
        let inner = self.inner.clone();
        self.inner
            .shutdown
            .get_or_init(|| async move { inner.run_shutdown(timeout, end_rooms).await })
            .await
            .clone()
    }

    #[cfg(test)]
    pub(crate) fn inner(&self) -> &Arc<Inner> {
        &self.inner
    }
}

impl Drop for Server {
    /// A dropped handle stops accepting; an orderly owner calls close first.
    fn drop(&mut self) {
        self.inner.stop.cancel();
    }
}

/// An empty database path selects memory-only authority. The file is opened
/// by `Store::initialize`, so a failed bind leaves no database behind.
pub(crate) fn build_store(config: &Config, now: Option<Clock>) -> Result<Store, Error> {
    let database = config
        .room_database_path
        .as_ref()
        .map(piik_room::Database::new)
        .transpose()
        .map_err(|error| Error::Startup(error.to_string()))?;
    Store::new(piik_room::Options {
        max_rooms: piik_room::CAPACITY,
        max_viewers_per_room: config.max_viewers_per_room,
        database,
        random: None,
        now,
    })
    .map_err(|error| Error::Startup(error.to_string()))
}

impl Inner {
    /// Signaling while traffic is accepted.
    pub(crate) fn accepting_signaling(&self) -> Option<Arc<piik_signal::Server>> {
        if !self.accepting.load(Ordering::Acquire) {
            return None;
        }
        self.signaling
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn start(self: &Arc<Self>) -> Result<u16, Error> {
        if let (Some(sfu), Some(address)) = (&self.config.sfu, &self.sfu_address) {
            match resolve_sfu_public_ip(&sfu.public_ip).await {
                Ok(resolved) => address.set(resolved),
                Err(error) => {
                    // Nothing media-owned is open yet.
                    let _ = self.stop_serving().await;
                    return Err(Error::Sfu(error));
                }
            }
        }
        // Bind first: a listener owned by another process must fail before
        // the room database is opened.
        let listeners = {
            let mut state = lock(&self.state);
            if state.listeners.is_empty() {
                let address = (self.config.listen_host.as_str(), self.config.port);
                let socket = std::net::TcpListener::bind(address)
                    .map_err(|error| Error::Bind(Arc::new(error)))?;
                let tls = state.tls.clone();
                state.listeners.push(Listener { socket, tls });
            }
            std::mem::take(&mut state.listeners)
        };
        let port = listeners[0]
            .socket
            .local_addr()
            .map_err(|error| Error::Bind(Arc::new(error)))?
            .port();
        let mut sockets = Vec::with_capacity(listeners.len());
        for listener in listeners {
            let ready = listener
                .socket
                .set_nonblocking(true)
                .and_then(|()| tokio::net::TcpListener::from_std(listener.socket));
            match ready {
                Ok(socket) => sockets.push((socket, listener.tls.map(|tls| tls.acceptor()))),
                Err(error) => return Err(Error::Bind(Arc::new(error))),
            }
        }
        if !self.config.stun_listen_addresses.is_empty() {
            let addresses: Vec<String> = self
                .config
                .stun_listen_addresses
                .iter()
                .map(ToString::to_string)
                .collect();
            match piik_stun::listen(&addresses).await {
                Ok(stun) => lock(&self.state).stun = Some(stun),
                Err(error) => return Err(Error::Stun(Arc::new(error))),
            }
        }
        if let Err(error) = self.bind_sfu() {
            // The failed bind left no SFU to close.
            let _ = self.stop_serving().await;
            return Err(error);
        }
        self.start_sfu_dns_refresh();
        for (socket, tls) in sockets {
            let fault = CancellationToken::new();
            #[cfg(test)]
            lock(&self.state).listener_faults.push(fault.clone());
            let task = tokio::spawn(accept_loop(self.clone(), socket, tls, fault));
            lock(&self.state).accept_tasks.push(task);
        }
        if let Err(error) = self.reconcile() {
            // Give the database and the listeners back before reporting.
            let mut cleanup = Vec::new();
            if let Some(mut pending) = lock(&self.state).pending.take()
                && let Err(close) = pending.store.close()
            {
                cleanup.push(close.to_string());
            }
            if let Err(close) = self.stop_serving().await {
                cleanup.push(close);
            }
            if !cleanup.is_empty() {
                return Err(Error::Startup(format!(
                    "Piik startup reconciliation failed: {error}\n{}",
                    cleanup.join("\n")
                )));
            }
            return Err(error);
        }
        Ok(port)
    }

    /// Restores room authority and enables traffic unless shutdown began.
    pub(crate) fn reconcile(self: &Arc<Self>) -> Result<(), Error> {
        let mut state = lock(&self.state);
        if let Some(failure) = &state.serve_failure {
            return Err(failure.clone());
        }
        match state.pending.as_mut() {
            Some(pending) => pending
                .store
                .initialize()
                .map_err(|error| Error::Startup(error.to_string()))?,
            None => return Ok(()),
        }
        // Serialized with shutdown: once closing is set, startup must never
        // re-enable traffic.
        if state.closing {
            return Ok(());
        }
        let pending = state.pending.take().expect("pending signaling inputs");
        let access = self.access.clone();
        let sfu_fallback = pending.sfu.as_ref().map(|sfu| SfuFallback {
            media: sfu.runtime.clone(),
            admission: Admission::new(AdmissionOptions {
                ingress_capacity: piik_room::CAPACITY,
                egress_capacity: piik_room::CAPACITY * self.config.max_viewers_per_room,
            }),
            prepare_timeout_ms: sfu.prepare_timeout_ms,
        });
        let signaling = piik_signal::Server::new(piik_signal::Options {
            store: pending.store,
            endpoint_media_copy_capacity: self.config.endpoint_media_copy_capacity,
            sfu_fallback,
            sfu_only: self.sfu_only,
            ice: piik_config::ice_config(&self.config),
            // The prediction capability is separate from the wire ICE settings.
            nat_prediction_enabled: self.config.nat_prediction_enabled,
            allowed_origins: self.config.allowed_origins.clone(),
            site_access_at_upgrade: Some(Arc::new(move |headers| {
                crate::routes::site_access_at_upgrade(&access, headers)
            })),
            public_base_url: self.config.public_base_url.clone(),
            now: pending.now,
            authentication_timeout_ms: pending.authentication_timeout_ms,
            viewer_disconnect_grace_ms: pending.viewer_disconnect_grace_ms,
            heartbeat_interval_ms: pending.heartbeat_interval_ms,
            room_empty_timeout_ms: i64::from(self.config.room_empty_timeout_seconds) * 1_000,
            max_connections: pending.max_connections,
            max_unauthenticated_connections: pending.max_unauthenticated_connections,
            after_func: pending.after_func,
            route_debug: None,
        })
        .map_err(|error| Error::Startup(error.to_string()))?;
        if let Some(sfu) = &pending.sfu {
            sfu.events.attach(&signaling);
        }
        *self
            .signaling
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(signaling);
        self.accepting.store(true, Ordering::Release);
        Ok(())
    }

    /// Releases the application-owned HTTP, STUN and SFU media. Accept loops
    /// are joined, so the TCP ports are free when this returns; the SFU's
    /// connections are closed and its UDP port is released once signaling
    /// drops its runtime handle.
    async fn stop_serving(&self) -> Result<(), String> {
        self.stop.cancel();
        let (tasks, stun, refresh, sfu) = {
            let mut state = lock(&self.state);
            state.listeners.clear();
            (
                std::mem::take(&mut state.accept_tasks),
                state.stun.take(),
                state.dns_refresh.take(),
                state.sfu.take(),
            )
        };
        for task in tasks {
            let _ = task.await;
        }
        if let Some(stun) = stun {
            stun.close().await;
        }
        if let Some(refresh) = refresh {
            let _ = refresh.await;
        }
        let Some(sfu) = sfu else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || sfu.close())
            .await
            .map_err(|error| format!("Piik SFU close failed: {error}"))?
            .map_err(|error| format!("Piik SFU close failed: {error}"))
    }

    async fn run_shutdown(
        self: Arc<Self>,
        timeout: Duration,
        end_rooms: bool,
    ) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        // Stop traffic, mark closing, then wait for an in-flight startup.
        let await_startup = {
            let mut state = lock(&self.state);
            self.accepting.store(false, Ordering::Release);
            state.closing = true;
            state.startup_requested
        };
        if await_startup {
            // The listen caller owns the startup error; shutdown only waits.
            let mut settled = self.startup_done.subscribe();
            let _ = settled.wait_for(|done| *done).await;
        }
        let mut failures = Vec::new();
        let signaling = self
            .signaling
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let mut pending = lock(&self.state).pending.take();
        if end_rooms {
            let ended = match (&signaling, pending.as_mut()) {
                (Some(signaling), _) => signaling.end_all_rooms(),
                // Without signaling no participant is connected.
                (None, Some(pending)) => pending.store.abandon_all_rooms().map(drop),
                (None, None) => Ok(()),
            };
            if let Err(error) = ended {
                failures.push(error.to_string());
            }
        }
        if let Some(signaling) = &signaling
            && let Err(error) = signaling.close(remaining(deadline)).await
        {
            failures.push(error.to_string());
        }
        // Signaling closes before the listeners and the SFU: its connections
        // live on them, and its rooms own the media they release.
        if let Err(error) = self.stop_serving().await {
            failures.push(error);
        }
        self.connections.close();
        if tokio::time::timeout(remaining(deadline), self.connections.wait())
            .await
            .is_err()
        {
            failures.push("HTTP requests did not finish before the shutdown deadline".to_owned());
        }
        // Storage closes after HTTP draining; an admitted mutation still
        // holding the signaling lock finishes first.
        let closed = match (&signaling, pending.as_mut()) {
            (Some(signaling), _) => {
                let signaling = signaling.clone();
                tokio::task::spawn_blocking(move || signaling.close_storage())
                    .await
                    .unwrap_or(Ok(()))
            }
            (None, Some(pending)) => pending.store.close(),
            (None, None) => Ok(()),
        };
        if let Err(error) = closed {
            failures.push(error.to_string());
        }
        // Signaling holds the last SFU runtime handle; releasing it gives the
        // closed SFU's UDP port back.
        drop(signaling);
        drop(pending);
        self.signaling
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::Shutdown(failures.join("\n")))
        }
    }
}
#[cfg(test)]
impl Inner {
    /// Fails one serving listener the way an out-of-band close would.
    pub(crate) fn fail_listener(&self, index: usize) {
        lock(&self.state).listener_faults[index].cancel();
    }

    pub(crate) fn stun_addresses(&self) -> Vec<SocketAddr> {
        lock(&self.state)
            .stun
            .as_ref()
            .map(|stun| stun.addresses().to_vec())
            .unwrap_or_default()
    }

    /// The embedded SFU while its UDP listener is open.
    pub(crate) fn embedded_sfu(&self) -> Option<piik_sfu::Media> {
        lock(&self.state).sfu.as_ref().map(|sfu| sfu.media.clone())
    }

    /// One DNS refresh tick for `host`, as the refresh worker runs it.
    pub(crate) async fn refresh_sfu(&self, host: &str) {
        let sfu = lock(&self.state).sfu.clone();
        let address = self.sfu_address.as_ref().expect("SFU address");
        refresh_sfu_public_ip(address, sfu.as_deref(), host).await;
    }

    pub(crate) fn has_dns_refresh(&self) -> bool {
        lock(&self.state).dns_refresh.is_some()
    }

    pub(crate) fn is_accepting(&self) -> bool {
        self.accepting.load(Ordering::Acquire)
    }

    pub(crate) fn has_signaling(&self) -> bool {
        self.signaling
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }
}
