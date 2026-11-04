//! The catalog's enrichment built at boot: the search counts and the
//! online lyrics.

use std::sync::Arc;

use crate::lyrics::{OnlineLyrics, OnlineLyricsSource};
use crate::search::enrichment::SearchCounts;
use crate::search::ports::EnrichmentPort;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::Providers;

/// The search counts plus the online lyrics, built once at boot and handed
/// to [`CatalogSetup::build`](crate::wiring::CatalogSetup::build).
pub struct CatalogEnrichment {
    /// Search counts behind the search `EnrichmentPort`.
    pub search: Arc<dyn EnrichmentPort>,
    /// The online lyrics the player route falls back to.
    pub lyrics: Arc<dyn OnlineLyricsSource>,
}

/// Build the production enrichment pair over the provider deps. Search
/// counts follow the ListenBrainz and Last.fm settings, read per call, and
/// each user's choice of service; `lyrics` is the online lyrics Library
/// Management also asks.
pub fn production_enrichment(
    providers: &Arc<Providers>,
    listenbrainz: Live<ListenBrainzConnection>,
    lastfm: Live<LastFmSettings>,
    listening: ListeningStore,
    users: UsersDeps,
    lyrics: OnlineLyrics,
) -> CatalogEnrichment {
    CatalogEnrichment {
        search: Arc::new(SearchCounts::new(
            providers,
            listenbrainz,
            lastfm,
            listening,
            users,
        )),
        lyrics: Arc::new(lyrics),
    }
}
