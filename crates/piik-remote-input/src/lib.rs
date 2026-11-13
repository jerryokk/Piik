//! Source-scoped attended desktop input grants for the Windows Host App.
//!
//! Only the owning Host control session chooses its authorization policy.
//! Media routing stays outside this crate; closing the exact source or
//! connection releases input. Pure grant, mapping and cleanup logic is kept
//! apart from the Windows calls in `platform`, which sit behind internal
//! traits so tests never inject real input.

mod api;
mod cipher;
mod diagnostics;
mod event;
mod gamepad;
mod held;
mod keyboard;
mod manager;
mod packets;
mod policy;
mod shortcut;
mod text;

mod platform;

#[cfg(test)]
mod tests;

pub use api::{Manager, Session};
pub use diagnostics::{DiagnosticCounters, DiagnosticFamilyCounters, DiagnosticSnapshot};
pub use manager::{State, View};
pub use policy::PolicyChange;
pub use shortcut::{DEFAULT_STOP_KEY, StopShortcut, valid_stop_key};

/// The input data channel label shared with the media route.
pub const CHANNEL_LABEL: &str = piik_input_route::LABEL;

/// The `platform` value in Host replies and state; the Host is Windows only.
pub const PLATFORM: &str = "windows";

/// Remote input failures. Messages are fixed facts and never contain input
/// values, credentials or identities.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("remote input is unavailable")]
    Unavailable,
    #[error("remote input is closed")]
    Closed,
    #[error("invalid input endpoint")]
    InvalidEndpoint,
    #[error("request is no longer available")]
    RequestUnavailable,
    #[error("remote input is busy")]
    Busy,
    #[error("invalid remote input policy")]
    InvalidPolicy,
    /// The Host intent that prepared this policy change was retired.
    #[error("remote input policy change was canceled")]
    Canceled,
    #[error("remote-control shortcut is unavailable")]
    ShortcutUnavailable,
    #[error("remote-control shortcut is invalid or unavailable")]
    ShortcutInvalid,
    #[error("remote-control shortcut is already in use")]
    ShortcutInUse,
    /// Windows accepted fewer input events than requested.
    #[error("{0}")]
    Rejected(InputRejection),
    /// A platform input, target or device failure.
    #[error("{0}")]
    Input(String),
}

impl Error {
    pub(crate) fn input(message: impl Into<String>) -> Self {
        Self::Input(message.into())
    }

    /// Prefixes a context as `context: error`.
    pub(crate) fn context(self, context: &str) -> Self {
        Self::Input(format!("{context}: {self}"))
    }
}

/// Joins an accumulated failure with another, one failure per line.
pub(crate) fn join(result: Option<Error>, next: Error) -> Option<Error> {
    Some(match result {
        None => next,
        Some(previous) => Error::Input(format!("{previous}\n{next}")),
    })
}

/// Permission boundaries observed only after a rejected `SendInput`. These
/// facts do not identify UIPI as the cause and contain no window, process or
/// desktop names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputRejection {
    pub accepted: u32,
    pub requested: u32,
    /// The thread's last system error code; 0 when Windows reported none.
    pub code: u32,
    pub desktop_accessible: bool,
    /// `"true"`, `"false"` or `"unknown"`.
    pub desktop_matches_thread: &'static str,
    pub foreground_present: bool,
    /// `"true"`, `"false"` or `"unknown"`.
    pub higher_integrity: &'static str,
    /// Failure restoring the thread desktop after a recovery attempt.
    pub restore: Option<String>,
}

impl std::fmt::Display for InputRejection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Windows accepted {} of {} input events (system code {}; input desktop accessible={}; input desktop matches thread={}; foreground present={}; foreground higher integrity={})",
            self.accepted,
            self.requested,
            self.code,
            self.desktop_accessible,
            self.desktop_matches_thread,
            self.foreground_present,
            self.higher_integrity
        )?;
        if let Some(restore) = &self.restore {
            write!(formatter, "\nrestore input thread desktop: {restore}")?;
        }
        Ok(())
    }
}
