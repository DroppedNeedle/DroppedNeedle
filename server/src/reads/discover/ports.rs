//! Ports behind discover: content, charts, previews, and stores.
//!
//! Production wires the adapters in [`adapters`](super::adapters): live
//! ListenBrainz charts, Deezer/iTunes previews, YouTube search, and honest
//! empty or "not available" answers for the shelves whose builders are not
//! ported yet. Tests run the fakes. Every fallible method returns a
//! [`ProviderFailure`]: provider detail stays in the log, never on the
//! wire.

use std::collections::HashMap;

pub use futures_util::future::BoxFuture;

use crate::reads::discover::models::{
    ChartRange, ChartSource, DiscoverActivityResponse, DiscoverQueuePreview,
    DiscoverQueueStatusResponse, DiscoverResponse, GenreDetailResponse, HomeResponse,
    IgnoredRelease, IntegrationStatus, NowPlayingEntry, PopularAlbumsPage, PreviewTrackItem,
    QueueEnrichment, QueueGenerateResponse, QueueIgnoreRequest, QueueItem, TrackPreviewResponse,
    TrendingArtistsPage, YouTubeQuotaResponse,
};

/// Why a port could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderFailure {
    /// A provider or store failed. The string is a log-only cause.
    Failed(String),
    /// The feature needs setup first (a key, an account). The string is a
    /// full sentence for the user.
    NotConfigured(String),
    /// The feature is not built in this version yet. The string is a full
    /// sentence for the user.
    NotBuilt(String),
    /// A usage limit is spent for now. The string is a full sentence for
    /// the user.
    Exhausted(String),
}

impl ProviderFailure {
    /// A provider or store failure with a log-only cause.
    pub fn failed(cause: impl Into<String>) -> Self {
        Self::Failed(cause.into())
    }

    /// A feature that needs setup first; `sentence` tells the user.
    pub fn not_configured(sentence: impl Into<String>) -> Self {
        Self::NotConfigured(sentence.into())
    }

    /// A feature not built yet; `sentence` tells the user.
    pub fn not_built(sentence: impl Into<String>) -> Self {
        Self::NotBuilt(sentence.into())
    }
}

impl std::fmt::Display for ProviderFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Failed(cause) => f.write_str(cause),
            Self::NotConfigured(sentence)
            | Self::NotBuilt(sentence)
            | Self::Exhausted(sentence) => f.write_str(sentence),
        }
    }
}

/// Clock seam. Loops and TTL math read time through this so tests pin it.
pub trait Clock: Send + Sync {
    /// Current unix timestamp in seconds.
    fn now_unix(&self) -> i64;
}

/// System clock for production.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Discover and home shelf content for one user.
pub trait DiscoverContent: Send + Sync {
    /// Cached discover shelves for the user, with section status attached.
    fn discover<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverResponse, ProviderFailure>>;
    /// Cached home shelves for the user.
    fn home<'a>(&'a self, user_id: &'a str)
    -> BoxFuture<'a, Result<HomeResponse, ProviderFailure>>;
    /// Integration availability behind the shelves.
    fn integration_status(&self) -> BoxFuture<'_, Result<IntegrationStatus, ProviderFailure>>;
    /// Whether local files back playback (refines the status row).
    fn has_local_files(&self) -> BoxFuture<'_, Result<bool, ProviderFailure>>;
    /// Record one discover interaction; returns the personalization cursor.
    fn record_activity<'a>(
        &'a self,
        user_id: &'a str,
        feature: &'a str,
        artist_mbid: Option<&'a str>,
        section: Option<&'a str>,
        provider: Option<&'a str>,
    ) -> BoxFuture<'a, Result<DiscoverActivityResponse, ProviderFailure>>;
    /// Trigger a background discover rebuild for the user.
    fn trigger_refresh<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>>;
}

/// What asked for a queue build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueTrigger {
    /// A person asked (generate, or an ignore that reshuffles the deck).
    Request {
        /// Rebuild even when the current deck is fresh.
        force: bool,
    },
    /// The background warm cycle. Skipped while the warm-cycle setting is
    /// off, and replaced by any request that arrives while it runs.
    Scheduled,
}

/// One deck as the queue route serves it.
#[derive(Debug, Clone)]
pub struct QueueDeck {
    /// Build id.
    pub queue_id: String,
    /// Deck cards in order.
    pub items: Vec<QueueItem>,
}

/// The queue deck: background builds, the ignore ledger, and the details
/// behind each card.
pub trait QueueStore: Send + Sync {
    /// The user's last built deck, stale or not, when there is one.
    fn current<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, Option<QueueDeck>>;
    /// Build a deck now, in the request, without keeping it. `count`
    /// overrides the configured queue size.
    fn build_now<'a>(
        &'a self,
        user_id: &'a str,
        count: Option<usize>,
    ) -> BoxFuture<'a, Result<QueueDeck, ProviderFailure>>;
    /// Where the user's background build stands.
    fn status<'a>(&'a self, user_id: &'a str) -> BoxFuture<'a, DiscoverQueueStatusResponse>;
    /// Start a background build, or report why none started.
    fn start_build<'a>(
        &'a self,
        user_id: &'a str,
        trigger: QueueTrigger,
    ) -> BoxFuture<'a, QueueGenerateResponse>;
    /// Record one ignored release for the user. Later decks skip it.
    fn ignore_release<'a>(
        &'a self,
        user_id: &'a str,
        release: &'a QueueIgnoreRequest,
    ) -> BoxFuture<'a, Result<(), ProviderFailure>>;
    /// The user's ignore ledger, newest first.
    fn ignored<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<IgnoredRelease>, ProviderFailure>>;
    /// Details behind one card: tags, date, country, bio, listens, video.
    fn enrich<'a>(
        &'a self,
        user_id: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<QueueEnrichment, ProviderFailure>>;
    /// An on-demand video preview behind one card.
    fn preview<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<DiscoverQueuePreview, ProviderFailure>>;
    /// The given release groups that the library already holds.
    fn validate<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<String>, ProviderFailure>>;
}

/// Chart pages and genre detail.
pub trait ChartsSource: Send + Sync {
    /// One trending-artists page.
    fn trending_artists(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<TrendingArtistsPage, ProviderFailure>>;
    /// One popular-albums page.
    fn popular_albums(
        &self,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'_, Result<PopularAlbumsPage, ProviderFailure>>;
    /// One your-top-albums page for the user.
    fn your_top_albums<'a>(
        &'a self,
        user_id: &'a str,
        range: ChartRange,
        limit: i64,
        offset: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<PopularAlbumsPage, ProviderFailure>>;
    /// Genre detail with owned and popular rows.
    fn genre_detail<'a>(
        &'a self,
        genre: &'a str,
        limit: i64,
        artist_offset: i64,
        album_offset: i64,
    ) -> BoxFuture<'a, Result<GenreDetailResponse, ProviderFailure>>;
}

/// Ordered album samples plus the provider that served them.
pub type AlbumSamples = (Vec<PreviewTrackItem>, Option<String>);

/// Keyless 30-second previews (Deezer, then iTunes).
pub trait PreviewSource: Send + Sync {
    /// Preview behind one track. `None` fields mean no provider had one.
    fn track_preview<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<TrackPreviewResponse, ProviderFailure>>;
    /// Ordered album samples plus the serving provider.
    fn album_preview<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        count: i64,
    ) -> BoxFuture<'a, Result<AlbumSamples, ProviderFailure>>;
}

/// YouTube lookups behind the queue deck.
pub trait YouTubeSource: Send + Sync {
    /// Whether the data API (quota'd) is configured.
    fn is_configured(&self) -> bool;
    /// Album video lookup. Returns the video id, when found.
    fn search_video<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>>;
    /// Track video lookup. Returns the video id, when found.
    fn search_track<'a>(
        &'a self,
        artist: &'a str,
        track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>>;
    /// Whether the pair resolves from cache.
    fn is_cached(&self, artist: &str, name: &str, track: bool) -> bool;
    /// Cache membership for deduped pairs, keyed `artist|track` (lowercased).
    fn cached_tracks(&self, pairs: &[(String, String)]) -> HashMap<String, bool>;
    /// Quota state. `None` when unconfigured (the route answers 404).
    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>>;
}

/// One discovery batch row.
#[derive(Debug, Clone)]
pub struct BatchRow {
    /// Batch id.
    pub id: String,
    /// Owning user id.
    pub owner_id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Album rows: (release_group_mbid, artist_mbid, album_name, artist_name,
    /// outcome, request_status, in_library).
    pub items: Vec<BatchItemRow>,
}

/// One album row inside a batch.
#[derive(Debug, Clone)]
pub struct BatchItemRow {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// MusicBrainz artist id.
    pub artist_mbid: String,
    /// Album name.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// `requested`, `skipped_in_library`, or `skipped_duplicate`.
    pub outcome: String,
    /// Request state, when requested.
    pub request_status: Option<String>,
    /// True when already owned.
    pub in_library: bool,
}

/// Discovery batch store. Reads are owner-scoped: foreign ids read as
/// missing so callers answer 404 without leaking existence.
pub trait BatchStore: Send + Sync {
    /// Create a batch for the user; returns the stored row.
    fn create(
        &self,
        owner_id: &str,
        name: &str,
        source_section: &str,
        items: Vec<BatchItemRow>,
    ) -> Result<BatchRow, ProviderFailure>;
    /// The user's batches, newest first.
    fn list_for_user(&self, owner_id: &str) -> Vec<BatchRow>;
    /// One batch, when owned by the user.
    fn get_for_user(&self, owner_id: &str, batch_id: &str) -> Option<BatchRow>;
    /// Remove one batch. Returns (removed, cancelled, kept) when owned.
    fn remove(
        &self,
        owner_id: &str,
        batch_id: &str,
        remove_albums: bool,
    ) -> Option<(i64, i64, i64)>;
}

/// Live now-playing presence, already privacy-projected per owner setting.
pub trait NowPlayingStore: Send + Sync {
    /// Current live sessions.
    fn snapshot(&self) -> Vec<NowPlayingEntry>;
}

/// Radio plans and playlist suggestions.
pub trait RadioPlanner: Send + Sync {
    /// Build a complete radio plan for the user.
    fn plan<'a>(
        &'a self,
        user_id: &'a str,
        seed_type: &'a str,
        seed_id: Option<&'a str>,
        mode: &'a str,
        count: i64,
        exclude: &'a [String],
    ) -> BoxFuture<'a, Result<crate::reads::discover::models::RadioPlanResponse, ProviderFailure>>;
    /// Build one radio shelf.
    fn shelf<'a>(
        &'a self,
        seed_type: &'a str,
        seed_id: &'a str,
        count: i64,
        source: ChartSource,
    ) -> BoxFuture<'a, Result<crate::reads::discover::models::ChartSection, ProviderFailure>>;
    /// Build suggestions for one playlist.
    fn playlist_suggestions<'a>(
        &'a self,
        user_id: &'a str,
        playlist_id: &'a str,
        count: i64,
        source: ChartSource,
    ) -> BoxFuture<
        'a,
        Result<crate::reads::discover::models::PlaylistSuggestionsResponse, ProviderFailure>,
    >;
}
