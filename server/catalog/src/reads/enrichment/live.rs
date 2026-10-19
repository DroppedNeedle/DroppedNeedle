//! Production enrichment roles: the concrete provider clients behind the
//! aggregator traits, live where credentials and persistence allow and
//! unconfigured stubs that report the gap elsewhere.
//!
//! The `From` impls map
//! core provider failures and statuses onto the enrichment leg types; the
//! aggregator keeps its own leg error and converts at the boundary.

use std::sync::Arc;

use crate::reads::enrichment as enrich;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::ListenBrainzConnection;
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_providers::{
    Providers,
    adapters::{CorePacer, CoreSink, ReqwestGet},
    degradation::IntegrationStatus as CoreStatus,
    error::ProviderError as CoreError,
    listenbrainz::{ListenBrainzClient, ListenBrainzCredentials},
    lrclib::LrclibClient,
};

impl From<CoreError> for enrich::ProviderError {
    /// Translate a typed core failure into an enrichment leg failure,
    /// keeping the source key and the human-readable cause.
    fn from(error: CoreError) -> Self {
        Self::new(error.source(), error.to_string())
    }
}

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

/// Live ListenBrainz popularity behind the enrichment role.
///
/// Popularity reads are anonymous (no token), so this adapter serves
/// production with default credentials. `Missing` (authoritative negative)
/// reads as empty, exactly like the client's fail-soft contract; only
/// `Unavailable` becomes a leg error and degrades the row.
#[derive(Clone)]
pub struct LiveListenBrainz {
    client: ListenBrainzClient<CorePacer, CoreSink>,
    settings: Option<Live<ListenBrainzConnection>>,
}

/// Whether ListenBrainz is switched on now. Off when the section cannot
/// be read.
fn listenbrainz_on(settings: &Live<ListenBrainzConnection>) -> bool {
    settings
        .get()
        .map(|settings| settings.enabled)
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "cannot read listenbrainz settings; popularity enrichment off");
            false
        })
}

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

impl std::fmt::Debug for LiveListenBrainz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveListenBrainz")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl LiveListenBrainz {
    /// Popularity reads against the given origin.
    pub fn with_base(http: reqwest::Client, base_url: &str, pacer: CorePacer) -> Self {
        Self {
            client: ListenBrainzClient::new(http, base_url, pacer, CoreSink),
            settings: None,
        }
    }

    /// Serve only while the ListenBrainz settings are switched on; off
    /// reads as unavailable. Read on every call, so a change in Settings
    /// takes effect at once.
    #[must_use]
    pub fn with_settings(mut self, settings: Live<ListenBrainzConnection>) -> Self {
        self.settings = Some(settings);
        self
    }

    fn enabled(&self) -> bool {
        self.settings.as_ref().is_none_or(listenbrainz_on)
    }

    fn switched_off(&self) -> Option<enrich::ProviderError> {
        (!self.enabled()).then(|| {
            enrich::ProviderError::new("listenbrainz", "listenbrainz is switched off".to_owned())
        })
    }
}

impl enrich::ListenBrainzClient for LiveListenBrainz {
    fn is_available(&self) -> bool {
        self.enabled()
    }

    fn artist_top_release_groups<'a>(
        &'a self,
        mbid: &'a str,
        count: usize,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::TopRelease>, enrich::ProviderError>> {
        Box::pin(async move {
            if let Some(off) = self.switched_off() {
                return Err(off);
            }
            let creds = ListenBrainzCredentials::default();
            match self
                .client
                .artist_top_release_groups(mbid, count, &creds)
                .await
            {
                droppedneedle_providers::listenbrainz::Outcome::Found(top) => Ok(top
                    .into_iter()
                    .map(|row| enrich::TopRelease {
                        mbid: row.release_group_mbid,
                        listen_count: row.listen_count,
                    })
                    .collect()),
                droppedneedle_providers::listenbrainz::Outcome::Missing => Ok(Vec::new()),
                droppedneedle_providers::listenbrainz::Outcome::Unavailable { message, .. } => {
                    Err(enrich::ProviderError::new("listenbrainz", message))
                }
            }
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> enrich::BoxFuture<'a, Result<std::collections::HashMap<String, i64>, enrich::ProviderError>>
    {
        Box::pin(async move {
            if let Some(off) = self.switched_off() {
                return Err(off);
            }
            let creds = ListenBrainzCredentials::default();
            match self.client.release_group_popularity(mbids, &creds).await {
                droppedneedle_providers::listenbrainz::Outcome::Found(counts) => Ok(counts),
                droppedneedle_providers::listenbrainz::Outcome::Missing => {
                    Ok(std::collections::HashMap::new())
                }
                droppedneedle_providers::listenbrainz::Outcome::Unavailable { message, .. } => {
                    Err(enrich::ProviderError::new("listenbrainz", message))
                }
            }
        })
    }
}

/// Live LRCLIB lyrics behind the enrichment role.
///
/// The client borrows its transport, so this adapter owns the [`ReqwestGet`]
/// and builds the client per call. Queries without a usable duration skip
/// the wire and read as not-found: duration is a required match key on
/// `/api/get`, so sending a fabricated zero would invent a query that can
/// never match the track.
#[derive(Debug, Clone)]
pub struct LiveLrclib {
    transport: ReqwestGet,
    base_url: String,
}

impl LiveLrclib {
    /// Lyrics reads against the given origin.
    pub fn with_base(transport: ReqwestGet, base_url: &str) -> Self {
        Self {
            transport,
            base_url: base_url.to_owned(),
        }
    }
}

impl enrich::LyricsClient for LiveLrclib {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
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
            let client = LrclibClient::with_base(&self.transport, &self.base_url);
            match client
                .get_exact_lyrics(&query.title, &query.artist, album, duration)
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
                Err(error) => Err(enrich::ProviderError::new("lrclib", lrclib_cause(&error))),
            }
        })
    }
}

/// Log-only cause for an LRCLIB leg failure. The wire never sees this; the
/// response carries the fixed degradation note.
fn lrclib_cause(error: &droppedneedle_providers::lrclib::FetchError) -> String {
    match error {
        droppedneedle_providers::lrclib::FetchError::Transport => {
            "lrclib transport failure".to_owned()
        }
        droppedneedle_providers::lrclib::FetchError::RateLimited { retry_after_secs } => {
            format!("lrclib rate limited; retry in {retry_after_secs:.1}s")
        }
        droppedneedle_providers::lrclib::FetchError::Unusable => {
            "lrclib answer unusable".to_owned()
        }
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
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::TopRelease>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "listenbrainz",
                "listenbrainz is not configured".to_owned(),
            ))
        })
    }

    fn release_group_popularity_batch<'a>(
        &'a self,
        _mbids: &'a [String],
    ) -> enrich::BoxFuture<'a, Result<std::collections::HashMap<String, i64>, enrich::ProviderError>>
    {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "listenbrainz",
                "listenbrainz is not configured".to_owned(),
            ))
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
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmArtistInfo>, enrich::ProviderError>>
    {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lastfm",
                "lastfm needs per-user credentials the port cannot supply".to_owned(),
            ))
        })
    }

    fn album_info<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
        _mbid: Option<&'a str>,
    ) -> enrich::BoxFuture<'a, Result<Option<enrich::LastFmAlbumInfo>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lastfm",
                "lastfm needs per-user credentials the port cannot supply".to_owned(),
            ))
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
    ) -> enrich::BoxFuture<'a, Result<enrich::ReleaseGroupCore, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "musicbrainz",
                "musicbrainz identity is not wired to a route".to_owned(),
            ))
        })
    }

    fn artist_core<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> enrich::BoxFuture<'a, Result<enrich::ArtistCore, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "musicbrainz",
                "musicbrainz identity is not wired to a route".to_owned(),
            ))
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
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::UserConcert>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "events",
                "events feed has no persistence behind it".to_owned(),
            ))
        })
    }

    fn cities<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> enrich::BoxFuture<'a, Result<Vec<enrich::EventCity>, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "events",
                "events feed has no persistence behind it".to_owned(),
            ))
        })
    }
}

/// Live production enrichment: the search batch adapter plus the lyrics
/// role, built once at boot and handed to [`ReadsSetup::build`](crate::reads::ReadsSetup::build).
pub struct ProductionEnrichment {
    /// Search enrichment behind the reads `EnrichmentPort`.
    pub search: Arc<enrich::AggregatingEnrichment>,
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
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
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
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
        Box::pin(async move {
            Err(enrich::ProviderError::new(
                "lrclib",
                "lrclib is not configured".to_owned(),
            ))
        })
    }
}

/// Build the production enrichment pair over the shared HTTP client and
/// provider deps. ListenBrainz popularity serves only while the
/// ListenBrainz settings are switched on, and lyrics only while the lyrics
/// setting is, both read per call;
/// Last.fm stays unconfigured (per-user credentials), the events feed stays
/// unconfigured (no persistence), and MusicBrainz identity stays
/// unconfigured (no route serves it).
pub fn production_enrichment(
    http: &reqwest::Client,
    providers: &Arc<Providers>,
    endpoints: &crate::reads::catalog::upstream::Endpoints,
    listenbrainz: Live<ListenBrainzConnection>,
    lyrics: Live<LyricsSettings>,
) -> ProductionEnrichment {
    let lb: Arc<dyn enrich::ListenBrainzClient> =
        match CorePacer::for_source(providers.clone(), "listenbrainz") {
            Some(pacer) => Arc::new(
                LiveListenBrainz::with_base(http.clone(), &endpoints.listenbrainz, pacer)
                    .with_settings(listenbrainz),
            ),
            None => Arc::new(UnconfiguredListenBrainz),
        };
    let live_lyrics = Arc::new(LiveLrclib::with_base(
        ReqwestGet::new(http.clone()),
        &endpoints.lrclib,
    ));
    let lyrics_role: Arc<dyn enrich::LyricsClient> =
        Arc::new(SwitchedLyrics::new(live_lyrics.clone(), lyrics.clone()));
    let aggregator = enrich::EnrichmentAggregator::new(
        Arc::new(UnconfiguredMusicBrainz),
        lb,
        Arc::new(UnconfiguredLastFm),
        lyrics_role,
        Arc::new(UnconfiguredEvents),
    );
    ProductionEnrichment {
        search: Arc::new(enrich::AggregatingEnrichment::new(Arc::new(aggregator))),
        lyrics: live_lyrics,
        lyrics_settings: lyrics,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use droppedneedle_kernel::http_client::HttpClientFactory;

    #[test]
    fn core_error_maps_to_a_leg_error_with_its_source() {
        let leg: enrich::ProviderError = CoreError::NotFound { provider: "lastfm" }.into();
        assert_eq!(leg.source, "lastfm");
        assert!(
            leg.message.contains("404"),
            "cause survives: {}",
            leg.message
        );
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
        assert_eq!(error.source, "listenbrainz");
        let error = UnconfiguredLastFm
            .artist_info("name", "mbid")
            .await
            .expect_err("unconfigured lastfm errors");
        assert_eq!(error.source, "lastfm");
        let error = UnconfiguredMusicBrainz
            .release_group("mbid")
            .await
            .expect_err("unconfigured identity errors");
        assert_eq!(error.source, "musicbrainz");
        let error = UnconfiguredEvents
            .cities("user")
            .await
            .expect_err("unconfigured feed errors");
        assert_eq!(error.source, "events");
    }

    #[tokio::test]
    async fn live_lrclib_skips_queries_without_a_match_key() {
        use crate::reads::enrichment::{LyricsClient as _, LyricsQuery};

        let transport = ReqwestGet::shared(&HttpClientFactory::new().unwrap());
        // Unroutable origin: any wire attempt fails, so Ok(not-found)
        // proves the skip.
        let role = LiveLrclib::with_base(transport, "http://127.0.0.1:9/");
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

    /// The role maps popularity rows onto enrichment rows and turns a dead
    /// upstream into a leg error, against a scripted loopback peer. No live
    /// network.
    #[tokio::test]
    async fn live_listenbrainz_role_maps_rows_and_failures() {
        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

        use crate::reads::enrichment::ListenBrainzClient as _;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let pacer = CorePacer::for_source(Arc::new(Providers::with_memory_cache()), "listenbrainz")
            .unwrap();
        let role = LiveListenBrainz::with_base(
            HttpClientFactory::new().unwrap().shared().clone(),
            &format!("http://{address}"),
            pacer,
        );
        let pending =
            tokio::spawn(async move { role.artist_top_release_groups("artist-mbid", 5).await });

        let (mut socket, _) = listener.accept().await.unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let read = socket.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0, "client closed before sending headers");
            raw.extend_from_slice(&chunk[..read]);
            if raw.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&raw);
        assert!(
            head.contains("/1/popularity/top-release-groups-for-artist/artist-mbid"),
            "role hits the popularity route: {head}"
        );
        let body = r#"[{"release_group_mbid":"rg-a","total_listen_count":10,"release_group":{"name":"Alpha"}},{"release_group_mbid":"","total_listen_count":99}]"#;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();

        let top = pending.await.unwrap().expect("role maps rows");
        assert_eq!(top.len(), 1, "blank-mbid row skipped: {top:?}");
        assert_eq!(top[0].mbid, "rg-a");
        assert_eq!(top[0].listen_count, 10);

        // A dead upstream becomes a leg error, never invented rows.
        let pacer = CorePacer::for_source(Arc::new(Providers::with_memory_cache()), "listenbrainz")
            .unwrap();
        let dead = LiveListenBrainz::with_base(
            HttpClientFactory::new().unwrap().shared().clone(),
            "http://127.0.0.1:9/",
            pacer,
        );
        let error = dead
            .artist_top_release_groups("artist-mbid", 5)
            .await
            .expect_err("dead upstream errors");
        assert_eq!(error.source, "listenbrainz");
    }

    #[tokio::test]
    async fn production_enrichment_without_listenbrainz_echoes_bare() {
        use crate::reads::search::models::{
            AlbumEnrichmentRequest, ArtistEnrichmentRequest, EnrichmentBatchRequest,
            EnrichmentSource,
        };
        use crate::reads::search::ports::EnrichmentPort as _;

        let http = HttpClientFactory::new().unwrap();
        let providers = Arc::new(Providers::with_memory_cache());
        // Nothing listens on the discard port.
        let closed = || "http://127.0.0.1:9".to_owned();
        let endpoints = crate::reads::catalog::upstream::Endpoints {
            listenbrainz: closed(),
            lastfm: closed(),
            audiodb: closed(),
            wikidata: closed(),
            wikipedia: closed(),
            commons: closed(),
            itunes: closed(),
            deezer: closed(),
            cover_art_archive: closed(),
            lrclib: closed(),
            github_releases: closed(),
            youtube: closed(),
            musicbrainz: closed(),
            brainzmash: closed(),
        };
        let dir = std::env::temp_dir().join(format!("dn-enrich-off-{}", std::process::id()));
        let crypto = droppedneedle_kernel::runtime_config::Crypto::from_key_bytes(&[7u8; 32]);
        let store =
            droppedneedle_kernel::runtime_config::ConfigStore::scratch(&dir, crypto.unwrap())
                .unwrap();
        store
            .save_blocking(LyricsSettings { enabled: false })
            .unwrap();
        let pair = production_enrichment(
            http.shared(),
            &providers,
            &endpoints,
            store.live(),
            store.live(),
        );
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
        let http = HttpClientFactory::new().unwrap();
        let live = Arc::new(LiveLrclib::with_base(
            ReqwestGet::new(http.shared().clone()),
            "http://127.0.0.1:9",
        ));
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
        assert_eq!(off.message, "lrclib is not configured");
        store
            .save_blocking(LyricsSettings { enabled: true })
            .unwrap();
        let on = role.exact_lyrics(&query).await.unwrap_err();
        assert_ne!(
            on.message, "lrclib is not configured",
            "switching on dials out"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
