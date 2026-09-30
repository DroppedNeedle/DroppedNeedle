//! Contribution errors, mirroring v2's `core.exceptions` contribution family.

use thiserror::Error;

/// Service/worker failures. Messages match v2's call-site strings so the UI
/// copy ports unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContribError {
    #[error("Library contribution not found.")]
    ContributionNotFound,
    #[error("{0}")]
    Missing(String),
    #[error("Library album not found.")]
    AlbumNotFound,
    #[error("{0}")]
    State(String),
    #[error("{0}")]
    Validation(String),
    #[error("{0}")]
    ProviderExpired(String),
    #[error("{0}")]
    DuplicateCheckRequired(String),
    #[error("{0}")]
    ExactDuplicate(String),
    #[error("{0}")]
    ResultMismatch(String),
    #[error("{0}")]
    Data(String),
    #[error("MusicBrainz is temporarily unavailable.")]
    ProviderUnavailable,
    #[error("The provider payload could not be mapped.")]
    ProviderUnmappable,
}

impl ContribError {
    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }
}
