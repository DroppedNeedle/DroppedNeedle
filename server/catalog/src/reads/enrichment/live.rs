//! Production enrichment built at boot: the search counts and the live
//! LRCLIB role behind provider lyrics.

use std::sync::Arc;

use crate::reads::enrichment as enrich;
use crate::reads::search::enrichment::SearchCounts;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_providers::{
    Providers, RequestPriority, error::ProviderError, lrclib::LrclibClient,
};

/// Whether lyrics are switched on now. Off when the section cannot be
/// read.
pub(crate) fn lyrics_on(settings: &Live<LyricsSettings>) -> bool {
    settings
        .get()
        .map(|settings| settings.enabled)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read lyrics settings; lyrics enrichment off");
            false
        })
}

/// Live LRCLIB lyrics behind the enrichment role, at user priority: a
/// person opened the lyrics.
///
/// Queries without a usable duration skip the wire and read as not-found:
/// duration is a required match key on `/api/get`, so sending a fabricated
/// zero would invent a query that can never match the track.
#[derive(Debug, Clone)]
pub struct LiveLrclib {
    client: LrclibClient,
}

impl LiveLrclib {
    /// Lyrics reads through `client`.
    pub fn new(client: LrclibClient) -> Self {
        Self { client }
    }
}

impl enrich::LyricsClient for LiveLrclib {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, ProviderError>> {
        Box::pin(async move {
            let duration = query
                .duration_secs
                .map(|seconds| seconds.round().max(0.0) as u32)
                .filter(|duration| *duration > 0);
            let album = query.album.as_deref().unwrap_or("");
            let Some(duration) = duration else {
                return Ok(enrich::LyricsLookup {
                    found: false,
                    plain: None,
                    synced: None,
                });
            };
            if query.artist.trim().is_empty() || query.title.trim().is_empty() {
                return Ok(enrich::LyricsLookup {
                    found: false,
                    plain: None,
                    synced: None,
                });
            }
            match self
                .client
                .get_exact_lyrics(
                    &query.title,
                    &query.artist,
                    album,
                    duration,
                    RequestPriority::UserInitiated,
                )
                .await
            {
                Ok(found) => {
                    let (plain, synced) = match found.candidate {
                        Some(candidate) => (candidate.plain_lyrics, candidate.synced_lyrics),
                        None => (None, None),
                    };
                    Ok(enrich::LyricsLookup {
                        found: found.found,
                        plain,
                        synced,
                    })
                }
                Err(error) => Err(error),
            }
        })
    }
}

/// Live production enrichment: the search counts plus the lyrics role,
/// built once at boot and handed to [`ReadsSetup::build`](crate::reads::ReadsSetup::build).
pub struct ProductionEnrichment {
    /// Search counts behind the reads `EnrichmentPort`.
    pub search: Arc<SearchCounts>,
    /// Live lyrics role behind the reads `LyricsPort` (via
    /// `ProviderLyrics`).
    pub lyrics: Arc<LiveLrclib>,
    /// The lyrics setting, read per call: while it is off, lyrics read as
    /// absent and touch no network.
    pub lyrics_settings: Live<LyricsSettings>,
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
        lyrics: Arc::new(LiveLrclib::new(providers.lrclib.clone())),
        lyrics_settings: lyrics,
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
    async fn live_lrclib_skips_queries_without_a_match_key() {
        use crate::reads::enrichment::{LyricsClient as _, LyricsQuery};

        // Unroutable origin: any wire attempt fails, so Ok(not-found)
        // proves the skip.
        let role = LiveLrclib::new(closed_providers().lrclib.clone());
        for query in [
            LyricsQuery {
                artist: "Artist".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: None,
            },
            LyricsQuery {
                artist: "Artist".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: Some(0.4),
            },
            LyricsQuery {
                artist: "  ".to_owned(),
                title: "Title".to_owned(),
                album: None,
                duration_secs: Some(200.0),
            },
        ] {
            let lookup = role
                .exact_lyrics(&query)
                .await
                .expect("skip reads as absence");
            assert!(!lookup.found);
            assert!(lookup.plain.is_none());
            assert!(lookup.synced.is_none());
        }
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
