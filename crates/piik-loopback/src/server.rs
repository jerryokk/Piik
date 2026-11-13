//! The loopback v10 server: lifecycle, claims, origin policy and listener
//! startup. HTTP handlers and the `/control` socket live in sibling modules.

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

mod control;
mod handlers;

use axum::Router;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use piik_app_config::QualityPreference;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::{TaskTracker, task_tracker::TaskTrackerToken};

use crate::buildinfo;
use crate::capabilities::NativeMediaCapabilities;
use crate::http::{header_text, json, method_not_allowed, not_found, request_host, text_error};
use crate::protocol::{MAX_CONTROL_MESSAGE_BYTES, encode_message, normalize_port_range};
use crate::session::ControlFactory;

const INSTANCE_TOKEN_BYTES: usize = 32;
pub(super) const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
pub(super) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CONTROL_SESSIONS: usize = 2;
pub(super) const MAX_PRESENTATION_BYTES: usize = 256;

pub type PresentationCallback = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Default, Clone)]
pub struct Options {
    /// Zero for both keeps the default 39721..=39730 range.
    pub port_start: u16,
    pub port_end: u16,
    /// Zero keeps the desktop default. Single-source platforms may admit one.
    pub max_control_sessions: usize,
    pub allowed_origins: Vec<String>,
    pub native_media: NativeMediaCapabilities,
    pub new_control: Option<ControlFactory>,
    pub presentation: Option<PresentationCallback>,
    pub quality_preference: Option<Arc<QualityPreference>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub url: String,
    pub host: String,
    pub port: u16,
    pub instance_token: String,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("native control session limit is invalid")]
    InvalidSessionLimit,
    #[error("helper could not bind port range {start}-{end}: {source}")]
    Bind {
        start: u16,
        end: u16,
        #[source]
        source: Arc<std::io::Error>,
    },
    #[error("loopback server failed: {0}")]
    Serve(#[source] Arc<std::io::Error>),
    /// Permission already changed; a revoked origin is never restored because
    /// one of its media resources is slow to close.
    #[error("App is still releasing the previous site's media; try again shortly")]
    StillReleasing,
    #[error("native control did not stop within its shutdown bound")]
    CloseTimeout,
}

/// One running loopback server. Clones share it.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    cancel: CancellationToken,
    endpoint: Endpoint,
    native_media: NativeMediaCapabilities,
    new_control: Option<ControlFactory>,
    max_sessions: usize,
    presentation: Option<PresentationCallback>,
    quality_preference: Option<Arc<QualityPreference>>,
    controls: TaskTracker,
    done: watch::Sender<Option<Result<(), Error>>>,
    closed: tokio::sync::OnceCell<Result<(), Error>>,
    state: Mutex<ServerState>,
}

struct ServerState {
    allowed_origins: Vec<String>,
    connect_url: String,
    connections: HashMap<u64, Arc<Claim>>,
    next_claim: u64,
    closed: bool,
}

/// Claims start before the WebSocket upgrade so revocation also retires a
/// pending handshake, even if that same origin is approved again later.
struct Claim {
    id: u64,
    origin: String,
    cancel: CancellationToken,
    done: CancellationToken,
    task: Mutex<Option<TaskTrackerToken>>,
}

impl Server {
    /// Binds the first free port in the configured range on 127.0.0.1. Must be
    /// called inside a Tokio runtime. Cancelling `parent` closes the server.
    pub fn start(parent: &CancellationToken, options: Options) -> Result<Self, Error> {
        let max_sessions = match options.max_control_sessions {
            0 => MAX_CONTROL_SESSIONS,
            limit @ 1..=MAX_CONTROL_SESSIONS => limit,
            _ => return Err(Error::InvalidSessionLimit),
        };
        let (start, end) = normalize_port_range(options.port_start, options.port_end);
        let listener = listen_in_range(start, end)?;
        let address = listener.local_addr().map_err(|source| Error::Bind {
            start,
            end,
            source: Arc::new(source),
        })?;
        let listener =
            tokio::net::TcpListener::from_std(listener).map_err(|source| Error::Bind {
                start,
                end,
                source: Arc::new(source),
            })?;
        let cancel = parent.child_token();
        let (done, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            cancel: cancel.clone(),
            endpoint: Endpoint {
                url: format!("http://{address}"),
                host: address.to_string(),
                port: address.port(),
                instance_token: new_instance_token(),
            },
            native_media: options.native_media,
            new_control: options.new_control,
            max_sessions,
            presentation: options.presentation,
            quality_preference: options.quality_preference,
            controls: TaskTracker::new(),
            done,
            closed: tokio::sync::OnceCell::new(),
            state: Mutex::new(ServerState {
                allowed_origins: normalized_origins(&options.allowed_origins),
                connect_url: String::new(),
                connections: HashMap::new(),
                next_claim: 0,
                closed: false,
            }),
        });
        let router = Router::new()
            .fallback(handlers::dispatch)
            .with_state(inner.clone());
        let served = inner.clone();
        tokio::spawn(async move {
            let result = crate::http::serve(listener, router, served.cancel.clone())
                .await
                .map_err(|error| Error::Serve(Arc::new(error)));
            if result.is_err() {
                served.cancel.cancel();
            }
            served.done.send_replace(Some(result));
        });
        let server = Self { inner };
        let closing = server.clone();
        tokio::spawn(async move {
            closing.inner.cancel.cancelled().await;
            let _ = closing.close().await;
        });
        Ok(server)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    /// Resolves when serving stops; an error is an unexpected accept failure.
    pub async fn done(&self) -> Result<(), Error> {
        let mut done = self.inner.done.subscribe();
        match done.wait_for(Option::is_some).await {
            Ok(result) => result.clone().unwrap_or(Ok(())),
            Err(_) => Ok(()),
        }
    }

    /// Replaces the approved origins, then waits (bounded) for every control
    /// connection that lost its approval to release its session.
    pub async fn set_allowed_origins(&self, origins: &[String]) -> Result<(), Error> {
        let retired = self
            .inner
            .set_allowed_origins_locked(&mut self.inner.lock(), origins);
        self.inner.retire_connections(retired).await
    }

    /// Retires one authority without replaying other origins.
    pub async fn remove_allowed_origin(&self, origin: &str) -> Result<(), Error> {
        let retired = {
            let mut state = self.inner.lock();
            let origins: Vec<String> = state
                .allowed_origins
                .iter()
                .filter(|allowed| !same_origin(allowed, origin))
                .cloned()
                .collect();
            self.inner.set_allowed_origins_locked(&mut state, &origins)
        };
        self.inner.retire_connections(retired).await
    }

    /// Publishes the local approval page through `/connect-discovery`; empty
    /// hides discovery.
    pub fn set_connect_url(&self, value: &str) {
        value.clone_into(&mut self.inner.lock().connect_url);
    }

    /// Stops serving and retires every control session, bounded by 2 s.
    /// Repeated calls return the first result.
    pub async fn close(&self) -> Result<(), Error> {
        let inner = &self.inner;
        inner
            .closed
            .get_or_init(|| async {
                {
                    let mut state = inner.lock();
                    state.closed = true;
                    for connection in state.connections.values() {
                        connection.cancel.cancel();
                    }
                }
                inner.cancel.cancel();
                let deadline = Instant::now() + SHUTDOWN_TIMEOUT;
                let mut done = inner.done.subscribe();
                let served = tokio::time::timeout_at(deadline, done.wait_for(Option::is_some))
                    .await
                    .map(drop);
                inner.controls.close();
                let retired = tokio::time::timeout_at(deadline, inner.controls.wait()).await;
                if served.is_err() || retired.is_err() {
                    return Err(Error::CloseTimeout);
                }
                Ok(())
            })
            .await
            .clone()
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, ServerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn set_allowed_origins_locked(
        &self,
        state: &mut ServerState,
        origins: &[String],
    ) -> Vec<Arc<Claim>> {
        state.allowed_origins = normalized_origins(origins);
        let mut retired = Vec::new();
        for connection in state.connections.values() {
            if connection.cancel.is_cancelled()
                || !self.origin_allowed_locked(state, &connection.origin)
            {
                connection.cancel.cancel();
                retired.push(connection.clone());
            }
        }
        retired
    }

    async fn retire_connections(&self, retired: Vec<Arc<Claim>>) -> Result<(), Error> {
        let all = async {
            for connection in &retired {
                connection.done.cancelled().await;
            }
        };
        tokio::select! {
            biased;
            () = all => Ok(()),
            () = tokio::time::sleep(SHUTDOWN_TIMEOUT) => Err(Error::StillReleasing),
            () = self.cancel.cancelled() => Err(Error::StillReleasing),
        }
    }

    fn allowed_origin(&self, origin: &str) -> bool {
        self.lock()
            .allowed_origins
            .iter()
            .any(|allowed| same_origin(origin, allowed))
    }

    /// Empty Origin is reserved for per-user native tooling; browsers always
    /// send it when opening a cross-origin control connection.
    fn origin_allowed_locked(&self, state: &ServerState, origin: &str) -> bool {
        origin.is_empty()
            || self.local_origin(origin)
            || state
                .allowed_origins
                .iter()
                .any(|allowed| same_origin(origin, allowed))
    }

    fn host_allowed(&self, host: &str) -> bool {
        host == self.endpoint.host || host == format!("localhost:{}", self.endpoint.port)
    }

    fn local_origin(&self, origin: &str) -> bool {
        let port = self.endpoint.port;
        same_origin(origin, &format!("http://127.0.0.1:{port}"))
            || same_origin(origin, &format!("http://localhost:{port}"))
    }

    fn claim_connection(&self, origin: &str) -> Option<Arc<Claim>> {
        let mut state = self.lock();
        if state.closed
            || state.connections.len() >= self.max_sessions
            || !self.origin_allowed_locked(&state, origin)
        {
            return None;
        }
        state.next_claim += 1;
        let claim = Arc::new(Claim {
            id: state.next_claim,
            origin: origin.to_owned(),
            cancel: self.cancel.child_token(),
            done: CancellationToken::new(),
            task: Mutex::new(Some(self.controls.token())),
        });
        state.connections.insert(claim.id, claim.clone());
        Some(claim)
    }

    /// A cancelled claim cannot attach a socket after leaving the lock.
    fn set_connection(&self, claim: &Claim) -> bool {
        let state = self.lock();
        !state.closed && !claim.cancel.is_cancelled()
    }

    fn release_connection(&self, claim: &Claim) {
        claim.cancel.cancel();
        self.lock().connections.remove(&claim.id);
        drop(
            claim
                .task
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take(),
        );
        claim.done.cancel();
    }
}

/// Releases a claim on every exit path, including a failed upgrade.
struct ClaimGuard {
    inner: Arc<Inner>,
    claim: Arc<Claim>,
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.inner.release_connection(&self.claim);
    }
}

fn listen_in_range(start: u16, end: u16) -> Result<std::net::TcpListener, Error> {
    let mut last = None;
    for port in start..=end {
        match std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port))) {
            Ok(listener) => {
                listener
                    .set_nonblocking(true)
                    .map_err(|source| Error::Bind {
                        start,
                        end,
                        source: Arc::new(source),
                    })?;
                return Ok(listener);
            }
            Err(error) => last = Some(error),
        }
    }
    Err(Error::Bind {
        start,
        end,
        source: Arc::new(last.unwrap_or_else(|| std::io::Error::other("empty port range"))),
    })
}

pub(crate) fn same_origin(actual: &str, expected: &str) -> bool {
    actual
        .trim_end_matches('/')
        .eq_ignore_ascii_case(expected.trim_end_matches('/'))
}

fn normalized_origins(origins: &[String]) -> Vec<String> {
    let mut result: Vec<String> = Vec::with_capacity(origins.len());
    for origin in origins {
        let origin = origin.trim().trim_end_matches('/');
        if origin.is_empty() || result.iter().any(|seen| seen.eq_ignore_ascii_case(origin)) {
            continue;
        }
        result.push(origin.to_owned());
    }
    result
}

fn new_instance_token() -> String {
    use rand::TryRng;
    let mut buffer = [0; INSTANCE_TOKEN_BYTES];
    // Like crypto/rand, an operating-system entropy failure is fatal.
    rand::rngs::SysRng
        .try_fill_bytes(&mut buffer)
        .expect("operating-system random source failed");
    URL_SAFE_NO_PAD.encode(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn pending_handshake_cannot_survive_origin_replacement() {
        let origin = "https://share.example.com".to_owned();
        let server = Server::start(
            &CancellationToken::new(),
            Options {
                port_start: 0,
                port_end: 39_999,
                allowed_origins: vec![origin.clone()],
                ..Options::default()
            },
        )
        .unwrap();
        let inner = server.inner.clone();
        let claim = inner
            .claim_connection(&origin)
            .expect("initial claim failed");
        let changing = server.clone();
        let done = tokio::spawn(async move { changing.set_allowed_origins(&[]).await });
        tokio::time::timeout(Duration::from_secs(1), claim.cancel.cancelled())
            .await
            .expect("pending upgrade was not cancelled");
        // Regrant the same origin before its old handshake returns.
        inner.lock().allowed_origins = vec![origin];
        assert!(
            !inner.set_connection(&claim),
            "old generation regained permission"
        );
        // Cancellation of the App also bounds an already pending permission change.
        inner.cancel.cancel();
        assert!(
            done.await.unwrap().is_err(),
            "cancelled change reported ready"
        );
        assert!(claim.cancel.is_cancelled());
        inner.release_connection(&claim);
    }

    #[test]
    fn origins_normalize_without_case_duplicates() {
        let origins = normalized_origins(&[
            " https://A.example/ ".to_owned(),
            "https://a.example".to_owned(),
            String::new(),
            "https://b.example".to_owned(),
        ]);
        assert_eq!(origins, ["https://A.example", "https://b.example"]);
        assert!(same_origin("https://a.EXAMPLE/", "https://A.example"));
    }
}
