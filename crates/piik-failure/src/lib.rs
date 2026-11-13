//! Identifies user-facing App failures without replacing their causes.
//!
//! A [`Failure`] pairs one [`Reason`] with the technical cause. Its text is the
//! reason followed by the cause on the next line, and [`of`] finds the first
//! reason anywhere in an error's source chain.

use std::error::Error;
use std::fmt;

/// Stable reason codes shown by the launcher and App problem views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Reason {
    TunnelExited,
    TunnelTimeout,
    TunnelUnavailable,
    TunnelStart,
    TunnelStopped,
    LocalPort,
    LocalNetwork,
    Configuration,
    NativeControl,
    Launcher,
    Diagnostics,
    LocalServer,
}

impl Reason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TunnelExited => "tunnel-exited",
            Self::TunnelTimeout => "tunnel-timeout",
            Self::TunnelUnavailable => "tunnel-unavailable",
            Self::TunnelStart => "tunnel-start",
            Self::TunnelStopped => "tunnel-stopped",
            Self::LocalPort => "local-port",
            Self::LocalNetwork => "local-network",
            Self::Configuration => "configuration",
            Self::NativeControl => "native-control",
            Self::Launcher => "launcher",
            Self::Diagnostics => "diagnostics",
            Self::LocalServer => "local-server",
        }
    }
}

impl fmt::Display for Reason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

type Cause = Box<dyn Error + Send + Sync + 'static>;

/// One reason with the cause it classifies.
#[derive(Debug)]
pub struct Failure {
    reason: Reason,
    cause: Cause,
}

impl Failure {
    pub fn new(reason: Reason, cause: impl Into<Cause>) -> Self {
        Self {
            reason,
            cause: cause.into(),
        }
    }

    pub fn reason(&self) -> Reason {
        self.reason
    }

    pub fn cause(&self) -> &(dyn Error + Send + Sync + 'static) {
        self.cause.as_ref()
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}\n{}", self.reason, self.cause)
    }
}

impl Error for Failure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(self.cause.as_ref())
    }
}

/// Returns the outermost reason in `error`'s source chain, if any.
pub fn of(error: &(dyn Error + 'static)) -> Option<Reason> {
    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(failure) = error.downcast_ref::<Failure>() {
            return Some(failure.reason);
        }
        current = error.source();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Wrapper(Failure);

    impl fmt::Display for Wrapper {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            write!(formatter, "wrapped: {}", self.0)
        }
    }

    impl Error for Wrapper {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            Some(&self.0)
        }
    }

    #[test]
    fn reason_survives_wrapping_and_keeps_its_cause() {
        let failure = Failure::new(Reason::TunnelStart, "child could not start");
        assert_eq!(failure.to_string(), "tunnel-start\nchild could not start");
        assert_eq!(failure.cause().to_string(), "child could not start");
        let wrapped = Wrapper(failure);
        assert_eq!(of(&wrapped), Some(Reason::TunnelStart));
        let plain = std::io::Error::other("plain");
        assert_eq!(of(&plain), None);
    }
}
