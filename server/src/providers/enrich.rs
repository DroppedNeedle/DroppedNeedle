//! Enrichment aggregation over provider clients.
//!
//! One request fans out to several providers and fans back into one page:
//! MusicBrainz resolves identity, ListenBrainz and Last.fm add popularity
//! and prose, LRCLIB adds lyrics, the events feed adds concerts. Providers
//! fail independently, so every leg carries a typed outcome and the page
//! still renders with honest gaps.
//!
//! The rules below are ported from the v2 Python reference
//! (`backend/services/search_enrichment_service.py`,
//! `backend/services/discover/enrichment_service.py`,
//! `backend/services/album_enrichment_service.py`,
//! `backend/services/artist_enrichment_service.py`,
//! `backend/services/events_service.py`,
//! `backend/infrastructure/integration_result.py`):
//!
//! - Typed degradation results: each leg reports ok, degraded, or error,
//!   and the worst status wins when legs combine.
//! - Operation/source matrix: identity resolution through MusicBrainz is
//!   identity-critical, so a dead MusicBrainz fails the page; everything
//!   else is stale-cache-acceptable and degrades to absent fields with a
//!   note, never an error.
//! - Fan-out/fan-in with per-source budgets: legs run concurrently, each
//!   under its own timeout, and batch legs run at most two abreast so one
//!   request cannot flood a provider.
//!
//! Seam note: the aggregation role traits below (`MusicBrainzClient`,
//! `ListenBrainzClient`, `LastFmClient`, `LyricsClient`, `EventsClient`)
//! are the stable aggregator surface. The integrator's adapters in
//! [`adapters`](super::adapters) implement them over the concrete HTTP
//! clients (live where credentials and persistence allow, honest
//! unconfigured stubs elsewhere); this module needs no other changes.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use super::degradation::{IntegrationStatus as CoreStatus, record_current};
use crate::reads::library::stores::{LibraryCatalog, LyricDoc, LyricsPort, StoreError};
use crate::reads::search::models::{
    AlbumEnrichment, ArtistEnrichment, Degradation, EnrichmentBatchRequest, EnrichmentResponse,
    EnrichmentSource,
};
use crate::reads::search::ports::{EnrichmentPort, MAX_ENRICHMENT_PER_BUCKET};

// ---------------------------------------------------------------------------
// Typed degradation results (v2 `integration_result.py`)
// ---------------------------------------------------------------------------

/// Outcome of one provider leg: full data, partial data, or none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationStatus {
    /// The provider answered.
    Ok,
    /// The provider answered partially, or stale data stands in.
    Degraded,
    /// The provider failed outright.
    Error,
}

/// Worst status across legs: error beats degraded beats ok.
pub fn aggregate_status(
    statuses: impl IntoIterator<Item = IntegrationStatus>,
) -> IntegrationStatus {
    let mut worst = IntegrationStatus::Ok;
    for status in statuses {
        match status {
            IntegrationStatus::Error => return IntegrationStatus::Error,
            IntegrationStatus::Degraded => worst = IntegrationStatus::Degraded,
            IntegrationStatus::Ok => {}
        }
    }
    worst
}

// ---------------------------------------------------------------------------
// Operation/source matrix and routing
// ---------------------------------------------------------------------------

/// What the caller is building. Identity operations resolve who or what a
/// row is; everything else decorates an already-identified row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Operation {
    /// Album identity through MusicBrainz.
    AlbumIdentity,
    /// Artist identity through MusicBrainz.
    ArtistIdentity,
    /// Popularity counts behind search rows.
    SearchCounts,
    /// Album prose, tags, and popularity.
    AlbumDetail,
    /// Artist prose, tags, and popularity.
    ArtistDetail,
    /// Lyrics for one track.
    Lyrics,
    /// Concerts near one user.
    Events,
}

/// Provider sources behind enrichment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProviderSource {
    /// MusicBrainz identity lookups.
    MusicBrainz,
    /// ListenBrainz listen counts.
    ListenBrainz,
    /// Last.fm bios, tags, and listener counts.
    LastFm,
    /// LRCLIB lyrics.
    Lrclib,
    /// The concerts feed (Ticketmaster/Skiddle watchers).
    EventsFeed,
}

impl ProviderSource {
    /// Log and degradation spelling for the source.
    pub fn name(self) -> &'static str {
        match self {
            Self::MusicBrainz => "musicbrainz",
            Self::ListenBrainz => "listenbrainz",
            Self::LastFm => "lastfm",
            Self::Lrclib => "lrclib",
            Self::EventsFeed => "events",
        }
    }
}

/// How a leg failure treats the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Routing {
    /// The request cannot render without this leg: fail it.
    IdentityCritical,
    /// The request renders with gaps: degrade the leg, keep the page.
    StaleCacheAcceptable,
}

/// Route one operation/source cell. Only MusicBrainz identity resolution is
/// identity-critical; a dead MusicBrainz fails identity pages, while every
/// other cell degrades. In particular, popularity, prose, lyrics, and
/// concerts never fail a request no matter which source backs them.
pub fn routing(operation: Operation, source: ProviderSource) -> Routing {
    match (operation, source) {
        (Operation::AlbumIdentity | Operation::ArtistIdentity, ProviderSource::MusicBrainz) => {
            Routing::IdentityCritical
        }
        _ => Routing::StaleCacheAcceptable,
    }
}

// ---------------------------------------------------------------------------
// Provider client traits (the stable aggregation roles; adapters implement them)
// ---------------------------------------------------------------------------

/// Boxed future for dyn-compatible client methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A provider-side failure. The message is for the log only; the wire gets
/// fixed degradation notes.
#[derive(Debug, Clone)]
pub struct ProviderError {
    /// Source that failed.
    pub source: &'static str,
    /// Internal cause, logged, never rendered.
    pub message: String,
}

impl ProviderError {
    /// Build a provider failure from its source and cause.
    pub fn new(source: &'static str, message: String) -> Self {
        Self { source, message }
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} provider failed: {}", self.source, self.message)
    }
}

/// Release-group identity from MusicBrainz.
#[derive(Debug, Clone)]
pub struct ReleaseGroupCore {
    /// Release-group MBID.
    pub mbid: String,
    /// Release-group title.
    pub title: String,
    /// Credited artist name.
    pub artist_name: String,
    /// Credited artist MBID, when known.
    pub artist_mbid: Option<String>,
    /// First-release date, when known.
    pub release_date: Option<String>,
    /// Folksonomy tags, best first.
    pub tags: Vec<String>,
}

/// Artist identity from MusicBrainz.
#[derive(Debug, Clone)]
pub struct ArtistCore {
    /// Artist MBID.
    pub mbid: String,
    /// Artist name.
    pub name: String,
    /// Country or area name, when known.
    pub country: Option<String>,
}

/// One top release-group row behind an artist popularity sum.
#[derive(Debug, Clone)]
pub struct TopRelease {
    /// Release-group MBID.
    pub mbid: String,
    /// Listen count behind the row.
    pub listen_count: i64,
}

/// Last.fm artist prose and counts.
#[derive(Debug, Clone, Default)]
pub struct LastFmArtistInfo {
    /// Short biography, when the provider reported one.
    pub bio_summary: Option<String>,
    /// Tag names.
    pub tags: Vec<String>,
    /// Listener count, when reported.
    pub listeners: Option<i64>,
    /// Play count, when reported.
    pub playcount: Option<i64>,
    /// Provider URL, when reported.
    pub url: Option<String>,
    /// Provider MBID, when reported.
    pub mbid: Option<String>,
}

/// Last.fm album prose and counts.
#[derive(Debug, Clone, Default)]
pub struct LastFmAlbumInfo {
    /// Short summary, when the provider reported one.
    pub summary: Option<String>,
    /// Tag names.
    pub tags: Vec<String>,
    /// Listener count, when reported.
    pub listeners: Option<i64>,
    /// Play count, when reported.
    pub playcount: Option<i64>,
    /// Provider URL, when reported.
    pub url: Option<String>,
}

/// MusicBrainz identity lookups. Failures here fail identity-critical
/// operations; callers check [`routing`] before degrading.
pub trait MusicBrainzClient: Send + Sync {
    /// Release-group identity by MBID.
    fn release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ReleaseGroupCore, ProviderError>>;

    /// Artist identity by MBID.
    fn artist_core<'a>(&'a self, mbid: &'a str)
    -> BoxFuture<'a, Result<ArtistCore, ProviderError>>;
}

/// ListenBrainz popularity lookups.
pub trait ListenBrainzClient: Send + Sync {
    /// True when the integration is configured and usable.
    fn is_available(&self) -> bool;

    /// Top release groups behind one artist, for the popularity sum.
    fn artist_top_release_groups<'a>(
        &'a self,
        mbid: &'a str,
        count: usize,
    ) -> BoxFuture<'a, Result<Vec<TopRelease>, ProviderError>>;

    /// Listen counts behind release groups in one call.
    fn release_group_popularity_batch<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, i64>, ProviderError>>;
}

/// Last.fm prose and popularity lookups.
pub trait LastFmClient: Send + Sync {
    /// True when the integration is configured and usable.
    fn is_available(&self) -> bool;

    /// Artist info by name with an MBID hint.
    fn artist_info<'a>(
        &'a self,
        name: &'a str,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<LastFmArtistInfo>, ProviderError>>;

    /// Album info by artist and album names with an MBID hint.
    fn album_info<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        mbid: Option<&'a str>,
    ) -> BoxFuture<'a, Result<Option<LastFmAlbumInfo>, ProviderError>>;
}

/// One lyrics lookup.
#[derive(Debug, Clone)]
pub struct LyricsQuery {
    /// Track artist name.
    pub artist: String,
    /// Track title.
    pub title: String,
    /// Album title, when known.
    pub album: Option<String>,
    /// Track length in seconds, when known.
    pub duration_secs: Option<f64>,
}

/// LRCLIB lyrics outcome. `found` false means the provider has no lyrics
/// for the track: absence, never failure.
#[derive(Debug, Clone)]
pub struct LyricsLookup {
    /// True when the provider holds lyrics for the track.
    pub found: bool,
    /// Plain lyrics, when held.
    pub plain: Option<String>,
    /// Synced (LRC) lyrics, when held.
    pub synced: Option<String>,
}

/// LRCLIB exact lyrics lookups.
pub trait LyricsClient: Send + Sync {
    /// Exact lyrics for one track.
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
    ) -> BoxFuture<'a, Result<LyricsLookup, ProviderError>>;
}

/// One stored concerts-feed row (read model, trimmed to lookup fields).
#[derive(Debug, Clone)]
pub struct LiveEvent {
    /// Feed source (`ticketmaster`, `skiddle`).
    pub source: String,
    /// Source-side event id.
    pub source_event_id: String,
    /// Performing artist name.
    pub artist_name: String,
    /// Event name.
    pub event_name: String,
    /// Venue-local date (YYYY-MM-DD).
    pub local_date: String,
    /// Venue name, when known.
    pub venue_name: Option<String>,
    /// Venue city, when known.
    pub city: Option<String>,
    /// Venue latitude, when known.
    pub latitude: Option<f64>,
    /// Venue longitude, when known.
    pub longitude: Option<f64>,
    /// Ticket URL, when known.
    pub ticket_url: Option<String>,
}

/// A feed row joined to one user's follow.
#[derive(Debug, Clone)]
pub struct UserConcert {
    /// The feed row.
    pub event: LiveEvent,
    /// Followed artist MBID (original case, for artist-page links).
    pub artist_mbid: String,
}

/// One entry in a user's city picker.
#[derive(Debug, Clone)]
pub struct EventCity {
    /// City name.
    pub city_name: String,
    /// City latitude.
    pub latitude: f64,
    /// City longitude.
    pub longitude: f64,
    /// Search radius in km.
    pub radius_km: f64,
}

/// One concert matched to one of the user's cities.
#[derive(Debug, Clone)]
pub struct MatchedConcert {
    /// The feed row.
    pub event: LiveEvent,
    /// Followed artist MBID.
    pub artist_mbid: String,
    /// City that matched.
    pub matched_city: String,
    /// Distance in km, or None for a coordinate-less name match.
    pub distance_km: Option<f64>,
}

/// Concerts-feed reads for one user.
pub trait EventsClient: Send + Sync {
    /// Candidate concerts in scope for the user (their follows, or the
    /// whole feed in library sweep scope).
    fn concerts_for_user<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<UserConcert>, ProviderError>>;

    /// The user's saved cities.
    fn cities<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<EventCity>, ProviderError>>;
}

// ---------------------------------------------------------------------------
// Fan-out/fan-in with per-source budgets
// ---------------------------------------------------------------------------

/// Max batch legs in flight at once, kept from v2 (`_ENRICH_CONCURRENCY`):
/// the service never pressures a provider beyond what one small batch needs.
pub const ENRICH_CONCURRENCY: usize = 2;
/// Per-leg timeout in seconds, kept from v2 (`_METADATA_TIMEOUT`).
pub const METADATA_TIMEOUT_SECS: u64 = 30;
/// Top release groups summed for one artist popularity figure, kept from v2.
const ARTIST_TOP_RELEASE_COUNT: usize = 5;

/// Per-source budgets for one aggregation.
#[derive(Debug, Clone, Copy)]
pub struct SourceBudgets {
    /// Timeout per provider leg.
    pub per_source: Duration,
    /// Max batch legs in flight at once.
    pub max_inflight: usize,
}

impl Default for SourceBudgets {
    fn default() -> Self {
        Self {
            per_source: Duration::from_secs(METADATA_TIMEOUT_SECS),
            max_inflight: ENRICH_CONCURRENCY,
        }
    }
}

/// Run one leg under its source budget. A timeout becomes a provider
/// failure like any other: the caller degrades the leg.
pub async fn with_budget<T>(
    source: &'static str,
    budget: Duration,
    leg: impl Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    match tokio::time::timeout(budget, leg).await {
        Ok(outcome) => outcome,
        Err(_) => Err(ProviderError::new(
            source,
            format!("exceeded {}s budget", budget.as_secs()),
        )),
    }
}

/// Run batch legs with at most `max_inflight` in flight, preserving input
/// order (v2 `_gather_bounded`). Each entry is independent: one leg failure
/// never cancels or poisons the rest.
pub async fn gather_bounded<T, F, Fut>(
    source: &'static str,
    calls: Vec<F>,
    max_inflight: usize,
) -> Vec<Result<T, ProviderError>>
where
    T: Send + 'static,
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, ProviderError>> + Send + 'static,
{
    let semaphore = Arc::new(tokio::sync::Semaphore::new(max_inflight.max(1)));
    let mut handles = Vec::with_capacity(calls.len());
    for call in calls {
        let permit_slot = semaphore.clone();
        handles.push(tokio::spawn(async move {
            let _permit = permit_slot.acquire_owned().await;
            call().await
        }));
    }
    let mut outcomes = Vec::with_capacity(handles.len());
    for handle in handles {
        match handle.await {
            Ok(outcome) => outcomes.push(outcome),
            Err(join_error) => outcomes.push(Err(ProviderError::new(
                source,
                format!("batch leg ended early: {join_error}"),
            ))),
        }
    }
    outcomes
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

/// Preferred popularity source behind search counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PreferredCounts {
    /// ListenBrainz first, Last.fm when ListenBrainz is unavailable.
    #[default]
    ListenBrainz,
    /// Last.fm first, ListenBrainz when Last.fm is unavailable.
    LastFm,
}

/// Fixed machine code for a degraded enrichment source.
const ENRICHMENT_UNAVAILABLE_CODE: &str = "ENRICHMENT_UNAVAILABLE";
/// Fixed human message for a degraded enrichment source.
const ENRICHMENT_UNAVAILABLE_MESSAGE: &str = "Enrichment temporarily unavailable";

/// One degradation note for a source. Provider detail never reaches the
/// wire: the code and message are fixed, only the source varies.
fn degradation_note(source: &str) -> Degradation {
    Degradation {
        source: source.to_owned(),
        code: ENRICHMENT_UNAVAILABLE_CODE.to_owned(),
        message: ENRICHMENT_UNAVAILABLE_MESSAGE.to_owned(),
    }
}

/// Enrichment over provider clients: one aggregator fans each request out
/// to the providers and back into one response.
pub struct EnrichmentAggregator {
    mb: Arc<dyn MusicBrainzClient>,
    lb: Arc<dyn ListenBrainzClient>,
    lfm: Arc<dyn LastFmClient>,
    lyrics: Arc<dyn LyricsClient>,
    events: Arc<dyn EventsClient>,
    budgets: SourceBudgets,
    preferred: PreferredCounts,
}

impl EnrichmentAggregator {
    /// Build an aggregator over provider clients.
    pub fn new(
        mb: Arc<dyn MusicBrainzClient>,
        lb: Arc<dyn ListenBrainzClient>,
        lfm: Arc<dyn LastFmClient>,
        lyrics: Arc<dyn LyricsClient>,
        events: Arc<dyn EventsClient>,
    ) -> Self {
        Self {
            mb,
            lb,
            lfm,
            lyrics,
            events,
            budgets: SourceBudgets::default(),
            preferred: PreferredCounts::default(),
        }
    }

    /// Override the per-source budgets (tests pin short timeouts).
    pub fn with_budgets(mut self, budgets: SourceBudgets) -> Self {
        self.budgets = budgets;
        self
    }

    /// Override the preferred popularity source.
    pub fn with_preferred(mut self, preferred: PreferredCounts) -> Self {
        self.preferred = preferred;
        self
    }

    /// Pick the popularity source: the preferred source when available,
    /// else the other one, else none (v2 `_get_enrichment_source`).
    fn counts_source(&self) -> EnrichmentSource {
        let lb_up = self.lb.is_available();
        let lfm_up = self.lfm.is_available();
        if !lb_up && !lfm_up {
            return EnrichmentSource::None;
        }
        match self.preferred {
            PreferredCounts::LastFm if lfm_up => EnrichmentSource::Lastfm,
            PreferredCounts::ListenBrainz if lb_up => EnrichmentSource::Listenbrainz,
            _ if lb_up => EnrichmentSource::Listenbrainz,
            _ => EnrichmentSource::Lastfm,
        }
    }

    /// Enrich one mixed batch of artists and albums (v2 `enrich_batch`).
    ///
    /// Popularity is stale-cache-acceptable, so this never fails: a dead
    /// source degrades its ids to bare echoes with a note. Blank ids are
    /// skipped and buckets cap at [`MAX_ENRICHMENT_PER_BUCKET`], matching
    /// the service trim so direct callers behave the same.
    pub async fn enrich_search_batch(&self, request: EnrichmentBatchRequest) -> EnrichmentResponse {
        let source = self.counts_source();
        let artists: Vec<_> = request
            .artists
            .into_iter()
            .filter(|item| !item.musicbrainz_id.trim().is_empty())
            .take(MAX_ENRICHMENT_PER_BUCKET)
            .collect();
        let albums: Vec<_> = request
            .albums
            .into_iter()
            .filter(|item| !item.musicbrainz_id.trim().is_empty())
            .take(MAX_ENRICHMENT_PER_BUCKET)
            .collect();
        let mut degradations: Vec<Degradation> = Vec::new();

        let enriched_artists = match source {
            EnrichmentSource::None => artists
                .into_iter()
                .map(|item| ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: None,
                })
                .collect(),
            EnrichmentSource::Listenbrainz => {
                self.enrich_artists_listenbrainz(artists, &mut degradations)
                    .await
            }
            EnrichmentSource::Lastfm => {
                self.enrich_artists_lastfm(artists, &mut degradations).await
            }
        };

        let enriched_albums = match source {
            EnrichmentSource::Listenbrainz => {
                self.enrich_albums_listenbrainz(albums, &mut degradations)
                    .await
            }
            EnrichmentSource::Lastfm => self.enrich_albums_lastfm(albums, &mut degradations).await,
            EnrichmentSource::None => albums
                .into_iter()
                .map(|item| AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count: None,
                })
                .collect(),
        };

        EnrichmentResponse {
            artists: enriched_artists,
            albums: enriched_albums,
            source,
            degradations,
        }
    }

    /// Artist popularity through ListenBrainz: each artist sums its top
    /// release groups (v2 `_enrich_artist`), bounded two abreast. One dead
    /// leg degrades its row only; the source note records once.
    async fn enrich_artists_listenbrainz(
        &self,
        artists: Vec<crate::reads::search::models::ArtistEnrichmentRequest>,
        degradations: &mut Vec<Degradation>,
    ) -> Vec<ArtistEnrichment> {
        let calls: Vec<_> = artists
            .iter()
            .map(|item| {
                let lb = self.lb.clone();
                let budgets = self.budgets;
                let mbid = item.musicbrainz_id.clone();
                move || async move {
                    with_budget(
                        ProviderSource::ListenBrainz.name(),
                        budgets.per_source,
                        lb.artist_top_release_groups(&mbid, ARTIST_TOP_RELEASE_COUNT),
                    )
                    .await
                }
            })
            .collect();
        let outcomes = gather_bounded(
            ProviderSource::ListenBrainz.name(),
            calls,
            self.budgets.max_inflight,
        )
        .await;
        let mut enriched = Vec::with_capacity(artists.len());
        for (item, outcome) in artists.into_iter().zip(outcomes) {
            match outcome {
                Ok(top) if !top.is_empty() => {
                    let listen_count: i64 = top.iter().map(|row| row.listen_count).sum();
                    enriched.push(ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: None,
                        listen_count: Some(listen_count),
                    });
                }
                Ok(_) => enriched.push(ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: None,
                }),
                Err(error) => {
                    tracing::debug!(
                        source = error.source,
                        cause = %error.message,
                        "artist popularity degraded",
                    );
                    record_once(degradations, ProviderSource::ListenBrainz.name());
                    enriched.push(ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: None,
                        listen_count: None,
                    });
                }
            }
        }
        enriched
    }

    /// Artist popularity through Last.fm: listener counts by name with an
    /// MBID hint (v2 `_enrich_artist`). A row without a name stays bare:
    /// Last.fm cannot look it up, and that is absence, not failure.
    async fn enrich_artists_lastfm(
        &self,
        artists: Vec<crate::reads::search::models::ArtistEnrichmentRequest>,
        degradations: &mut Vec<Degradation>,
    ) -> Vec<ArtistEnrichment> {
        let mut enriched = Vec::with_capacity(artists.len());
        let mut lookup_at: Vec<usize> = Vec::new();
        for (index, item) in artists.iter().enumerate() {
            if !item.name.trim().is_empty() {
                lookup_at.push(index);
            }
        }
        let calls: Vec<_> = lookup_at
            .iter()
            .map(|index| {
                let item = &artists[*index];
                let lfm = self.lfm.clone();
                let budgets = self.budgets;
                let name = item.name.clone();
                let mbid = item.musicbrainz_id.clone();
                move || async move {
                    with_budget(
                        ProviderSource::LastFm.name(),
                        budgets.per_source,
                        lfm.artist_info(&name, &mbid),
                    )
                    .await
                }
            })
            .collect();
        let outcomes = gather_bounded(
            ProviderSource::LastFm.name(),
            calls,
            self.budgets.max_inflight,
        )
        .await;
        let mut outcomes = outcomes.into_iter();
        let mut looked_up = lookup_at.into_iter().peekable();
        for (index, item) in artists.into_iter().enumerate() {
            let is_lookup = looked_up.peek() == Some(&index);
            if !is_lookup {
                enriched.push(ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: None,
                });
                continue;
            }
            let _ = looked_up.next();
            match outcomes.next() {
                Some(Ok(Some(info))) => enriched.push(ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: info.listeners,
                }),
                Some(Ok(None)) => enriched.push(ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: None,
                }),
                Some(Err(error)) => {
                    tracing::debug!(
                        source = error.source,
                        cause = %error.message,
                        "artist popularity degraded",
                    );
                    record_once(degradations, ProviderSource::LastFm.name());
                    enriched.push(ArtistEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        release_group_count: None,
                        listen_count: None,
                    });
                }
                None => enriched.push(ArtistEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    release_group_count: None,
                    listen_count: None,
                }),
            }
        }
        enriched
    }

    /// Album popularity through ListenBrainz: one batch call for the whole
    /// bucket (v2 `get_release_group_popularity_batch`). A dead call
    /// degrades every row to a bare echo with one note.
    async fn enrich_albums_listenbrainz(
        &self,
        albums: Vec<crate::reads::search::models::AlbumEnrichmentRequest>,
        degradations: &mut Vec<Degradation>,
    ) -> Vec<AlbumEnrichment> {
        let mbids: Vec<String> = albums
            .iter()
            .map(|item| item.musicbrainz_id.clone())
            .collect();
        let counts = match with_budget(
            ProviderSource::ListenBrainz.name(),
            self.budgets.per_source,
            self.lb.release_group_popularity_batch(&mbids),
        )
        .await
        {
            Ok(counts) => counts,
            Err(error) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "album popularity degraded",
                );
                record_once(degradations, ProviderSource::ListenBrainz.name());
                HashMap::new()
            }
        };
        albums
            .into_iter()
            .map(|item| {
                let listen_count = counts.get(&item.musicbrainz_id).copied();
                AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count,
                }
            })
            .collect()
    }

    /// Album popularity through Last.fm: per-row lookups by artist and
    /// album names (v2 `_enrich_album_lastfm`). Rows without both names
    /// stay bare: Last.fm cannot look them up.
    async fn enrich_albums_lastfm(
        &self,
        albums: Vec<crate::reads::search::models::AlbumEnrichmentRequest>,
        degradations: &mut Vec<Degradation>,
    ) -> Vec<AlbumEnrichment> {
        let mut lookup_at: Vec<usize> = Vec::new();
        for (index, item) in albums.iter().enumerate() {
            if !item.artist_name.trim().is_empty() && !item.album_name.trim().is_empty() {
                lookup_at.push(index);
            }
        }
        let calls: Vec<_> = lookup_at
            .iter()
            .map(|index| {
                let item = &albums[*index];
                let lfm = self.lfm.clone();
                let budgets = self.budgets;
                let artist = item.artist_name.clone();
                let album = item.album_name.clone();
                let mbid = item.musicbrainz_id.clone();
                move || async move {
                    with_budget(
                        ProviderSource::LastFm.name(),
                        budgets.per_source,
                        lfm.album_info(&artist, &album, Some(mbid.as_str())),
                    )
                    .await
                }
            })
            .collect();
        let outcomes = gather_bounded(
            ProviderSource::LastFm.name(),
            calls,
            self.budgets.max_inflight,
        )
        .await;
        let mut outcomes = outcomes.into_iter();
        let mut looked_up = lookup_at.into_iter().peekable();
        let mut enriched = Vec::with_capacity(albums.len());
        for (index, item) in albums.into_iter().enumerate() {
            let is_lookup = looked_up.peek() == Some(&index);
            if !is_lookup {
                enriched.push(AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count: None,
                });
                continue;
            }
            let _ = looked_up.next();
            match outcomes.next() {
                Some(Ok(Some(info))) => enriched.push(AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count: info.listeners,
                }),
                Some(Ok(None)) => enriched.push(AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count: None,
                }),
                Some(Err(error)) => {
                    tracing::debug!(
                        source = error.source,
                        cause = %error.message,
                        "album popularity degraded",
                    );
                    record_once(degradations, ProviderSource::LastFm.name());
                    enriched.push(AlbumEnrichment {
                        musicbrainz_id: item.musicbrainz_id,
                        track_count: None,
                        listen_count: None,
                    });
                }
                None => enriched.push(AlbumEnrichment {
                    musicbrainz_id: item.musicbrainz_id,
                    track_count: None,
                    listen_count: None,
                }),
            }
        }
        enriched
    }

    /// Enrich one album page: MusicBrainz identity plus popularity and
    /// prose legs fanned out together.
    ///
    /// Identity is identity-critical: a dead MusicBrainz fails the page
    /// with [`AlbumPageError::IdentityUnavailable`], and the local-degraded
    /// fallback for owned albums composes above this call (the page slice
    /// owns the catalog). Popularity and prose are stale-cache-acceptable:
    /// a dead ListenBrainz or Last.fm leaves gaps with notes, and the page
    /// still completes.
    pub async fn enrich_album_page(
        &self,
        input: AlbumPageInput,
    ) -> Result<AlbumPage, AlbumPageError> {
        let mbid = input.rg_mbid.clone();
        let mb_leg = with_budget(
            ProviderSource::MusicBrainz.name(),
            self.budgets.per_source,
            self.mb.release_group(&mbid),
        );
        let batch_ids = vec![input.rg_mbid.clone()];
        let lb_leg = with_budget(
            ProviderSource::ListenBrainz.name(),
            self.budgets.per_source,
            self.lb.release_group_popularity_batch(&batch_ids),
        );
        let lfm_leg = if self.lfm.is_available()
            && !input.artist_name.trim().is_empty()
            && !input.album_title.trim().is_empty()
        {
            Some(with_budget(
                ProviderSource::LastFm.name(),
                self.budgets.per_source,
                self.lfm.album_info(
                    &input.artist_name,
                    &input.album_title,
                    Some(input.rg_mbid.as_str()),
                ),
            ))
        } else {
            None
        };
        let (identity, popularity, prose) = match lfm_leg {
            Some(leg) => {
                let (identity, popularity, prose) = tokio::join!(mb_leg, lb_leg, leg);
                (identity, popularity, Some(prose))
            }
            None => {
                let (identity, popularity) = tokio::join!(mb_leg, lb_leg);
                (identity, popularity, None)
            }
        };

        let identity = match identity {
            Ok(identity) => identity,
            Err(error) => {
                debug_assert_eq!(
                    routing(Operation::AlbumIdentity, ProviderSource::MusicBrainz),
                    Routing::IdentityCritical
                );
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "album identity unavailable",
                );
                return Err(AlbumPageError::IdentityUnavailable {
                    source: ProviderSource::MusicBrainz.name(),
                });
            }
        };

        let mut degradations: Vec<Degradation> = Vec::new();
        let listen_count = match popularity {
            Ok(counts) => counts.get(&input.rg_mbid).copied(),
            Err(error) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "album popularity degraded",
                );
                record_once(&mut degradations, ProviderSource::ListenBrainz.name());
                None
            }
        };
        let (bio, tags) = match prose {
            None => (None, Vec::new()),
            Some(Ok(Some(info))) => (info.summary, info.tags),
            Some(Ok(None)) => (None, Vec::new()),
            Some(Err(error)) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "album prose degraded",
                );
                record_once(&mut degradations, ProviderSource::LastFm.name());
                (None, Vec::new())
            }
        };

        Ok(AlbumPage {
            identity,
            listen_count,
            bio,
            tags,
            degradations,
        })
    }

    /// Fetch lyrics for one track. Lyrics are stale-cache-acceptable: a
    /// dead LRCLIB degrades to no lyrics with a note, never an error, and
    /// a track the provider never held is absence (`found` false), also
    /// with no note.
    pub async fn fetch_lyrics(&self, query: &LyricsQuery) -> LyricsOutcome {
        let lookup = match with_budget(
            ProviderSource::Lrclib.name(),
            self.budgets.per_source,
            self.lyrics.exact_lyrics(query),
        )
        .await
        {
            Ok(lookup) => lookup,
            Err(error) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "lyrics degraded",
                );
                return LyricsOutcome {
                    doc: None,
                    degradation: Some(degradation_note(ProviderSource::Lrclib.name())),
                };
            }
        };
        if !lookup.found {
            return LyricsOutcome {
                doc: None,
                degradation: None,
            };
        }
        match lyric_doc(&lookup) {
            Ok(doc) => LyricsOutcome {
                doc,
                degradation: None,
            },
            Err(cause) => {
                tracing::debug!(cause = %cause, "lyrics payload unusable; degrading");
                LyricsOutcome {
                    doc: None,
                    degradation: Some(degradation_note(ProviderSource::Lrclib.name())),
                }
            }
        }
    }

    /// Concerts near one user: feed rows narrowed to their saved cities
    /// (v2 `EventsService::list_concerts`). A user with no saved cities
    /// sees an empty list, and a dead feed degrades to empty with a note.
    pub async fn lookup_events(&self, user_id: &str) -> EventsOutcome {
        let cities = match with_budget(
            ProviderSource::EventsFeed.name(),
            self.budgets.per_source,
            self.events.cities(user_id),
        )
        .await
        {
            Ok(cities) => cities,
            Err(error) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "events degraded",
                );
                return EventsOutcome {
                    concerts: Vec::new(),
                    degradation: Some(degradation_note(ProviderSource::EventsFeed.name())),
                };
            }
        };
        if cities.is_empty() {
            return EventsOutcome {
                concerts: Vec::new(),
                degradation: None,
            };
        }
        let concerts = match with_budget(
            ProviderSource::EventsFeed.name(),
            self.budgets.per_source,
            self.events.concerts_for_user(user_id),
        )
        .await
        {
            Ok(concerts) => concerts,
            Err(error) => {
                tracing::debug!(
                    source = error.source,
                    cause = %error.message,
                    "events degraded",
                );
                return EventsOutcome {
                    concerts: Vec::new(),
                    degradation: Some(degradation_note(ProviderSource::EventsFeed.name())),
                };
            }
        };
        EventsOutcome {
            concerts: filter_to_cities(&concerts, &cities),
            degradation: None,
        }
    }
}

/// Record one degradation note per source: repeated leg failures note once.
fn record_once(degradations: &mut Vec<Degradation>, source: &str) {
    if !degradations.iter().any(|note| note.source == source) {
        degradations.push(degradation_note(source));
    }
}

// ---------------------------------------------------------------------------
// Album page shapes
// ---------------------------------------------------------------------------

/// Input behind one album page enrichment.
#[derive(Debug, Clone)]
pub struct AlbumPageInput {
    /// Release-group MBID.
    pub rg_mbid: String,
    /// Album artist name (Last.fm lookup key).
    pub artist_name: String,
    /// Album title (Last.fm lookup key).
    pub album_title: String,
}

/// One enriched album page: identity plus optional legs.
#[derive(Debug, Clone)]
pub struct AlbumPage {
    /// MusicBrainz identity (identity-critical).
    pub identity: ReleaseGroupCore,
    /// Listen count, when a popularity leg answered.
    pub listen_count: Option<i64>,
    /// Prose summary, when Last.fm answered.
    pub bio: Option<String>,
    /// Tag names, when a prose leg answered.
    pub tags: Vec<String>,
    /// Legs that degraded while answering; usually empty.
    pub degradations: Vec<Degradation>,
}

/// Ways album-page enrichment fails. Only identity failure fails: every
/// other leg degrades inside [`AlbumPage::degradations`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AlbumPageError {
    /// MusicBrainz could not resolve identity.
    IdentityUnavailable {
        /// Source that failed (`musicbrainz`).
        source: &'static str,
    },
}

impl std::fmt::Display for AlbumPageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::IdentityUnavailable { source } => {
                write!(f, "{source} identity unavailable")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Lyrics shaping (v2 `lrclib_repository.py`)
// ---------------------------------------------------------------------------

/// Max lyrics payload in bytes, kept from v2 (`_MAX_LYRICS_BYTES`).
const MAX_LYRICS_BYTES: usize = 2 * 1024 * 1024;
/// Max lyrics payload in characters, kept from v2 (`_MAX_LYRICS_CHARACTERS`).
const MAX_LYRICS_CHARACTERS: usize = 1_000_000;

/// One lyrics outcome: a document, or honest absence with an optional note.
#[derive(Debug, Clone)]
pub struct LyricsOutcome {
    /// Lyrics document, when the provider held usable lyrics.
    pub doc: Option<LyricDoc>,
    /// Degradation note, when the provider failed.
    pub degradation: Option<Degradation>,
}

/// Shape an LRCLIB lookup into a lyrics document. Oversized or unusable
/// payloads fail so the caller degrades; empty-but-found payloads read as
/// absence.
fn lyric_doc(lookup: &LyricsLookup) -> Result<Option<LyricDoc>, String> {
    let plain = lookup.plain.as_deref().map(str::trim).unwrap_or("");
    let synced = lookup.synced.as_deref().map(str::trim).unwrap_or("");
    let bytes = lookup
        .plain
        .as_ref()
        .map(|text| text.len())
        .unwrap_or(0)
        .saturating_add(lookup.synced.as_ref().map(|text| text.len()).unwrap_or(0));
    if bytes > MAX_LYRICS_BYTES {
        return Err("lyrics payload exceeds the byte cap".to_owned());
    }
    let characters: usize = plain.chars().count().saturating_add(synced.chars().count());
    if characters > MAX_LYRICS_CHARACTERS {
        return Err("lyrics payload exceeds the character cap".to_owned());
    }
    if synced.is_empty() && plain.is_empty() {
        return Ok(None);
    }
    if synced.is_empty() {
        return Ok(Some(LyricDoc {
            lines: plain
                .lines()
                .map(|line| (line.trim().to_owned(), None))
                .filter(|(text, _)| !text.is_empty())
                .collect(),
            synced: false,
        }));
    }
    let mut lines: Vec<(String, Option<i64>)> = Vec::new();
    for raw in synced.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match parse_lrc_line(line) {
            Some((text, start_ms)) => {
                if !text.is_empty() {
                    lines.push((text, Some(start_ms)));
                }
            }
            None => {
                if !line.starts_with('[') && !line.is_empty() {
                    lines.push((line.to_owned(), None));
                }
            }
        }
    }
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some(LyricDoc {
        lines,
        synced: true,
    }))
}

/// Parse one `[mm:ss.xx] text` LRC line into text and start milliseconds.
/// Returns None for tag lines (`[ar:...]`) and malformed stamps.
fn parse_lrc_line(line: &str) -> Option<(String, i64)> {
    if !line.starts_with('[') {
        return None;
    }
    let end = line.find(']')?;
    let stamp = &line[1..end];
    let (minutes_raw, rest) = stamp.split_once(':')?;
    let minutes: i64 = minutes_raw.parse().ok()?;
    let (seconds_raw, fraction_raw) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => match rest.split_once(':') {
            Some(_) => return None,
            None => (rest, None),
        },
    };
    let seconds: i64 = seconds_raw.parse().ok()?;
    if minutes < 0 || !(0..60).contains(&seconds) {
        return None;
    }
    let fraction_ms: i64 = match fraction_raw {
        None => 0,
        Some(digits) => {
            if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let value: i64 = digits.parse().ok()?;
            match digits.len() {
                1 => value * 100,
                2 => value * 10,
                _ => value,
            }
        }
    };
    let text = line[end + 1..].trim().to_owned();
    Some((text, minutes * 60_000 + seconds * 1_000 + fraction_ms))
}

// ---------------------------------------------------------------------------
// Events matching (v2 `events_service.py`)
// ---------------------------------------------------------------------------

/// Max cities honored per user, kept from v2 (`MAX_CITIES`).
const MAX_CITIES: usize = 50;
/// Min search radius in km, kept from v2 (`MIN_RADIUS_KM`).
const MIN_RADIUS_KM: f64 = 1.0;
/// Max search radius in km, kept from v2 (`MAX_RADIUS_KM`).
const MAX_RADIUS_KM: f64 = 500.0;

/// One events outcome: matched concerts with an optional note.
#[derive(Debug, Clone)]
pub struct EventsOutcome {
    /// Concerts inside the user's cities, in feed order.
    pub concerts: Vec<MatchedConcert>,
    /// Degradation note, when the feed failed.
    pub degradation: Option<Degradation>,
}

/// Great-circle distance in km (v2 `haversine_km`). The clamp keeps
/// near-antipodal pairs from pushing past 1.0 through float error.
fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    const RADIUS_KM: f64 = 6371.0;
    let phi1 = lat1.to_radians();
    let phi2 = lat2.to_radians();
    let delta_phi = (lat2 - lat1).to_radians();
    let delta_lambda = (lon2 - lon1).to_radians();
    let a = (delta_phi / 2.0).sin().powi(2)
        + phi1.cos() * phi2.cos() * (delta_lambda / 2.0).sin().powi(2);
    2.0 * RADIUS_KM * a.clamp(0.0, 1.0).sqrt().asin()
}

/// Match one event against one city: the distance when the event falls
/// inside the radius, the sentinel -1.0 for a coordinate-less name match,
/// or None for no match (v2 `_match_city`).
fn match_city(event: &LiveEvent, city: &EventCity) -> Option<f64> {
    match (event.latitude, event.longitude) {
        (Some(lat), Some(lon)) => {
            let distance = haversine_km(city.latitude, city.longitude, lat, lon);
            if distance <= city.radius_km {
                Some(distance)
            } else {
                None
            }
        }
        _ => match event.city.as_deref() {
            Some(name) if name.trim().eq_ignore_ascii_case(city.city_name.trim()) => Some(-1.0),
            _ => None,
        },
    }
}

/// Narrow feed rows to the user's cities (v2 `filter_to_cities`). Each
/// concert keeps its best (nearest) city; a coordinate-less name match
/// reports no distance.
fn filter_to_cities(concerts: &[UserConcert], cities: &[EventCity]) -> Vec<MatchedConcert> {
    let clamped: Vec<EventCity> = cities
        .iter()
        .take(MAX_CITIES)
        .map(|city| EventCity {
            city_name: city.city_name.clone(),
            latitude: city.latitude,
            longitude: city.longitude,
            radius_km: city.radius_km.clamp(MIN_RADIUS_KM, MAX_RADIUS_KM),
        })
        .collect();
    let mut matched = Vec::new();
    for concert in concerts {
        let mut best: Option<(f64, &EventCity)> = None;
        for city in &clamped {
            let Some(distance) = match_city(&concert.event, city) else {
                continue;
            };
            let is_better = best.as_ref().is_none_or(|(held, _)| distance < *held);
            if is_better {
                best = Some((distance, city));
            }
        }
        if let Some((distance, city)) = best {
            matched.push(MatchedConcert {
                event: concert.event.clone(),
                artist_mbid: concert.artist_mbid.clone(),
                matched_city: city.city_name.clone(),
                distance_km: if distance < 0.0 {
                    None
                } else {
                    Some((distance * 10.0).round() / 10.0)
                },
            });
        }
    }
    matched
}

// ---------------------------------------------------------------------------
// Port adapters: providers behind the stage-4 seams
// ---------------------------------------------------------------------------

/// Search enrichment behind the stage-4 [`EnrichmentPort`] seam.
///
/// The aggregator never fails a counts batch (popularity is
/// stale-cache-acceptable), so this adapter always answers `Ok` with
/// degradation notes inside; the service-level `Err` mapping stays for
/// genuinely broken wiring. Production wires this in `ReadsSetup::build`
/// over the integrator's role adapters; the `None` pair keeps
/// `UnconfiguredEnrichment` for hermetic tests.
pub struct AggregatingEnrichment {
    aggregator: Arc<EnrichmentAggregator>,
}

impl AggregatingEnrichment {
    /// Aggregate batches through these provider clients.
    pub fn new(aggregator: Arc<EnrichmentAggregator>) -> Self {
        Self { aggregator }
    }
}

impl EnrichmentPort for AggregatingEnrichment {
    fn enrich_batch(
        &self,
        request: EnrichmentBatchRequest,
    ) -> crate::reads::search::ports::BoxFuture<
        '_,
        Result<EnrichmentResponse, crate::reads::search::ports::EnrichmentPortError>,
    > {
        Box::pin(async move { Ok(self.aggregator.enrich_search_batch(request).await) })
    }
}

/// Provider lyrics behind the stage-4 [`LyricsPort`] seam.
///
/// The port only carries a track id, so the adapter resolves artist and
/// title through the catalog first: an unknown track reads as absent, and
/// a dead lyrics provider also reads as absent (lyrics are
/// stale-cache-acceptable) with the cause logged. Production wires this
/// in `ReadsSetup::build` over live LRCLIB; the `None` pair keeps the
/// empty memory lyrics for hermetic tests.
pub struct ProviderLyrics {
    catalog: Arc<dyn LibraryCatalog>,
    lyrics: Arc<dyn LyricsClient>,
    budgets: SourceBudgets,
}

impl ProviderLyrics {
    /// Fetch provider lyrics for catalog tracks.
    pub fn new(
        catalog: Arc<dyn LibraryCatalog>,
        lyrics: Arc<dyn LyricsClient>,
        budgets: SourceBudgets,
    ) -> Self {
        Self {
            catalog,
            lyrics,
            budgets,
        }
    }
}

impl LyricsPort for ProviderLyrics {
    fn get<'a>(
        &'a self,
        track_id: &'a str,
    ) -> crate::reads::library::stores::BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move {
            let track = self
                .catalog
                .get_track(track_id)
                .await
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            let Some(track) = track else {
                return Ok(None);
            };
            let query = LyricsQuery {
                artist: track.artist_name.clone(),
                title: track.title.clone(),
                album: Some(track.album_title.clone()),
                duration_secs: track.duration_seconds,
            };
            let lookup = match with_budget(
                ProviderSource::Lrclib.name(),
                self.budgets.per_source,
                self.lyrics.exact_lyrics(&query),
            )
            .await
            {
                Ok(lookup) => lookup,
                Err(error) => {
                    tracing::debug!(
                        source = error.source,
                        cause = %error.message,
                        "provider lyrics degraded to absence",
                    );
                    record_current(ProviderSource::Lrclib.name(), CoreStatus::Error, false);
                    return Ok(None);
                }
            };
            if !lookup.found {
                return Ok(None);
            }
            match lyric_doc(&lookup) {
                Ok(doc) => Ok(doc),
                Err(cause) => {
                    tracing::debug!(cause = %cause, "provider lyrics unusable; reading as absent");
                    record_current(ProviderSource::Lrclib.name(), CoreStatus::Error, false);
                    Ok(None)
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_status_wins() {
        assert_eq!(
            aggregate_status([IntegrationStatus::Ok, IntegrationStatus::Ok]),
            IntegrationStatus::Ok
        );
        assert_eq!(
            aggregate_status([IntegrationStatus::Ok, IntegrationStatus::Degraded]),
            IntegrationStatus::Degraded
        );
        assert_eq!(
            aggregate_status([IntegrationStatus::Degraded, IntegrationStatus::Error]),
            IntegrationStatus::Error
        );
    }

    #[test]
    fn only_musicbrainz_identity_is_critical() {
        use Operation as Op;
        use ProviderSource as Src;
        assert_eq!(
            routing(Op::AlbumIdentity, Src::MusicBrainz),
            Routing::IdentityCritical
        );
        assert_eq!(
            routing(Op::ArtistIdentity, Src::MusicBrainz),
            Routing::IdentityCritical
        );
        for operation in [
            Op::AlbumIdentity,
            Op::ArtistIdentity,
            Op::SearchCounts,
            Op::AlbumDetail,
            Op::ArtistDetail,
            Op::Lyrics,
            Op::Events,
        ] {
            for source in [
                Src::MusicBrainz,
                Src::ListenBrainz,
                Src::LastFm,
                Src::Lrclib,
                Src::EventsFeed,
            ] {
                let critical = matches!(operation, Op::AlbumIdentity | Op::ArtistIdentity)
                    && source == Src::MusicBrainz;
                assert_eq!(
                    routing(operation, source),
                    if critical {
                        Routing::IdentityCritical
                    } else {
                        Routing::StaleCacheAcceptable
                    },
                    "{operation:?} x {source:?}"
                );
            }
        }
    }

    #[test]
    fn haversine_matches_known_distance() {
        let london_to_paris = haversine_km(51.5074, -0.1278, 48.8566, 2.3522);
        assert!(
            (london_to_paris - 344.0).abs() < 1.0,
            "got {london_to_paris}"
        );
        assert!(haversine_km(0.0, 0.0, 0.0, 180.0).is_finite());
    }

    #[test]
    fn lrc_stamps_parse_to_milliseconds() {
        assert_eq!(
            parse_lrc_line("[01:02.34] hello"),
            Some(("hello".to_owned(), 62_340))
        );
        assert_eq!(parse_lrc_line("[00:00.5] x"), Some(("x".to_owned(), 500)));
        assert_eq!(parse_lrc_line("[ar:Someone]"), None);
        assert_eq!(parse_lrc_line("[99:99.99] x"), None);
        assert_eq!(parse_lrc_line("no stamp"), None);
    }
}
