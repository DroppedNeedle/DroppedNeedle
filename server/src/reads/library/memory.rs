//! Test fakes: a scriptable in-memory catalog, favorite flags, lyrics,
//! and failing stores proving the 5xx leak contract.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::stores::{
    AlbumFilter, AlbumRecord, AlbumSort, ArtistListing, ArtistRecord, ArtistScope, ArtistSort,
    BoxFuture, DecadeRecord, FavoriteReads, GenreRecord, LibraryCatalog, LyricDoc, LyricsPort,
    StatsRecord, StoreError, TrackFilter, TrackRecord, TrackSort,
};

fn fail() -> StoreError {
    StoreError::Internal("injected fault with /tmp/secret.db host=db.internal".to_owned())
}

/// In-memory catalog with deterministic ordering. Seeds mirror the SQLite
/// adapter's semantics (streamable-only, folded matching) so handler briefs
/// pin behavior, not SQL.
#[derive(Debug, Default)]
pub struct MemoryCatalog {
    albums: Mutex<Vec<AlbumRecord>>,
    artists: Mutex<Vec<ArtistRecord>>,
    tracks: Mutex<Vec<TrackRecord>>,
    /// Album id pairs sharing a release group: album -> sibling albums.
    copies: Mutex<HashMap<String, Vec<AlbumRecord>>>,
    /// Artist id -> led albums / appearance albums.
    led: Mutex<HashMap<String, Vec<AlbumRecord>>>,
    appearances: Mutex<HashMap<String, Vec<AlbumRecord>>>,
    /// Folded genre name -> (display, track ids in order).
    genre_tracks: Mutex<HashMap<String, (String, Vec<TrackRecord>)>>,
}

impl MemoryCatalog {
    /// Empty catalog: every list is empty, every lookup misses.
    pub fn new() -> Self {
        Self::default()
    }

    /// Seed albums (also used for copies/led/appearance lookups by id).
    pub fn with_albums(self, albums: Vec<AlbumRecord>) -> Self {
        *self
            .albums
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = albums;
        self
    }

    /// Seed artists.
    pub fn with_artists(self, artists: Vec<ArtistRecord>) -> Self {
        *self
            .artists
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = artists;
        self
    }

    /// Seed tracks.
    pub fn with_tracks(self, tracks: Vec<TrackRecord>) -> Self {
        *self
            .tracks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = tracks;
        self
    }

    /// Seed release-group siblings for one album.
    pub fn with_copies(self, album_id: &str, copies: Vec<AlbumRecord>) -> Self {
        self.copies
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(album_id.to_owned(), copies);
        self
    }

    /// Seed led and appearance albums for one artist.
    pub fn with_artist_albums(
        self,
        artist_id: &str,
        led: Vec<AlbumRecord>,
        appearances: Vec<AlbumRecord>,
    ) -> Self {
        self.led
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(artist_id.to_owned(), led);
        self.appearances
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(artist_id.to_owned(), appearances);
        self
    }

    /// Seed one genre's tracks in serving order.
    pub fn with_genre(self, folded: &str, display: &str, tracks: Vec<TrackRecord>) -> Self {
        self.genre_tracks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(folded.to_owned(), (display.to_owned(), tracks));
        self
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Case-folded substring match, mirroring the SQL LIKE on folded columns.
fn matches(haystack: &str, q: &str) -> bool {
    crate::db::fold_text(haystack).contains(&crate::db::fold_text(q))
}

fn paged<T: Clone>(items: &[T], limit: u64, offset: u64) -> (Vec<T>, u64) {
    let total = items.len() as u64;
    let start = offset.min(total) as usize;
    let end = start.saturating_add(limit as usize).min(items.len());
    (items[start..end].to_vec(), total)
}

fn album_matches(record: &AlbumRecord, filter: &AlbumFilter) -> bool {
    if let Some(artist_id) = filter.artist_id.as_deref()
        && record.artist_id != artist_id
    {
        return false;
    }
    if let Some(decade) = filter.decade
        && record
            .year
            .is_none_or(|year| year < decade || year >= decade + 10)
    {
        return false;
    }
    if let Some(format) = filter.format.as_deref()
        && record
            .format
            .as_deref()
            .is_none_or(|primary| !primary.eq_ignore_ascii_case(format))
    {
        return false;
    }
    filter
        .q
        .as_deref()
        .is_none_or(|q| matches(&record.title, q) || matches(&record.artist_name, q))
}

fn sort_albums(records: &mut [AlbumRecord], sort: AlbumSort, descending: bool) {
    match sort {
        AlbumSort::Name => records.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id))),
        AlbumSort::DateAdded => records.sort_by(|a, b| {
            a.date_added
                .partial_cmp(&b.date_added)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        }),
        AlbumSort::Year => records.sort_by(|a, b| a.year.cmp(&b.year).then(a.id.cmp(&b.id))),
        AlbumSort::Artist => records.sort_by(|a, b| {
            a.artist_name
                .cmp(&b.artist_name)
                .then(a.title.cmp(&b.title))
                .then(a.id.cmp(&b.id))
        }),
        AlbumSort::Random => records.sort_by(|a, b| b.id.cmp(&a.id)),
        AlbumSort::Rediscover => records.sort_by(|a, b| {
            a.date_added
                .partial_cmp(&b.date_added)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.id.cmp(&b.id))
        }),
    }
    if descending && !matches!(sort, AlbumSort::Random) {
        records.reverse();
    }
}

fn track_matches(record: &TrackRecord, filter: &TrackFilter) -> bool {
    if let Some(album_id) = filter.album_id.as_deref()
        && record.album_id != album_id
    {
        return false;
    }
    if let Some(artist_id) = filter.artist_id.as_deref()
        && record.artist_id.as_deref() != Some(artist_id)
    {
        return false;
    }
    if let Some(genre) = filter.genre.as_deref()
        && record
            .genre
            .as_deref()
            .is_none_or(|tag| crate::db::fold_text(tag) != crate::db::fold_text(genre.trim()))
    {
        return false;
    }
    filter.q.as_deref().is_none_or(|q| {
        matches(&record.title, q)
            || matches(&record.artist_name, q)
            || matches(&record.album_title, q)
    })
}

impl LibraryCatalog for MemoryCatalog {
    fn list_albums<'a>(
        &'a self,
        filter: &'a AlbumFilter,
        sort: AlbumSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let mut records: Vec<AlbumRecord> = lock(&self.albums)
                .iter()
                .filter(|record| album_matches(record, filter))
                .cloned()
                .collect();
            sort_albums(&mut records, sort, descending);
            Ok(paged(&records, limit, offset))
        })
    }

    fn get_album<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            Ok(lock(&self.albums)
                .iter()
                .find(|record| record.id == id)
                .cloned())
        })
    }

    fn get_album_by_release_group<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            let mut matches: Vec<AlbumRecord> = lock(&self.albums)
                .iter()
                .filter(|record| record.release_group_mbid.as_deref() == Some(release_group_mbid))
                .cloned()
                .collect();
            matches.sort_by(|a, b| {
                a.date_added
                    .partial_cmp(&b.date_added)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            });
            Ok(matches.into_iter().next())
        })
    }

    fn album_tracks<'a>(
        &'a self,
        album_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let mut records: Vec<TrackRecord> = lock(&self.tracks)
                .iter()
                .filter(|record| record.album_id == album_id)
                .cloned()
                .collect();
            records.sort_by(|a, b| {
                (a.disc_number, a.track_number, &a.id).cmp(&(b.disc_number, b.track_number, &b.id))
            });
            Ok(paged(&records, limit, offset))
        })
    }

    fn album_copies<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            Ok(lock(&self.copies)
                .get(album_id)
                .cloned()
                .unwrap_or_default())
        })
    }

    fn list_artists<'a>(
        &'a self,
        scope: ArtistScope,
        q: Option<&'a str>,
        sort: ArtistSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<ArtistListing, StoreError>> {
        Box::pin(async move {
            let in_q = |record: &ArtistRecord| q.is_none_or(|text| matches(&record.name, text));
            let all: Vec<ArtistRecord> = lock(&self.artists)
                .iter()
                .filter(|record| in_q(record))
                .cloned()
                .collect();
            let led_total = all.iter().filter(|record| record.album_count > 0).count() as u64;
            let contributor_total = all
                .iter()
                .filter(|record| record.album_count == 0 && record.track_count > 0)
                .count() as u64;
            let mut records: Vec<ArtistRecord> = all
                .into_iter()
                .filter(|record| match scope {
                    ArtistScope::All => true,
                    ArtistScope::AlbumArtists => record.album_count > 0,
                    ArtistScope::Contributors => record.album_count == 0 && record.track_count > 0,
                })
                .collect();
            match sort {
                ArtistSort::Name => {
                    records.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)))
                }
                ArtistSort::AlbumCount => records.sort_by(|a, b| {
                    a.album_count
                        .cmp(&b.album_count)
                        .then(a.name.cmp(&b.name))
                        .then(a.id.cmp(&b.id))
                }),
                ArtistSort::DateAdded => records.sort_by(|a, b| {
                    a.date_added
                        .partial_cmp(&b.date_added)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.id.cmp(&b.id))
                }),
            }
            if descending {
                records.reverse();
            }
            let (items, total) = paged(&records, limit, offset);
            Ok((items, total, led_total, contributor_total))
        })
    }

    fn get_artist<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ArtistRecord>, StoreError>> {
        Box::pin(async move {
            Ok(lock(&self.artists)
                .iter()
                .find(|record| record.id == id)
                .cloned())
        })
    }

    fn artist_albums<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let mut records = lock(&self.led).get(artist_id).cloned().unwrap_or_default();
            sort_albums(&mut records, AlbumSort::Name, false);
            Ok(paged(&records, limit, offset))
        })
    }

    fn artist_appearances<'a>(
        &'a self,
        artist_id: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<AlbumRecord>, u64), StoreError>> {
        Box::pin(async move {
            let mut records = lock(&self.appearances)
                .get(artist_id)
                .cloned()
                .unwrap_or_default();
            sort_albums(&mut records, AlbumSort::Name, false);
            Ok(paged(&records, limit, offset))
        })
    }

    fn list_tracks<'a>(
        &'a self,
        filter: &'a TrackFilter,
        sort: TrackSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let mut records: Vec<TrackRecord> = lock(&self.tracks)
                .iter()
                .filter(|record| track_matches(record, filter))
                .cloned()
                .collect();
            match sort {
                TrackSort::Title => {
                    records.sort_by(|a, b| a.title.cmp(&b.title).then(a.id.cmp(&b.id)))
                }
                TrackSort::DateAdded => records.sort_by(|a, b| {
                    a.date_added
                        .partial_cmp(&b.date_added)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(a.id.cmp(&b.id))
                }),
            }
            if descending {
                records.reverse();
            }
            Ok(paged(&records, limit, offset))
        })
    }

    fn get_track<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<TrackRecord>, StoreError>> {
        Box::pin(async move {
            Ok(lock(&self.tracks)
                .iter()
                .find(|record| record.id == id)
                .cloned())
        })
    }

    fn stats<'a>(&'a self) -> BoxFuture<'a, Result<StatsRecord, StoreError>> {
        Box::pin(async move {
            let tracks = lock(&self.tracks);
            let mut format_breakdown = HashMap::new();
            for record in tracks.iter() {
                *format_breakdown.entry(record.format.clone()).or_insert(0) += 1;
            }
            Ok(StatsRecord {
                total_albums: lock(&self.albums).len() as u64,
                total_artists: lock(&self.artists).len() as u64,
                total_tracks: tracks.len() as u64,
                total_size_bytes: tracks.iter().map(|record| record.file_size_bytes).sum(),
                format_breakdown,
            })
        })
    }

    fn recently_added<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<AlbumRecord>, StoreError>> {
        Box::pin(async move {
            let mut records: Vec<AlbumRecord> = lock(&self.albums).clone();
            records.sort_by(|a, b| {
                b.date_added
                    .partial_cmp(&a.date_added)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            });
            records.truncate(limit as usize);
            Ok(records)
        })
    }

    fn genres<'a>(&'a self) -> BoxFuture<'a, Result<Vec<GenreRecord>, StoreError>> {
        Box::pin(async move {
            let mut records: Vec<GenreRecord> = lock(&self.genre_tracks)
                .iter()
                .map(|(folded, (name, tracks))| {
                    let albums: HashSet<&str> = tracks
                        .iter()
                        .map(|record| record.album_id.as_str())
                        .collect();
                    GenreRecord {
                        name: name.clone(),
                        folded_name: folded.clone(),
                        track_count: tracks.len() as u64,
                        album_count: albums.len() as u64,
                    }
                })
                .collect();
            records.sort_by(|a, b| b.track_count.cmp(&a.track_count).then(a.name.cmp(&b.name)));
            Ok(records)
        })
    }

    fn genre_tracks<'a>(
        &'a self,
        genre_folded: &'a str,
        limit: u64,
        offset: u64,
    ) -> BoxFuture<'a, Result<(Vec<TrackRecord>, u64), StoreError>> {
        Box::pin(async move {
            let folded = crate::db::fold_text(genre_folded.trim());
            let records = lock(&self.genre_tracks)
                .get(&folded)
                .cloned()
                .map_or(Vec::new(), |(_, tracks)| tracks);
            Ok(paged(&records, limit, offset))
        })
    }

    fn decades<'a>(&'a self) -> BoxFuture<'a, Result<Vec<DecadeRecord>, StoreError>> {
        Box::pin(async move {
            let mut counts: HashMap<i64, u64> = HashMap::new();
            for record in lock(&self.albums).iter().filter_map(|record| record.year) {
                *counts.entry(record / 10 * 10).or_insert(0) += 1;
            }
            let mut records: Vec<DecadeRecord> = counts
                .into_iter()
                .map(|(decade, album_count)| DecadeRecord {
                    decade,
                    album_count,
                })
                .collect();
            records.sort_by_key(|record| record.decade);
            Ok(records)
        })
    }

    fn newest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let mut records: Vec<TrackRecord> = lock(&self.tracks).clone();
            records.sort_by(|a, b| {
                b.date_added
                    .partial_cmp(&a.date_added)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            });
            records.truncate(limit as usize);
            Ok(records)
        })
    }

    fn oldest_tracks<'a>(
        &'a self,
        limit: u64,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let mut records: Vec<TrackRecord> = lock(&self.tracks).clone();
            records.sort_by(|a, b| {
                a.date_added
                    .partial_cmp(&b.date_added)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.id.cmp(&b.id))
            });
            records.truncate(limit as usize);
            Ok(records)
        })
    }

    fn random_tracks<'a>(
        &'a self,
        limit: u64,
        decade: Option<i64>,
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let mut records: Vec<TrackRecord> = lock(&self.tracks)
                .iter()
                .filter(|record| {
                    decade.is_none_or(|start| {
                        record
                            .year
                            .is_some_and(|year| year >= start && year < start + 10)
                    })
                })
                .cloned()
                .collect();
            records.sort_by(|a, b| a.id.cmp(&b.id));
            records.truncate(limit as usize);
            Ok(records)
        })
    }
}

/// Scriptable favorite flags: user -> kind -> ids.
#[derive(Debug, Default)]
pub struct MemoryFavorites {
    flags: Mutex<HashMap<(String, String), HashSet<String>>>,
}

impl MemoryFavorites {
    /// No favorites for anyone.
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark ids of one kind as favorited by one user.
    pub fn with_favorites(self, user_id: &str, kind: &str, ids: &[&str]) -> Self {
        self.flags
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                (user_id.to_owned(), kind.to_owned()),
                ids.iter().map(ToString::to_string).collect(),
            );
        self
    }
}

impl FavoriteReads for MemoryFavorites {
    fn filter_favorites<'a>(
        &'a self,
        user_id: &'a str,
        kind: &'a str,
        ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            let flags = lock(&self.flags);
            let marked = flags.get(&(user_id.to_owned(), kind.to_owned()));
            Ok(ids
                .iter()
                .filter(|id| marked.is_some_and(|set| set.contains(id.as_str())))
                .cloned()
                .collect())
        })
    }

    fn favorite_counts<'a>(
        &'a self,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(u64, u64, u64), StoreError>> {
        Box::pin(async move {
            let flags = lock(&self.flags);
            let count = |kind: &str| {
                flags
                    .get(&(user_id.to_owned(), kind.to_owned()))
                    .map_or(0, |set| set.len() as u64)
            };
            Ok((count("album"), count("artist"), count("track")))
        })
    }
}

/// Scriptable stored lyrics: track id -> doc.
#[derive(Debug, Default)]
pub struct MemoryLyrics {
    docs: Mutex<HashMap<String, LyricDoc>>,
}

impl MemoryLyrics {
    /// No stored lyrics.
    pub fn new() -> Self {
        Self::default()
    }

    /// Store one doc.
    pub fn with_doc(self, track_id: &str, doc: LyricDoc) -> Self {
        self.docs
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(track_id.to_owned(), doc);
        self
    }
}

impl LyricsPort for MemoryLyrics {
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move { Ok(lock(&self.docs).get(track_id).cloned()) })
    }
}

/// Catalog that fails every call. The fault text carries a fake path and
/// host so leak briefs can prove neither reaches the wire.
#[derive(Debug, Default)]
pub struct FailingCatalog;

impl FailingCatalog {
    /// Failing store.
    pub fn new() -> Self {
        Self
    }
}

macro_rules! fail_catalog {
    ($name:ident ( $($arg:ident : $type:ty),* ) -> $ret:ty) => {
        fn $name<'a>(
            &'a self,
            $($arg : $type),*
        ) -> BoxFuture<'a, Result<$ret, StoreError>> {
            $(let _ = &$arg;)*
            Box::pin(async move { Err(fail()) })
        }
    };
}

impl LibraryCatalog for FailingCatalog {
    fail_catalog!(list_albums(filter: &'a AlbumFilter, sort: AlbumSort, descending: bool, limit: u64, offset: u64) -> (Vec<AlbumRecord>, u64));
    fail_catalog!(get_album(id: &'a str) -> Option<AlbumRecord>);
    fail_catalog!(get_album_by_release_group(release_group_mbid: &'a str) -> Option<AlbumRecord>);
    fail_catalog!(album_tracks(album_id: &'a str, limit: u64, offset: u64) -> (Vec<TrackRecord>, u64));
    fail_catalog!(album_copies(album_id: &'a str) -> Vec<AlbumRecord>);
    fail_catalog!(list_artists(scope: ArtistScope, q: Option<&'a str>, sort: ArtistSort, descending: bool, limit: u64, offset: u64) -> ArtistListing);
    fail_catalog!(get_artist(id: &'a str) -> Option<ArtistRecord>);
    fail_catalog!(artist_albums(artist_id: &'a str, limit: u64, offset: u64) -> (Vec<AlbumRecord>, u64));
    fail_catalog!(artist_appearances(artist_id: &'a str, limit: u64, offset: u64) -> (Vec<AlbumRecord>, u64));
    fail_catalog!(list_tracks(filter: &'a TrackFilter, sort: TrackSort, descending: bool, limit: u64, offset: u64) -> (Vec<TrackRecord>, u64));
    fail_catalog!(get_track(id: &'a str) -> Option<TrackRecord>);
    fail_catalog!(stats() -> StatsRecord);
    fail_catalog!(recently_added(limit: u64) -> Vec<AlbumRecord>);
    fail_catalog!(genres() -> Vec<GenreRecord>);
    fail_catalog!(genre_tracks(genre_folded: &'a str, limit: u64, offset: u64) -> (Vec<TrackRecord>, u64));
    fail_catalog!(decades() -> Vec<DecadeRecord>);
    fail_catalog!(newest_tracks(limit: u64) -> Vec<TrackRecord>);
    fail_catalog!(oldest_tracks(limit: u64) -> Vec<TrackRecord>);
    fail_catalog!(random_tracks(limit: u64, decade: Option<i64>) -> Vec<TrackRecord>);
}

/// Favorites that fail every call.
#[derive(Debug, Default)]
pub struct FailingFavorites;

impl FailingFavorites {
    /// Failing store.
    pub fn new() -> Self {
        Self
    }
}

impl FavoriteReads for FailingFavorites {
    fn filter_favorites<'a>(
        &'a self,
        _user_id: &'a str,
        _kind: &'a str,
        _ids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move { Err(fail()) })
    }

    fn favorite_counts<'a>(
        &'a self,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<(u64, u64, u64), StoreError>> {
        Box::pin(async move { Err(fail()) })
    }
}

/// Lyrics port that fails every call.
#[derive(Debug, Default)]
pub struct FailingLyrics;

impl FailingLyrics {
    /// Failing port.
    pub fn new() -> Self {
        Self
    }
}

impl LyricsPort for FailingLyrics {
    fn get<'a>(
        &'a self,
        _track_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move { Err(fail()) })
    }
}
