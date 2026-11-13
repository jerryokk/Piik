//! Locking, deferred work and timer primitives shared by the server and the
//! router.
//!
//! One authority mutex guards all signaling state. Work that must run outside
//! it (media I/O, physical close, password derivation) runs on a blocking
//! thread and re-enters the authority through a [`Locker`], after which the
//! continuation revalidates identities.

use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::server::{Core, locked_with_io};

/// Re-enters the authority lock from deferred work while the server is
/// alive. It blocks on the lock, so only threads that do not hold it may
/// call it; I/O the closure queues runs after the lock is released.
#[derive(Clone)]
pub(crate) struct Locker(Weak<Mutex<Core>>);

impl Locker {
    pub(crate) fn from_weak(core: Weak<Mutex<Core>>) -> Self {
        Self(core)
    }

    /// Locks and runs `f`; `None` once the server is gone.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut Core) -> R) -> Option<R> {
        let core = self.0.upgrade()?;
        Some(locked_with_io(&core, f))
    }
}

/// Locks a std mutex, ignoring poisoning: every invariant the lock protects is
/// re-established by the code that holds it.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Prefers the Tokio blocking pool and falls back to a thread outside a runtime.
pub(crate) fn spawn_blocking(job: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) => {
            runtime.spawn_blocking(job);
        }
        Err(_) => {
            std::thread::spawn(job);
        }
    }
}

/// A settable-once result that blocking and async waiters can observe.
#[derive(Default)]
pub(crate) struct Completion {
    state: Mutex<Option<Result<(), String>>>,
    ready: Condvar,
}

impl Completion {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub(crate) fn complete(&self, result: Result<(), String>) {
        *lock(&self.state) = Some(result);
        self.ready.notify_all();
    }

    pub(crate) fn wait(&self) -> Result<(), String> {
        let mut state = lock(&self.state);
        loop {
            if let Some(result) = state.as_ref() {
                return result.clone();
            }
            state = self
                .ready
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// `None` when the deadline passed first.
    pub(crate) fn wait_until(&self, deadline: Instant) -> Option<Result<(), String>> {
        let mut state = lock(&self.state);
        loop {
            if let Some(result) = state.as_ref() {
                return Some(result.clone());
            }
            let remaining = deadline.checked_duration_since(Instant::now())?;
            state = self
                .ready
                .wait_timeout(state, remaining)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Unix milliseconds.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// Stops a pending timer; `true` when it had not fired yet.
pub type TimerStop = Box<dyn FnOnce() -> bool + Send>;

/// The timer factory: runs the callback once after the duration and returns
/// its stop function.
pub type AfterFunc = Arc<dyn Fn(Duration, Box<dyn FnOnce() + Send>) -> TimerStop + Send + Sync>;

pub(crate) fn wall_clock() -> Clock {
    Arc::new(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
            })
    })
}

/// Tokio timers when a runtime is present, a sleeping thread otherwise.
pub(crate) fn default_after_func() -> AfterFunc {
    Arc::new(|duration, callback| {
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            let task = runtime.spawn(async move {
                tokio::time::sleep(duration).await;
                spawn_blocking(callback);
            });
            return Box::new(move || {
                let pending = !task.is_finished();
                task.abort();
                pending
            });
        }
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = cancelled.clone();
        std::thread::spawn(move || {
            std::thread::sleep(duration);
            if !flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
                callback();
            }
        });
        Box::new(move || !cancelled.swap(true, std::sync::atomic::Ordering::SeqCst))
    })
}

/// Encodes 16 random bytes as base64url without padding.
pub(crate) fn opaque_id() -> String {
    use base64::Engine;
    let mut value = [0u8; 16];
    rand::fill(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}
