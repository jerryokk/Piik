//! Gateway selection: PCP for IPv4 is preferred, PCPv6 pinholes are added
//! independently, and otherwise the first UPnP or NAT-PMP gateway is used.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{BoxFuture, Error, Gateway, Scope, SharedGateway, natpmp, pcp, upnp};

/// A PCP server sits on the local link and answers in milliseconds; the full
/// RFC 6887 retransmission schedule would only delay the common fallback.
const PCP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Optional IPv6 mapping and cleanup finish inside the three-second mapping
/// budget, so a silent IPv6 server cannot hide a successful IPv4 result.
const PINHOLE_TIMEOUT: Duration = Duration::from_secs(2);

pub(crate) async fn discover_gateway(scope: Scope) -> Result<SharedGateway, Error> {
    let probes = CancellationToken::new();
    let _stop = probes.clone().drop_guard();
    let probe_scope = |timeout: Option<Duration>| {
        let child = Scope::new(probes.clone());
        let child = match scope.deadline() {
            Some(deadline) => {
                child.with_timeout(deadline.saturating_duration_since(tokio::time::Instant::now()))
            }
            None => child,
        };
        match timeout {
            Some(timeout) => child.with_timeout(timeout),
            None => child,
        }
    };

    let (pcp4_sender, pcp4) = oneshot::channel();
    let probe = probe_scope(Some(PCP_PROBE_TIMEOUT));
    tokio::spawn(async move {
        if let Ok(client) = pcp::discover_v4(&probe).await {
            let _ = pcp4_sender.send(Arc::new(client) as SharedGateway);
        }
    });
    let (pcp6_sender, pcp6) = oneshot::channel();
    let probe = probe_scope(Some(PCP_PROBE_TIMEOUT));
    tokio::spawn(async move {
        if let Ok(client) = pcp::discover_v6(&probe).await {
            let _ = pcp6_sender.send(Arc::new(client));
        }
    });
    let (fallback_sender, fallbacks) = mpsc::channel::<SharedGateway>(3);
    for source in [
        Fallback::UpnpMulticast,
        Fallback::UpnpUnicast,
        Fallback::NatPmp,
    ] {
        let sender = fallback_sender.clone();
        let probe = probe_scope(None);
        tokio::spawn(async move {
            if let Some(gateway) = source.discover(&probe).await {
                let _ = sender.send(gateway).await;
            }
        });
    }
    drop(fallback_sender);

    select_gateway(&scope, pcp4, pcp6, fallbacks).await
}

#[derive(Clone, Copy)]
enum Fallback {
    UpnpMulticast,
    UpnpUnicast,
    NatPmp,
}

impl Fallback {
    async fn discover(self, scope: &Scope) -> Option<SharedGateway> {
        match self {
            Self::UpnpMulticast => upnp::discover_multicast(scope)
                .await
                .ok()
                .map(|client| Arc::new(client) as SharedGateway),
            Self::UpnpUnicast => upnp::discover_unicast(scope)
                .await
                .ok()
                .map(|client| Arc::new(client) as SharedGateway),
            Self::NatPmp => natpmp::discover(scope)
                .await
                .ok()
                .map(|client| Arc::new(client) as SharedGateway),
        }
    }
}

/// Waits until both PCP probes have settled and either PCPv4, a fallback, or
/// the end of fallback discovery is known. At the deadline it keeps whatever
/// has already been published.
pub(crate) async fn select_gateway<P6>(
    scope: &Scope,
    mut pcp4: oneshot::Receiver<SharedGateway>,
    mut pcp6: oneshot::Receiver<Arc<P6>>,
    mut fallbacks: mpsc::Receiver<SharedGateway>,
) -> Result<SharedGateway, Error>
where
    P6: Pinhole + 'static,
{
    let mut pcp4_open = true;
    let mut pcp6_open = true;
    let mut fallbacks_open = true;
    let mut found_pcp4: Option<SharedGateway> = None;
    let mut found_pcp6: Option<Arc<P6>> = None;
    let mut found_fallback: Option<SharedGateway> = None;
    let mut expired = false;

    loop {
        let settled = !pcp4_open
            && !pcp6_open
            && (found_pcp4.is_some() || found_fallback.is_some() || !fallbacks_open);
        if settled {
            break;
        }
        tokio::select! {
            result = &mut pcp4, if pcp4_open => {
                pcp4_open = false;
                found_pcp4 = result.ok();
            }
            result = &mut pcp6, if pcp6_open => {
                pcp6_open = false;
                found_pcp6 = result.ok();
            }
            gateway = fallbacks.recv(), if fallbacks_open => match gateway {
                Some(gateway) => { found_fallback.get_or_insert(gateway); }
                None => fallbacks_open = false,
            },
            _ = scope.ended() => {
                expired = true;
                if pcp4_open && let Ok(gateway) = pcp4.try_recv() {
                    found_pcp4 = Some(gateway);
                }
                if pcp6_open && let Ok(client) = pcp6.try_recv() {
                    found_pcp6 = Some(client);
                }
                while fallbacks_open && found_fallback.is_none() {
                    match fallbacks.try_recv() {
                        Ok(gateway) => found_fallback = Some(gateway),
                        Err(_) => break,
                    }
                }
                break;
            }
        }
    }

    let Some(gateway) = found_pcp4.or(found_fallback) else {
        // Distinguish "this network has no NAT" from "the probes ran out of time".
        return Err(if expired {
            Error::NoNatFoundInTime
        } else {
            Error::NoNatFound
        });
    };
    Ok(match found_pcp6 {
        Some(pcp6) => Arc::new(DualStack {
            ipv4: gateway,
            pcp6,
        }),
        None => gateway,
    })
}

/// The IPv6 firewall pinhole part of a PCP server.
pub(crate) trait Pinhole: Send + Sync {
    fn add<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        lease: Duration,
    ) -> BoxFuture<'a, Result<(), Error>>;
    fn delete<'a>(&'a self, scope: &'a Scope, local_port: u16) -> BoxFuture<'a, Result<(), Error>>;
}

impl Pinhole for pcp::Client {
    fn add<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        lease: Duration,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            pcp::Client::add(self, scope, local_port, lease)
                .await
                .map(|_| ())
        })
    }

    fn delete<'a>(&'a self, scope: &'a Scope, local_port: u16) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(pcp::Client::delete(self, scope, local_port))
    }
}

/// Adds best-effort PCPv6 pinholes to an IPv4 gateway. A peer reaching this
/// host may have no IPv6 at all, so a pinhole failure never fails the call.
struct DualStack<P6> {
    ipv4: SharedGateway,
    pcp6: Arc<P6>,
}

impl<P6: Pinhole + 'static> Gateway for DualStack<P6> {
    fn add_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
        description: &'a str,
        lease: Duration,
    ) -> BoxFuture<'a, Result<u16, Error>> {
        Box::pin(async move {
            let pinhole_scope = scope.with_timeout(PINHOLE_TIMEOUT);
            let (port, pinhole) = tokio::join!(
                self.ipv4
                    .add_port_mapping(scope, local_port, description, lease),
                self.pcp6.add(&pinhole_scope, local_port, lease),
            );
            let pinhole = match (&port, pinhole) {
                // The caller sees a failed mapping and will never delete it, so
                // close the pinhole; detached because the IPv4 failure may be
                // the caller's own deadline.
                (Err(_), Ok(())) => {
                    let rollback = Scope::detached(PINHOLE_TIMEOUT);
                    match self.pcp6.delete(&rollback, local_port).await {
                        Ok(()) => Err("rolled back after the IPv4 mapping failed".to_owned()),
                        Err(error) => {
                            Err(format!("left open after the IPv4 mapping failed: {error}"))
                        }
                    }
                }
                (_, result) => result.map_err(|error| error.to_string()),
            };
            log_pinhole("pinhole-add", local_port, pinhole);
            port
        })
    }

    fn delete_port_mapping<'a>(
        &'a self,
        scope: &'a Scope,
        local_port: u16,
    ) -> BoxFuture<'a, Result<(), Error>> {
        Box::pin(async move {
            let pinhole_scope = scope.with_timeout(PINHOLE_TIMEOUT);
            let (result, pinhole) = tokio::join!(
                self.ipv4.delete_port_mapping(scope, local_port),
                self.pcp6.delete(&pinhole_scope, local_port),
            );
            log_pinhole(
                "pinhole-delete",
                local_port,
                pinhole.map_err(|error| error.to_string()),
            );
            result
        })
    }
}

fn log_pinhole(event: &'static str, local_port: u16, result: Result<(), String>) {
    if let Err(cause) = result {
        tracing::debug!(event, localPort = local_port, cause = %format!("pcp ipv6: {cause}"), "nat-mapping");
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<String>>,
        fail_ipv4: bool,
    }

    impl Recorder {
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
        fn record(&self, call: &str) {
            self.calls.lock().unwrap().push(call.to_owned());
        }
    }

    struct Ipv4(Arc<Recorder>, &'static str);

    impl Gateway for Ipv4 {
        fn add_port_mapping<'a>(
            &'a self,
            _: &'a Scope,
            port: u16,
            _: &'a str,
            _: Duration,
        ) -> BoxFuture<'a, Result<u16, Error>> {
            Box::pin(async move {
                self.0.record(&format!("{}-add", self.1));
                if self.0.fail_ipv4 {
                    Err(Error::NoNatFound)
                } else {
                    Ok(port + 1)
                }
            })
        }
        fn delete_port_mapping<'a>(
            &'a self,
            _: &'a Scope,
            _: u16,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async move {
                self.0.record(&format!("{}-delete", self.1));
                Ok(())
            })
        }
    }

    struct Pcp6(Arc<Recorder>);

    impl Pinhole for Pcp6 {
        fn add<'a>(
            &'a self,
            _: &'a Scope,
            _: u16,
            _: Duration,
        ) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async move {
                self.0.record("pcp6-add");
                Ok(())
            })
        }
        fn delete<'a>(&'a self, _: &'a Scope, _: u16) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async move {
                self.0.record("pcp6-delete");
                Ok(())
            })
        }
    }

    fn scope(seconds: u64) -> Scope {
        Scope::new(CancellationToken::new()).with_timeout(Duration::from_secs(seconds))
    }

    #[tokio::test(start_paused = true)]
    async fn pcp4_outranks_fallbacks_and_waits_for_pcp6() {
        let recorder = Arc::new(Recorder::default());
        let (pcp4_sender, pcp4) = oneshot::channel();
        let (pcp6_sender, pcp6) = oneshot::channel::<Arc<Pcp6>>();
        let (fallback_sender, fallbacks) = mpsc::channel(3);
        fallback_sender
            .send(Arc::new(Ipv4(Arc::clone(&recorder), "upnp")) as SharedGateway)
            .await
            .unwrap();
        let pcp_recorder = Arc::clone(&recorder);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = pcp4_sender
                .send(Arc::new(Ipv4(Arc::clone(&pcp_recorder), "pcp4")) as SharedGateway);
            tokio::time::sleep(Duration::from_millis(500)).await;
            let _ = pcp6_sender.send(Arc::new(Pcp6(pcp_recorder)));
        });
        let gateway = select_gateway(&scope(3), pcp4, pcp6, fallbacks)
            .await
            .unwrap();
        assert_eq!(
            gateway
                .add_port_mapping(&scope(3), 40000, "Piik", Duration::from_secs(1))
                .await
                .unwrap(),
            40001
        );
        let mut calls = recorder.calls();
        calls.sort();
        assert_eq!(calls, vec!["pcp4-add", "pcp6-add"]);
        drop(fallback_sender);
    }

    #[tokio::test(start_paused = true)]
    async fn fallback_is_used_once_pcp_probes_fail() {
        let recorder = Arc::new(Recorder::default());
        let (pcp4_sender, pcp4) = oneshot::channel::<SharedGateway>();
        let (pcp6_sender, pcp6) = oneshot::channel::<Arc<Pcp6>>();
        let (fallback_sender, fallbacks) = mpsc::channel(3);
        drop((pcp4_sender, pcp6_sender));
        fallback_sender
            .send(Arc::new(Ipv4(Arc::clone(&recorder), "natpmp")) as SharedGateway)
            .await
            .unwrap();
        let gateway = select_gateway(&scope(3), pcp4, pcp6, fallbacks)
            .await
            .unwrap();
        gateway.delete_port_mapping(&scope(3), 40000).await.unwrap();
        assert_eq!(recorder.calls(), vec!["natpmp-delete"]);
    }

    #[tokio::test(start_paused = true)]
    async fn no_gateway_reports_whether_the_deadline_passed() {
        let (pcp4_sender, pcp4) = oneshot::channel::<SharedGateway>();
        let (pcp6_sender, pcp6) = oneshot::channel::<Arc<Pcp6>>();
        let (fallback_sender, fallbacks) = mpsc::channel::<SharedGateway>(3);
        drop((pcp4_sender, pcp6_sender, fallback_sender));
        assert!(matches!(
            select_gateway(&scope(3), pcp4, pcp6, fallbacks).await,
            Err(Error::NoNatFound)
        ));

        let (_pcp4_sender, pcp4) = oneshot::channel::<SharedGateway>();
        let (_pcp6_sender, pcp6) = oneshot::channel::<Arc<Pcp6>>();
        let (_fallback_sender, fallbacks) = mpsc::channel::<SharedGateway>(3);
        assert!(matches!(
            select_gateway(&scope(3), pcp4, pcp6, fallbacks).await,
            Err(Error::NoNatFoundInTime)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn failed_ipv4_mapping_rolls_back_its_pinhole() {
        let recorder = Arc::new(Recorder {
            fail_ipv4: true,
            ..Recorder::default()
        });
        let gateway = DualStack {
            ipv4: Arc::new(Ipv4(Arc::clone(&recorder), "pcp4")),
            pcp6: Arc::new(Pcp6(Arc::clone(&recorder))),
        };
        assert!(
            gateway
                .add_port_mapping(&scope(3), 40000, "Piik", Duration::from_secs(1))
                .await
                .is_err()
        );
        let calls = recorder.calls();
        assert_eq!(calls.last().map(String::as_str), Some("pcp6-delete"));
        assert_eq!(calls.len(), 3);
    }
}
