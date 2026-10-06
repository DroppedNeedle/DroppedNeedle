//! The enrichment port: the seam providers implement.
//!
//! The trait has exactly one method, matching the single HTTP method on
//! the enrich-batch route. Both enrichable buckets (artists, albums)
//! travel through that one call. Providers report counts; a provider
//! failure surfaces as `Err`, and the service turns it into a typed
//! degradation inside a successful response, never a 5xx.

use std::future::Future;
use std::pin::Pin;

use super::models::{
    AlbumEnrichment, ArtistEnrichment, EnrichmentBatchRequest, EnrichmentResponse,
};

/// Max ids honored per bucket per batch, kept from v2 (`MAX_ENRICHMENT`).
/// Extras are ignored, never an error.
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

/// The single enrichment seam. `reads::enrichment` implements it for
/// ListenBrainz and Last.fm; [`UnconfiguredEnrichment`] serves when no
/// provider is wired.
pub trait EnrichmentPort: Send + Sync {
    /// Enrich one mixed batch of artists and albums.
    fn enrich_batch(
        &self,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'_, Result<EnrichmentResponse, EnrichmentPortError>>;
}

/// Enrichment with no provider wired: every id echoes back with absent
/// counts and source `none`. This is real behavior, not a fake: absence
/// means unknown, and nothing is zero-filled to look real.
#[derive(Debug, Clone, Default)]
pub struct UnconfiguredEnrichment;

impl EnrichmentPort for UnconfiguredEnrichment {
    fn enrich_batch(
        &self,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'_, Result<EnrichmentResponse, EnrichmentPortError>> {
        Box::pin(async move {
            Ok(EnrichmentResponse {
                artists: request
                    .artists
                    .into_iter()
                    .take(MAX_ENRICHMENT_PER_BUCKET)
                    .filter(|item| !item.musicbrainz_id.trim().is_empty())
                    .map(|item| ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: None,
                        listen_count: None,
                    })
                    .collect(),
                albums: request
                    .albums
                    .into_iter()
                    .take(MAX_ENRICHMENT_PER_BUCKET)
                    .filter(|item| !item.musicbrainz_id.trim().is_empty())
                    .map(|item| AlbumEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        track_count: None,
                        listen_count: None,
                    })
                    .collect(),
                source: super::models::EnrichmentSource::None,
                degradations: Vec::new(),
            })
        })
    }
}
