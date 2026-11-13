//! Tunnel errors keep every joined cause: the text is each part on its own
//! line, and predicates search all parts.

use std::fmt;
use std::io;
use std::sync::Arc;

use piik_failure::Reason;

const STARTUP_EXITED: &str =
    "public invitation service exited before connecting; reopen Piik to try again";

/// How the child process ended when its owner had not asked it to stop.
#[derive(Debug, Clone)]
pub(crate) struct ExitFailure {
    pub code: Option<i32>,
    /// Exit status text, followed by the last dependency detail if any.
    pub text: String,
}

#[derive(Debug, Clone)]
enum Part {
    Reason(Reason),
    Message(String),
    Io {
        context: &'static str,
        error: Arc<io::Error>,
    },
    Canceled,
    DeadlineExceeded,
    StartupExited,
    Exit(ExitFailure),
}

#[derive(Debug, Clone)]
pub struct Error {
    parts: Vec<Part>,
}

impl Error {
    fn of(part: Part) -> Self {
        Self { parts: vec![part] }
    }

    pub(crate) fn message(text: impl Into<String>) -> Self {
        Self::of(Part::Message(text.into()))
    }

    pub(crate) fn io(context: &'static str, error: io::Error) -> Self {
        Self::of(Part::Io {
            context,
            error: Arc::new(error),
        })
    }

    pub(crate) fn canceled() -> Self {
        Self::of(Part::Canceled)
    }

    pub(crate) fn deadline_exceeded() -> Self {
        Self::of(Part::DeadlineExceeded)
    }

    pub(crate) fn exited_before_connecting() -> Self {
        Self::of(Part::StartupExited)
    }

    pub(crate) fn exit(failure: ExitFailure) -> Self {
        Self::of(Part::Exit(failure))
    }

    pub(crate) fn with_reason(mut self, reason: Reason) -> Self {
        self.parts.insert(0, Part::Reason(reason));
        self
    }

    pub(crate) fn join(mut self, other: Self) -> Self {
        self.parts.extend(other.parts);
        self
    }

    pub(crate) fn line(mut self, text: String) -> Self {
        self.parts.push(Part::Message(text));
        self
    }

    /// Only a startup deadline or cancellation, with nothing joined.
    pub(crate) fn is_bare(&self) -> bool {
        matches!(
            self.parts.as_slice(),
            [Part::Canceled | Part::DeadlineExceeded]
        )
    }

    /// The outermost user-facing reason.
    pub fn reason(&self) -> Option<Reason> {
        self.parts.iter().find_map(|part| match part {
            Part::Reason(reason) => Some(*reason),
            _ => None,
        })
    }

    /// The caller's own clean cancellation, which is a normal stopped outcome.
    pub fn is_canceled(&self) -> bool {
        matches!(self.parts.as_slice(), [Part::Canceled])
    }

    pub fn is_deadline_exceeded(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, Part::DeadlineExceeded))
    }

    /// A pre-ready child exited; startup retries only this case.
    pub fn startup_exited(&self) -> bool {
        self.parts
            .iter()
            .any(|part| matches!(part, Part::StartupExited))
    }

    /// The tunnel child's exit code, when it exited on its own.
    pub fn exit_code(&self) -> Option<i32> {
        self.parts.iter().find_map(|part| match part {
            Part::Exit(failure) => failure.code,
            _ => None,
        })
    }

    pub fn io_error(&self) -> Option<&io::Error> {
        self.parts.iter().find_map(|part| match part {
            Part::Io { error, .. } => Some(error.as_ref()),
            _ => None,
        })
    }
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, part) in self.parts.iter().enumerate() {
            if index > 0 {
                formatter.write_str("\n")?;
            }
            match part {
                Part::Reason(reason) => formatter.write_str(reason.as_str())?,
                Part::Message(text) => formatter.write_str(text)?,
                Part::Io { context, error } => write!(formatter, "{context}: {error}")?,
                Part::Canceled => formatter.write_str("context canceled")?,
                Part::DeadlineExceeded => formatter.write_str("context deadline exceeded")?,
                Part::StartupExited => formatter.write_str(STARTUP_EXITED)?,
                Part::Exit(failure) => formatter.write_str(&failure.text)?,
            }
        }
        Ok(())
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.parts.iter().find_map(|part| match part {
            Part::Io { error, .. } => Some(error.as_ref() as &(dyn std::error::Error + 'static)),
            _ => None,
        })
    }
}
