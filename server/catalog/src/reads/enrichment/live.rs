//! Production enrichment roles: the concrete provider clients behind the
//! aggregator traits, live where credentials and persistence allow and
//! unconfigured stubs that report the gap elsewhere.
//!
//! A failed leg is the provider's own [`ProviderError`]; the `From`
//! impls here only translate integration statuses.

use std::sync::Arc;

use crate::reads::enrichment as enrich;
use crate::reads::search::enrichment::SearchCounts;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::{
    LastFmSettings, ListenBrainzConnection,
};
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_providers::{
    Providers, RequestPriority, degradation::IntegrationStatus as CoreStatus, error::ProviderError,
    lrclib::LrclibClient,
};

impl From<CoreStatus> for enrich::IntegrationStatus {
    fn from(status: CoreStatus) -> Self {
        match status {
            CoreStatus::Ok => Self::Ok,
            CoreStatus::Degraded => Self::Degraded,
            CoreStatus::Error => Self::Error,
        }
    }
}

impl From<enrich::IntegrationStatus> for CoreStatus {
    fn from(status: enrich::IntegrationStatus) -> Self {
        match status {
            enrich::IntegrationStatus::Ok => Self::Ok,
            enrich::IntegrationStatus::Degraded => Self::Degraded,
            enrich::IntegrationStatus::Error => Self::Error,
        }
    }
}

// ---------------------------------------------------------------------------
// Enrichment roles: concrete clients behind the aggregator traits
// ---------------------------------------------------------------------------

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

/// ListenBrainz role when the integration is disabled: never available, and
/// every call fails so the aggregator degrades the leg with a note instead
/// of rendering invented counts.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredListenBrainz;

impl enrich::ListenBrainzClient for UnconfiguredListenBrainz {
    fn is_available(&self) -> bool {
        false
    }

    fn artist_top_release_groups<'a>(
        &'a self,
        _mbid: &'a str,
        _count: usize,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::TopRelease>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "listenbrainz",
                message: "no connection is set up".to_owned(),
            })
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        _mbids: &'a [String],
    ) -> enrich::BoxFuture<'a, Result<std::collections::HashMap<String, i64>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "listenbrainz",
                message: "no connection is set up".to_owned(),
            })
        })
    }
}

/// Last.fm role for production: never available. Credentials are per-user
/// and the enrichment port carries no user, so no request can
/// authenticate a Last.fm call; the aggregator falls back to ListenBrainz
/// or bare echoes. Per-user fan-out awaits a user-scoped port.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLastFm;

impl enrich::LastFmClient for UnconfiguredLastFm {
    fn is_available(&self) -> bool {
        false
    }

    fn artist_info<'a>(
        &'a self,
        _name: &'a str,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmArtistInfo>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "lastfm",
                message: "it needs per-user credentials the port cannot supply".to_owned(),
            })
        })
    }

    fn album_info<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _mbid: Option<&'a str>,
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmAlbumInfo>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "lastfm",
                message: "it needs per-user credentials the port cannot supply".to_owned(),
            })
        })
    }
}

/// MusicBrainz role for production: identity lookups always fail. No
/// route serves album-page identity (the discover ports are synchronous
/// and cannot host the async client), so nothing can reach this leg;
/// failing closed keeps any future caller from trusting it.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredMusicBrainz;

impl enrich::MusicBrainzClient for UnconfiguredMusicBrainz {
    fn release_group<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<enrich::ReleaseGroupCore, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "musicbrainz",
                message: "identity reads are not wired to a route".to_owned(),
            })
        })
    }

    fn artist_core<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<enrich::ArtistCore, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "musicbrainz",
                message: "identity reads are not wired to a route".to_owned(),
            })
        })
    }
}

/// Events role for production: the feed always fails. Concert rows and
/// saved cities live in persistence that does not exist yet (no feed
/// fetching, no city store), so there is nothing to read; the lookup
/// degrades to empty with a note instead of inventing concerts.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredEvents;

impl enrich::EventsClient for UnconfiguredEvents {
    fn concerts_for_user<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::UserConcert>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "events",
                message: "the feed has no persistence behind it".to_owned(),
            })
        })
    }

    fn cities<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::EventCity>, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "events",
                message: "the feed has no persistence behind it".to_owned(),
            })
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

/// The live lyrics role behind the lyrics setting, read per call: while it
/// is off it answers as [`UnconfiguredLyrics`] without touching the wire.
pub struct SwitchedLyrics {
    live: Arc<LiveLrclib>,
    settings: Live<LyricsSettings>,
}

impl SwitchedLyrics {
    /// `live` while the lyrics setting is on.
    pub fn new(live: Arc<LiveLrclib>, settings: Live<LyricsSettings>) -> Self {
        Self { live, settings }
    }
}

impl enrich::LyricsClient for SwitchedLyrics {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, ProviderError>> {
        if lyrics_on(&self.settings) {
            self.live.exact_lyrics(query)
        } else {
            UnconfiguredLyrics.exact_lyrics(query)
        }
    }
}

/// Lyrics role when the integration is disabled: every call fails so the
/// aggregator degrades the leg with a note instead of rendering invented
/// lyrics.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnconfiguredLyrics;

impl enrich::LyricsClient for UnconfiguredLyrics {
    fn exact_lyrics<'a>(
        &'a self,
        _query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, ProviderError>> {
        Box::pin(async move {
            Err(ProviderError::NotConfigured {
                provider: "lrclib",
                message: "lyrics are switched off in Settings".to_owned(),
            })
        })
    }
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

    #[test]
    fn statuses_translate_both_ways() {
        assert_eq!(
            enrich::IntegrationStatus::from(CoreStatus::Degraded),
            enrich::IntegrationStatus::Degraded
        );
        assert_eq!(
            CoreStatus::from(enrich::IntegrationStatus::Error),
            CoreStatus::Error
        );
    }

    /// The shared transport answers through a module port: status, body,
    /// query encoding, and the `Retry-After` capture, against a scripted
    /// loopback peer. No live network.
    #[tokio::test]
    async fn unconfigured_roles_fail_with_their_source() {
        use crate::reads::enrichment::{
            EventsClient as _, LastFmClient as _, ListenBrainzClient as _, MusicBrainzClient as _,
        };

        assert!(!UnconfiguredListenBrainz.is_available());
        assert!(!UnconfiguredLastFm.is_available());

        let error = UnconfiguredListenBrainz
            .artist_top_release_groups("mbid", 1)
            .await
            .expect_err("unconfigured popularity errors");
        assert_eq!(error.source(), "listenbrainz");
        let error = UnconfiguredLastFm
            .artist_info("name", "mbid")
            .await
            .expect_err("unconfigured lastfm errors");
        assert_eq!(error.source(), "lastfm");
        let error = UnconfiguredMusicBrainz
            .release_group("mbid")
            .await
            .expect_err("unconfigured identity errors");
        assert_eq!(error.source(), "musicbrainz");
        let error = UnconfiguredEvents
            .cities("user")
            .await
            .expect_err("unconfigured feed errors");
        assert_eq!(error.source(), "events");
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
            EnrichmentSource,
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
        assert_eq!(response.source, EnrichmentSource::None);
        assert!(response.degradations.is_empty());
        assert_eq!(response.artists.len(), 1);
        assert_eq!(response.artists[0].listen_count, None);
        assert_eq!(response.albums.len(), 1);
        assert_eq!(response.albums[0].listen_count, None);
    }

    #[tokio::test]
    async fn switched_lyrics_follow_the_setting_per_call() {
        use enrich::LyricsClient;
        let live = Arc::new(LiveLrclib::new(closed_providers().lrclib.clone()));
        let dir = std::env::temp_dir().join(format!("dn-lyrics-switch-{}", std::process::id()));
        let crypto = droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]);
        let store =
            droppedneedle_kernel::runtime_config::ConfigStore::scratch(&dir, crypto.unwrap())
                .unwrap();
        store
            .save_blocking(LyricsSettings { enabled: false })
            .unwrap();
        let role = SwitchedLyrics::new(live, store.live());
        let query = enrich::LyricsQuery {
            artist: "Radiohead".to_owned(),
            title: "Airbag".to_owned(),
            album: None,
            duration_secs: Some(284.0),
        };
        let off = role.exact_lyrics(&query).await.unwrap_err();
        assert!(matches!(off, ProviderError::NotConfigured { .. }), "{off}");
        store
            .save_blocking(LyricsSettings { enabled: true })
            .unwrap();
        let on = role.exact_lyrics(&query).await.unwrap_err();
        assert!(
            !matches!(on, ProviderError::NotConfigured { .. }),
            "switching on dials out: {on}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
