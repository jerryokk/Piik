use piik_media_transport::CandidateError;
use piik_media_transport::sdp::SdpError;

/// A native media edge failure.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A contract violation or bounded failure with a fixed message.
    #[error("{0}")]
    Invalid(&'static str),
    #[error("native media engine is closed")]
    EngineClosed,
    #[error("native media source capacity is exhausted")]
    SourceCapacity,
    /// The owning source or edge has retired.
    #[error("native media owner is closed")]
    Closed,
    #[error(transparent)]
    Session(#[from] piik_media_session::Error),
    #[error(transparent)]
    Forwarding(#[from] piik_media_forwarding::SourceError),
    #[error(transparent)]
    Rtc(#[from] piik_media_transport::str0m::RtcError),
    #[error(transparent)]
    Sdp(#[from] SdpError),
    #[error(transparent)]
    Candidate(#[from] CandidateError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Capture(#[from] piik_native_capture::Error),
    #[error(transparent)]
    Audio(#[from] piik_native_audio::Error),
}

impl Error {
    /// Whether capture PTS went backwards within one generation, which ends
    /// the capture input.
    pub fn is_invalid_timestamp(&self) -> bool {
        matches!(
            self,
            Self::Session(piik_media_session::Error::Encoded(
                piik_encoded::Error::InvalidTimestamp
            ))
        )
    }
}

pub type Result<T> = std::result::Result<T, Error>;
