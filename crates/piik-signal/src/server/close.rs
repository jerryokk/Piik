//! Service shutdown: close every session, then drain the router.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use piik_room::ErrorCode as RoomErrorCode;
use url::Url;

use crate::runtime::{lock, spawn_blocking};
use crate::server::{Core, Error, SERVICE_RESTART_CLOSE_GRACE, Server, locked_with_io};
use crate::session::CLOSE_SERVICE_RESTART;

impl Server {
    pub async fn close(&self, timeout: Duration) -> Result<(), Error> {
        let deadline = Instant::now() + timeout;
        let grace = Instant::now() + SERVICE_RESTART_CLOSE_GRACE;
        let (readers, drains) = locked_with_io(&self.core, |core| {
            core.closing = true;
            let rooms: Vec<String> = core.voices.keys().cloned().collect();
            for room_id in rooms {
                core.close_room_voice(&room_id);
            }
            if let Some(stop) = core.heartbeat_stop.take() {
                stop();
            }
            for (_, timer) in core.viewer_grace_timers.drain() {
                if let Some(stop) = timer.stop {
                    stop();
                }
            }
            let mut readers = Vec::new();
            for key in core.sessions.keys() {
                let session = core.sessions.get_mut(&key).expect("session");
                session.clear_authentication_timer();
                session.close(CLOSE_SERVICE_RESTART, "Service restart");
                readers.push((key, session.reader_done.clone()));
            }
            // Keep the lock through the router's synchronous closing prefix so a
            // timer callback cannot start a new media operation during shutdown.
            (readers, core.begin_router_close())
        });

        let core = self.core.clone();
        let errors = run_blocking(move || finish_router_close(&core, drains, deadline)).await;

        // Once the grace expired every session still in its handshake survives.
        let mut survivors = Vec::new();
        let mut expired = false;
        for (key, done) in readers {
            if !expired {
                let limit = grace.min(deadline);
                if tokio::time::timeout_at(limit.into(), done.cancelled())
                    .await
                    .is_ok()
                {
                    continue;
                }
                expired = true;
            }
            if !done.is_cancelled() {
                survivors.push((key, done));
            }
        }
        if !survivors.is_empty() {
            {
                let mut core = lock(&self.core);
                for (key, _) in &survivors {
                    if let Some(session) = core.sessions.get_mut(key) {
                        session.terminate();
                    }
                }
            }
            for (_, done) in survivors {
                let _ = tokio::time::timeout_at(deadline.into(), done.cancelled()).await;
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(Error(format!(
                "SFU drain failed during shutdown: {}",
                errors.join("\n")
            )))
        }
    }
}

/// Awaits every registered drain unlocked, rerunning the ones still
/// registered afterwards; returns each failure.
pub(crate) fn finish_router_close(
    core: &Mutex<Core>,
    drains: Vec<RouterDrain>,
    deadline: Instant,
) -> Vec<String> {
    let wait = |operation: Arc<crate::runtime::Completion>| {
        operation
            .wait_until(deadline)
            .unwrap_or_else(|| Err("context deadline exceeded".to_owned()))
    };
    let mut errors = Vec::new();
    for (key, task_id, operation) in drains {
        if let Some(operation) = operation
            && let Err(error) = wait(operation)
        {
            errors.push(error);
            continue;
        }
        let rerun = locked_with_io(core, |core| core.rerun_registered_drain(&key, task_id));
        if let Some(operation) = rerun
            && let Err(error) = wait(operation)
        {
            errors.push(error);
        }
    }
    errors
}

/// One registered drain captured by `begin_router_close`.
pub(crate) type RouterDrain = (
    piik_sfu::runtime::SubscriptionFence,
    u64,
    Option<Arc<crate::runtime::Completion>>,
);

/// Runs blocking work on the Tokio blocking pool.
pub(crate) async fn run_blocking<R: Send + 'static>(job: impl FnOnce() -> R + Send + 'static) -> R {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    spawn_blocking(move || {
        let _ = sender.send(job());
    });
    receiver.await.expect("blocking job completed")
}

/// Adds an optional Viewer grant only in the URL fragment, keeping it out of
/// HTTP requests. Existing base URL query/fragment values are discarded.
pub fn invite_url(public_base_url: &Url, room_id: &str, viewer_grant: &str) -> String {
    let mut invite = public_base_url.clone();
    invite.set_path(&format!("/r/{room_id}"));
    invite.set_query(None);
    invite.set_fragment(
        (!viewer_grant.is_empty())
            .then(|| format!("v={viewer_grant}"))
            .as_deref(),
    );
    invite.to_string()
}

pub(crate) fn room_limit(error: &piik_room::Error) -> bool {
    error.code() == Some(RoomErrorCode::RoomLimit)
}
