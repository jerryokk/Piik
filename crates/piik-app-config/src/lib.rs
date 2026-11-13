//! Piik App settings: the strict `client.json`, the separate sharing-quality
//! preference beside it, and the public-link STUN policy.

#![forbid(unsafe_code)]

mod config;
mod public_stun;
mod quality_preference;

pub use config::{
    AppConfig, CURRENT_VERSION, default_path, load_or_create, normalize_site, normalize_sites, save,
};
pub use public_stun::public_stun_urls;
pub use quality_preference::{MAX_QUALITY_PREFERENCE_BYTES, QualityPreference};

use thiserror::Error;

/// An App configuration failure.
#[derive(Debug, Error)]
pub enum Error {
    /// The stored or requested settings are invalid; the text is user-facing.
    #[error("{0}")]
    Invalid(String),
    /// A file-system step failed.
    #[error("{context}: {source}")]
    Io {
        context: &'static str,
        #[source]
        source: std::io::Error,
    },
}

impl Error {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::Invalid(message.into())
    }

    pub(crate) fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Self {
        move |source| Self::Io { context, source }
    }

    /// Reports whether the failure is a missing file.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
