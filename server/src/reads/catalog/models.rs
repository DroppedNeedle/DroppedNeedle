//! Artist and album page DTOs: the native `/api/v3/artists` and
//! `/api/v3/albums` shapes.
//!
//! These keep v2's field names so the pages port over directly, minus the
//! fields v2 always left at their defaults (per-user follow state lives on
//! its own route). Everything is snake_case. Absent means unknown, never
//! zero-filled to look real.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

/// How one optional source fared while a page was built. Sources that
/// answered normally are left out of the map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SourceStatus {
    /// The source answered partially, or stale data stood in.
    Degraded,
    /// The source failed; its fields are absent.
    Error,
}

/// Per-source health for one page, keyed by source (`musicbrainz`,
/// `audiodb`, `lastfm`, ...). `None` when every source answered.
pub type ServiceStatus = Option<BTreeMap<String, SourceStatus>>;

/// Where a page's core data came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CatalogSource {
    /// MusicBrainz answered.
    Musicbrainz,
    /// MusicBrainz was down; the page was built from the local library.
    Library,
}

/// One external link on an artist page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ExternalLink {
    /// MusicBrainz relationship type (`official homepage`, `bandcamp`, ...).
    #[serde(rename = "type")]
    pub link_type: String,
    /// The URL.
    pub url: String,
    /// Display label (`Bandcamp`, `Wikipedia`, ...).
    pub label: String,
    /// `music`, `social`, `info` or `other`.
    pub category: String,
}

/// An artist's life span.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LifeSpan {
    /// Begin date, partial shapes allowed (`1991`, `1991-06`).
    pub begin: Option<String>,
    /// End date, partial shapes allowed.
    pub end: Option<String>,
    /// Whether the artist has ended, when MusicBrainz says.
    pub ended: Option<bool>,
}

/// Artist images from TheAudioDB. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ArtistImages {
    /// Thumbnail.
    pub thumb_url: Option<String>,
    /// Fanart.
    pub fanart_url: Option<String>,
    /// Second fanart.
    pub fanart_url_2: Option<String>,
    /// Third fanart.
    pub fanart_url_3: Option<String>,
    /// Fourth fanart.
    pub fanart_url_4: Option<String>,
    /// Wide thumbnail.
    pub wide_thumb_url: Option<String>,
    /// Banner.
    pub banner_url: Option<String>,
    /// Logo.
    pub logo_url: Option<String>,
    /// Clear art.
    pub clearart_url: Option<String>,
    /// Cutout.
    pub cutout_url: Option<String>,
}

/// `GET /artists/{artist_mbid}`: the artist header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistInfo {
    /// Artist name.
    pub name: String,
    /// Canonical artist MBID (after any MusicBrainz merge redirect).
    pub musicbrainz_id: String,
    /// Disambiguation comment.
    pub disambiguation: Option<String>,
    /// Artist type (`Person`, `Group`, ...).
    #[serde(rename = "type")]
    pub artist_type: Option<String>,
    /// ISO country code.
    pub country: Option<String>,
    /// Life span.
    pub life_span: Option<LifeSpan>,
    /// Up to ten tags, most voted first.
    pub tags: Vec<String>,
    /// Up to ten aliases.
    pub aliases: Vec<String>,
    /// Known streaming, store, social and reference links.
    pub external_links: Vec<ExternalLink>,
    /// Artist images already cached from TheAudioDB. The extended route
    /// fetches them, so a first visit may show none.
    pub images: ArtistImages,
    /// True when the library holds this artist.
    pub in_library: bool,
    /// Albums, newest first, filtered by the release-type preferences.
    pub albums: Vec<ReleaseItem>,
    /// Singles, newest first.
    pub singles: Vec<ReleaseItem>,
    /// EPs, newest first.
    pub eps: Vec<ReleaseItem>,
    /// Release groups MusicBrainz lists for the artist, before filtering.
    pub release_group_count: u64,
    /// Whether the caller follows the artist.
    pub followed: bool,
    /// Whether the caller asked for new releases to download on their own.
    pub auto_download: bool,
    /// Approval state of that ask: none, pending, approved, rejected or
    /// revoked.
    pub auto_download_state: String,
    /// Where the page came from.
    pub source: CatalogSource,
    /// Sources that degraded while building the page.
    pub service_status: ServiceStatus,
}

/// `GET /artists/{artist_mbid}/extended`: biography and portrait.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistExtendedInfo {
    /// Wikipedia introduction.
    pub description: Option<String>,
    /// Portrait from Wikidata/Commons.
    pub image: Option<String>,
    /// Artist images from TheAudioDB, fetched and cached by this call.
    pub images: ArtistImages,
}

/// One release group on an artist page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReleaseItem {
    /// Release-group MBID.
    pub id: String,
    /// Title.
    pub title: Option<String>,
    /// Primary type (`Album`, `EP`, `Single`).
    #[serde(rename = "type")]
    pub release_type: Option<String>,
    /// First release date.
    pub first_release_date: Option<String>,
    /// First release year.
    pub year: Option<i32>,
    /// True when the library holds this album.
    pub in_library: bool,
    /// True when an acquisition request is open for it.
    pub requested: bool,
}

/// `GET /artists/{artist_mbid}/releases`: one page of the discography,
/// filtered by the release-type preferences. Pages run across albums, then
/// EPs, then singles, each newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ArtistReleases {
    /// Albums on this page.
    pub albums: Vec<ReleaseItem>,
    /// Singles on this page.
    pub singles: Vec<ReleaseItem>,
    /// EPs on this page.
    pub eps: Vec<ReleaseItem>,
    /// Echoed offset.
    pub offset: u32,
    /// Echoed page size.
    pub limit: u32,
    /// Items on this page.
    pub returned_count: u32,
    /// Offset of the next page, when there is one.
    pub next_offset: Option<u32>,
    /// Whether another page exists.
    pub has_more: bool,
    /// Total items after filtering. Absent while `warming`.
    pub source_total_count: Option<u32>,
    /// True while the rest of a large discography is still being fetched
    /// in the background; ask again shortly for the full list.
    pub warming: bool,
    /// Where the list came from.
    pub source: CatalogSource,
    /// Sources that degraded while building the page.
    pub service_status: ServiceStatus,
}

/// Which listening-data provider answered a discovery section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum DiscoverySource {
    /// ListenBrainz.
    Listenbrainz,
    /// Last.fm.
    Lastfm,
}

/// One similar artist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SimilarArtist {
    /// Artist MBID.
    pub musicbrainz_id: String,
    /// Artist name.
    pub name: String,
    /// Listen count from the provider (zero when it reports none).
    pub listen_count: i64,
    /// True when the library holds this artist.
    pub in_library: bool,
}

/// `GET /artists/{artist_mbid}/similar`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SimilarArtistsResponse {
    /// Similar artists, most similar first.
    pub similar_artists: Vec<SimilarArtist>,
    /// Provider that answered.
    pub source: DiscoverySource,
    /// False when the provider is not set up; the list is then empty.
    pub configured: bool,
}

/// One of an artist's most played songs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopSong {
    /// Song title.
    pub title: String,
    /// Credited artist.
    pub artist_name: String,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Release group the listens point at, when known.
    pub release_group_mbid: Option<String>,
    /// Release the listens point at, when known.
    pub original_release_mbid: Option<String>,
    /// That release's title, when known.
    pub release_name: Option<String>,
    /// Listen or play count.
    pub listen_count: i64,
}

/// `GET /artists/{artist_mbid}/top-songs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TopSongsResponse {
    /// Songs, most played first.
    pub songs: Vec<TopSong>,
    /// Provider that answered.
    pub source: DiscoverySource,
    /// False when the provider is not set up.
    pub configured: bool,
}

/// One of an artist's most played albums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct TopAlbum {
    /// Album title.
    pub title: String,
    /// Credited artist.
    pub artist_name: String,
    /// Release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// Listen or play count.
    pub listen_count: i64,
    /// True when the library holds this album.
    pub in_library: bool,
    /// True when an acquisition request is open for it.
    pub requested: bool,
    /// The album's cover, when the release group is known.
    pub cover_url: Option<String>,
}

/// `GET /artists/{artist_mbid}/top-albums`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct TopAlbumsResponse {
    /// Albums, most played first.
    pub albums: Vec<TopAlbum>,
    /// Provider that answered.
    pub source: DiscoverySource,
    /// False when the provider is not set up.
    pub configured: bool,
}

/// A Last.fm tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LastFmTag {
    /// Tag name.
    pub name: String,
    /// Tag page.
    pub url: Option<String>,
}

/// A similar artist according to Last.fm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LastFmSimilarArtist {
    /// Artist name.
    pub name: String,
    /// Artist MBID, when Last.fm knows it.
    pub mbid: Option<String>,
    /// Similarity, 0 to 1.
    #[serde(rename = "match")]
    pub match_score: f64,
    /// Last.fm page.
    pub url: Option<String>,
}

/// `GET /artists/{artist_mbid}/lastfm`. Empty when Last.fm is off or the
/// user has no Last.fm key.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LastFmArtistEnrichment {
    /// Biography, HTML stripped.
    pub bio: Option<String>,
    /// Short summary, HTML stripped.
    pub summary: Option<String>,
    /// Tags.
    pub tags: Vec<LastFmTag>,
    /// Listener count.
    pub listeners: i64,
    /// Play count.
    pub playcount: i64,
    /// Similar artists.
    pub similar_artists: Vec<LastFmSimilarArtist>,
    /// Last.fm page.
    pub url: Option<String>,
}

/// `GET /albums/{album_id}/lastfm`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct LastFmAlbumEnrichment {
    /// Wiki summary, HTML stripped.
    pub summary: Option<String>,
    /// Tags.
    pub tags: Vec<LastFmTag>,
    /// Listener count.
    pub listeners: i64,
    /// Play count.
    pub playcount: i64,
    /// Last.fm page.
    pub url: Option<String>,
}

/// What kind of purchase a link offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PurchaseKind {
    /// Download store.
    Digital,
    /// Mail order: CD, vinyl, cassette.
    Physical,
    /// Free download.
    Free,
}

/// One "where to buy" link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PurchaseLink {
    /// Store key (`bandcamp`, `qobuz`, `itunes`, `amazon`, ..., `other`).
    pub store: String,
    /// Display name, or the bare domain for unknown stores.
    pub label: String,
    /// Store page.
    pub url: String,
    /// Digital, physical or free.
    pub kind: PurchaseKind,
}

/// `GET /albums/{album_id}/purchase-options`. Bandcamp sorts first, then
/// specialist stores, then the large storefronts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PurchaseOptionsResponse {
    /// Download stores.
    pub digital: Vec<PurchaseLink>,
    /// Mail-order stores.
    pub physical: Vec<PurchaseLink>,
    /// Free downloads.
    pub free: Vec<PurchaseLink>,
    /// Bandcamp search for the album, always present as the fallback.
    pub bandcamp_search_url: String,
}

/// `GET /artists/{artist_mbid}/purchase-options`: the artist's own store
/// pages (Bandcamp, merch shop).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ArtistPurchaseOptionsResponse {
    /// Store pages.
    pub links: Vec<PurchaseLink>,
    /// Bandcamp search for the artist, empty when the name is unknown.
    pub bandcamp_search_url: String,
}

/// `GET /albums/{album_id}/basic`: the album header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumBasicInfo {
    /// Album title.
    pub title: String,
    /// Canonical release-group MBID.
    pub musicbrainz_id: String,
    /// Credited artist.
    pub artist_name: String,
    /// Artist MBID, empty when the credit has none.
    pub artist_id: String,
    /// First release date.
    pub release_date: Option<String>,
    /// First release year.
    pub year: Option<i32>,
    /// Primary type (`Album`, `EP`, ...).
    #[serde(rename = "type")]
    pub album_type: Option<String>,
    /// Disambiguation comment.
    pub disambiguation: Option<String>,
    /// True when the library holds this album.
    pub in_library: bool,
    /// True when an acquisition request is open for it.
    pub requested: bool,
    /// The album's cover, served by the covers route.
    pub cover_url: Option<String>,
    /// Album thumbnail already cached from TheAudioDB.
    pub album_thumb_url: Option<String>,
    /// Where the page came from.
    pub source: CatalogSource,
    /// Sources that degraded while building the page.
    pub service_status: ServiceStatus,
}

/// One track on an album page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumTrack {
    /// Position within its disc.
    pub position: u32,
    /// Track title.
    pub title: String,
    /// Disc number, starting at 1.
    pub disc_number: u32,
    /// Length in milliseconds.
    pub length: Option<u64>,
    /// Recording MBID.
    pub recording_id: Option<String>,
    /// Release-track MBID.
    pub release_track_id: Option<String>,
    /// Medium format (`CD`, `Digital Media`, `DVD`, ...).
    pub media_format: Option<String>,
}

/// `GET /albums/{album_id}/tracks`: the tracklist of the edition the page
/// shows (pinned, owned, or MusicBrainz's best).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumTracksInfo {
    /// Tracks in disc and position order.
    pub tracks: Vec<AlbumTrack>,
    /// Track count.
    pub total_tracks: u32,
    /// Total length in milliseconds, when any track has one.
    pub total_length: Option<u64>,
    /// First label.
    pub label: Option<String>,
    /// Barcode.
    pub barcode: Option<String>,
    /// Release country.
    pub country: Option<String>,
    /// The edition shown.
    pub selected_release_mbid: Option<String>,
    /// Why that edition: `pin`, `owned`, `file_count` or `ranked`.
    pub pick_basis: Option<String>,
}

/// TheAudioDB album images. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumImages {
    /// Front thumbnail.
    pub album_thumb_url: Option<String>,
    /// Back cover.
    pub album_back_url: Option<String>,
    /// CD art.
    pub album_cdart_url: Option<String>,
    /// Spine.
    pub album_spine_url: Option<String>,
    /// 3D case.
    pub album_3d_case_url: Option<String>,
    /// 3D flat.
    pub album_3d_flat_url: Option<String>,
    /// 3D face.
    pub album_3d_face_url: Option<String>,
    /// 3D thumbnail.
    pub album_3d_thumb_url: Option<String>,
}

/// `GET /albums/{album_id}`: header, tracklist and artwork in one call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct AlbumInfo {
    /// The header.
    #[serde(flatten)]
    pub basic: AlbumBasicInfo,
    /// The tracklist.
    #[serde(flatten)]
    pub tracks: AlbumTracksInfo,
    /// TheAudioDB artwork, fetched and cached by this call.
    pub images: AlbumImages,
}

/// One edition (MusicBrainz release) of an album.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumEditionItem {
    /// Release MBID.
    pub release_mbid: String,
    /// Tracks across all media.
    pub track_count: u32,
    /// Release title.
    pub title: Option<String>,
    /// Disambiguation (`deluxe edition`, `remaster`).
    pub disambiguation: Option<String>,
    /// Release date.
    pub date: Option<String>,
    /// Release country.
    pub country: Option<String>,
    /// Packaging.
    pub packaging: Option<String>,
    /// Status (`Official`, `Promotion`, ...).
    pub status: Option<String>,
    /// True when the library's copy is identified as this edition.
    pub is_owned: bool,
    /// True when a curator pinned this edition.
    pub is_pinned: bool,
}

/// `GET /albums/{album_id}/editions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AlbumEditionsResponse {
    /// Every edition MusicBrainz lists.
    pub items: Vec<AlbumEditionItem>,
    /// Pinned edition, if any.
    pub pinned_release_mbid: Option<String>,
    /// Owned edition, if identified.
    pub owned_release_mbid: Option<String>,
    /// The edition the album page shows.
    pub selected_release_mbid: Option<String>,
    /// Why that edition: `pin`, `owned`, `file_count` or `ranked`.
    pub selected_basis: Option<String>,
}

/// An album in a discovery row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct DiscoveryAlbum {
    /// Release-group MBID.
    pub musicbrainz_id: String,
    /// Title.
    pub title: String,
    /// Credited artist.
    pub artist_name: String,
    /// Artist MBID, when known.
    pub artist_id: Option<String>,
    /// First release year.
    pub year: Option<i32>,
    /// True when the library holds this album.
    pub in_library: bool,
    /// True when an acquisition request is open for it.
    pub requested: bool,
    /// The album's cover.
    pub cover_url: Option<String>,
}

/// `GET /albums/{album_id}/similar`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SimilarAlbumsResponse {
    /// Albums by similar artists.
    pub albums: Vec<DiscoveryAlbum>,
    /// Provider that answered.
    pub source: DiscoverySource,
    /// False when ListenBrainz is not set up.
    pub configured: bool,
}

/// `GET /albums/{album_id}/more-by-artist`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct MoreByArtistResponse {
    /// Other release groups by the artist, newest first.
    pub albums: Vec<DiscoveryAlbum>,
    /// The artist's name, empty when MusicBrainz had nothing.
    pub artist_name: String,
}

/// Query for `GET /artists/{artist_mbid}/releases`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ReleasesQuery {
    /// Items to skip (default 0).
    pub offset: Option<u32>,
    /// Page size, 1 to 200 (default 50).
    pub limit: Option<u32>,
}

/// Query for the artist discovery sections.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct DiscoveryQuery {
    /// How many to return, 1 to 50.
    pub count: Option<u32>,
    /// Provider to ask; default is the primary music source setting.
    pub source: Option<DiscoverySource>,
}

/// Query for `GET /artists/{artist_mbid}/lastfm`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ArtistLastFmQuery {
    /// Artist name, the Last.fm fallback key.
    pub artist_name: String,
}

/// Query for `GET /artists/{artist_mbid}/purchase-options`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct ArtistPurchaseQuery {
    /// Artist name for the Bandcamp search fallback.
    #[serde(default)]
    pub name: String,
}

/// Query for the album discovery rows.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct AlbumArtistQuery {
    /// The album's artist MBID.
    pub artist_id: String,
    /// How many to return, 1 to 30 (default 10).
    pub count: Option<u32>,
}

/// Query for `GET /albums/{album_id}/lastfm`.
#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct AlbumLastFmQuery {
    /// Artist name for the Last.fm lookup.
    pub artist_name: String,
    /// Album title for the Last.fm lookup.
    pub album_name: String,
}
