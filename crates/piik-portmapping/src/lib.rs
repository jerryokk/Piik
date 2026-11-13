//! Best-effort lifetime of one UDP gateway mapping for an ICE socket.
//!
//! ICE remains fully functional when no mapping service is available: a
//! [`Mapping`] only adds an advertised external port when a gateway grants one.
//! Discovery prefers PCP for IPv4, independently adds PCPv6 pinholes, and
//! otherwise uses the first UPnP IGD or NAT-PMP gateway that answers.

mod discovery;
mod natpmp;
mod pcp;
mod route;
mod scope;
mod upnp;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Mutex, oneshot};
use tokio_util::sync::CancellationToken;

pub(crate) use scope::Scope;

/// Gateway discovery is link-local and must finish inside the existing
/// five-second direct-connection head start.
const ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);
const DELETE_TIMEOUT: Duration = Duration::from_secs(2);
/// RFC 6886 recommends requesting a two-hour lease. A new gathering
/// revalidates it: gateway state can disappear before the requested expiry.
const LEASE_DURATION: Duration = Duration::from_secs(2 * 60 * 60);
const DESCRIPTION: &str = "Piik";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("operation canceled")]
    Canceled,
    #[error("deadline exceeded")]
    DeadlineExceeded,
    #[error("no NAT found")]
    NoNatFound,
    #[error("no NAT found: deadline exceeded")]
    NoNatFoundInTime,
    #[error("invalid internal port: {0}")]
    InvalidPort(u16),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Gateway(Box<dyn std::error::Error + Send + Sync>),
}

impl Error {
    pub(crate) fn gateway(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Gateway(Box::new(error))
    }
}

pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One mapping service. Implementations observe `scope` for cancellation and
/// deadline; the [`Mapping`] still bounds callers if one does not.
pub(crate) trait Gateway: Send + Sync {
    /// Maps the local UDP port and returns the external port.
    fn add_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        description: &'a str,
        lease: Duration,
    ) -> BoxFuture<'a, Result<u16, Error>>;

    fn delete_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
    ) -> BoxFuture<'a, Result<(), Error>>;
}

pub(crate) type SharedGateway = Arc<dyn Gateway>;

#[derive(Default)]
struct State {
    gateway: Option<SharedGateway>,
    attempted: bool,
    mapped: bool,
    external_port: u16,
    /// Completes once the latest gateway call has settled.
    attempt_done: Option<CancellationToken>,
    closed: bool,
}

struct Inner {
    local_port: u16,
    cancel: CancellationToken,
    /// Completes once discovery has settled.
    ready: CancellationToken,
    state: Mutex<State>,
}

/// Owns the best-effort lifetime of one UDP mapping. Call [`Mapping::close`]
/// before reusing the local port: a late deletion must not remove a
/// replacement connection's mapping.
pub struct Mapping {
    inner: Arc<Inner>,
}

impl Mapping {
    /// Starts gateway discovery for `local_port`. Must be called inside a
    /// Tokio runtime.
    pub fn start(local_port: u16) -> Self {
        Self::start_with(local_port, discovery::discover_gateway)
    }

    pub(crate) fn start_with<F, Fut>(local_port: u16, discover: F) -> Self
    where
        F: FnOnce(Scope) -> Fut + Send + 'static,
        Fut: Future<Output = Result<SharedGateway, Error>> + Send + 'static,
    {
        let inner = Arc::new(Inner {
            local_port,
            cancel: CancellationToken::new(),
            ready: CancellationToken::new(),
            state: Mutex::new(State::default()),
        });
        let task = Arc::clone(&inner);
        tokio::spawn(async move {
            let _ready = task.ready.clone().drop_guard();
            let scope = Scope::new(task.cancel.clone()).with_timeout(ATTEMPT_TIMEOUT);
            let started = tokio::time::Instant::now();
            let result = discover(scope).await;
            let error = result.as_ref().err();
            tracing::debug!(
                event = "discovery",
                localPort = task.local_port,
                durationMs = started.elapsed().as_millis() as u64,
                available = error.is_none(),
                error = error.map(|error| error as &dyn std::error::Error),
                "nat-mapping"
            );
            let Ok(gateway) = result else { return };
            let mut state = task.state.lock().await;
            if state.closed || task.cancel.is_cancelled() {
                return;
            }
            state.gateway = Some(gateway);
        });
        Self { inner }
    }

    /// Confirms the mapping for one ICE gathering and returns the external
    /// port to advertise. A gathering calls it once; ordinary renegotiation
    /// does not refresh mappings. A failed attempt is not retried.
    pub async fn prepare(&self) -> Option<u16> {
        let inner = &self.inner;
        inner.ready.cancelled().await;
        let mut state = inner.state.lock().await;
        let gateway = state.gateway.clone()?;
        if state.closed || inner.cancel.is_cancelled() || (state.attempted && !state.mapped) {
            return None;
        }
        let scope = Scope::new(inner.cancel.clone()).with_timeout(ATTEMPT_TIMEOUT);
        self.map_port_locked(&mut state, gateway, scope).await;
        state.mapped.then_some(state.external_port)
    }

    async fn map_port_locked(&self, state: &mut State, gateway: SharedGateway, scope: Scope) {
        let local_port = self.inner.local_port;
        // A request can succeed on the gateway even if its reply is lost. Close
        // must attempt cleanup once the request settles, including failed renewal.
        state.attempted = true;
        // The detached call keeps the caller bounded even while a gateway
        // finishes its own rollback; Close waits before touching its bookkeeping.
        let done = CancellationToken::new();
        state.attempt_done = Some(done.clone());
        let (sender, mut receiver) = oneshot::channel();
        let call_scope = scope.clone();
        tokio::spawn(async move {
            let _done = done.drop_guard();
            let result = gateway
                .add_port_mapping(&call_scope, local_port, DESCRIPTION, LEASE_DURATION)
                .await;
            let _ = sender.send(result);
        });
        let result = tokio::select! {
            result = &mut receiver => result.unwrap_or(Err(Error::Canceled)),
            error = scope.ended() => receiver.try_recv().unwrap_or(Err(error)),
        };
        let external_port = *result.as_ref().unwrap_or(&0);
        tracing::debug!(
            event = "mapping-result",
            localPort = local_port,
            externalPort = external_port,
            leaseSeconds = LEASE_DURATION.as_secs(),
            error = result
                .as_ref()
                .err()
                .map(|error| error as &dyn std::error::Error),
            "nat-mapping"
        );
        state.mapped = external_port != 0;
        state.external_port = external_port;
    }

    /// Stops discovery and pending attempts, then releases an attempted
    /// mapping. Repeated calls return immediately.
    pub async fn close(&self) {
        let inner = &self.inner;
        inner.cancel.cancel();
        inner.ready.cancelled().await;
        let mut state = inner.state.lock().await;
        if state.closed {
            return;
        }
        state.closed = true;
        state.mapped = false;
        state.external_port = 0;
        let gateway = state.gateway.clone();
        let attempted = state.attempted;
        let attempt_done = state.attempt_done.clone();
        drop(state);
        let Some(gateway) = gateway else { return };
        if !attempted {
            return;
        }
        // A gateway call may still be finishing detached PCPv6 rollback. Allow
        // a brief settlement window before touching its mapping bookkeeping.
        if let Some(done) = attempt_done
            && tokio::time::timeout(DELETE_TIMEOUT, done.cancelled())
                .await
                .is_err()
        {
            return;
        }
        let scope = Scope::new(CancellationToken::new()).with_timeout(DELETE_TIMEOUT);
        let result = gateway.delete_port_mapping(&scope, inner.local_port).await;
        tracing::debug!(
            event = "released",
            localPort = inner.local_port,
            error = result
                .as_ref()
                .err()
                .map(|error| error as &dyn std::error::Error),
            "nat-mapping"
        );
    }
}

#[cfg(test)]
mod tests;
