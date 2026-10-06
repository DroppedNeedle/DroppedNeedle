//! Production enrichment roles: the concrete provider clients behind the
//! aggregator traits, live where credentials and persistence allow and
//! unconfigured stubs that report the gap elsewhere.
//!
//! The `From` impls map
//! core provider failures and statuses onto the enrichment leg types; the
//! aggregator keeps its own leg error and converts at the boundary.

use std::sync::Arc;

use crate::providers::{
    Providers,
    adapters::{CorePacer, CoreSink, ReqwestGet},
    degradation::IntegrationStatus as CoreStatus,
    error::ProviderError as CoreError,
    listenbrainz::{ListenBrainzClient, ListenBrainzCredentials},
    lrclib::LrclibClient,
};
use crate::reads::enrichment as enrich;

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
    enabled: Switch,
}

/// A live settings switch, read on every call so a change in Settings
/// takes effect without a restart.
pub type Switch = Arc<dyn Fn() -> bool + Send + Sync>;

impl std::fmt::Debug for LiveListenBrainz {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveListenBrainz")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl LiveListenBrainz {
    /// Popularity reads against the live service.
    pub fn new(http: reqwest::Client, pacer: CorePacer) -> Self {
        Self::with_base(
            http,
            crate::providers::listenbrainz::DEFAULT_BASE_URL,
            pacer,
        )
    }

    /// Popularity reads against a scripted or mirrored origin.
    pub fn with_base(http: reqwest::Client, base_url: &str, pacer: CorePacer) -> Self {
        Self {
            client: ListenBrainzClient::new(http, base_url, pacer, CoreSink),
            enabled: Arc::new(|| true),
        }
    }

    /// Serve only while `enabled` reads true; off reads as unavailable.
    #[must_use]
    pub fn with_switch(mut self, enabled: Switch) -> Self {
        self.enabled = enabled;
        self
    }

    fn switched_off(&self) -> Option<enrich::ProviderError> {
        (!(self.enabled)()).then(|| {
            enrich::ProviderError::new("listenbrainz", "listenbrainz is switched off".to_owned())
        })
    }
}

impl enrich::ListenBrainzClient for LiveListenBrainz {
    fn is_available(&self) -> bool {
        (self.enabled)()
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
                crate::providers::listenbrainz::Outcome::Found(top) => Ok(top
                    .into_iter()
                    .map(|row| enrich::TopRelease {
                        mbid: row.release_group_mbid,
                        listen_count: row.listen_count,
                    })
                    .collect()),
                crate::providers::listenbrainz::Outcome::Missing => Ok(Vec::new()),
                crate::providers::listenbrainz::Outcome::Unavailable { message, .. } => {
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
                crate::providers::listenbrainz::Outcome::Found(counts) => Ok(counts),
                crate::providers::listenbrainz::Outcome::Missing => {
                    Ok(std::collections::HashMap::new())
                }
                crate::providers::listenbrainz::Outcome::Unavailable { message, .. } => {
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
    /// Lyrics reads against the live service.
    pub fn new(transport: ReqwestGet) -> Self {
        Self::with_base(transport, crate::providers::lrclib::API_BASE)
    }

    /// Lyrics reads against a scripted or mirrored origin.
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
fn lrclib_cause(error: &crate::providers::lrclib::FetchError) -> String {
    match error {
        crate::providers::lrclib::FetchError::Transport => "lrclib transport failure".to_owned(),
        crate::providers::lrclib::FetchError::RateLimited { retry_after_secs } => {
            format!("lrclib rate limited; retry in {retry_after_secs:.1}s")
        }
        crate::providers::lrclib::FetchError::Unusable => "lrclib answer unusable".to_owned(),
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
    /// The lyrics setting, read per call: while it reads false, lyrics
    /// read as absent and touch no network.
    pub lyrics_enabled: Switch,
}

/// The live lyrics role behind a per-call switch: while the switch reads
/// false it answers as [`UnconfiguredLyrics`] without touching the wire.
pub struct SwitchedLyrics {
    live: Arc<LiveLrclib>,
    enabled: Switch,
}

impl SwitchedLyrics {
    /// `live` while `enabled` reads true.
    pub fn new(live: Arc<LiveLrclib>, enabled: Switch) -> Self {
        Self { live, enabled }
    }
}

impl enrich::LyricsClient for SwitchedLyrics {
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a enrich::LyricsQuery,
    ) -> enrich::BoxFuture<'a, Result<enrich::LyricsLookup, enrich::ProviderError>> {
        if (self.enabled)() {
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
/// provider deps. ListenBrainz popularity serves only while the live
/// `listenbrainz_enabled` switch reads true, and lyrics serve only while
/// the `lyrics_enabled` switch does;
/// Last.fm stays unconfigured (per-user credentials), the events feed stays
/// unconfigured (no persistence), and MusicBrainz identity stays
/// unconfigured (no route serves it).
pub fn production_enrichment(
    http: &reqwest::Client,
    providers: &Arc<Providers>,
    listenbrainz_enabled: Switch,
    lyrics_enabled: Switch,
) -> ProductionEnrichment {
    let lb: Arc<dyn enrich::ListenBrainzClient> =
        match CorePacer::for_source(providers.clone(), "listenbrainz") {
            Some(pacer) => Arc::new(
                LiveListenBrainz::new(http.clone(), pacer).with_switch(listenbrainz_enabled),
            ),
            None => Arc::new(UnconfiguredListenBrainz),
        };
    let live_lyrics = Arc::new(LiveLrclib::new(ReqwestGet::new(http.clone())));
    let lyrics_role: Arc<dyn enrich::LyricsClient> = Arc::new(SwitchedLyrics::new(
        live_lyrics.clone(),
        lyrics_enabled.clone(),
    ));
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
        lyrics_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http_client::HttpClientFactory;

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
        let pair = production_enrichment(
            http.shared(),
            &providers,
            Arc::new(|| false),
            Arc::new(|| false),
        );
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
        use std::sync::atomic::{AtomicBool, Ordering};
        let http = HttpClientFactory::new().unwrap();
        let live = Arc::new(LiveLrclib::with_base(
            ReqwestGet::new(http.shared().clone()),
            "http://127.0.0.1:9",
        ));
        let flag = Arc::new(AtomicBool::new(false));
        let reader = flag.clone();
        let role = SwitchedLyrics::new(live, Arc::new(move || reader.load(Ordering::SeqCst)));
        let query = enrich::LyricsQuery {
            artist: "Radiohead".to_owned(),
            title: "Airbag".to_owned(),
            album: None,
            duration_secs: Some(284.0),
        };
        let off = role.exact_lyrics(&query).await.unwrap_err();
        assert_eq!(off.message, "lrclib is not configured");
        flag.store(true, Ordering::SeqCst);
        let on = role.exact_lyrics(&query).await.unwrap_err();
        assert_ne!(
            on.message, "lrclib is not configured",
            "switching on dials out"
        );
    }
}
