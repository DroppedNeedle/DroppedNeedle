//! Production enrichment built at boot: the search counts and the online
//! lyrics.

use std::sync::Arc;

use crate::reads::lyrics::OnlineLyrics;
use crate::reads::search::enrichment::SearchCounts;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_providers::Providers;

/// Live production enrichment: the search counts plus the online lyrics,
/// built once at boot and handed to [`ReadsSetup::build`](crate::reads::ReadsSetup::build).
pub struct ProductionEnrichment {
    /// Search counts behind the reads `EnrichmentPort`.
    pub search: Arc<SearchCounts>,
    /// The online lyrics the player route and Library Management share.
    pub lyrics: OnlineLyrics,
}

/// Build the production enrichment pair over the provider deps. Search
/// counts follow the ListenBrainz and Last.fm settings and lyrics the
/// lyrics setting, all read per call.
pub fn production_enrichment(
    providers: &Arc<Providers>,
    listenbrainz: Live<ListenBrainzConnection>,
    lyrics: Live<LyricsSettings>,
    lastfm: Live<LastFmSettings>,
) -> ProductionEnrichment {
    ProductionEnrichment {
        search: Arc::new(SearchCounts::new(providers, listenbrainz, lastfm)),
        lyrics: OnlineLyrics::new(providers, lyrics),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::http_client::HttpClientFactory;

    /// The providers with every address on the discard port, where nothing
    /// listens.
    fn closed_providers() -> Arc<Providers> {
        providers_with_listenbrainz("http://127.0.0.1:9")
    }

    /// The providers with ListenBrainz at `listenbrainz` and every other
    /// address on the discard port.
    fn providers_with_listenbrainz(listenbrainz: &str) -> Arc<Providers> {
        let closed = "http://127.0.0.1:9".to_owned();
        Arc::new(Providers::over_wire(
            Arc::new(droppedneedle_providers::ReqwestWire::new(
                &HttpClientFactory::new().unwrap(),
            )),
            Arc::new(droppedneedle_kernel::clock::SystemClock),
            droppedneedle_providers::ProviderEndpoints {
                musicbrainz: closed.clone(),
                brainzmash: closed.clone(),
                coverart: closed.clone(),
                listenbrainz: listenbrainz.to_owned(),
                lastfm: closed.clone(),
                audiodb: closed.clone(),
                acoustid: closed.clone(),
                discogs: closed.clone(),
                itunes: closed.clone(),
                deezer: closed.clone(),
                lrclib: closed.clone(),
                wikidata: closed.clone(),
                wikipedia: closed.clone(),
                commons: closed.clone(),
                ticketmaster: closed.clone(),
                skiddle: closed.clone(),
                geocoding: closed.clone(),
                youtube: closed.clone(),
                github_releases: closed,
            },
        ))
    }

    #[tokio::test]
    async fn production_enrichment_without_listenbrainz_echoes_bare() {
        use crate::reads::search::models::{
            AlbumEnrichmentRequest, ArtistEnrichmentRequest, EnrichmentBatchRequest,
        };
        use crate::reads::search::ports::EnrichmentPort as _;

        // Nothing listens on the discard port.
        let providers = closed_providers();
        let dir = std::env::temp_dir().join(format!("dn-enrich-off-{}", std::process::id()));
        let crypto = droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]);
        let store =
            droppedneedle_kernel::runtime_config::ConfigStore::scratch(&dir, crypto.unwrap())
                .unwrap();
        store
            .save_blocking(LyricsSettings { enabled: false })
            .unwrap();
        let pair = production_enrichment(&providers, store.live(), store.live(), store.live());
        let _ = std::fs::remove_dir_all(&dir);
        let response = pair
            .search
            .enrich_batch(EnrichmentBatchRequest {
                artists: vec![ArtistEnrichmentRequest {
                    musicbrainz_id: "artist-mbid".to_owned(),
                    name: "Artist".to_owned(),
                }],
                albums: vec![AlbumEnrichmentRequest {
                    musicbrainz_id: "rg-mbid".to_owned(),
                    artist_name: "Artist".to_owned(),
                    album_name: "Album".to_owned(),
                }],
            })
            .await
            .expect("unconfigured batch still answers");
        assert_eq!(response.source, None);
        assert!(response.degradations.is_empty());
        assert_eq!(response.artists.len(), 1);
        assert_eq!(response.artists[0].listen_count, None);
        assert_eq!(response.albums.len(), 1);
        assert_eq!(response.albums[0].listen_count, None);
    }
}
