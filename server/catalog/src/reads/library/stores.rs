//! The domain structs the local catalog reads return (one per entity:
//! [`AlbumRecord`], [`ArtistRecord`], [`TrackRecord`]), their filters and
//! errors, and the lyrics port (the provider fetch sits behind it). The
//! store that fills them is [`super::store::LocalCatalog`]. Futures are
//! boxed by hand because `async fn` is not object-safe.
//!
//! Absence is `None` or an empty page, never failure. Counts are
//! streamable-only: tracks with `availability` other than `indexed` are
//! invisible to every method here.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;

use droppedneedle_kernel::db::DbError;
use thiserror::Error;

/// Boxed sendable future for object-safe async ports.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Every way a store call can fail. The string goes to the log only,
/// never the wire: handlers render the fixed 5xx body.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum StoreError {
    /// The database was too busy to answer in time. The string names the
    /// operation for the log; handlers answer 503 with `Retry-After`.
    #[error("database busy during {0}")]
    Busy(String),
    /// Anything unexpected. The string goes to the log only, never the wire.
    #[error("store failure: {0}")]
    Internal(String),
}

impl StoreError {
    /// A failed read named `operation`: busy stays busy, anything else is
    /// internal.
    pub(crate) fn db(operation: &str, error: DbError) -> Self {
        match error {
            DbError::Busy { .. } => Self::Busy(operation.to_owned()),
            error => Self::Internal(error.to_string()),
        }
    }
}

droppedneedle_kernel::sql_choice! {
    /// Album sort orders the list methods accept.
    pub enum AlbumSort {
        /// Title, case folded.
        Name,
        /// Import time.
        DateAdded,
        /// Release year, unknowns last.
        Year,
        /// Album artist, case folded.
        Artist,
        /// One seeded shuffle per walk.
        Random,
        /// Oldest imports first.
        Rediscover,
    }
}

droppedneedle_kernel::sql_choice! {
    /// Artist sort orders the list method accepts.
    pub enum ArtistSort {
        /// Display name, case folded.
        Name,
        /// Album-artist album count.
        AlbumCount,
        /// Albums the artist appears on without leading.
        AppearanceCount,
        /// First import time.
        DateAdded,
    }
}

droppedneedle_kernel::sql_choice! {
    /// Track sort orders the list method accepts.
    pub enum TrackSort {
        /// Title, case folded.
        Title,
        /// Import time.
        DateAdded,
        /// Track artist, then album, disc and track number.
        Artist,
        /// Album, then disc and track number.
        Album,
        /// One seeded shuffle per walk.
        Random,
    }
}

droppedneedle_kernel::sql_choice! {
    /// Which artists a listing covers.
    #[derive(Default)]
    pub enum ArtistScope {
        /// Every credited artist.
        #[default]
        All,
        /// Artists leading at least one album.
        AlbumArtists,
        /// Artists appearing without leading.
        Contributors,
        /// Artists leading an album or credited on a streamable track, which
        /// leaves out empty rows such as an unused placeholder.
        WithMusic,
    }
}

/// Album list filter, shared by the catalog and browse surfaces.
#[derive(Debug, Clone, Default)]
pub struct AlbumFilter {
    /// Title/artist substring, case folded.
    pub q: Option<String>,
    /// Restrict to one album artist.
    pub artist_id: Option<String>,
    /// Restrict to one decade start year.
    pub decade: Option<i64>,
    /// Restrict to one primary track format, already lowercased.
    pub format: Option<String>,
}

/// Track list filter.
#[derive(Debug, Clone, Default)]
pub struct TrackFilter {
    /// Title/artist/album substring, case folded.
    pub q: Option<String>,
    /// Restrict to one album.
    pub album_id: Option<String>,
    /// Restrict to one credited artist.
    pub artist_id: Option<String>,
    /// Restrict to one genre tag, case folded.
    pub genre: Option<String>,
}

/// One library album with its streamable-only aggregates: what the album
/// projection reads, and what every wire album is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumRecord {
    /// Local album id.
    pub id: String,
    /// Album title.
    pub title: String,
    /// Album artist display name.
    pub artist_name: String,
    /// Local album-artist id.
    pub artist_id: String,
    /// Linked release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Linked exact release, when known.
    pub release_mbid: Option<String>,
    /// Linked artist id, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// True when an identity row exists.
    pub linked: bool,
    /// Streamable track count.
    pub track_count: u64,
    /// Summed streamable-track duration, seconds.
    pub total_duration_seconds: f64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Most common track format, when any track exists.
    pub format: Option<String>,
    /// Release year, when known.
    pub year: Option<i64>,
    /// True for compilations.
    pub is_compilation: bool,
    /// True when an artwork row exists.
    pub cover_available: bool,
    /// The version of the album's own art (a folder image or an embedded
    /// picture), when it has some. It goes up when the art changes.
    pub art_version: Option<i64>,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// The album's open MusicBrainz contribution, if any.
    pub contribution_id: Option<String>,
    /// That contribution's state (`draft`, `ready`, `seeded`, ...).
    pub contribution_state: Option<String>,
}

/// Why library management leaves an album's files alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementHoldRecord {
    /// `your_choice` for a person's own exclusion, else why the app
    /// stepped back: `files_changed`, `retag_followed`, `run_undone` or
    /// `replaced_hands_off`.
    pub kind: String,
    /// Since when, unix milliseconds.
    pub held_at: i64,
    /// Row revision of a person's exclusion.
    pub exclusion_revision: Option<i64>,
    /// True while the album waits for its turn to be handed back.
    pub handing_back: bool,
}

/// One library artist with its streamable-only aggregates: what the artist
/// projection and its counts read, and what every wire artist is built
/// from.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistRecord {
    /// Local artist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Linked MusicBrainz id, when identified.
    pub artist_mbid: Option<String>,
    /// True when an identity row exists.
    pub linked: bool,
    /// Albums led as album artist.
    pub album_count: u64,
    /// Streamable tracks credited.
    pub track_count: u64,
    /// Albums with an appearance but no lead.
    pub appearance_album_count: u64,
    /// Streamable tracks credited on albums led by someone else.
    pub appearance_track_count: u64,
    /// First import time, unix seconds.
    pub date_added: Option<f64>,
}

/// One streamable library track: what the track projection reads, and
/// what every wire track is built from.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackRecord {
    /// Local track id.
    pub id: String,
    /// Track title.
    pub title: String,
    /// Owning local album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Track artist display name.
    pub artist_name: String,
    /// Local track-artist id, when credited.
    pub artist_id: Option<String>,
    /// Album artist display name.
    pub album_artist_name: String,
    /// Local album-artist id of the owning album.
    pub album_artist_id: Option<String>,
    /// Linked MusicBrainz recording, when identified.
    pub recording_mbid: Option<String>,
    /// Linked release group of the owning album, when identified.
    pub release_group_mbid: Option<String>,
    /// Linked MusicBrainz id of the track artist (the album artist when
    /// the track has no credit), when identified.
    pub artist_mbid: Option<String>,
    /// Linked MusicBrainz id of the album artist, when identified.
    pub album_artist_mbid: Option<String>,
    /// Disc number.
    pub disc_number: i64,
    /// Track number within the disc.
    pub track_number: i64,
    /// Release year, when known.
    pub year: Option<i64>,
    /// First genre tag, when tagged.
    pub genre: Option<String>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// Container or codec label.
    pub format: String,
    /// Bit rate in kbit/s, when probed.
    pub bit_rate: Option<i64>,
    /// Sample rate, when probed.
    pub sample_rate: Option<i64>,
    /// Bit depth, when probed (lossless and PCM files).
    pub bit_depth: Option<i64>,
    /// Channel count, when probed.
    pub channels: Option<i64>,
    /// File size, bytes.
    pub file_size_bytes: i64,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// True when the owning album has artwork.
    pub cover_available: bool,
}

/// Library totals. Track and size totals count streamable tracks only.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsRecord {
    /// Albums in the catalog.
    pub total_albums: u64,
    /// Artists in the catalog.
    pub total_artists: u64,
    /// Streamable tracks in the catalog.
    pub total_tracks: u64,
    /// Summed streamable-track size, bytes.
    pub total_size_bytes: i64,
    /// Streamable-track counts by format label.
    pub format_breakdown: HashMap<String, u64>,
}

/// Library totals that live outside the catalog tables: the review
/// queue, unidentified albums and the last finished scan.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatsExtras {
    /// Identification reviews waiting on a person.
    pub review_count: u64,
    /// Albums with streamable tracks and no MusicBrainz identity.
    pub local_only_count: u64,
    /// When the last scan finished successfully, unix seconds.
    pub last_scan_at: Option<f64>,
}

/// The upgrade settings album status judges tracks against, read from the
/// download policy on every call.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct UpgradePolicy {
    /// Tier a track must reach before it stops counting as an upgrade
    /// candidate, e.g. `lossless`.
    pub quality_cutoff: Option<String>,
    /// Whether upgrades are switched on at all.
    pub upgrade_allowed: bool,
}

impl UpgradePolicy {
    /// The download policy's upgrade settings now; upgrades off when there
    /// is no policy (test builds) or it cannot be read.
    pub fn read(
        policy: Option<
            &droppedneedle_kernel::runtime_config::Live<
                droppedneedle_kernel::runtime_config::sections::DownloadPolicy,
            >,
        >,
    ) -> Self {
        let Some(policy) = policy else {
            return Self::default();
        };
        match policy.get() {
            Ok(policy) => Self {
                quality_cutoff: Some(policy.quality_cutoff.clone()),
                upgrade_allowed: policy.upgrade_allowed,
            },
            Err(error) => {
                tracing::warn!(%error, "download policy unreadable; album status shows no upgrades");
                Self::default()
            }
        }
    }
}

/// One genre with streamable-only counts.
#[derive(Debug, Clone, PartialEq)]
pub struct GenreRecord {
    /// Display name, representative casing.
    pub name: String,
    /// Folded name, the stable key.
    pub folded_name: String,
    /// Streamable tracks carrying this tag.
    pub track_count: u64,
    /// Albums holding at least one such track.
    pub album_count: u64,
}

/// One decade with its album count.
#[derive(Debug, Clone, PartialEq)]
pub struct DecadeRecord {
    /// Decade start year, e.g. 1990.
    pub decade: i64,
    /// Albums dated to this decade.
    pub album_count: u64,
}

/// Artist listing: page items, match total, album-artist total,
/// contributor total.
pub type ArtistListing = (Vec<ArtistRecord>, u64, u64, u64);

/// Where a page of a long list starts: the seed of a random order, and the
/// sort key of the last row already sent (none on the first page).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct After {
    /// The shuffle seed; read by random orders only.
    pub seed: u32,
    /// The last sent row's sort key.
    pub key: Option<Vec<rusqlite::types::Value>>,
}

/// One page of a long list: its rows, the list's total, and the last row's
/// sort key when more rows follow.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyedPage<T> {
    /// This page's rows.
    pub items: Vec<T>,
    /// Rows in the whole list.
    pub total: u64,
    /// The key to read on from; `None` on the last page.
    pub next: Option<Vec<rusqlite::types::Value>>,
}

/// A page of artists with the album-artist and contributor totals.
pub type ArtistKeyedListing = (KeyedPage<ArtistRecord>, u64, u64);

/// Stored lyrics for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricDoc {
    /// Lines in order: text plus optional start, milliseconds.
    pub lines: Vec<(String, Option<i64>)>,
    /// True when lines carry timestamps.
    pub synced: bool,
}

/// Lyrics reads for one track.
///
/// Production serves [`crate::reads::lyrics::LyricsService`]: the lyrics
/// the track's file carries first, then the online lookup.
/// None is absence (the handler answers 404), never failure.
pub trait LyricsPort: Send + Sync {
    /// Stored lyrics for one streamable track id.
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>>;
}

/// Every held release group and every open album request, lowercase and
/// sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumMbids {
    /// Release groups held with at least one streamable track.
    pub owned: Vec<String>,
    /// Album ids with an open acquisition request.
    pub requested: Vec<String>,
}

/// What reading one catalog track's tags from disk found.
#[derive(Debug, Clone, PartialEq)]
pub enum TagRead {
    /// The file's tags.
    Found(Box<droppedneedle_library::tags::AudioTag>),
    /// No streamable catalog track has this id.
    Unknown,
    /// The catalog knows the track, but its file is gone from disk.
    Gone,
    /// The file resolves outside every library root.
    Outside,
    /// The file is there but its tags cannot be read.
    Unreadable,
}
