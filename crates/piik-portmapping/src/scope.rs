//! Cancellation and deadline shared by one gateway operation.

use std::future::Future;
use std::time::Duration;

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::Error;

#[derive(Clone)]
pub(crate) struct Scope {
    cancel: CancellationToken,
    deadline: Option<Instant>,
}

impl Scope {
    pub(crate) fn new(cancel: CancellationToken) -> Self {
        Self {
            cancel,
            deadline: None,
        }
    }

    /// Narrows the deadline; it never extends an earlier one.
    pub(crate) fn with_timeout(&self, timeout: Duration) -> Self {
        let deadline = Instant::now() + timeout;
        Self {
            cancel: self.cancel.clone(),
            deadline: Some(
                self.deadline
                    .map_or(deadline, |current| current.min(deadline)),
            ),
        }
    }

    /// A scope that ignores this one's cancellation, for cleanup that must
    /// outlive the caller's deadline.
    pub(crate) fn detached(timeout: Duration) -> Self {
        Self::new(CancellationToken::new()).with_timeout(timeout)
    }

    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    pub(crate) fn remaining(&self) -> Option<Duration> {
        self.deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()))
    }

    pub(crate) fn error(&self) -> Option<Error> {
        if self.cancel.is_cancelled() {
            Some(Error::Canceled)
        } else if self
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            Some(Error::DeadlineExceeded)
        } else {
            None
        }
    }

    pub(crate) fn is_done(&self) -> bool {
        self.error().is_some()
    }

    /// Completes with the reason once the scope is canceled or expires.
    pub(crate) async fn ended(&self) -> Error {
        match self.deadline {
            Some(deadline) => tokio::select! {
                _ = self.cancel.cancelled() => Error::Canceled,
                _ = tokio::time::sleep_until(deadline) => Error::DeadlineExceeded,
            },
            None => {
                self.cancel.cancelled().await;
                Error::Canceled
            }
        }
    }

    /// Runs `operation` until it completes or the scope ends.
    pub(crate) async fn run<T>(
        &self,
        operation: impl Future<Output = Result<T, Error>>,
    ) -> Result<T, Error> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        tokio::select! {
            result = operation => result,
            error = self.ended() => Err(error),
        }
    }
}
