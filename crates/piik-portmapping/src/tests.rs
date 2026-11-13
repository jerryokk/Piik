use std::sync::Mutex as StdMutex;
use std::time::Duration;

use tokio::time::Instant;

use super::*;

#[derive(Default)]
struct FakeState {
    added: Vec<u16>,
    deleted: Vec<u16>,
    add_error: bool,
    /// Holds an add until released, ignoring the scope, to exercise the
    /// caller's bound when a gateway does not observe cancellation.
    block: Option<CancellationToken>,
    external_port: u16,
}

#[derive(Default)]
struct Fake {
    state: StdMutex<FakeState>,
    /// When set, adds wait for their scope to end and fail.
    honor_scope: Option<CancellationToken>,
}

impl Fake {
    fn with(update: impl FnOnce(&mut FakeState)) -> Arc<Self> {
        let fake = Self::default();
        update(&mut fake.state.lock().unwrap());
        Arc::new(fake)
    }

    fn update(&self, update: impl FnOnce(&mut FakeState)) {
        update(&mut self.state.lock().unwrap());
    }

    fn calls(&self) -> (Vec<u16>, Vec<u16>) {
        let state = self.state.lock().unwrap();
        (state.added.clone(), state.deleted.clone())
    }
}

impl Gateway for Fake {
    fn add_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        _description: &'a str,
        _lease: Duration,
    ) -> BoxFuture<'a, Result<u16, Error>> {
        Box::pin(async move {
            if let Some(started) = &self.honor_scope {
                started.cancel();
                return Err(scope.ended().await);
            }
            let (block, add_error, external_port) = {
                let mut state = self.state.lock().unwrap();
                state.added.push(local_port);
                (state.block.clone(), state.add_error, state.external_port)
            };
            if let Some(block) = block {
                block.cancelled().await;
            }
            if add_error {
                return Err(Error::gateway(std::io::Error::other("mapping rejected")));
            }
            Ok(if external_port != 0 {
                external_port
            } else {
                local_port + 1
            })
        })
    }

    fn delete_port_mapping<'a>(
        &'a self,
        _scope: &'a Scope,
        local_port: u16,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            self.state.lock().unwrap().deleted.push(local_port);
            Ok(())
        })
    }
}

fn start(fake: &Arc<Fake>) -> Mapping {
    let gateway: SharedGateway = Arc::clone(fake) as SharedGateway;
    Mapping::start_with(43210, move |_| async move { Ok(gateway) })
}

#[tokio::test(start_paused = true)]
async fn mapping_refreshes_for_each_gathering_and_releases_once() {
    let fake = Fake::with(|_| {});
    let mapping = start(&fake);
    assert_eq!(mapping.prepare().await, Some(43211));
    assert_eq!(
        mapping.prepare().await,
        Some(43211),
        "refreshed mapped port"
    );
    mapping.close().await;
    mapping.close().await;
    assert_eq!(fake.calls(), (vec![43210, 43210], vec![43210]));
}

#[tokio::test(start_paused = true)]
async fn mapping_absence_leaves_ice_available() {
    let mapping = Mapping::start_with(43210, |_| async { Err(Error::NoNatFound) });
    assert_eq!(mapping.prepare().await, None);
    mapping.close().await;
}

#[tokio::test(start_paused = true)]
async fn mapping_gathering_after_gateway_state_loss_uses_new_allocation() {
    let fake = Fake::with(|state| state.external_port = 43211);
    let mapping = start(&fake);
    assert_eq!(mapping.prepare().await, Some(43211));
    // A gateway reset can discard a mapping before its requested lease expires.
    // An ICE restart retains this socket and starts a fresh gathering.
    fake.update(|state| state.external_port = 43212);
    assert_eq!(
        mapping.prepare().await,
        Some(43212),
        "new gathering advertised a stale mapped port"
    );
    mapping.close().await;
}

#[tokio::test(start_paused = true)]
async fn mapping_failure_is_not_retried_within_the_connection() {
    let fake = Fake::with(|state| state.add_error = true);
    let mapping = start(&fake);
    assert_eq!(mapping.prepare().await, None);
    assert_eq!(mapping.prepare().await, None);
    mapping.close().await;
    let (added, deleted) = fake.calls();
    assert_eq!((added.len(), deleted.len()), (1, 1));
}

#[tokio::test(start_paused = true)]
async fn mapping_attempt_stays_bounded_when_the_client_ignores_cancellation() {
    let block = CancellationToken::new();
    let fake = Fake::with(|state| state.block = Some(block.clone()));
    let mapping = start(&fake);
    let started = Instant::now();
    assert_eq!(mapping.prepare().await, None, "unanswered attempt");
    assert!(
        started.elapsed() <= ATTEMPT_TIMEOUT + Duration::from_secs(2),
        "prepare blocked {:?}",
        started.elapsed()
    );
    block.cancel();
    mapping.close().await;
}

#[tokio::test(start_paused = true)]
async fn close_skips_delete_while_an_attempt_is_still_abandoned() {
    let fake = Fake::with(|_| {});
    let mapping = start(&fake);
    assert_eq!(mapping.prepare().await, Some(43211));
    let block = CancellationToken::new();
    fake.update(|state| state.block = Some(block.clone()));
    // The renewal is abandoned after the attempt bound while the fake keeps
    // ignoring cancellation. Close must stay bounded and must not race the
    // still-running call with a delete.
    assert_eq!(mapping.prepare().await, None);
    let started = Instant::now();
    mapping.close().await;
    assert!(
        started.elapsed() <= DELETE_TIMEOUT * 2 + Duration::from_secs(1),
        "close blocked {:?}",
        started.elapsed()
    );
    assert_eq!(fake.calls(), (vec![43210, 43210], vec![]));
    block.cancel();
}

#[tokio::test(start_paused = true)]
async fn renewal_failure_keeps_the_original_mapping_owned_until_close() {
    let fake = Fake::with(|_| {});
    let mapping = start(&fake);
    assert_eq!(mapping.prepare().await, Some(43211));
    fake.update(|state| state.add_error = true);
    assert_eq!(
        mapping.prepare().await,
        None,
        "failed renewal advertised a stale port"
    );
    mapping.close().await;
    assert_eq!(fake.calls(), (vec![43210, 43210], vec![43210]));
}

#[tokio::test(start_paused = true)]
async fn mapping_close_cancels_pending_creation() {
    let started = CancellationToken::new();
    let fake = Arc::new(Fake {
        honor_scope: Some(started.clone()),
        ..Fake::default()
    });
    let mapping = Arc::new(start(&fake));
    let preparing = Arc::clone(&mapping);
    let prepared = tokio::spawn(async move { preparing.prepare().await });
    started.cancelled().await;
    let close = tokio::time::timeout(Duration::from_secs(1), mapping.close()).await;
    assert!(close.is_ok(), "mapping close did not cancel creation");
    assert_eq!(
        prepared.await.unwrap(),
        None,
        "retired mapping returned a port"
    );
    assert_eq!(
        fake.calls().1.len(),
        1,
        "the canceled attempt was not released"
    );
}

#[tokio::test(start_paused = true)]
async fn mapping_discovery_stops_with_the_connection() {
    let mapping = Mapping::start_with(
        43210,
        |scope: Scope| async move { Err(scope.ended().await) },
    );
    let close = tokio::time::timeout(Duration::from_secs(1), mapping.close()).await;
    assert!(close.is_ok(), "mapping close did not cancel discovery");
}

#[tokio::test]
#[ignore = "requires a real LAN gateway with PCP, UPnP IGD or NAT-PMP"]
async fn real_gateway_maps_and_releases() {
    let socket = tokio::net::UdpSocket::bind(("0.0.0.0", 0)).await.unwrap();
    let port = socket.local_addr().unwrap().port();
    let mapping = Mapping::start(port);
    let external = mapping.prepare().await;
    mapping.close().await;
    assert!(external.is_some(), "no gateway granted a mapping");
}
