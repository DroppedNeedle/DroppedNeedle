//! Production enrichment built at boot: the search counts and the online
//! lyrics.

use std::sync::Arc;

use crate::lyrics::OnlineLyrics;
use crate::search::enrichment::SearchCounts;
use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_media::playback::listening::ListeningStore;
use droppedneedle_providers::Providers;

/// Live production enrichment: the search counts plus the online lyrics,
/// built once at boot and handed to [`CatalogSetup::build`](crate::wiring::CatalogSetup::build).
pub struct ProductionEnrichment {
    /// Search counts behind the search `EnrichmentPort`.
    pub search: Arc<SearchCounts>,
    /// The online lyrics the player route and Library Management share.
    pub lyrics: OnlineLyrics,
}

/// Build the production enrichment pair over the provider deps. Search
/// counts follow the ListenBrainz and Last.fm settings and lyrics the
/// lyrics setting, all read per call, and each user's choice of service.
pub fn production_enrichment(
    providers: &Arc<Providers>,
    listenbrainz: Live<ListenBrainzConnection>,
    lyrics: Live<LyricsSettings>,
    lastfm: Live<LastFmSettings>,
    listening: ListeningStore,
    users: UsersDeps,
) -> ProductionEnrichment {
    ProductionEnrichment {
        search: Arc::new(SearchCounts::new(
            providers,
            listenbrainz,
            lastfm,
            listening,
            users,
        )),
        lyrics: OnlineLyrics::new(providers, lyrics),
    }
}
