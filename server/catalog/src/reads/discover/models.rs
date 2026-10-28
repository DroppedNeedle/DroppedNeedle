//! Discover and home DTOs for `/api/v3`.
//!
//! Album and artist rows are the catalog's [`AlbumRef`] and [`ArtistRef`],
//! so a row reads the same on every page. Fields with a closed set of
//! values are enums.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use super::sections::{SectionKey, SectionZone};
pub use crate::reads::catalog::models::{AlbumRef, ArtistRef};
pub use droppedneedle_kernel::runtime_config::sections::ListenSource;

// ---------------------------------------------------------------------------
// Shared section items
// ---------------------------------------------------------------------------

/// One track inside a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TrackRef {
    /// Track title.
    pub title: String,
    /// MusicBrainz recording id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub musicbrainz_id: Option<String>,
    /// Artist name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    /// The artist's MusicBrainz id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
    /// Album title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Listen or play count behind the row, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// Last listen timestamp (RFC 3339), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened_at: Option<String>,
    /// Cover URL, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
}

/// One genre inside a section.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenreRef {
    /// Genre name.
    pub name: String,
    /// Listen or play count behind the row, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// Owned artists tagged with this genre, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_count: Option<i64>,
    /// MusicBrainz id of the seed artist behind the row, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_id: Option<String>,
}

/// One row inside a section, tagged by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SectionItem {
    /// An artist row.
    Artist(ArtistRef),
    /// An album row.
    Album(AlbumRef),
    /// A track row.
    Track(TrackRef),
    /// A genre row.
    Genre(GenreRef),
}

/// What a section's rows are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKind {
    /// Artist rows.
    Artists,
    /// Album rows.
    Albums,
    /// Track rows.
    Tracks,
    /// Genre rows.
    Genres,
}

/// Where a shelf's rows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShelfSource {
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
    /// The user's Jellyfin server.
    Jellyfin,
    /// MusicBrainz.
    Musicbrainz,
    /// The native library.
    Library,
    /// The user's own plays in DroppedNeedle.
    Local,
}

impl From<ListenSource> for ShelfSource {
    fn from(source: ListenSource) -> Self {
        match source {
            ListenSource::Listenbrainz => Self::Listenbrainz,
            ListenSource::Lastfm => Self::Lastfm,
        }
    }
}

/// The seed a radio shelf or plan starts from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RadioSeedKind {
    /// An artist MBID.
    Artist,
    /// A release-group MBID.
    Album,
    /// A genre name.
    Genre,
    /// A list of seed rows (radio plans only).
    Items,
}

impl RadioSeedKind {
    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Genre => "genre",
            Self::Items => "items",
        }
    }
}

/// A titled shelf of rows: the unit both home and discover render.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChartSection {
    /// Shelf title.
    pub title: String,
    /// What the rows are.
    #[serde(rename = "type")]
    pub kind: SectionKind,
    /// Shelf rows.
    pub items: Vec<SectionItem>,
    /// Where the rows came from, when the shelf names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ShelfSource>,
    /// Explanation shown in place of rows when the shelf could not fill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_message: Option<String>,
    /// Service the user must connect for the full shelf, when gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_service: Option<ShelfSource>,
    /// Radio seed kind, when the shelf is playable as radio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_seed_type: Option<RadioSeedKind>,
    /// Radio seed id, when playable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_seed_id: Option<String>,
}

/// A service a connect-a-service card points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "kebab-case")]
pub enum PromptService {
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
    /// Jellyfin.
    Jellyfin,
    /// A download client.
    DownloadClient,
}

/// A card nudging the user to connect a service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ServicePrompt {
    /// The service to connect.
    pub service: PromptService,
    /// Card title.
    pub title: String,
    /// Card body.
    pub description: String,
    /// Icon name.
    pub icon: String,
    /// Accent color token.
    pub color: String,
    /// Feature bullets.
    pub features: Vec<String>,
}

/// Which integrations back the shelves right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct IntegrationStatus {
    /// ListenBrainz charts and history are available.
    pub listenbrainz: bool,
    /// A Jellyfin source is connected.
    pub jellyfin: bool,
    /// A download client is configured.
    pub download_client: bool,
    /// YouTube playback is configured.
    pub youtube: bool,
    /// Last.fm is connected.
    pub lastfm: bool,
    /// A Navidrome source is connected.
    pub navidrome: bool,
    /// The YouTube data API (quota'd) is configured.
    pub youtube_api: bool,
    /// A Plex source is connected.
    pub plex: bool,
    /// The native library has files.
    pub library: bool,
    /// Local files back playback. Home-only; discover leaves it false.
    pub localfiles: bool,
}

/// One track of the weekly exploration playlist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WeeklyTrack {
    /// Track title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_name: String,
    /// MusicBrainz recording id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    /// MusicBrainz artist id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// MusicBrainz release-group id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group_mbid: Option<String>,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// Duration in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
}

/// The weekly exploration playlist shelf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WeeklyExploration {
    /// Shelf title.
    pub title: String,
    /// Playlist date (`YYYY-MM-DD`).
    pub playlist_date: String,
    /// Playlist tracks in order.
    pub tracks: Vec<WeeklyTrack>,
    /// Provider playlist URL, when there is one.
    pub source_url: String,
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

/// How a genre's mosaic art is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtworkKind {
    /// A collage of owned covers.
    Collage,
    /// A flat gradient.
    Gradient,
}

/// Genre mosaic art: a collage of owned covers or a flat gradient.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenreArtwork {
    /// How the art is drawn.
    pub kind: ArtworkKind,
    /// Artwork schema version.
    pub version: String,
    /// Owned covers composing the collage (empty for gradients).
    pub albums: Vec<GenreArtworkAlbum>,
}

/// One cover inside a genre collage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenreArtworkAlbum {
    /// Native album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Cover revision for cache busting.
    pub cover_version: i64,
    /// Album artist name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_artist_name: Option<String>,
}

/// A discover teaser on home: one seed artist plus similar artists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPreview {
    /// Seed artist name.
    pub seed_artist: String,
    /// Seed MusicBrainz artist id.
    pub seed_artist_mbid: String,
    /// Similar artists.
    pub items: Vec<ArtistRef>,
}

/// Home: its sections, in page order, plus the page-level fields. A
/// section with no rows is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct HomeResponse {
    /// The sections, in page order.
    pub sections: Vec<PageSection>,
    /// Connect-a-service cards.
    pub service_prompts: Vec<ServicePrompt>,
    /// Integration availability behind the shelves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<IntegrationStatus>,
    /// Mosaic art keyed by genre name.
    pub genre_artwork: HashMap<String, GenreArtwork>,
    /// Genre artwork schema version.
    pub genre_artwork_schema_version: String,
    /// Discover teaser: the first "because you listen to" shelf of the
    /// user's last discover page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discover_preview: Option<DiscoverPreview>,
    /// True while a fuller build runs in the background (the UI polls).
    pub refreshing: bool,
}

/// Owned plus popular rows behind one genre page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenreDetailResponse {
    /// Genre name as requested.
    pub genre: String,
    /// Mosaic art for the genre.
    pub genre_artwork: GenreArtwork,
    /// Owned rows for the genre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library: Option<GenreLibrarySection>,
    /// Popular rows for the genre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popular: Option<GenrePopularSection>,
    /// Total popular rows, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
}

/// Owned artists and albums for a genre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenreLibrarySection {
    /// Owned artists.
    pub artists: Vec<ArtistRef>,
    /// Owned albums.
    pub albums: Vec<AlbumRef>,
    /// Total owned artists.
    pub artist_count: i64,
    /// Total owned albums.
    pub album_count: i64,
}

/// Popular artists and albums for a genre.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct GenrePopularSection {
    /// Popular artists.
    pub artists: Vec<ArtistRef>,
    /// Popular albums.
    pub albums: Vec<AlbumRef>,
    /// More artists behind the offsets.
    pub has_more_artists: bool,
    /// More albums behind the offsets.
    pub has_more_albums: bool,
}

// ---------------------------------------------------------------------------
// Charts
// ---------------------------------------------------------------------------

/// A listening-statistics window: the chart pages' `range`, and the
/// ListenBrainz statistics range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StatsRange {
    /// This week.
    #[default]
    ThisWeek,
    /// This month.
    ThisMonth,
    /// This year.
    ThisYear,
    /// All time.
    AllTime,
}

impl StatsRange {
    /// Parse a `range` query value. Unknown values fail rather than fall
    /// back, which would hide client bugs.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "this_week" => Some(Self::ThisWeek),
            "this_month" => Some(Self::ThisMonth),
            "this_year" => Some(Self::ThisYear),
            "all_time" => Some(Self::AllTime),
            _ => None,
        }
    }

    /// Wire spelling, which is also ListenBrainz's.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisWeek => "this_week",
            Self::ThisMonth => "this_month",
            Self::ThisYear => "this_year",
            Self::AllTime => "all_time",
        }
    }

    /// Human label.
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisWeek => "This Week",
            Self::ThisMonth => "This Month",
            Self::ThisYear => "This Year",
            Self::AllTime => "All Time",
        }
    }
}

/// One page of a ranked chart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ChartPage<T> {
    /// Range behind this page.
    pub range_key: StatsRange,
    /// Human range label.
    pub label: String,
    /// Ranked rows for the page.
    pub items: Vec<T>,
    /// Offset behind this page.
    pub offset: i64,
    /// Limit behind this page.
    pub limit: i64,
    /// True when more rows follow.
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Discover
// ---------------------------------------------------------------------------

/// One "because you listen to" shelf: seed plus similar albums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BecauseYouListenTo {
    /// Seed artist name.
    pub seed_artist: String,
    /// Seed MusicBrainz artist id.
    pub seed_artist_mbid: String,
    /// Similar-albums shelf.
    pub section: ChartSection,
    /// Seed listen count backing the pick.
    pub listen_count: i64,
    /// Banner art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner_url: Option<String>,
    /// Wide thumbnail URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wide_thumb_url: Option<String>,
    /// Fanart URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fanart_url: Option<String>,
}

/// One personalized top pick.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopPickItem {
    /// Picked album.
    pub album: AlbumRef,
    /// Match score, 0-100.
    pub match_pct: i64,
    /// Human reasons behind the pick.
    pub reasons: Vec<String>,
    /// Seed artist name, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_artist: Option<String>,
}

/// The personalized top-picks shelf.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopPicksSection {
    /// Shelf title.
    pub title: String,
    /// Ranked picks.
    pub items: Vec<TopPickItem>,
    /// Provider behind the picks, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ShelfSource>,
    /// True while picks are trending-only because personalization has not
    /// resolved yet (the UI shows a "still personalising" hint).
    pub personalizing: bool,
}

/// How a listening service fared in a discover build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// It answered only in part.
    Degraded,
    /// Every read of it failed.
    Unavailable,
}

/// One section of a page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PageSection {
    /// Which section it is.
    pub key: SectionKey,
    /// The zone it is shown in.
    pub zone: SectionZone,
    /// Its rows.
    pub content: SectionContent,
}

/// A section's rows, tagged by `shape`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "shape", rename_all = "snake_case")]
pub enum SectionContent {
    /// One shelf.
    Shelf {
        /// The shelf.
        shelf: ChartSection,
    },
    /// Several shelves (Daily Mixes, radio).
    Shelves {
        /// The shelves.
        shelves: Vec<ChartSection>,
    },
    /// The "because you listen to" shelves.
    Because {
        /// One per seed artist.
        entries: Vec<BecauseYouListenTo>,
    },
    /// Top Picks.
    Picks {
        /// The picks.
        picks: TopPicksSection,
    },
    /// The weekly exploration playlist.
    Weekly {
        /// The playlist.
        weekly: WeeklyExploration,
    },
}

/// Discover: its sections, in page order, plus the page-level fields. A
/// section with no rows is left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverResponse {
    /// The sections, in page order.
    pub sections: Vec<PageSection>,
    /// Whether the queue deck is enabled for the user.
    pub discover_queue_enabled: bool,
    /// Integration availability behind the shelves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<IntegrationStatus>,
    /// Connect-a-service cards.
    pub service_prompts: Vec<ServicePrompt>,
    /// Mosaic art keyed by genre name.
    pub genre_artwork: HashMap<String, GenreArtwork>,
    /// Genre artwork schema version.
    pub genre_artwork_schema_version: String,
    /// Build timestamp (unix seconds), when built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<i64>,
    /// Refresh-start timestamp (unix seconds), when refreshing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_started_at: Option<i64>,
    /// True while a fuller build runs in the background (the UI polls).
    pub refreshing: bool,
    /// Listening services that failed during the build, when any did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<HashMap<ListenSource, ServiceState>>,
}

/// Body recording one discover interaction for personalization.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverActivityRequest {
    /// Surface the interaction happened on (`home`, `discover`, `queue`, `artist`).
    pub feature: String,
    /// Artist the interaction concerned, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Artist-page section (`similar`, `top_songs`, `top_albums`), when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<String>,
    /// Provider behind the row, when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<ListenSource>,
}

/// Personalization cursor after recording activity.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverActivityResponse {
    /// The MusicBrainz source tier the activity was recorded under.
    pub source_mode: String,
    /// The MusicBrainz source id.
    pub source_id: String,
    /// The MusicBrainz source generation (bumps when the source changes).
    pub generation: i64,
}

/// Acknowledgement of a triggered background refresh.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RefreshResponse {
    /// Always `ok` when the trigger lands.
    pub status: String,
    /// Human message.
    pub message: String,
}

// ---------------------------------------------------------------------------
// Queue
// ---------------------------------------------------------------------------

/// One lightweight queue card: the deck renders these before enrichment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct QueueItemLight {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// Album title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// The artist's MusicBrainz id.
    pub artist_id: String,
    /// Human reason behind the pick.
    pub recommendation_reason: String,
    /// Cover URL, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// True for wildcard (out-of-profile) picks.
    pub is_wildcard: bool,
    /// True when already owned.
    pub in_library: bool,
}

/// Enrichment behind one queue card.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueEnrichment {
    /// MusicBrainz artist id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Release date, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Release country, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Genre/style tags.
    pub tags: Vec<String>,
    /// YouTube URL for the album, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube_url: Option<String>,
    /// YouTube search URL fallback.
    pub youtube_search_url: String,
    /// Whether YouTube search is available at all.
    pub youtube_search_available: bool,
    /// Artist biography snippet, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_description: Option<String>,
    /// Artist listen count, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
}

/// The queue deck: cards plus the build they came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueueResponse {
    /// Deck cards in order.
    #[serde(default)]
    pub items: Vec<QueueItemLight>,
    /// Build id behind the deck.
    pub queue_id: String,
}

/// Where one user's queue build stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueBuildStatus {
    /// No build has run since the server started.
    Idle,
    /// A build is running.
    Building,
    /// A built queue is saved.
    Ready,
    /// The last build failed.
    Error,
}

/// Queue build state for polling.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueueStatusResponse {
    /// Build state. A saved queue past its age is `ready` with `stale` set.
    pub status: QueueBuildStatus,
    /// Build id, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    /// Card count, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<i64>,
    /// Build timestamp (unix seconds), when built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at: Option<i64>,
    /// True when the deck is older than its TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// Build failure, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Body triggering a queue build.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueGenerateRequest {
    /// Rebuild even when a fresh deck exists.
    #[serde(default)]
    pub force: bool,
}

/// What a generate trigger did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueueAction {
    /// A build started.
    Started,
    /// A build was already running.
    AlreadyBuilding,
    /// A fresh deck is saved; nothing started.
    AlreadyReady,
    /// Scheduled builds are switched off; nothing started.
    Disabled,
}

/// What the generate trigger did, plus current build state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueGenerateResponse {
    /// What the trigger did.
    pub action: QueueAction,
    /// Build state after the trigger.
    pub status: QueueBuildStatus,
    /// Build id, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_id: Option<String>,
    /// Card count, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_count: Option<i64>,
    /// Build timestamp (unix seconds), when built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub built_at: Option<i64>,
    /// True when the deck is older than its TTL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    /// Build failure, when failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Whether a queue card's YouTube preview was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewStatus {
    /// A video was found.
    Available,
    /// The search ran and found nothing.
    NotFound,
    /// No search could run (no key, no quota left, or no names); the
    /// search link still works.
    Unavailable,
}

/// On-demand YouTube preview behind one queue card.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueuePreview {
    /// Whether a preview was found.
    pub status: PreviewStatus,
    /// Watch URL, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube_url: Option<String>,
    /// Search URL fallback, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube_search_url: Option<String>,
}

/// Body ignoring one queue card.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueIgnoreRequest {
    /// MusicBrainz release-group id to ignore.
    pub release_group_mbid: String,
    /// MusicBrainz artist id behind the card.
    pub artist_mbid: String,
    /// Release name for the ignore ledger.
    pub release_name: String,
    /// Artist name for the ignore ledger.
    pub artist_name: String,
}

/// One ignored release.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IgnoredRelease {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// MusicBrainz artist id.
    pub artist_mbid: String,
    /// Release name.
    pub release_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Ignore timestamp (unix seconds).
    pub ignored_at: i64,
}

/// The user's ignore ledger.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct IgnoredReleasesResponse {
    /// Ignored releases, newest first.
    pub items: Vec<IgnoredRelease>,
}

/// Body checking which cards already landed in the library.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueValidateRequest {
    /// Release-group ids to check.
    pub release_group_mbids: Vec<String>,
}

/// Library membership behind the checked cards.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueValidateResponse {
    /// The checked ids that are already owned.
    #[serde(default)]
    pub in_library: Vec<String>,
}

// ---------------------------------------------------------------------------
// Radio and playlist suggestions
// ---------------------------------------------------------------------------

/// One seed row for an item-seeded radio plan.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioSeedItem {
    /// Seed MusicBrainz artist id.
    pub artist_mbid: String,
    /// Seed artist name.
    #[serde(default)]
    pub artist_name: String,
    /// Seed MusicBrainz release id, when seeded by album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_mbid: Option<String>,
    /// Seed album name.
    #[serde(default)]
    pub album_name: String,
}

/// Which tracks a radio plan may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RadioMode {
    /// Owned tracks only.
    Library,
    /// Owned tracks plus provider picks.
    #[default]
    Hybrid,
}

/// Body planning track-level radio. The UI resolves playback per track
/// (library rows use the native stream endpoint, the rest use YouTube or
/// 30-second previews).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioPlanRequest {
    /// What the plan starts from.
    pub seed_type: RadioSeedKind,
    /// Seed id (mbid or genre name); empty for item seeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    /// Seed rows for `items` plans.
    #[serde(default)]
    pub items: Vec<RadioSeedItem>,
    /// Which tracks the plan may use.
    #[serde(default)]
    pub mode: RadioMode,
    /// Wanted track count.
    #[serde(default = "default_radio_count")]
    pub count: i64,
    /// Recording mbids to leave out.
    #[serde(default)]
    pub exclude_recording_mbids: Vec<String>,
}

fn default_radio_count() -> i64 {
    30
}

/// One planned radio track.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioPlanTrack {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// MusicBrainz artist id, when known.
    pub artist_mbid: String,
    /// MusicBrainz recording id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recording_mbid: Option<String>,
    /// MusicBrainz release id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_mbid: Option<String>,
    /// Album title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// True when owned (resolves to the native stream endpoint).
    pub in_library: bool,
    /// Native file id for owned rows, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_file_id: Option<String>,
    /// File container for owned rows, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_format: Option<String>,
    /// Duration in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<f64>,
}

/// A complete radio plan.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioPlanResponse {
    /// Plan title.
    pub title: String,
    /// Planned tracks in order.
    pub tracks: Vec<RadioPlanTrack>,
}

/// Body generating one radio shelf.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioRequest {
    /// What the shelf starts from (`artist`, `album` or `genre`).
    pub seed_type: RadioSeedKind,
    /// Seed id (mbid or genre name).
    pub seed_id: String,
    /// Wanted row count.
    #[serde(default = "default_radio_shelf_count")]
    pub count: i64,
    /// Provider preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ListenSource>,
}

fn default_radio_shelf_count() -> i64 {
    10
}

/// Taste profile behind playlist suggestions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistProfile {
    /// Artist mbids the playlist leans on.
    #[serde(default)]
    pub artist_mbids: Vec<String>,
    /// Genres per artist MBID on the playlist, from the library.
    #[serde(default)]
    pub genre_distribution: HashMap<String, Vec<String>>,
    /// Playlist track count.
    #[serde(default)]
    pub track_count: i64,
}

/// Body requesting playlist suggestions.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSuggestionsRequest {
    /// Playlist to extend.
    pub playlist_id: String,
    /// Wanted suggestion count.
    #[serde(default = "default_radio_shelf_count")]
    pub count: i64,
    /// Provider preference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ListenSource>,
}

/// Suggested rows for a playlist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSuggestionsResponse {
    /// Suggestions shelf.
    pub suggestions: ChartSection,
    /// Playlist the suggestions extend.
    pub playlist_id: String,
    /// Taste profile behind the suggestions.
    pub profile: PlaylistProfile,
}

// ---------------------------------------------------------------------------
// Previews (30-second, keyless)
// ---------------------------------------------------------------------------

/// One previewed track.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PreviewTrackItem {
    /// Track title.
    pub title: String,
    /// Artist name.
    pub artist_name: String,
    /// Preview audio URL (short-lived, never long-cached).
    pub preview_url: String,
    /// Preview length in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<i64>,
    /// Album position, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position: Option<i64>,
}

/// A 30-second track preview. Empty (no URL) means no provider had one.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackPreviewResponse {
    /// Preview audio URL, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_url: Option<String>,
    /// Track title, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Preview length in seconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<i64>,
    /// Provider that served the preview, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Ordered 30-second samples of an album's first tracks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlbumPreviewResponse {
    /// Sampled tracks in album order.
    #[serde(default)]
    pub tracks: Vec<PreviewTrackItem>,
    /// Provider that served the previews, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

// ---------------------------------------------------------------------------
// YouTube helpers behind the queue deck
// ---------------------------------------------------------------------------

/// A resolved YouTube video. `error: "not_found"` means the search ran and
/// found nothing; absence of both video and error never happens.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct YouTubeSearchResponse {
    /// Resolved video id, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video_id: Option<String>,
    /// Embed URL, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embed_url: Option<String>,
    /// Failure reason (`not_found`), when the search found nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// True when the answer came from cache.
    #[serde(default)]
    pub cached: bool,
}

/// YouTube data-API quota state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct YouTubeQuotaResponse {
    /// Quota units spent in the current window.
    pub used: i64,
    /// Quota units available per window.
    pub limit: i64,
    /// Unix seconds when the window resets.
    pub resets_at: i64,
}

/// One artist/track pair to check against the YouTube cache.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackCacheCheckItem {
    /// Artist name.
    pub artist: String,
    /// Track name.
    pub track: String,
}

/// Body checking track cache membership in bulk.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackCacheCheckRequest {
    /// Pairs to check (capped, deduped case-insensitively).
    #[serde(default)]
    pub items: Vec<TrackCacheCheckItem>,
}

/// Cache membership behind one pair.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackCacheCheckResponseItem {
    /// Artist name as sent.
    pub artist: String,
    /// Track name as sent.
    pub track: String,
    /// True when a cached video covers the pair.
    #[serde(default)]
    pub cached: bool,
}

/// Bulk cache membership.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrackCacheCheckResponse {
    /// Membership rows in request order (deduped).
    #[serde(default)]
    pub items: Vec<TrackCacheCheckResponseItem>,
}

// ---------------------------------------------------------------------------
// Discovery batches
// ---------------------------------------------------------------------------

/// One album to request inside a new batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchItemIn {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// The artist's MusicBrainz id.
    #[serde(default)]
    pub artist_id: String,
    /// Album title.
    #[serde(default)]
    pub title: String,
    /// Artist name.
    #[serde(default)]
    pub artist_name: String,
}

/// Body creating a discovery batch (one request per album).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchCreate {
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Albums to request.
    #[serde(default)]
    pub items: Vec<DiscoveryBatchItemIn>,
    /// A UUID naming this create. Sending the same key again returns the
    /// batch it made instead of filing the albums twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// What a batch did with one album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BatchOutcome {
    /// A request was filed.
    Requested,
    /// The library already held it.
    SkippedInLibrary,
    /// Someone had already requested it.
    SkippedDuplicate,
    /// The requests service did not file it.
    NotRequested,
}

impl BatchOutcome {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::SkippedInLibrary => "skipped_in_library",
            Self::SkippedDuplicate => "skipped_duplicate",
            Self::NotRequested => "not_requested",
        }
    }

    /// Reads a stored spelling back.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "requested" => Some(Self::Requested),
            "skipped_in_library" => Some(Self::SkippedInLibrary),
            "skipped_duplicate" => Some(Self::SkippedDuplicate),
            "not_requested" => Some(Self::NotRequested),
            _ => None,
        }
    }
}

/// Per-album outcome inside a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchItemStatus {
    /// MusicBrainz release-group id.
    pub musicbrainz_id: String,
    /// The artist's MusicBrainz id.
    #[serde(default)]
    pub artist_id: String,
    /// Album title.
    #[serde(default)]
    pub title: String,
    /// Artist name.
    #[serde(default)]
    pub artist_name: String,
    /// What the batch did with the album.
    pub outcome: BatchOutcome,
    /// Request state, when requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_status: Option<String>,
    /// True when already owned.
    #[serde(default)]
    pub in_library: bool,
}

/// A batch plus its album outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchDetail {
    /// Batch id.
    pub id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Album count.
    pub item_count: i64,
    /// Albums already imported.
    pub imported_count: i64,
    /// Albums still pending.
    pub pending_count: i64,
    /// Per-album outcomes.
    #[serde(default)]
    pub items: Vec<DiscoveryBatchItemStatus>,
}

/// A batch without its album outcomes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchSummary {
    /// Batch id.
    pub id: String,
    /// Batch name.
    pub name: String,
    /// Discover section the batch came from.
    #[serde(default)]
    pub source_section: String,
    /// Creation timestamp (RFC 3339).
    #[serde(default)]
    pub created_at: String,
    /// Album count.
    pub item_count: i64,
    /// Albums already imported.
    pub imported_count: i64,
    /// Albums still pending.
    pub pending_count: i64,
}

/// The user's batches.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchListResponse {
    /// Batches, newest first.
    pub batches: Vec<DiscoveryBatchSummary>,
}

/// What removing a batch did.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchRemoveResult {
    /// Albums removed with the batch.
    #[serde(default)]
    pub removed_albums: i64,
    /// Requests cancelled with the batch.
    #[serde(default)]
    pub cancelled_requests: i64,
    /// Albums kept (already landed or owned elsewhere).
    #[serde(default)]
    pub kept: i64,
}
