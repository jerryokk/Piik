//! Room authority and admission errors.

/// Identifies a room authority or admission failure; its wire spelling is
/// [`ErrorCode::as_str`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    InvalidToken,
    RoomAccessDenied,
    RoomBusy,
    RoomNotFound,
    RoomFull,
    HostAlreadyConnected,
    RoomLimit,
}

impl ErrorCode {
    /// The wire spelling.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidToken => "INVALID_TOKEN",
            Self::RoomAccessDenied => "ROOM_ACCESS_DENIED",
            Self::RoomBusy => "ROOM_BUSY",
            Self::RoomNotFound => "ROOM_NOT_FOUND",
            Self::RoomFull => "ROOM_FULL",
            Self::HostAlreadyConnected => "HOST_ALREADY_CONNECTED",
            Self::RoomLimit => "ROOM_LIMIT",
        }
    }
}

impl std::fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A room operation failure. Only [`Error::Room`] is a client-visible
/// admission result; the others are internal failures.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A room authority or admission result.
    #[error("{0}")]
    Room(ErrorCode),
    /// The queued derivation lost authority before it started. Commits map it
    /// to the operation's own error after rechecking authority.
    #[error("password derivation was cancelled")]
    DerivationCancelled,
    /// A SQLite failure; the stable authority is unchanged.
    #[error(transparent)]
    Database(#[from] rusqlite::Error),
    /// An invariant, configuration or stored-data violation.
    #[error("{0}")]
    Invalid(String),
}

impl Error {
    /// Returns the admission code, if this is one.
    pub fn code(&self) -> Option<ErrorCode> {
        match self {
            Self::Room(code) => Some(*code),
            _ => None,
        }
    }

    /// Reports whether the commit boundary resolves this derivation outcome
    /// after rechecking room authority.
    pub(crate) fn is_gate_outcome(&self) -> bool {
        matches!(
            self,
            Self::DerivationCancelled | Self::Room(ErrorCode::RoomBusy)
        )
    }
}

impl From<ErrorCode> for Error {
    fn from(code: ErrorCode) -> Self {
        Self::Room(code)
    }
}

pub(crate) fn invalid(message: impl Into<String>) -> Error {
    Error::Invalid(message.into())
}
