//! Discover, home, and now-playing DTOs for `/api/v3`.
//!
//! Clean-slate shapes in snake_case. They carry the same information the v2
//! UI consumed (sections, charts, queue deck, batches, presence) without
//! keeping the v2 field spellings.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

// ---------------------------------------------------------------------------
// Shared section items
// ---------------------------------------------------------------------------

/// One artist inside a section or chart page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChartArtist {
    /// Display name.
    pub name: String,
    /// MusicBrainz artist id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Native library id, when the artist is owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// Scrobble/listen count backing the rank, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// True when the artist has files in the library.
    #[serde(default)]
    pub in_library: bool,
    /// Provider that ranked this row (`listenbrainz`, `lastfm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One album inside a section or chart page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChartAlbum {
    /// Album title.
    pub name: String,
    /// MusicBrainz release-group id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Native library id, when the album is owned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_id: Option<String>,
    /// Album artist name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    /// MusicBrainz artist id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    /// Release date (`YYYY-MM-DD` or coarser), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    /// Scrobble/listen count backing the rank, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// True when the album has files in the library.
    #[serde(default)]
    pub in_library: bool,
    /// True when the album already has a request.
    #[serde(default)]
    pub requested: bool,
    /// Provider that ranked this row (`listenbrainz`, `lastfm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

/// One track inside a section.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChartTrack {
    /// Track title.
    pub name: String,
    /// MusicBrainz recording id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    /// Artist name, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_name: Option<String>,
    /// MusicBrainz artist id, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Album title, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Scrobble/listen count backing the rank, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// Last listen timestamp (RFC 3339), when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listened_at: Option<String>,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
}

/// One genre inside a section.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChartGenre {
    /// Genre name.
    pub name: String,
    /// Scrobble/listen count backing the rank, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
    /// Owned artists tagged with this genre, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_count: Option<i64>,
    /// Seed artist id behind the row, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
}

/// One row inside a section. The `type` field on the section names which
/// variant the rows hold; untagged so the wire keeps flat v2-style items.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum SectionItem {
    /// An artist row.
    Artist(ChartArtist),
    /// An album row.
    Album(ChartAlbum),
    /// A track row.
    Track(ChartTrack),
    /// A genre row.
    Genre(ChartGenre),
}

/// A titled shelf of rows: the unit both home and discover render.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ChartSection {
    /// Shelf title.
    pub title: String,
    /// Row kind (`artist`, `album`, `track`, `genre`).
    #[serde(rename = "type")]
    pub section_type: String,
    /// Shelf rows.
    #[serde(default)]
    pub items: Vec<SectionItem>,
    /// Provider behind the shelf, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Fallback explanation when the shelf degraded, when shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_message: Option<String>,
    /// Service the user must connect for the full shelf, when gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect_service: Option<String>,
    /// Radio seed kind (`artist`, `album`, `genre`), when playable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_seed_type: Option<String>,
    /// Radio seed id, when playable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radio_seed_id: Option<String>,
}

/// A card nudging the user to connect a service.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ServicePrompt {
    /// Service key (`listenbrainz`, `lastfm`, ...).
    pub service: String,
    /// Card title.
    pub title: String,
    /// Card body.
    pub description: String,
    /// Icon name.
    pub icon: String,
    /// Accent color token.
    pub color: String,
    /// Feature bullets.
    #[serde(default)]
    pub features: Vec<String>,
}

/// Which integrations back the shelves right now.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    #[serde(default)]
    pub navidrome: bool,
    /// The YouTube data API (quota'd) is configured.
    #[serde(default)]
    pub youtube_api: bool,
    /// A Plex source is connected.
    #[serde(default)]
    pub plex: bool,
    /// The native library has files.
    #[serde(default)]
    pub library: bool,
    /// Local files back playback. Home-only; discover leaves it false.
    #[serde(default)]
    pub localfiles: bool,
}

/// One track of the weekly exploration playlist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WeeklyExploration {
    /// Shelf title.
    pub title: String,
    /// Playlist date (`YYYY-MM-DD`).
    pub playlist_date: String,
    /// Playlist tracks in order.
    #[serde(default)]
    pub tracks: Vec<WeeklyTrack>,
    /// Provider playlist URL, when there is one.
    #[serde(default)]
    pub source_url: String,
}

// ---------------------------------------------------------------------------
// Home
// ---------------------------------------------------------------------------

/// Genre mosaic art: a collage of owned covers or a flat gradient.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenreArtwork {
    /// `collage` or `gradient`.
    pub kind: String,
    /// Artwork schema version.
    pub version: String,
    /// Owned covers composing the collage (empty for gradients).
    #[serde(default)]
    pub albums: Vec<GenreArtworkAlbum>,
}

/// One cover inside a genre collage.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPreview {
    /// Seed artist name.
    pub seed_artist: String,
    /// Seed MusicBrainz artist id.
    pub seed_artist_mbid: String,
    /// Similar artists.
    #[serde(default)]
    pub items: Vec<ChartArtist>,
}

/// Home shelves. Every shelf is optional: absence means the shelf has no
/// data, never a failure.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HomeResponse {
    /// Recently added library albums.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recently_added: Option<ChartSection>,
    /// Library artists shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_artists: Option<ChartSection>,
    /// Library albums shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_albums: Option<ChartSection>,
    /// Recommended artists for the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recommended_artists: Option<ChartSection>,
    /// Trending artists teaser (full pages live on the charts routes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trending_artists: Option<ChartSection>,
    /// Popular albums teaser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popular_albums: Option<ChartSection>,
    /// Recently played tracks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recently_played: Option<ChartSection>,
    /// Top genres.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_genres: Option<ChartSection>,
    /// Full genre list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre_list: Option<ChartSection>,
    /// Fresh releases shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_releases: Option<ChartSection>,
    /// Favorite artists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favorite_artists: Option<ChartSection>,
    /// The user's top albums teaser.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub your_top_albums: Option<ChartSection>,
    /// Weekly exploration playlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_exploration: Option<WeeklyExploration>,
    /// Connect-a-service cards.
    #[serde(default)]
    pub service_prompts: Vec<ServicePrompt>,
    /// Integration availability behind the shelves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<IntegrationStatus>,
    /// Mosaic art keyed by genre name.
    #[serde(default)]
    pub genre_artwork: std::collections::HashMap<String, GenreArtwork>,
    /// Genre artwork schema version.
    pub genre_artwork_schema_version: String,
    /// Discover teaser shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discover_preview: Option<DiscoverPreview>,
    /// Services that let the last build down: `unavailable` when every
    /// read failed, `degraded` while ListenBrainz refuses popularity reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<std::collections::HashMap<String, String>>,
    /// True while a fuller build runs in the background (the UI polls).
    #[serde(default)]
    pub refreshing: bool,
}

/// Owned plus popular rows behind one genre page.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
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
    /// Flat artist rows (legacy consumers).
    #[serde(default)]
    pub artists: Vec<ChartArtist>,
    /// Total popular rows, when counted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_count: Option<i64>,
}

/// Owned artists and albums for a genre.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenreLibrarySection {
    /// Owned artists.
    #[serde(default)]
    pub artists: Vec<ChartArtist>,
    /// Owned albums.
    #[serde(default)]
    pub albums: Vec<ChartAlbum>,
    /// Total owned artists.
    pub artist_count: i64,
    /// Total owned albums.
    pub album_count: i64,
}

/// Popular artists and albums for a genre.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct GenrePopularSection {
    /// Popular artists.
    #[serde(default)]
    pub artists: Vec<ChartArtist>,
    /// Popular albums.
    #[serde(default)]
    pub albums: Vec<ChartAlbum>,
    /// More artists behind the offsets.
    pub has_more_artists: bool,
    /// More albums behind the offsets.
    pub has_more_albums: bool,
}

// ---------------------------------------------------------------------------
// Charts (range-pair redesign)
// ---------------------------------------------------------------------------

/// Valid chart ranges. v2 spelled these as path pairs
/// (`/trending/artists` plus `/trending/artists/{range_key}`); v3 takes one
/// route with `?range=`. Keys keep the v2 spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartRange {
    /// This week.
    ThisWeek,
    /// This month.
    ThisMonth,
    /// This year.
    ThisYear,
    /// All time.
    AllTime,
}

impl ChartRange {
    /// Parse a `range` query value. Unknown values fail; v2 silently fell
    /// back to `this_week`, which hid client bugs.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "this_week" => Some(Self::ThisWeek),
            "this_month" => Some(Self::ThisMonth),
            "this_year" => Some(Self::ThisYear),
            "all_time" => Some(Self::AllTime),
            _ => None,
        }
    }

    /// Wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ThisWeek => "this_week",
            Self::ThisMonth => "this_month",
            Self::ThisYear => "this_year",
            Self::AllTime => "all_time",
        }
    }

    /// Human label, kept from v2.
    pub fn label(self) -> &'static str {
        match self {
            Self::ThisWeek => "This Week",
            Self::ThisMonth => "This Month",
            Self::ThisYear => "This Year",
            Self::AllTime => "All Time",
        }
    }
}

impl Default for ChartRange {
    fn default() -> Self {
        Self::ThisWeek
    }
}

/// Chart provider preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartSource {
    /// ListenBrainz charts.
    Listenbrainz,
    /// Last.fm charts.
    Lastfm,
}

impl ChartSource {
    /// Parse a `source` query value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "listenbrainz" => Some(Self::Listenbrainz),
            "lastfm" => Some(Self::Lastfm),
            _ => None,
        }
    }
}

impl Default for ChartSource {
    fn default() -> Self {
        Self::Listenbrainz
    }
}

/// One page of a trending-artists chart.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TrendingArtistsPage {
    /// Range key behind this page.
    pub range_key: String,
    /// Human range label.
    pub label: String,
    /// Ranked artists for the page.
    pub items: Vec<ChartArtist>,
    /// Offset behind this page.
    pub offset: i64,
    /// Limit behind this page.
    pub limit: i64,
    /// True when more rows follow.
    pub has_more: bool,
}

/// One page of a popular-albums or your-top chart.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PopularAlbumsPage {
    /// Range key behind this page.
    pub range_key: String,
    /// Human range label.
    pub label: String,
    /// Ranked albums for the page.
    pub items: Vec<ChartAlbum>,
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BecauseYouListenTo {
    /// Seed artist name.
    pub seed_artist: String,
    /// Seed MusicBrainz artist id.
    pub seed_artist_mbid: String,
    /// Similar-albums shelf.
    pub section: ChartSection,
    /// Seed listen count backing the pick.
    #[serde(default)]
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TopPickItem {
    /// Picked album.
    pub album: ChartAlbum,
    /// Match score, 0-100.
    pub match_pct: i64,
    /// Human reasons behind the pick.
    #[serde(default)]
    pub reasons: Vec<String>,
    /// Seed artist name, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_artist: Option<String>,
}

/// The personalized top-picks shelf.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TopPicksSection {
    /// Shelf title.
    pub title: String,
    /// Ranked picks.
    #[serde(default)]
    pub items: Vec<TopPickItem>,
    /// Provider behind the picks, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// True while picks are trending-only because personalization has not
    /// resolved yet (the UI shows a "still personalising" hint).
    #[serde(default)]
    pub personalizing: bool,
}

/// Discover shelves. Every shelf is optional: absence means no data, never
/// a failure.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverResponse {
    /// Seed-driven similar-album shelves.
    #[serde(default)]
    pub because_you_listen_to: Vec<BecauseYouListenTo>,
    /// Whether the queue deck is enabled for the user.
    pub discover_queue_enabled: bool,
    /// Fresh releases shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_releases: Option<ChartSection>,
    /// Missing essentials shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub missing_essentials: Option<ChartSection>,
    /// Rediscover shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rediscover: Option<ChartSection>,
    /// Artists-you-might-like shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artists_you_might_like: Option<ChartSection>,
    /// Popular-in-your-genres shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub popular_in_your_genres: Option<ChartSection>,
    /// Genre list shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub genre_list: Option<ChartSection>,
    /// Globally trending shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub globally_trending: Option<ChartSection>,
    /// Weekly exploration playlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekly_exploration: Option<WeeklyExploration>,
    /// Integration availability behind the shelves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integration_status: Option<IntegrationStatus>,
    /// Connect-a-service cards.
    #[serde(default)]
    pub service_prompts: Vec<ServicePrompt>,
    /// Mosaic art keyed by genre name.
    #[serde(default)]
    pub genre_artwork: std::collections::HashMap<String, GenreArtwork>,
    /// Genre artwork schema version.
    pub genre_artwork_schema_version: String,
    /// Last.fm weekly artist chart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastfm_weekly_artist_chart: Option<ChartSection>,
    /// Last.fm weekly album chart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastfm_weekly_album_chart: Option<ChartSection>,
    /// Last.fm recent scrobbles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lastfm_recent_scrobbles: Option<ChartSection>,
    /// Daily mix shelves.
    #[serde(default)]
    pub daily_mixes: Vec<ChartSection>,
    /// Radio shelves.
    #[serde(default)]
    pub radio_sections: Vec<ChartSection>,
    /// Personalized top picks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_picks: Option<TopPicksSection>,
    /// Listeners-like-you shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listeners_like_you: Option<ChartSection>,
    /// Anniversaries shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anniversaries: Option<ChartSection>,
    /// New-from-followed shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_from_followed: Option<ChartSection>,
    /// Unexplored-genres shelf.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unexplored_genres: Option<ChartSection>,
    /// Build timestamp (unix seconds), when built.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_at: Option<i64>,
    /// Refresh-start timestamp (unix seconds), when refreshing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_started_at: Option<i64>,
    /// Status per page zone (`picks`, `lounge`, `weekly`, `made`,
    /// `because`, `fresh`, `library`, `genres`, `trending`): `loading`
    /// before the first page, `updating` while a rebuild runs, else
    /// `ready` or `empty`.
    #[serde(default)]
    pub section_status: std::collections::HashMap<String, String>,
    /// True while a fuller build runs in the background (the UI polls).
    #[serde(default)]
    pub refreshing: bool,
    /// Per-service degradation notes (`ok`/`degraded`/`down`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_status: Option<std::collections::HashMap<String, String>>,
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
    /// Provider behind the row (`lastfm`, `listenbrainz`), when it had one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
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
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueItemLight {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// Album name.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// MusicBrainz artist id.
    pub artist_mbid: String,
    /// Human reason behind the pick.
    pub recommendation_reason: String,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// True for wildcard (out-of-profile) picks.
    #[serde(default)]
    pub is_wildcard: bool,
    /// True when already owned.
    #[serde(default)]
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
    #[serde(default)]
    pub tags: Vec<String>,
    /// YouTube URL for the album, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub youtube_url: Option<String>,
    /// YouTube search URL fallback.
    #[serde(default)]
    pub youtube_search_url: String,
    /// Whether YouTube search is available at all.
    #[serde(default)]
    pub youtube_search_available: bool,
    /// Artist biography snippet, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_description: Option<String>,
    /// Artist listen count, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_count: Option<i64>,
}

/// One queue card with its enrichment attached.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueItemFull {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// Album name.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// MusicBrainz artist id.
    pub artist_mbid: String,
    /// Human reason behind the pick.
    pub recommendation_reason: String,
    /// Cover art URL, when resolved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_url: Option<String>,
    /// True for wildcard (out-of-profile) picks.
    #[serde(default)]
    pub is_wildcard: bool,
    /// True when already owned.
    #[serde(default)]
    pub in_library: bool,
    /// Attached enrichment, when loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enrichment: Option<QueueEnrichment>,
}

/// One queue card, light or enriched.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum QueueItem {
    /// Card with enrichment attached.
    Full(QueueItemFull),
    /// Card without enrichment.
    Light(QueueItemLight),
}

/// The queue deck: cards plus the build they came from.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueueResponse {
    /// Deck cards in order.
    #[serde(default)]
    pub items: Vec<QueueItem>,
    /// Build id behind the deck.
    pub queue_id: String,
}

/// Queue build state for polling.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueueStatusResponse {
    /// `ready`, `building`, `stale`, or `error`.
    pub status: String,
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

/// What the generate trigger did, plus current build state.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct QueueGenerateResponse {
    /// `started`, `already_building`, or `fresh`.
    pub action: String,
    /// `ready`, `building`, `stale`, or `error`.
    pub status: String,
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

/// On-demand YouTube preview behind one queue card.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverQueuePreview {
    /// `available`, `not_found`, or `unavailable`.
    pub status: String,
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

/// Body planning track-level radio. The UI resolves playback per track
/// (library rows use the native stream endpoint, the rest use YouTube or
/// 30-second previews).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioPlanRequest {
    /// Seed kind (`artist`, `album`, `genre`, `items`).
    pub seed_type: String,
    /// Seed id (mbid or genre name); empty for item seeds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_id: Option<String>,
    /// Seed rows for `items` plans.
    #[serde(default)]
    pub items: Vec<RadioSeedItem>,
    /// `library` (owned only) or `hybrid` (owned plus provider picks).
    pub mode: String,
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
    #[serde(default)]
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
    #[serde(default)]
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
    #[serde(default)]
    pub tracks: Vec<RadioPlanTrack>,
}

/// Body generating one radio shelf.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RadioRequest {
    /// Seed kind (`artist`, `album`, `genre`).
    pub seed_type: String,
    /// Seed id (mbid or genre name).
    pub seed_id: String,
    /// Wanted row count.
    #[serde(default = "default_radio_shelf_count")]
    pub count: i64,
    /// Provider preference (`listenbrainz`, `lastfm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
    /// Genre distribution (genre to artist names).
    #[serde(default)]
    pub genre_distribution: std::collections::HashMap<String, Vec<String>>,
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
    /// Provider preference (`listenbrainz`, `lastfm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
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
    #[serde(default)]
    pub artist_name: String,
    /// Preview audio URL (short-lived, never long-cached).
    #[serde(default)]
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
    pub release_group_mbid: String,
    /// MusicBrainz artist id.
    #[serde(default)]
    pub artist_mbid: String,
    /// Album name.
    #[serde(default)]
    pub album_name: String,
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
}

/// Per-album outcome inside a batch.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryBatchItemStatus {
    /// MusicBrainz release-group id.
    pub release_group_mbid: String,
    /// MusicBrainz artist id.
    #[serde(default)]
    pub artist_mbid: String,
    /// Album name.
    #[serde(default)]
    pub album_name: String,
    /// Artist name.
    #[serde(default)]
    pub artist_name: String,
    /// `requested`, `skipped_in_library`, or `skipped_duplicate`.
    pub outcome: String,
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

// ---------------------------------------------------------------------------
// Now playing
// ---------------------------------------------------------------------------

/// One live listening session. Redacted rows keep identity and progress
/// but carry empty song fields (the owner chose `track_hidden`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingEntry {
    /// Session key (`user_id:device`).
    pub id: String,
    /// Display name of the listener.
    pub user_name: String,
    /// Track title (empty when redacted).
    pub track_name: String,
    /// Artist name (empty when redacted).
    pub artist_name: String,
    /// Album title (none when redacted or unknown).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_name: Option<String>,
    /// Cover art URL (empty when redacted).
    pub cover_url: String,
    /// Device label.
    pub device_name: String,
    /// True when paused.
    pub is_paused: bool,
    /// Playback source (`local`, `youtube`, `jellyfin`, `navidrome`, `plex`).
    pub source: String,
    /// Position in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress_ms: Option<i64>,
    /// Track length in milliseconds, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    /// True when the owner hides the track.
    #[serde(default)]
    pub redacted: bool,
}

/// The live listening snapshot across users.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct NowPlayingSnapshot {
    /// Live sessions.
    #[serde(default)]
    pub sessions: Vec<NowPlayingEntry>,
}
