//! Shared discover types: the failures the services answer with, the
//! queue trigger and deck, and radio failures.
//!
//! Every fallible service call returns a [`ProviderFailure`]: a provider's
//! typed error, or a local cause that stays in the log.

pub use futures_util::future::BoxFuture;

use droppedneedle_providers::ProviderError;

use crate::reads::discover::models::{PreviewTrackItem, QueueItemLight};

/// Why a discover service could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFailure {
    /// An outside service failed. It answers by its kind: an outage names
    /// the service and its wait.
    Provider(ProviderError),
    /// A store or another local read failed. The string is a log-only
    /// cause.
    Failed(String),
    /// The feature needs setup first (a key, an account). The string is a
    /// full sentence for the user.
    NotConfigured(String),
    /// The feature is not built in this version yet. The string is a full
    /// sentence for the user.
    NotBuilt(String),
    /// A usage limit is spent for now. The string is a full sentence for
    /// the user.
    Exhausted(String),
}

impl ProviderFailure {
    /// A local failure with a log-only cause.
    pub fn failed(cause: impl Into<String>) -> Self {
        Self::Failed(cause.into())
    }

    /// A feature that needs setup first; `sentence` tells the user.
    pub fn not_configured(sentence: impl Into<String>) -> Self {
        Self::NotConfigured(sentence.into())
    }

    /// A feature not built yet; `sentence` tells the user.
    pub fn not_built(sentence: impl Into<String>) -> Self {
        Self::NotBuilt(sentence.into())
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Provider(error) => write!(f, "{error}"),
            Self::Failed(cause) => f.write_str(cause),
            Self::NotConfigured(sentence)
            | Self::NotBuilt(sentence)
            | Self::Exhausted(sentence) => f.write_str(sentence),
        }
    }
}

/// What asked for a queue build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTrigger {
    /// A person asked (generate, or an ignore that reshuffles the deck).
    Request {
        /// Rebuild even when the current deck is fresh.
        force: bool,
    },
    /// The background warm cycle. Skipped while the warm-cycle setting is
    /// off, and replaced by any request that arrives while it runs.
    Scheduled,
}

/// One deck as the queue route serves it.
#[derive(Debug, Clone)]
pub struct QueueDeck {
    /// Build id.
    pub queue_id: String,
    /// Deck cards in order.
    pub items: Vec<QueueItemLight>,
}

/// Ordered album samples plus the provider that served them.
pub type AlbumSamples = (Vec<PreviewTrackItem>, Option<String>);

/// Why a radio station or playlist suggestion could not be built.
#[derive(Debug, Clone, PartialEq)]
pub enum RadioFailure {
    /// The seed album or playlist does not exist (or is private).
    NotFound,
    /// The seed cannot make a station. The string is user-facing.
    Invalid(String),
    /// A provider or store failed.
    Provider(ProviderFailure),
}

impl From<ProviderFailure> for RadioFailure {
    fn from(failure: ProviderFailure) -> Self {
        Self::Provider(failure)
    }
}
