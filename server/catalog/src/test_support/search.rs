//! Search enrichment with no provider.

use crate::search::models::{
    AlbumEnrichment, ArtistEnrichment, EnrichmentBatchRequest, EnrichmentResponse,
};
use crate::search::ports::*;

/// Enrichment with no provider wired: every id echoes back with absent
/// counts and source `none`. This is real behavior, not a fake: absence
/// means unknown, and nothing is zero-filled to look real.
#[derive(Debug, Clone, Default)]
pub struct UnconfiguredEnrichment;

impl EnrichmentPort for UnconfiguredEnrichment {
    fn enrich_batch<'a>(
        &'a self,
        _user_id: &'a str,
        request: EnrichmentBatchRequest,
    ) -> BoxFuture<'a, Result<EnrichmentResponse, EnrichmentPortError>> {
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
                source: None,
                degradations: Vec::new(),
            })
        })
    }
}
