//! The enrichment port: the seam providers implement.
//!
//! The trait has exactly one method, matching the single HTTP method on
//! the enrich-batch route. Both enrichable buckets (artists, albums)
//! travel through that one call. Providers report counts; a provider
//! failure surfaces as `Err`, and the service turns it into a typed
//! degradation inside a successful response, never a 5xx.

use std::future::Future;
use std::pin::Pin;

use super::models::{EnrichmentBatchRequest, EnrichmentResponse};

/// Max ids honored per bucket per batch. Extras are ignored, never an error.
pub const MAX_ENRICHMENT_PER_BUCKET: usize = 10;

/// Boxed future for dyn-compatible port methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A provider-side enrichment failure. The message is for the log only;
/// the wire gets a fixed degradation note.
#[derive(Debug, Clone)]
pub struct EnrichmentPortError {
    /// Source that failed (`listenbrainz`, `lastfm`).
    pub source: String,
    /// Internal cause, logged with the error id, never rendered.
    pub message: String,
}

impl EnrichmentPortError {
    /// Build a provider failure from its source and cause.
    pub fn new(source: &str, message: String) -> Self {
        Self {
            source: source.to_owned(),
            message,
        }
    }
}

impl std::fmt::Display for EnrichmentPortError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} enrichment failed: {}", self.source, self.message)
    }
}

/// The single enrichment seam. [`SearchCounts`](super::enrichment::SearchCounts)
/// implements it over ListenBrainz and Last.fm.
pub trait EnrichmentPort: Send + Sync {
    /// Enrich one mixed batch of artists and albums for `user_id`.
    fn enrich_batch<'a>(
        &'a self,
        user_id: &'a str,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'a, Result<EnrichmentResponse, EnrichmentPortError>>;
}
