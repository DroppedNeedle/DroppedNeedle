//! Contribution errors, mirroring v2's `core.exceptions` contribution family.
//!
//! Every error carries three things a curator can act on: a stable code,
//! the plain sentence in its message, and [`ContribError::action`] saying
//! what to do next. Handlers map the kind to an HTTP status; nothing here
//! knows about HTTP.

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
    /// The request does not fit the contribution's current state.
    #[error("{0}")]
    State(String),
    /// Someone (or something) changed the contribution or its album since
    /// the caller last read it.
    #[error("{0}")]
    Stale(String),
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
    /// A persisted document this build cannot read, or a provider seam that
    /// is not wired. Not the curator's fault.
    #[error("{0}")]
    Data(String),
    #[error("MusicBrainz is temporarily unavailable.")]
    ProviderUnavailable,
    #[error("Discogs is temporarily unavailable.")]
    DiscogsUnavailable,
    #[error("The provider payload could not be mapped.")]
    ProviderUnmappable,
    /// The contribution store failed. The cause goes to the log only.
    #[error("The contribution could not be saved.")]
    Storage(String),
}

impl ContribError {
    pub fn state(msg: impl Into<String>) -> Self {
        Self::State(msg.into())
    }

    pub fn stale(msg: impl Into<String>) -> Self {
        Self::Stale(msg.into())
    }

    pub fn validation(msg: impl Into<String>) -> Self {
        Self::Validation(msg.into())
    }

    /// Stable machine code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            Self::ContributionNotFound => "CONTRIBUTION_NOT_FOUND",
            Self::Missing(_) => "CONTRIBUTION_SOURCE_NOT_FOUND",
            Self::AlbumNotFound => "CONTRIBUTION_ALBUM_NOT_FOUND",
            Self::State(_) => "CONTRIBUTION_STATE_CONFLICT",
            Self::Stale(_) => "CONTRIBUTION_CHANGED",
            Self::Validation(_) => "CONTRIBUTION_INVALID_INPUT",
            Self::ProviderExpired(_) => "DISCOGS_SOURCE_EXPIRED",
            Self::DuplicateCheckRequired(_) => "DUPLICATE_CHECK_REQUIRED",
            Self::ExactDuplicate(_) => "MUSICBRAINZ_RELEASE_EXISTS",
            Self::ResultMismatch(_) => "MUSICBRAINZ_RESULT_MISMATCH",
            Self::Data(_) => "CONTRIBUTION_DATA_UNREADABLE",
            Self::ProviderUnavailable => "MUSICBRAINZ_UNAVAILABLE",
            Self::DiscogsUnavailable => "DISCOGS_UNAVAILABLE",
            Self::ProviderUnmappable => "PROVIDER_DATA_UNREADABLE",
            Self::Storage(_) => "CONTRIBUTION_STORAGE_FAILED",
        }
    }

    /// What the curator can do about it, in one plain sentence.
    pub fn action(&self) -> &'static str {
        match self {
            Self::ContributionNotFound => {
                "Open the album and start a new contribution from its page."
            }
            Self::Missing(_) => "Check the ID or link you entered and try again.",
            Self::AlbumNotFound => {
                "Rescan the library so the album has indexed tracks, then start again from the album page."
            }
            Self::State(_) => "Reload the contribution to see which step it needs next.",
            Self::Stale(_) => "Reload the contribution and try again.",
            Self::Validation(_) => "Correct the value and try again.",
            Self::ProviderExpired(_) => "Select the Discogs release again to refresh it.",
            Self::DuplicateCheckRequired(_) => {
                "Run the MusicBrainz duplicate check, then try again."
            }
            Self::ExactDuplicate(_) => {
                "Attach the existing MusicBrainz release instead of adding a new one."
            }
            Self::ResultMismatch(_) => {
                "Check the release on MusicBrainz, then run the duplicate check again."
            }
            Self::Data(_) | Self::Storage(_) => {
                "Try again. If it keeps failing, check the server log and report it."
            }
            Self::ProviderUnavailable => "Wait a minute and try again.",
            Self::DiscogsUnavailable => "Wait a minute and try again.",
            Self::ProviderUnmappable => {
                "Try again later. If it keeps failing, report it so the app can be updated."
            }
        }
    }
}
