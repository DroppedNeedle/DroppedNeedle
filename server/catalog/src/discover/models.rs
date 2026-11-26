//! Discover and home DTOs for `/api/v3`.
//!
//! Album and artist rows are the catalog's [`AlbumRef`] and [`ArtistRef`],
//! so a row reads the same on every page. Fields with a closed set of
//! values are enums.

use std::collections::HashMap;

use droppedneedle_kernel::page::Page;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use super::feedback::{FeedbackKind, FeedbackTarget};
pub use super::health::PageHealth;
pub use super::record::{MadeFrom, PickEvent};
pub use super::sections::{SectionKey, SectionZone};
pub use super::tunings::TuningKind;
pub use crate::metadata::models::{AlbumRef, ArtistRef};
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
    /// Last.fm and ListenBrainz blended, for a user who links both.
    Blended,
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
    /// ListenBrainz reads are switched on for this server. The home page's
    /// copy is also false while the person has no ListenBrainz link.
    pub listenbrainz: bool,
    /// A Jellyfin source is connected.
    pub jellyfin: bool,
    /// A download client is configured.
    pub download_client: bool,
    /// YouTube playback is configured.
    pub youtube: bool,
    /// The admin has switched Last.fm on for this server.
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

/// Home's way into Discover: the person's newest set, with the first pick
/// they have not answered, or none once every pick is answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPreview {
    /// The ISO week number of the set's first day.
    pub week_number: i64,
    /// When the next set comes out (unix ms).
    pub next_set_at: i64,
    /// The set's albums, in pick order.
    pub albums: Vec<AlbumRef>,
    /// The first pick not answered yet; null once every pick is answered.
    #[serde(default)]
    pub next: Option<TeaserPick>,
}

/// The pick Home's teaser shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TeaserPick {
    /// Its place in the set, from 1.
    pub pick_no: i64,
    /// The album, with its year and type.
    pub album: AlbumRef,
    /// Why it is in the set, as shown when the set was built.
    pub reason: String,
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
    /// Discover teaser: the newest set's next pick. Not sent while the
    /// person has no set, nor while a new set is due and every pick of the
    /// last one is answered.
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

/// The person's own Last.fm lists for their charts page: recent scrobbles
/// and this week's top albums and artists, each left out when empty or
/// when its read failed. Empty for a person without Last.fm linked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct YourLastfmResponse {
    /// The lists, in page order.
    pub shelves: Vec<ChartSection>,
}

// ---------------------------------------------------------------------------
// Discover
// ---------------------------------------------------------------------------

/// How a listening service fared in a discover build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServiceState {
    /// It answered only in part.
    Degraded,
    /// Every read of it failed.
    Unavailable,
}

/// Why a section switched on is not plainly built. The one list of these
/// reasons, for the page's sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionCondition {
    /// No build has made it since it was switched on.
    Building,
    /// Its last build failed, so the one built before is shown.
    Kept,
    /// Its last build failed and there is nothing earlier to show.
    SourceDown,
    /// It needs a service the person has not linked.
    NeedsLink,
    /// It came back under half its usual size.
    Thin,
    /// Every read answered, with nothing to show.
    Empty,
}

/// How one section of a page stands, when it is not plainly built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SectionState {
    /// Which section it is.
    pub key: SectionKey,
    /// The zone it is shown in.
    pub zone: SectionZone,
    /// How it stands.
    pub state: SectionCondition,
    /// The service behind it, when one is the cause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<ListenSource>,
    /// When the section shown was built (unix seconds), for `kept`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<i64>,
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
    /// The weekly exploration playlist.
    Weekly {
        /// The playlist.
        weekly: WeeklyExploration,
    },
    /// New releases from the person's artists, dated and typed.
    NewReleases {
        /// The newest of them.
        new_releases: NewReleaseSheet,
    },
    /// Albums missing from artists the person plays and the library
    /// collects, each with the artist's albums the library holds.
    Gaps {
        /// The gaps, the person's heaviest artists first.
        gaps: Vec<Gap>,
    },
    /// The person's own albums turned over.
    OwnShelves {
        /// Its two lists.
        own_shelves: OwnShelves,
    },
}

/// One release on a person's list of new releases from their artists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewRelease {
    /// The release: its date (`release_date`, `YYYY-MM-DD`), its type,
    /// whether the library holds it and whether a request for it is open.
    pub album: AlbumRef,
    /// Why it is listed, for a release by an artist close to theirs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// What kind of release it is besides its type, when it is a live
    /// set, a demo or a compilation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secondary_kind: Option<SecondaryKind>,
}

/// A new release's kind besides its type, of those the list names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SecondaryKind {
    /// A live recording.
    Live,
    /// A demo.
    Demo,
    /// A compilation.
    Compilation,
}

impl SecondaryKind {
    /// The first of `types` (MusicBrainz secondary types, as spelled there)
    /// the list names, if any.
    pub fn of(types: &[String]) -> Option<Self> {
        types
            .iter()
            .find_map(|kind| match kind.trim().to_ascii_lowercase().as_str() {
                "live" => Some(Self::Live),
                "demo" => Some(Self::Demo),
                "compilation" => Some(Self::Compilation),
                _ => None,
            })
    }
}

/// The newest of a person's new releases, as the page shows them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewReleaseSheet {
    /// The newest releases, newest first.
    pub releases: Vec<NewRelease>,
    /// How many the whole list holds.
    pub total: u64,
    /// When ListenBrainz's list of new releases was read (unix seconds);
    /// none when none is kept, and the list holds only what the person's
    /// follows found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<i64>,
}

/// An album missing from an artist the person plays and the library
/// collects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Gap {
    /// The missing album: its type and first release year when known, and
    /// whether a request for it is open.
    pub album: AlbumRef,
    /// The line: "You have 6 of their albums, not this one."
    pub reason: String,
    /// The artist's albums the library holds, oldest first, each with its
    /// library id and its year when tagged. The line counts them.
    pub held: Vec<AlbumRef>,
}

/// From your shelves: the person's own albums, in two lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnShelves {
    /// Played a lot, not lately: most played first.
    pub played_a_lot: Vec<OwnShelfAlbum>,
    /// Asked for and never played: the oldest arrival first.
    pub never_played: Vec<OwnShelfAlbum>,
    /// The first month of the person's history (`YYYY-MM`, server time);
    /// none while they have none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_from: Option<String>,
    /// The month the lists were made in (`YYYY-MM`, server time).
    pub month: String,
}

/// One of the person's own albums on their shelves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OwnShelfAlbum {
    /// The album, with its library id.
    pub album: AlbumRef,
    /// The line built from its own numbers: "18 plays in February, none
    /// since.", "17 plays, last in July." or "Asked for in March, not
    /// played yet."
    pub line: String,
    /// The month of the person's last play of it (`YYYY-MM`, server time);
    /// none for one never played.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_played: Option<String>,
}

/// Discover: its sections, in page order, plus the page-level fields. A
/// section with no rows is left out, and `section_states` says why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverResponse {
    /// The sections, in page order.
    pub sections: Vec<PageSection>,
    /// In page order, each section switched on that is not plainly built:
    /// kept from an earlier build, short, empty or missing, and why.
    #[serde(default)]
    pub section_states: Vec<SectionState>,
    /// Whether the weekly set is switched on for the user.
    pub weekly_set_enabled: bool,
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

/// One user's latest discover page numbers.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct PageHealthRow {
    /// The user's id.
    pub user_id: String,
    /// When the page was built, unix seconds.
    pub built_at: i64,
    /// Its numbers.
    pub health: PageHealth,
}

/// Every user's latest discover numbers: counts only, no music named.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverHealthResponse {
    /// Discover pages, one per user.
    pub pages: Vec<PageHealthRow>,
}

/// One thing a person tells Discover: about an album (`no`, `know_it`,
/// `not_now`, which need `release_group_mbid`) or an artist
/// (`not_artist`, which needs only `artist_mbid`). Names are kept as given
/// for the list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverFeedbackRequest {
    /// What the person said.
    pub kind: FeedbackKind,
    /// MusicBrainz release-group id of the album.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_group_mbid: Option<String>,
    /// MusicBrainz id of the artist the album is filed under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist_mbid: Option<String>,
    /// Album name, for the list.
    #[serde(default)]
    pub release_name: String,
    /// Artist name, for the list.
    #[serde(default)]
    pub artist_name: String,
    /// Where the answer was given, when the caller says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_from: Option<MadeFrom>,
}

/// One thing the web UI saw happen to a pick.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPickEventRequest {
    /// MusicBrainz release-group id of the pick.
    pub release_group_mbid: String,
    /// What happened.
    pub event: PickEvent,
}

/// One thing a person told Discover, in effect.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverFeedbackEntry {
    /// Whether it is about an album or an artist.
    pub target: FeedbackTarget,
    /// The target's MusicBrainz id, lowercased: the album's release group
    /// or the artist.
    pub target_mbid: String,
    /// What the person said.
    pub kind: FeedbackKind,
    /// MusicBrainz release-group id, as given; empty when none was.
    pub release_group_mbid: String,
    /// MusicBrainz artist id, as given; empty when none was.
    pub artist_mbid: String,
    /// Album name, as given.
    pub release_name: String,
    /// Artist name, as given.
    pub artist_name: String,
    /// When it was said (unix seconds).
    pub created_at: i64,
    /// When it lapses (unix seconds); null while it holds until undone.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

/// Everything a person told Discover that is in effect, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverFeedbackResponse {
    /// The answers, newest first.
    pub items: Vec<DiscoverFeedbackEntry>,
}

/// One way a person tuned what Discover offers them, about one of their
/// artists or every artist of one of their taste groups: exactly one of
/// `target` and `group_no`, or a source the person turns off.
/// `less_from_seed`, `more_from_seed`, `phase` and `not_this_source` are
/// taken today; `less_of_kind` is refused.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverTuningRequest {
    /// What the tuning is.
    pub kind: TuningKind,
    /// The MusicBrainz id of one of the person's own artists, or for
    /// `not_this_source` the source: `lastfm` or `listenbrainz`.
    #[serde(default)]
    pub target: Option<String>,
    /// One of the person's taste groups, by its number: the tuning is kept
    /// for each artist the group holds now.
    #[serde(default)]
    pub group_no: Option<i64>,
}

/// One way a person tuned what Discover offers them.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverTuningEntry {
    /// What the tuning is.
    pub kind: TuningKind,
    /// What it is about, lowercased.
    pub target: String,
    /// The target's name, as the person's taste rows give it, or the
    /// source's.
    pub name: String,
    /// The taste group it was said for, by its anchor artist's MusicBrainz
    /// id (lowercased); null for a tuning about one artist.
    pub group_anchor_mbid: Option<String>,
    /// That group's name when it was said; null for one artist.
    pub group_label: Option<String>,
    /// When it was said (unix seconds).
    pub created_at: i64,
}

/// A person's taste as Discover reads it: the groups their picks are
/// shared out by, each with its artists, what it was worked out from, and
/// whether a tuning they made is still being followed.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverTasteResponse {
    /// What the taste was worked out from; null before their first taste
    /// build.
    pub made_from: Option<TasteMadeFrom>,
    /// Their groups, largest share first; empty without any.
    pub groups: Vec<TasteGroup>,
    /// Their picks are still catching up: a tuning they made is newer than
    /// the picks built from it, or a taste or pool build is queued or
    /// running.
    pub updating: bool,
}

/// What a person's taste was worked out from.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TasteMadeFrom {
    /// Their plays in the app.
    pub plays_counted: i64,
    /// Their listens imported from ListenBrainz.
    pub imported_listens_counted: i64,
    /// The earliest of them (unix seconds); null with none.
    pub history_from_at: Option<i64>,
}

/// One of a person's taste groups.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TasteGroup {
    /// Its number, by share: 0 is the largest.
    pub group_no: i64,
    /// Its heaviest artist's MusicBrainz id, lowercased: the key of a
    /// statement about the whole group.
    pub anchor_artist_mbid: String,
    /// The name of its heaviest artist, which it is named after.
    pub anchor_name: String,
    /// Its share of each set, 0 to 1.
    pub share: f64,
    /// Whether it is played more or less of late than all told.
    pub trend: super::engine::taste::Trend,
    /// The latest play of any artist in it (unix seconds); null with none.
    pub last_played_at: Option<i64>,
    /// Its artists, most played first.
    pub artists: Vec<TasteArtist>,
}

/// One artist in a person's taste group.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TasteArtist {
    /// MusicBrainz id, lowercased.
    pub artist_mbid: String,
    /// Name, as the person's rows give it.
    pub name: String,
    /// Their plays of it in the app.
    pub plays: i64,
    /// Their imported listens of it.
    pub imported_listens: i64,
    /// Whether their picks start from it.
    pub is_seed: bool,
    /// What they said about it, when they did.
    pub tuning: Option<TasteArtistTuning>,
}

/// A statement a person made about one artist.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TasteArtistTuning {
    /// What it is.
    pub kind: TuningKind,
    /// The group it was said for, by its anchor's MusicBrainz id; null
    /// when said for the artist.
    pub group_anchor_mbid: Option<String>,
    /// That group's name; null when said for the artist.
    pub group_label: Option<String>,
}

/// Every way a person tuned what Discover offers them, newest first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DiscoverTuningsResponse {
    /// The tunings, newest first.
    pub items: Vec<DiscoverTuningEntry>,
}

/// An album to put on the caller's Later pile, named for the list.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LaterRequest {
    /// The album's title.
    pub title: String,
    /// The album's artist.
    pub artist_name: String,
    /// The artist's MusicBrainz id, when known. One that is not an id is
    /// left out.
    #[serde(default)]
    pub artist_mbid: Option<String>,
}

/// One album on the caller's Later pile.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct LaterAlbum {
    /// The album, with whether an album request for it is open. The library
    /// owns none of the pile's albums: one it comes to own leaves the list.
    pub album: AlbumRef,
    /// When it went on the pile (unix seconds).
    pub added_at: i64,
}

/// An artist the caller names as one they love, named as it was shown.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatedArtistRequest {
    /// The artist's name.
    pub artist_name: String,
}

/// One artist the caller named as one they love.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StatedArtist {
    /// The artist's MusicBrainz id, lowercased.
    pub artist_mbid: String,
    /// The artist's name, as it was shown.
    pub artist_name: String,
    /// When it was named (unix seconds).
    pub stated_at: i64,
}

/// The artists the caller named as ones they love.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct StatedArtistsResponse {
    /// The artists, in the order they were named.
    pub items: Vec<StatedArtist>,
    /// The most one person may name.
    pub limit: u32,
}

/// What a pick's reason rests on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PickReasonKind {
    /// The person's own artists lead to it.
    YourArtists,
    /// A service picked it for the person.
    Source,
    /// A fact about one of the person's artists leads to it: its band.
    Fact,
}

/// One of the person's own artists a reason names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PickReasonArtist {
    /// Its MusicBrainz id, lowercased.
    pub artist_mbid: String,
    /// Its name.
    pub name: String,
}

/// Why a pick is offered: one line built from the path that found it, and
/// the person's own artists it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PickReason {
    /// What it rests on.
    pub kind: PickReasonKind,
    /// The line, a sentence or two.
    pub line: String,
    /// Up to three of the person's artists the line names, strongest
    /// first; none for a service's pick.
    pub artists: Vec<PickReasonArtist>,
}

/// How a pick was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FoundBy {
    /// People who play the person's artists also play its artist.
    CoListening,
    /// A service named it for the person.
    SourcePick,
    /// One of the person's artists' bands, members or other names, as
    /// MusicBrainz links them.
    BandLink,
}

/// A service a pick's facts came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
    /// MusicBrainz.
    Musicbrainz,
}

/// One of the person's artists that led to a pick.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct EvidenceSeed {
    /// Its MusicBrainz id, lowercased.
    pub artist_mbid: String,
    /// Its name.
    pub name: String,
    /// The person's plays and imported listens of it.
    pub your_plays: i64,
    /// How many of its albums the library holds.
    pub albums_owned: i64,
    /// The pick's artist's place in its list of similar artists, from 1.
    pub rank: i64,
    /// How long that list is.
    pub of: i64,
    /// Whether the person already asked for less from it.
    pub turned_down: bool,
}

/// Whether a pick's artist is new to the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct NewToYou {
    /// No plays or listens of the artist, or of the album, in their
    /// history.
    pub no_plays: bool,
    /// The library holds no album of the artist.
    pub not_in_library: bool,
}

/// Everything behind one pick: what "Why this?" opens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PickEvidence {
    /// MusicBrainz release-group id, lowercased.
    pub release_group_mbid: String,
    /// Album title.
    pub title: String,
    /// The artist it is filed under.
    pub artist_name: String,
    /// The line shown on the pick.
    pub reason: PickReason,
    /// The person's artists that led to it through co-listening, strongest
    /// first; none for a pick found otherwise.
    pub seeds: Vec<EvidenceSeed>,
    /// Whether its artist is new to the person.
    pub new_to_you: NewToYou,
    /// The album's listeners, as last counted; null when not counted.
    #[serde(default)]
    pub listeners: Option<i64>,
    /// Where that count came from; null when there is none.
    #[serde(default)]
    pub listeners_source: Option<EvidenceSource>,
    /// How it was found.
    pub found_by: FoundBy,
    /// The service the finding fact came from.
    pub source: EvidenceSource,
    /// When that fact was read (unix ms); null when not known.
    #[serde(default)]
    pub source_read_at: Option<i64>,
}

// ---------------------------------------------------------------------------
// The weekly set
// ---------------------------------------------------------------------------

pub use super::engine::weekly::{Depth, SetDay, ShortReason};

/// Where the person's week stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WeekStatus {
    /// This week's set is out: `set` is it.
    Ready,
    /// This week's set is being chosen while facts it needs come in;
    /// `found_so_far` says how many picks are found. `set` is the last
    /// one, when there is one.
    BeingSetOut,
    /// The person's first set waits for their ListenBrainz history to be
    /// read, six hours at most.
    ReadingHistory,
    /// This week's set is not built yet and the last one stays, as `set`:
    /// while a service it waits on is not answering (`failing` says which
    /// and since when), or, for someone away a while, until their next
    /// visit (`failing` is null).
    KeptLastWeek,
    /// Nothing to make a set from yet.
    NothingYet,
}

/// A service a set waits on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SetSource {
    /// ListenBrainz.
    Listenbrainz,
    /// MusicBrainz.
    Musicbrainz,
    /// Last.fm.
    Lastfm,
}

/// A service that stopped answering, and since when.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SourceFailing {
    /// The service.
    pub source: SetSource,
    /// When it first failed (unix ms).
    pub since: i64,
}

/// What became of one pick, read from the person's request for it, the
/// library, their Later pile and what they told Discover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PickState {
    /// They asked for it, and it is on its way.
    Asked,
    /// They asked for it, and the request waits for an admin.
    AwaitingApproval,
    /// They asked for it, and the request was turned down.
    Refused,
    /// They asked for it, and the download failed or was stopped.
    Failed,
    /// The library holds it.
    Arrived,
    /// It is on their Later pile.
    Later,
    /// They said no.
    No,
    /// They said they know it already.
    Known,
    /// They said not now.
    NotNow,
    /// They turned its artist down.
    NotArtist,
    /// Nothing yet.
    Open,
}

droppedneedle_kernel::spelled!(PickState {
    Asked => "asked",
    AwaitingApproval => "awaiting_approval",
    Refused => "refused",
    Failed => "failed",
    Arrived => "arrived",
    Later => "later",
    No => "no",
    Known => "known",
    NotNow => "not_now",
    NotArtist => "not_artist",
    Open => "open",
});

/// One taste a set's picks are filed under.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SetTaste {
    /// "Around" its heaviest artist.
    pub name: String,
    /// That artist's MusicBrainz id.
    pub anchor_artist_mbid: String,
    /// Its share of the person's taste; null for a person with no plays.
    #[serde(default)]
    pub share: Option<f64>,
    /// How many of the set's picks are filed under it.
    pub picks: i64,
}

/// What a set was made from, as counted when it was built.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SetCounts {
    /// Plays in the app and imported listens counted.
    pub plays_counted: i64,
    /// The first of them (unix ms); null with none.
    #[serde(default)]
    pub history_from_at: Option<i64>,
    /// The latest of them (unix ms); null with none.
    #[serde(default)]
    pub history_to_at: Option<i64>,
    /// The person's artists the set started from.
    pub seeds: i64,
    /// Artists those point at; null when no pool was built.
    #[serde(default)]
    pub neighbours_found: Option<i64>,
    /// Of those, the ones the person knows or the library holds.
    #[serde(default)]
    pub known_left_out: Option<i64>,
    /// Artists everybody likes, left out; null when that rule did not run.
    #[serde(default)]
    pub hubs_left_out: Option<i64>,
    /// Artists on the library's shelves.
    pub shelf_artists: i64,
    /// Albums on the library's shelves.
    pub shelf_albums: i64,
    /// Whether the person's ListenBrainz history had been read in full.
    pub linked: bool,
}

/// One pick of a set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SetPick {
    /// Its place in the set, from 1.
    pub pick_no: i64,
    /// The album, with its year and type.
    pub album: AlbumRef,
    /// Why it is in the set, as shown when the set was built.
    pub reason: PickReason,
    /// The entry of the set's `tastes` it is filed under; null is "further
    /// out".
    #[serde(default)]
    pub taste_no: Option<i64>,
    /// What became of it.
    pub state: PickState,
    /// Whether the person has heard its sample.
    pub heard: bool,
}

/// One week's set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WeeklySet {
    /// When its week starts (unix ms): 00:00 server time on the set day.
    pub week_start_at: i64,
    /// The ISO week number of that day.
    pub week_number: i64,
    /// When it was built (unix ms).
    pub built_at: i64,
    /// When the next set comes out (unix ms).
    pub next_set_at: i64,
    /// Why it holds fewer than eight; null for a full set.
    #[serde(default)]
    pub short_reason: Option<ShortReason>,
    /// The line shown after "Only {n} this week."; null for a full set.
    #[serde(default)]
    pub short_line: Option<String>,
    /// What it was made from.
    pub counts: SetCounts,
    /// The tastes its picks are filed under, largest share first.
    pub tastes: Vec<SetTaste>,
    /// Its picks, in order.
    pub picks: Vec<SetPick>,
}

/// The person's week: where it stands, and the set to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct WeekResponse {
    /// Where it stands.
    pub status: WeekStatus,
    /// While `being_set_out`: how many picks are found so far.
    #[serde(default)]
    pub found_so_far: Option<i64>,
    /// While `kept_last_week` because a service is not answering: which,
    /// and since when. Null otherwise, `kept_last_week` for someone away a
    /// while included.
    #[serde(default)]
    pub failing: Option<SourceFailing>,
    /// This week's set when `ready`, else the newest one kept; null when
    /// there is none.
    #[serde(default)]
    pub set: Option<WeeklySet>,
}

/// One album past the week's set, from the person's pool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DigFurtherPick {
    /// The album, with its year and type.
    pub album: AlbumRef,
    /// Why it is offered.
    pub reason: PickReason,
}

/// How the person's weekly set is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoverPrefs {
    /// The day their set comes out, from 00:00 server time. Friday unless
    /// they chose another; a new day counts from the next week.
    pub set_day: SetDay,
    /// How far from what they know their picks go. Next door unless they
    /// chose another; a new depth rebuilds their pool, and this week's set
    /// stays as it is.
    pub depth: Depth,
}

// ---------------------------------------------------------------------------
// Past weeks
// ---------------------------------------------------------------------------

/// What the person did with a pick, as past weeks count and filter it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PickAnswer {
    /// They asked for it, whatever became of the request.
    Kept,
    /// It is on their Later pile.
    Later,
    /// They said they know it already.
    Known,
    /// They said no or not now, or turned its artist down.
    Passed,
    /// Nothing yet.
    Open,
}

impl PickAnswer {
    /// Every pick state, for [`Self::states`].
    const STATES: [PickState; 11] = [
        PickState::Asked,
        PickState::AwaitingApproval,
        PickState::Refused,
        PickState::Failed,
        PickState::Arrived,
        PickState::Later,
        PickState::No,
        PickState::Known,
        PickState::NotNow,
        PickState::NotArtist,
        PickState::Open,
    ];

    /// The answer a pick in `state` counts as.
    pub fn of(state: PickState) -> Self {
        match state {
            PickState::Asked
            | PickState::AwaitingApproval
            | PickState::Refused
            | PickState::Failed
            | PickState::Arrived => Self::Kept,
            PickState::Later => Self::Later,
            PickState::Known => Self::Known,
            PickState::No | PickState::NotNow | PickState::NotArtist => Self::Passed,
            PickState::Open => Self::Open,
        }
    }

    /// The pick states that count as this answer.
    pub fn states(self) -> Vec<PickState> {
        Self::STATES
            .into_iter()
            .filter(|state| Self::of(*state) == self)
            .collect()
    }
}

/// How many of some picks got each answer.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AnswerCounts {
    /// Asked for.
    pub kept: i64,
    /// On the Later pile.
    pub later: i64,
    /// Known already.
    pub known: i64,
    /// Passed on.
    pub passed: i64,
    /// Not answered.
    pub open: i64,
}

impl AnswerCounts {
    /// Counts `picks` more as `answer`.
    pub fn add(&mut self, answer: PickAnswer, picks: i64) {
        let count = match answer {
            PickAnswer::Kept => &mut self.kept,
            PickAnswer::Later => &mut self.later,
            PickAnswer::Known => &mut self.known,
            PickAnswer::Passed => &mut self.passed,
            PickAnswer::Open => &mut self.open,
        };
        *count += picks;
    }
}

/// One pick of a past week, as it was shown, with what became of it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PastPick {
    /// Its place in the set, from 1.
    pub pick_no: i64,
    /// The album, with its year and type.
    pub album: AlbumRef,
    /// Why it was in the set, as shown that week.
    pub reason: PickReason,
    /// What became of it.
    pub state: PickState,
    /// For a kept pick, when it arrived in the library (unix ms); null
    /// before it has.
    #[serde(default)]
    pub arrived_at: Option<i64>,
    /// For a kept pick, how often the person has played the album the
    /// library holds for it; null while they have not.
    #[serde(default)]
    pub plays: Option<i64>,
}

/// One past week: the picks of its set that match the filter, in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PastWeek {
    /// When its week started (unix ms).
    pub week_start_at: i64,
    /// The ISO week number of that day.
    pub week_number: i64,
    /// What the person did with every pick of the set, filter or not.
    pub tally: AnswerCounts,
    /// The picks that match the filter; every pick without one.
    pub picks: Vec<PastPick>,
}

/// What every set of the person's came to, counts only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct WeeksRecord {
    /// Picks in every set.
    pub picked: i64,
    /// Of them, the ones the person sampled.
    pub sampled: i64,
    /// What the person did with them.
    pub answers: AnswerCounts,
    /// From when "still played a month on" can be counted: the first kept
    /// pick's arrival plus 30 days (unix ms); null while none has arrived.
    #[serde(default)]
    pub counted_from_at: Option<i64>,
    /// Kept picks that arrived 30 days ago or more; null while none has.
    #[serde(default)]
    pub month_on: Option<i64>,
    /// Of those, the ones played after their first 30 days (within the
    /// first 90); null while `month_on` is.
    #[serde(default)]
    pub still_played: Option<i64>,
}

/// One page of past weeks, newest first; the first page also carries the
/// record of every set.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PastWeeksPage {
    #[serde(flatten)]
    pub page: Page<PastWeek>,
    /// On the first page only.
    #[schema(required = true)]
    pub record: Option<WeeksRecord>,
}

/// An album the person kept from a set, and the week it was first picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FoundPick {
    /// The album's release-group id, lowercased.
    pub release_group_mbid: String,
    /// When that week started (unix ms).
    pub week_start_at: i64,
    /// Its ISO week number.
    pub week_number: i64,
}

/// The albums the person kept from their sets, newest week first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FoundOnDiscover {
    pub items: Vec<FoundPick>,
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

/// Which of an album's tracks an album preview answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AlbumPreviewMode {
    /// The first tracks with a preview, in album order.
    #[default]
    First,
    /// Up to three clips that show the album: its opener, its best-known
    /// track and a deep cut, in that order.
    Audition,
}

/// What an audition clip is to its album.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewRole {
    /// The first track that has a preview.
    Opener,
    /// The track Deezer ranks highest, past the opener; the second track
    /// with a preview where the source has no ranking.
    BestKnown,
    /// The track nearest two thirds of the way through the album.
    DeepCut,
}

/// The service a preview clip comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewSource {
    /// Deezer.
    Deezer,
    /// iTunes.
    Itunes,
}

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
    /// The service the clip comes from.
    pub source: PreviewSource,
    /// What the clip is to its album, in the audition mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<PreviewRole>,
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

/// Ordered 30-second samples of an album.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AlbumPreviewResponse {
    /// Sampled tracks: in album order, or in the audition's play order.
    #[serde(default)]
    pub tracks: Vec<PreviewTrackItem>,
    /// Provider that served the previews, when found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<PreviewSource>,
}

// ---------------------------------------------------------------------------
// YouTube video lookups
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
