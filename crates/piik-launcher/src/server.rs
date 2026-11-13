//! The launcher server on a loopback port: launch attempts, lifecycle and
//! the request handlers in the `http` sibling module.

use std::error::Error as StdError;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

mod http;

use axum::Router;
use axum::extract::Request;
use axum::http::{Method, header};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::assets::{Assets, LAUNCHER_INDEX};
use crate::entry::entry_url;
use piik_loopback::http::{header_text, request_host};

use http::{same_origin, valid_target};

const MAX_REQUEST_BYTES: usize = 2 << 10;
pub(super) const MAX_DETAIL_CHARS: usize = 2048;
const CLOSE_TIMEOUT: Duration = Duration::from_secs(3);

pub type BoxError = Box<dyn StdError + Send + Sync + 'static>;
type SharedError = Arc<dyn StdError + Send + Sync + 'static>;

/// Applies one approval change and returns the permission actually in force,
/// even when the change failed part-way (for example, a revoked origin whose
/// media is still releasing).
pub type ChangeSite = Arc<
    dyn Fn(String, bool) -> BoxFuture<'static, (Vec<String>, Result<(), BoxError>)> + Send + Sync,
>;
pub type SaveStartupSite = Arc<dyn Fn(&str) -> Result<(), BoxError> + Send + Sync>;
pub type EnableDebug = Arc<dyn Fn() -> Result<(), BoxError> + Send + Sync>;
pub type LanAddresses = Arc<dyn Fn() -> Result<Vec<piik_lan::Address>, BoxError> + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    Local,
    Link,
}

impl Mode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Link => "link",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "local" => Some(Self::Local),
            "link" => Some(Self::Link),
            _ => None,
        }
    }
}

/// One accepted launch request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub mode: Mode,
    pub language: String,
    pub local_access_password: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub debug: bool,
    #[serde(rename = "lanAddress", skip_serializing_if = "Option::is_none")]
    pub lan_address: Option<String>,
}

#[derive(Default, Clone)]
pub struct Options {
    /// Empty offers both modes.
    pub modes: Vec<Mode>,
    /// `None` defaults to [`Mode::Link`].
    pub default_mode: Option<Mode>,
    pub sites: Vec<String>,
    pub startup_site: String,
    pub version: String,
    pub revision: String,
    pub control_port: u16,
    pub local_access_password: String,
    pub debug: bool,
    pub lan_address: String,
    pub change_site: Option<ChangeSite>,
    pub save_startup_site: Option<SaveStartupSite>,
    pub enable_debug: Option<EnableDebug>,
    /// `None` lists the host's active LAN IPv4 addresses.
    pub lan_addresses: Option<LanAddresses>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    #[error("App launcher Site is invalid: {0}")]
    InvalidSite(String),
    #[error("App launcher assets are unavailable")]
    AssetsUnavailable,
    #[error("App launcher could not start: {0}")]
    Bind(#[source] Arc<std::io::Error>),
    #[error("App launcher failed: {0}")]
    Serve(#[source] Arc<std::io::Error>),
    #[error("App launcher did not stop within its shutdown bound")]
    CloseTimeout,
}

/// One running launcher. Clones share it.
#[derive(Clone)]
pub struct Server {
    inner: Arc<Inner>,
}

struct Inner {
    cancel: CancellationToken,
    address: SocketAddr,
    assets: Arc<dyn Assets>,
    modes: Vec<Mode>,
    default_mode: Option<Mode>,
    local_access_password: String,
    version: String,
    revision: String,
    control_port: u16,
    lan_address: String,
    change_site: Option<ChangeSite>,
    save_startup_site: Option<SaveStartupSite>,
    enable_debug: Option<EnableDebug>,
    lan_addresses: LanAddresses,
    selection_sender: mpsc::UnboundedSender<Selection>,
    selection: tokio::sync::Mutex<mpsc::UnboundedReceiver<Selection>>,
    /// Serializes approval and startup changes with their persistence.
    site_change: tokio::sync::Mutex<()>,
    done: watch::Sender<Option<Result<(), Error>>>,
    state: Mutex<LauncherState>,
}

struct LauncherState {
    sites: Vec<String>,
    startup_site: String,
    debug: bool,
    attempt: Option<Arc<Attempt>>,
}

/// One accepted launch. Its requests join it until it fails.
struct Attempt {
    ready: CancellationToken,
    result: Mutex<AttemptResult>,
}

#[derive(Default, Clone)]
struct AttemptResult {
    target: String,
    error: Option<SharedError>,
}

impl Attempt {
    fn result(&self) -> AttemptResult {
        self.result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl Server {
    /// Serves the launcher page and its API on `127.0.0.1:0`. `assets` is the
    /// built Browser UI; without its `index.html` the launcher cannot run.
    /// Must be called inside a Tokio runtime. Cancelling `parent` closes it.
    pub fn start(
        parent: &CancellationToken,
        assets: impl Assets,
        options: Options,
    ) -> Result<Self, Error> {
        let sites = piik_app_config::normalize_sites(&options.sites)
            .map_err(|error| Error::InvalidSite(error.to_string()))?;
        if assets.read(LAUNCHER_INDEX).is_none() {
            return Err(Error::AssetsUnavailable);
        }
        let bind = |error| Error::Bind(Arc::new(error));
        let listener = std::net::TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
            .map_err(bind)?;
        listener.set_nonblocking(true).map_err(bind)?;
        let address = listener.local_addr().map_err(bind)?;
        let listener = tokio::net::TcpListener::from_std(listener).map_err(bind)?;
        let (selection_sender, selection) = mpsc::unbounded_channel();
        let (done, _) = watch::channel(None);
        let inner = Arc::new(Inner {
            cancel: parent.child_token(),
            address,
            assets: Arc::new(assets),
            modes: options.modes,
            default_mode: options.default_mode,
            local_access_password: options.local_access_password,
            version: options.version,
            revision: options.revision.trim().to_owned(),
            control_port: options.control_port,
            lan_address: options.lan_address,
            change_site: options.change_site,
            save_startup_site: options.save_startup_site,
            enable_debug: options.enable_debug,
            lan_addresses: options
                .lan_addresses
                .unwrap_or_else(|| Arc::new(|| piik_lan::list().map_err(BoxError::from))),
            selection_sender,
            selection: tokio::sync::Mutex::new(selection),
            site_change: tokio::sync::Mutex::new(()),
            done,
            state: Mutex::new(LauncherState {
                sites,
                startup_site: options.startup_site,
                debug: options.debug,
                attempt: None,
            }),
        });
        let router = Router::new()
            .fallback(http::dispatch)
            .with_state(inner.clone());
        let served = inner.clone();
        tokio::spawn(async move {
            let result = piik_loopback::http::serve(listener, router, served.cancel.clone())
                .await
                .map_err(|error| Error::Serve(Arc::new(error)));
            served.cancel.cancel();
            served.done.send_replace(Some(result));
        });
        Ok(Self { inner })
    }

    /// The launcher page itself, including when returning from a room.
    pub fn url(&self) -> String {
        format!("http://{}/client", self.inner.address)
    }

    /// The current approved startup Site's App entry, otherwise [`Self::url`].
    pub fn entry_url(&self) -> String {
        let startup = self.inner.lock().startup_site.clone();
        if startup.is_empty() {
            self.url()
        } else {
            entry_url(&format!("{startup}/"), self.inner.control_port)
        }
    }

    /// The next accepted launch selection; `None` after the server closed.
    pub async fn next_selection(&self) -> Option<Selection> {
        let mut selection = self.inner.selection.lock().await;
        tokio::select! {
            selection = selection.recv() => selection,
            () = self.inner.cancel.cancelled() => None,
        }
    }

    /// An already queued selection, without waiting.
    pub fn try_selection(&self) -> Option<Selection> {
        self.inner.selection.try_lock().ok()?.try_recv().ok()
    }

    /// Starts an attempt for a launch that did not come from the page (such
    /// as a command-line mode). False joins the current pending or ready one.
    pub fn begin(&self) -> bool {
        self.inner.begin().1
    }

    /// Completes the current attempt; called by the serial room owner after
    /// readiness or full retirement. A later failure replaces a ready target.
    pub fn set_result(&self, result: Result<String, BoxError>) {
        let result = match result {
            Ok(target) if valid_target(&target) => AttemptResult {
                target,
                error: None,
            },
            Ok(target) => AttemptResult {
                target,
                error: Some(Arc::from(BoxError::from("App target is invalid"))),
            },
            Err(error) => AttemptResult {
                target: String::new(),
                error: Some(Arc::from(error)),
            },
        };
        let state = self.inner.lock();
        if let Some(attempt) = &state.attempt {
            *attempt
                .result
                .lock()
                .unwrap_or_else(PoisonError::into_inner) = result;
            attempt.ready.cancel();
        }
    }

    /// Resolves when serving stops.
    pub async fn done(&self) -> Result<(), Error> {
        let mut done = self.inner.done.subscribe();
        match done.wait_for(Option::is_some).await {
            Ok(result) => result.clone().unwrap_or(Ok(())),
            Err(_) => Ok(()),
        }
    }

    /// Stops serving and releases waiting launch requests.
    pub async fn close(&self) -> Result<(), Error> {
        self.inner.cancel.cancel();
        tokio::time::timeout(CLOSE_TIMEOUT, self.done())
            .await
            .map_err(|_| Error::CloseTimeout)?
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, LauncherState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn begin(&self) -> (Arc<Attempt>, bool) {
        let mut state = self.lock();
        if let Some(attempt) = &state.attempt
            && attempt.result().error.is_none()
        {
            return (attempt.clone(), false);
        }
        let attempt = Arc::new(Attempt {
            ready: CancellationToken::new(),
            result: Mutex::new(AttemptResult::default()),
        });
        state.attempt = Some(attempt.clone());
        (attempt, true)
    }

    fn request_allowed(&self, request: &Request) -> bool {
        let host = request_host(request);
        let Some((name, port)) = host.rsplit_once(':') else {
            return false;
        };
        if port != self.address.port().to_string() {
            return false;
        }
        let name = name.to_ascii_lowercase();
        if name != "127.0.0.1" && name != "localhost" {
            return false;
        }
        let origin = header_text(request.headers(), header::ORIGIN).trim();
        let local = format!("http://{host}");
        if request.method() == Method::POST {
            let media_type = header_text(request.headers(), header::CONTENT_TYPE)
                .split(';')
                .next()
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            return media_type == "application/json" && same_origin(origin, &local);
        }
        origin.is_empty() || same_origin(origin, &local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryAssets;
    use axum::body::Body;
    use axum::http::StatusCode;
    use http::{admit_launch, await_launch, handle_launch, reason};

    fn assets() -> MemoryAssets {
        MemoryAssets::new().with("index.html", b"<!doctype html>".as_slice())
    }

    fn start() -> Server {
        Server::start(&CancellationToken::new(), assets(), Options::default()).unwrap()
    }

    #[tokio::test]
    async fn launcher_retains_accepted_selection_after_request_cancellation() {
        // Cancellation may race the handoff, but cannot discard an accepted launch.
        for _ in 0..32 {
            let server = start();
            let inner = server.inner.clone();
            let request = Request::post("/api/client-launcher/launch")
                .body(Body::from(r#"{"mode":"link","language":"en"}"#))
                .unwrap();
            // Poll once, then drop the request as a departing page does.
            let launch = handle_launch(&inner, request);
            assert!(
                tokio::time::timeout(Duration::ZERO, launch).await.is_err(),
                "launch completed without a result"
            );
            let selection = server
                .try_selection()
                .expect("cancelled request left no selection");
            assert_eq!(
                (selection.mode, selection.language.as_str()),
                (Mode::Link, "en")
            );
            assert!(!server.begin(), "cancelled request allowed a second launch");
        }
    }

    #[tokio::test]
    async fn failed_attempt_can_retry_without_replacing_an_older_request_result() {
        let server = start();
        let inner = server.inner.clone();
        let first = admit_launch(&inner, br#"{"mode":"link","language":"en"}"#).unwrap();
        assert_eq!(server.try_selection().unwrap().mode, Mode::Link);
        assert!(!server.begin(), "pending room allowed another start");
        server.set_result(Err("first attempt failed".into()));

        let second = admit_launch(&inner, br#"{"mode":"local","language":"en"}"#).unwrap();
        assert_eq!(
            server
                .try_selection()
                .expect("failure did not permit retry")
                .mode,
            Mode::Local
        );
        server.set_result(Ok("http://localhost:8787/#piik-client=1".to_owned()));
        assert!(!server.begin(), "successful room allowed replacement");
        // The old waiter resumes only after the retry has already succeeded.
        let first = await_launch(&inner, &first).await;
        assert_eq!(first.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(first.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("first attempt failed"));
        let second = await_launch(&inner, &second).await;
        assert_eq!(second.status(), StatusCode::OK);
        let body = axum::body::to_bytes(second.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(String::from_utf8_lossy(&body).contains("http://localhost:8787/"));
    }

    #[tokio::test]
    async fn launcher_keeps_the_original_failure_cause() {
        #[derive(Debug, thiserror::Error)]
        #[error("public tunnel failed")]
        struct Cause;
        let server = start();
        server.begin();
        server.set_result(Err(Box::new(piik_failure::Failure::new(
            piik_failure::Reason::TunnelExited,
            Cause,
        ))));
        let state = server.inner.lock();
        let error = state.attempt.as_ref().unwrap().result().error.unwrap();
        let source = error.source().expect("launcher lost original cause");
        assert!(source.downcast_ref::<Cause>().is_some());
        assert_eq!(reason(Some(&error)), "tunnel-exited");
    }

    #[test]
    fn invalid_targets_are_rejected() {
        assert!(valid_target("http://localhost:8787/#piik-client=1"));
        assert!(valid_target("https://share.example/"));
        for target in ["", "localhost:8787", "file:///C:/x", "http://"] {
            assert!(!valid_target(target), "{target}");
        }
    }

    #[test]
    fn origins_must_be_exact() {
        assert!(same_origin("http://127.0.0.1:1", "http://127.0.0.1:1"));
        assert!(same_origin("HTTP://LOCALHOST:1", "http://localhost:1"));
        for origin in [
            "",
            "null",
            "http://127.0.0.1:1/",
            "https://127.0.0.1:1",
            "http://127.0.0.1:2",
        ] {
            assert!(!same_origin(origin, "http://127.0.0.1:1"), "{origin}");
        }
    }
}
