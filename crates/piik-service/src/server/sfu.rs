//! Embedded SFU binding, listener failure and public-address refresh.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::media::{EmbeddedSfu, MediaEvents, SfuPublicAddress, resolve_sfu_public_ip};
use crate::server::{Error, Inner, SFU_DNS_REFRESH, lock};

impl Inner {
    /// Binds the embedded SFU's UDP listener unless media was injected. Runs
    /// before room authority is restored, so an occupied port never touches
    /// the room database.
    pub(super) fn bind_sfu(&self) -> Result<(), Error> {
        let (Some(config), Some(address)) = (&self.config.sfu, &self.sfu_address) else {
            return Ok(());
        };
        let mut state = lock(&self.state);
        let Some(pending) = state.pending.as_mut() else {
            return Ok(());
        };
        if pending.sfu.is_some() {
            return Ok(());
        }
        let sfu = EmbeddedSfu::bind(config, &address.get())
            .map_err(|error| Error::Sfu(format!("Piik SFU listener failed: {error}")))?;
        pending.sfu = Some(sfu.runtime(&MediaEvents::new(), address));
        state.sfu = Some(Arc::new(sfu));
        Ok(())
    }

    pub(super) fn start_sfu_dns_refresh(self: &Arc<Self>) {
        let (Some(sfu), Some(address)) = (&self.config.sfu, &self.sfu_address) else {
            return;
        };
        let host = sfu.public_ip.clone();
        if host.is_empty() || host.parse::<IpAddr>().is_ok() {
            return;
        }
        let address = address.clone();
        let stop = self.stop.clone();
        let mut state = lock(&self.state);
        let media = state.sfu.clone();
        let task = tokio::spawn(async move {
            let mut ticker = tokio::time::interval_at(
                tokio::time::Instant::now() + SFU_DNS_REFRESH,
                SFU_DNS_REFRESH,
            );
            loop {
                tokio::select! {
                    () = stop.cancelled() => return,
                    _ = ticker.tick() => {
                        refresh_sfu_public_ip(&address, media.as_deref(), &host).await;
                    }
                }
            }
        });
        state.dns_refresh = Some(task);
    }

    /// Records the first permanent accept failure for its owner.
    pub(super) fn listener_failed(&self, error: io::Error) {
        let kind = format!("{:?}", error.kind());
        {
            let mut state = lock(&self.state);
            if state.closing || state.serve_failure.is_some() {
                return;
            }
            let failure = Error::Listener(Arc::new(error));
            state.serve_failure = Some(failure.clone());
            self.accepting.store(false, Ordering::Release);
            self.failure.send_replace(Some(failure));
        }
        tracing::error!(errorType = %kind, "Piik HTTP server stopped unexpectedly");
    }
}

pub(crate) async fn refresh_sfu_public_ip(
    address: &SfuPublicAddress,
    sfu: Option<&EmbeddedSfu>,
    host: &str,
) {
    match resolve_sfu_public_ip(host).await {
        Ok(resolved) => {
            if let Some(sfu) = sfu {
                sfu.set_public_ip(&resolved);
            }
            address.set(resolved);
        }
        Err(error) => tracing::debug!(error = %error, "sfu-dns-refresh-failed"),
    }
}
