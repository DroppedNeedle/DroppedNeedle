//! The library pages' reads on [`LocalCatalog`], and the caller's
//! favorites.
//!
//! Conventions:
//!
//! - Every track read filters `local_tracks.availability = 'indexed'`.
//!   Missing or excluded files are invisible here, which is what keeps all
//!   counts streamable-only.
//! - Text filters match the pre-folded columns (`title_folded`,
//!   `album_artist_name_folded`, `folded_name`) with an escaped LIKE pattern
//!   folded by the shared [`fold_text`](droppedneedle_kernel::db::fold_text), the
//!   same fold the scan path writes.
//! - Optional filters bind NULL-means-absent so every statement keeps a
//!   static bind list; only id IN-lists build placeholders.
//! - Retired artists (`retired_into_artist_id` set) are merged away and read
//!   as absent everywhere. Live, owned albums and a track's recording id
//!   mean what [`super::definitions`] says.
//! - Long lists page by keyset: each page reads the rows after the last
//!   one sent, in an order that ends in the row id, so a deep page costs
//!   what the first does. Their totals come from the maintained counts or
//!   are remembered per catalog revision.
//! - List methods page ids first, then aggregate over the page only; the
//!   0005 read models (trigram FTS index, maintained format totals) keep
//!   text search and stats off full scans.
//! - Track text search runs through the trigram FTS index: every word of 3
//!   or more characters must appear in the title, artist or album (any
//!   column, any order), so "midnight radio" finds a "Radio" track by a
//!   "Midnight" artist. Queries with no such word keep the LIKE substring
//!   match.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::Row;
use rusqlite::types::Value;

use super::sql;
use super::store::{
    Credits, LocalCatalog, album_row, artist_row, count, first, int, rows, scalar, text, texts,
    track_row, unsigned,
};
use super::stores::{
    After, AlbumFilter, AlbumRecord, AlbumSort, ArtistKeyedListing, ArtistListing, ArtistRecord,
    ArtistScope, ArtistSort, DecadeRecord, GenreRecord, KeyedPage, ManagementHoldRecord,
    StatsRecord, StoreError, TrackFilter, TrackRecord, TrackSort,
};

/// Escape the LIKE metacharacters in a folded filter, then wrap for
/// substring matching.
fn like_pattern(raw: &str) -> String {
    let mut folded = droppedneedle_kernel::db::fold_text(raw);
    folded = folded
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{folded}%")
}

/// FTS5 trigram match expression for the words of 3 or more characters in
/// a track search, or `None` when there is no such word or the query holds
/// a NUL byte (which would truncate the bind). Each word becomes a quoted
/// phrase and the phrases are AND-ed, so every word must appear, in any of
/// the indexed columns. A trigram phrase is exact substring, but only for 3
/// or more characters: shorter phrases silently match nothing, which is why
/// [`TextSearch`] checks short words with LIKE instead.
pub(crate) fn fts_match_phrase(raw: &str) -> Option<String> {
    let folded = droppedneedle_kernel::db::fold_text(raw);
    if folded.contains('\0') {
        return None;
    }
    let phrases = folded
        .split_whitespace()
        .filter(|word| word.chars().count() >= 3)
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!phrases.is_empty()).then(|| phrases.join(" AND "))
}

/// A track text search on the FTS index. Every word must appear in the
/// title, artist or album, in any order: words of 3+ characters through
/// the trigram index, shorter ones ("the xx") with LIKE over those
/// candidates. A query with no word of 3+ characters ("U2", "xx", one CJK
/// character) is not a `TextSearch`; callers keep the LIKE substring match
/// on the whole query for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TextSearch {
    /// The FTS expression from [`fts_match_phrase`].
    pub fts: String,
    /// Escaped LIKE patterns for the words under 3 characters.
    pub short_words: Vec<String>,
}

impl TextSearch {
    /// Split a query, or `None` when the LIKE path must run.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let fts = fts_match_phrase(raw)?;
        let short_words = droppedneedle_kernel::db::fold_text(raw)
            .split_whitespace()
            .filter(|word| word.chars().count() < 3)
            // A lone "-" or "&" between words is punctuation, not a word.
            .filter(|word| word.chars().any(char::is_alphanumeric))
            .map(like_pattern)
            .collect();
        Some(Self { fts, short_words })
    }

    /// The predicate over alias `t`; bind [`Self::binds`] in order.
    pub(crate) fn sql(&self) -> String {
        let mut text = sql::TRACK_TEXT_MATCH.to_owned();
        for _ in &self.short_words {
            text.push_str(sql::TRACK_WORD_LIKE);
        }
        text
    }

    /// The binds for [`Self::sql`]: the FTS expression, then each short
    /// word's pattern once per column.
    pub(crate) fn binds(&self) -> Vec<String> {
        let mut binds = vec![self.fts.clone()];
        for word in &self.short_words {
            binds.extend(std::iter::repeat_n(word.clone(), 3));
        }
        binds
    }
}

/// The album list's filter values, in [`sql::ALBUM_FILTER`]'s order.
fn album_filter_values(filter: &AlbumFilter) -> Vec<Value> {
    let pattern = filter.q.as_deref().map(like_pattern);
    let pattern = text(pattern.as_deref());
    let artist = text(filter.artist_id.as_deref());
    let decade = int(filter.decade);
    let format = text(filter.format.as_deref());
    vec![
        pattern.clone(),
        pattern.clone(),
        pattern,
        artist.clone(),
        artist,
        decade.clone(),
        decade.clone(),
        decade,
        format.clone(),
        format,
    ]
}

/// The track list's text and scope values: the search's binds, or the
/// LIKE pattern four times, then the album, artist and genre binds of
/// [`sql::TRACK_SCOPE`].
fn track_filter_values(
    search: Option<&TextSearch>,
    pattern: Option<&str>,
    filter: &TrackFilter,
    genre: Option<&str>,
) -> Vec<Value> {
    let mut values = match search {
        Some(search) => search.binds().into_iter().map(Value::Text).collect(),
        None => vec![text(pattern); 4],
    };
    let album = text(filter.album_id.as_deref());
    let artist = text(filter.artist_id.as_deref());
    let genre = text(genre);
    values.extend([
        album.clone(),
        album,
        artist.clone(),
        artist,
        genre.clone(),
        genre.clone(),
        genre,
    ]);
    values
}

/// The artist list's name filter, bound twice.
fn name_filter(q: Option<&str>) -> Vec<Value> {
    let pattern = text(q.map(like_pattern).as_deref());
    vec![pattern.clone(), pattern]
}

/// The last `width` columns of a page row: its sort key.
fn key_of(row: &Row<'_>, width: usize) -> rusqlite::Result<Vec<Value>> {
    let from = row.as_ref().column_count() - width;
    (from..from + width).map(|index| row.get(index)).collect()
}

/// A page read one row long, split: the rows to send, and the last sent
/// row's key when more follow.
fn split_page<T>(mut rows: Vec<(T, Vec<Value>)>, limit: u64) -> (Vec<T>, Option<Vec<Value>>) {
    let more = rows.len() as u64 > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next = if more {
        rows.last().map(|(_, key)| key.clone())
    } else {
        None
    };
    (rows.into_iter().map(|(item, _)| item).collect(), next)
}

/// The first remembered total, zero when none.
fn first_total(totals: &[u64]) -> u64 {
    totals.first().copied().unwrap_or(0)
}

impl LocalCatalog {
    /// One page of albums plus the match total, remembered per catalog
    /// revision.
    pub async fn list_albums(
        &self,
        filter: &AlbumFilter,
        sort: AlbumSort,
        descending: bool,
        limit: u64,
        after: &After,
    ) -> Result<KeyedPage<AlbumRecord>, StoreError> {
        let keys = sql::album_keys(sort, descending);
        let filter_values = album_filter_values(filter);
        let mut values = Vec::new();
        if sort == AlbumSort::Random {
            values.extend(sql::shuffle_values(after.seed));
        }
        values.extend(filter_values.iter().cloned());
        if let Some(key) = &after.key {
            values.extend(sql::after_values(&keys, key));
        }
        values.push(count(limit.saturating_add(1)));
        let text = sql::album_page(sort, descending, after.key.is_some());
        // Page the keys first: filtering and ordering touch albums only,
        // so the track aggregation below runs over the page, never the
        // catalog.
        let (items, next) = self
            .album_key_page("library.albums.list", text, values, keys.len(), limit)
            .await?;
        let total = self
            .totals(
                format!("albums {filter:?}"),
                "library.albums.count",
                move |conn| {
                    let total: i64 = scalar(conn, sql::ALBUM_COUNT.text(), &filter_values)?;
                    Ok(vec![unsigned(total)])
                },
            )
            .await?;
        Ok(KeyedPage {
            items,
            total: first_total(&total),
            next,
        })
    }

    /// One page of albums by a statement that selects their sort keys, the
    /// id last, one row past `limit`.
    async fn album_key_page(
        &self,
        name: &'static str,
        text: String,
        values: Vec<Value>,
        width: usize,
        limit: u64,
    ) -> Result<(Vec<AlbumRecord>, Option<Vec<Value>>), StoreError> {
        let keyed = self
            .read(name, move |conn| {
                rows(conn, &text, &values, |row| {
                    let key = key_of(row, width)?;
                    let id: String = row.get(width - 1)?;
                    Ok((id, key))
                })
            })
            .await?;
        let (ids, next) = split_page(keyed, limit);
        let mut albums = self.albums_by_id(&ids).await?;
        let ordered = ids.iter().filter_map(|id| albums.remove(id)).collect();
        Ok((ordered, next))
    }

    /// The album projection for `ids`, by id.
    async fn albums_by_id(
        &self,
        ids: &[String],
    ) -> Result<HashMap<String, AlbumRecord>, StoreError> {
        if ids.is_empty() {
            return Ok(HashMap::new());
        }
        let values = texts(ids);
        let albums = self
            .read("library.albums.list", move |conn| {
                rows(
                    conn,
                    &sql::album_rows(ListLen::new(values.len())),
                    &values,
                    album_row,
                )
            })
            .await?;
        Ok(albums
            .into_iter()
            .map(|album| (album.id.clone(), album))
            .collect())
    }

    /// One album by id. None is absence, never failure.
    pub async fn get_album(&self, id: &str) -> Result<Option<AlbumRecord>, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        self.read("library.albums.get", move |conn| {
            first(conn, sql::ALBUM_BY_ID.text(), &values, album_row)
        })
        .await
    }

    /// Why library management leaves the album's files alone. None when
    /// it does not. A person's own choice wins over the app's step back.
    pub async fn management_hold(
        &self,
        album_id: &str,
    ) -> Result<Option<ManagementHoldRecord>, StoreError> {
        let values = vec![Value::Text(album_id.to_owned())];
        self.read("library.albums.management_hold", move |conn| {
            first(conn, sql::MANAGEMENT_HOLD.text(), &values, |row| {
                Ok(ManagementHoldRecord {
                    kind: row.get("kind")?,
                    held_at: row.get("held_at")?,
                    exclusion_revision: row.get("exclusion_revision")?,
                    handing_back: row.get("handing_back")?,
                })
            })
        })
        .await
    }

    /// The live album one release-group mbid names: the oldest copy with
    /// files, else the oldest copy. None is absence, never failure.
    pub async fn get_album_by_release_group(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<AlbumRecord>, StoreError> {
        let values = vec![Value::Text(release_group_mbid.to_owned())];
        self.read("library.albums.match", move |conn| {
            first(conn, sql::ALBUM_BY_GROUP.text(), &values, album_row)
        })
        .await
    }

    /// One page of an album's streamable tracks, disc/track order, plus total.
    pub async fn album_tracks(
        &self,
        album_id: &str,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<TrackRecord>, u64), StoreError> {
        let album = Value::Text(album_id.to_owned());
        let page = vec![album.clone(), count(limit), count(offset)];
        let tracks = self
            .read("library.albums.tracks", move |conn| {
                rows(conn, sql::ALBUM_TRACKS.text(), &page, track_row)
            })
            .await?;
        let total: i64 = self
            .read("library.albums.tracks.count", move |conn| {
                scalar(conn, sql::ALBUM_TRACK_COUNT.text(), &[album])
            })
            .await?;
        Ok((tracks, unsigned(total)))
    }

    /// Other live albums sharing the album's release group, oldest first.
    /// Empty when the album is unidentified or has no siblings.
    pub async fn album_copies(&self, album_id: &str) -> Result<Vec<AlbumRecord>, StoreError> {
        let album = Value::Text(album_id.to_owned());
        let values = vec![album.clone(), album];
        self.read("library.albums.copies", move |conn| {
            rows(conn, sql::ALBUM_COPIES.text(), &values, album_row)
        })
        .await
    }

    /// One page of artists in scope at an offset, for the player apps,
    /// plus the match total, the album-artist total, and the contributor
    /// total (totals honor `q`, ignore paging).
    pub async fn list_artists(
        &self,
        scope: ArtistScope,
        q: Option<&str>,
        sort: ArtistSort,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> Result<ArtistListing, StoreError> {
        let mut page_values = name_filter(q);
        page_values.extend([count(limit), count(offset)]);
        // Page the ids first; counts aggregate over the page below.
        let ids: Vec<String> = self
            .read("library.artists.list", move |conn| {
                rows(
                    conn,
                    &sql::artist_offset_page(scope, sort, descending),
                    &page_values,
                    |row| row.get(0),
                )
            })
            .await?;
        let artists = self.hydrate_artists(&ids).await?;
        let (total, led, contributors) = self.artist_totals(scope, q).await?;
        Ok((artists, total, led, contributors))
    }

    /// One page of artists in scope from a cursor, plus the album-artist
    /// and contributor totals. Totals honor `q` and are remembered per
    /// catalog revision.
    pub async fn artist_list(
        &self,
        scope: ArtistScope,
        q: Option<&str>,
        sort: ArtistSort,
        descending: bool,
        limit: u64,
        after: &After,
    ) -> Result<ArtistKeyedListing, StoreError> {
        let keys = sql::artist_keys(scope, sort, descending);
        let width = keys.len();
        let mut values = name_filter(q);
        if let Some(key) = &after.key {
            values.extend(sql::after_values(&keys, key));
        }
        values.push(count(limit.saturating_add(1)));
        let text = sql::artist_page(scope, sort, descending, after.key.is_some());
        // Page the ids first; counts aggregate over the page below.
        let keyed = self
            .read("library.artists.list", move |conn| {
                rows(conn, &text, &values, |row| {
                    let key = key_of(row, width)?;
                    let id: String = row.get(width - 1)?;
                    Ok((id, key))
                })
            })
            .await?;
        let (ids, next) = split_page(keyed, limit);
        let items = self.hydrate_artists(&ids).await?;
        let (total, led, contributors) = self.artist_totals(scope, q).await?;
        Ok((KeyedPage { items, total, next }, led, contributors))
    }

    /// Artists in scope, album artists and contributors matching `q`,
    /// remembered per catalog revision.
    async fn artist_totals(
        &self,
        scope: ArtistScope,
        q: Option<&str>,
    ) -> Result<(u64, u64, u64), StoreError> {
        let names = name_filter(q);
        let totals = self
            .totals(
                format!("artists {scope:?} {q:?}"),
                "library.artists.count",
                move |conn| {
                    let mut statement = conn.prepare_cached(sql::ARTIST_TOTALS.text())?;
                    let (all, led, contributors): (i64, i64, i64) = statement
                        .query_row(rusqlite::params_from_iter(&names), |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                        })?;
                    let total = if matches!(scope, ArtistScope::All) {
                        all
                    } else {
                        scalar(conn, &sql::artist_scope_count(scope), &names)?
                    };
                    Ok(vec![unsigned(total), unsigned(led), unsigned(contributors)])
                },
            )
            .await?;
        match totals.as_slice() {
            [total, led, contributors] => Ok((*total, *led, *contributors)),
            _ => Ok((0, 0, 0)),
        }
    }

    /// Artist rows with their counts for `ids`, in `ids` order; unknown ids
    /// drop out.
    pub(crate) async fn hydrate_artists(
        &self,
        ids: &[String],
    ) -> Result<Vec<ArtistRecord>, StoreError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let values = texts(ids);
        let artists = self
            .read("library.artists.list", move |conn| {
                rows(
                    conn,
                    &sql::artist_rows(ListLen::new(values.len())),
                    &values,
                    artist_row,
                )
            })
            .await?;
        let artists = self.with_counts(artists).await?;
        let mut by_id: HashMap<String, ArtistRecord> = artists
            .into_iter()
            .map(|artist| (artist.id.clone(), artist))
            .collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// `artists` with their album and credit counts filled in.
    async fn with_counts(
        &self,
        mut artists: Vec<ArtistRecord>,
    ) -> Result<Vec<ArtistRecord>, StoreError> {
        let ids: Vec<String> = artists.iter().map(|artist| artist.id.clone()).collect();
        if ids.is_empty() {
            return Ok(artists);
        }
        let (album_counts, credit_counts) = self.artist_counts(&ids).await?;
        for artist in &mut artists {
            artist.album_count = album_counts.get(&artist.id).copied().unwrap_or(0);
            let credits: Credits = credit_counts.get(&artist.id).copied().unwrap_or_default();
            (
                artist.track_count,
                artist.appearance_album_count,
                artist.appearance_track_count,
            ) = credits;
        }
        Ok(artists)
    }

    /// Batched artist aggregates for one id set: owned albums led per id,
    /// plus [`Credits`] per id. Ids with no rows are absent from the maps
    /// and read as zero.
    async fn artist_counts(
        &self,
        ids: &[String],
    ) -> Result<(HashMap<String, u64>, HashMap<String, Credits>), StoreError> {
        let values = texts(ids);
        let albums = self
            .read("library.artists.list", move |conn| {
                rows(
                    conn,
                    &sql::artist_album_counts(ListLen::new(values.len())),
                    &values,
                    |row| Ok((row.get::<_, String>(0)?, unsigned(row.get(1)?))),
                )
            })
            .await?;
        let values = texts(ids);
        let credits = self
            .read("library.artists.list", move |conn| {
                rows(
                    conn,
                    &sql::artist_credit_counts(ListLen::new(values.len())),
                    &values,
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            (
                                unsigned(row.get(1)?),
                                unsigned(row.get(2)?),
                                unsigned(row.get(3)?),
                            ),
                        ))
                    },
                )
            })
            .await?;
        Ok((albums.into_iter().collect(), credits.into_iter().collect()))
    }

    /// One artist by id. None is absence, never failure.
    pub async fn get_artist(&self, id: &str) -> Result<Option<ArtistRecord>, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        let found = self
            .read("library.artists.get", move |conn| {
                first(conn, sql::ARTIST_BY_ID.text(), &values, artist_row)
            })
            .await?;
        let Some(found) = found else {
            return Ok(None);
        };
        Ok(self.with_counts(vec![found]).await?.pop())
    }

    /// One page of live albums led by the artist, name order, plus total.
    pub async fn led_albums(
        &self,
        artist_id: &str,
        limit: u64,
        after: &After,
    ) -> Result<KeyedPage<AlbumRecord>, StoreError> {
        let keys = sql::title_keys();
        let artist = Value::Text(artist_id.to_owned());
        let mut values = vec![artist.clone()];
        if let Some(key) = &after.key {
            values.extend(sql::after_values(&keys, key));
        }
        values.push(count(limit.saturating_add(1)));
        let text = sql::led_album_page(after.key.is_some());
        let (items, next) = self
            .album_key_page("library.artists.albums", text, values, keys.len(), limit)
            .await?;
        let total: i64 = self
            .read("library.artists.albums.count", move |conn| {
                scalar(conn, sql::LED_ALBUM_COUNT.text(), &[artist])
            })
            .await?;
        Ok(KeyedPage {
            items,
            total: unsigned(total),
            next,
        })
    }

    /// One page of albums where the artist appears without leading, name
    /// order, plus total.
    pub async fn artist_appearances(
        &self,
        artist_id: &str,
        limit: u64,
        after: &After,
    ) -> Result<KeyedPage<AlbumRecord>, StoreError> {
        let keys = sql::title_keys();
        let artist = Value::Text(artist_id.to_owned());
        let filter = vec![artist.clone(), artist];
        let mut values = filter.clone();
        if let Some(key) = &after.key {
            values.extend(sql::after_values(&keys, key));
        }
        values.push(count(limit.saturating_add(1)));
        let text = sql::appearance_page(after.key.is_some());
        let (items, next) = self
            .album_key_page(
                "library.artists.appearances",
                text,
                values,
                keys.len(),
                limit,
            )
            .await?;
        let total: i64 = self
            .read("library.artists.appearances.count", move |conn| {
                scalar(conn, sql::APPEARANCE_COUNT.text(), &filter)
            })
            .await?;
        Ok(KeyedPage {
            items,
            total: unsigned(total),
            next,
        })
    }

    /// One page of streamable tracks plus the match total: the maintained
    /// count when unfiltered, else remembered per catalog revision.
    pub async fn list_tracks(
        &self,
        filter: &TrackFilter,
        sort: TrackSort,
        descending: bool,
        limit: u64,
        after: &After,
    ) -> Result<KeyedPage<TrackRecord>, StoreError> {
        // Words of 3+ characters search the FTS index; anything else
        // keeps the LIKE substring match.
        let search = filter.q.as_deref().and_then(TextSearch::parse);
        // Miss oracle: when no track text matches, the page and total
        // are empty whatever the other filters say (they only narrow).
        if let Some(search) = &search {
            let fts = vec![Value::Text(search.fts.clone())];
            let hit: bool = self
                .read("library.tracks.search", move |conn| {
                    scalar(conn, sql::TRACK_MISS.text(), &fts)
                })
                .await?;
            if !hit {
                return Ok(KeyedPage {
                    items: Vec::new(),
                    total: 0,
                    next: None,
                });
            }
        }
        let pattern = match search {
            Some(_) => None,
            None => filter.q.as_deref().map(like_pattern),
        };
        let genre = filter
            .genre
            .as_deref()
            .map(|name| droppedneedle_kernel::db::fold_text(name.trim()));
        let filter_values = track_filter_values(
            search.as_ref(),
            pattern.as_deref(),
            filter,
            genre.as_deref(),
        );
        let short = search
            .as_ref()
            .map(|search| ListLen::new(search.short_words.len()));
        let keys = sql::track_keys(sort, descending);
        let width = keys.len();
        let mut values = Vec::new();
        if sort == TrackSort::Random {
            values.extend(sql::shuffle_values(after.seed));
        }
        values.extend(filter_values.iter().cloned());
        if let Some(key) = &after.key {
            values.extend(sql::after_values(&keys, key));
        }
        values.push(count(limit.saturating_add(1)));
        let after_cursor = after.key.is_some();
        let text = match short {
            Some(short) => sql::track_page_words(short, sort, descending, after_cursor),
            None => sql::track_page_like(sort, descending, after_cursor),
        };
        let keyed = self
            .read("library.tracks.list", move |conn| {
                rows(conn, &text, &values, |row| {
                    Ok((track_row(row)?, key_of(row, width)?))
                })
            })
            .await?;
        let (items, next) = split_page(keyed, limit);
        let unfiltered = filter.q.is_none()
            && filter.album_id.is_none()
            && filter.artist_id.is_none()
            && filter.genre.is_none();
        // Unfiltered, the maintained exact count of indexed tracks
        // (migration 0005) answers.
        let total = if unfiltered {
            let total: i64 = self
                .read("library.tracks.count", |conn| {
                    scalar(conn, sql::TRACK_TOTAL.text(), &[])
                })
                .await?;
            unsigned(total)
        } else {
            let totals = self
                .totals(
                    format!("tracks {filter:?}"),
                    "library.tracks.count",
                    move |conn| {
                        let total: i64 = match short {
                            Some(short) => {
                                scalar(conn, &sql::track_count_words(short), &filter_values)?
                            }
                            None => scalar(conn, sql::TRACK_COUNT_LIKE.text(), &filter_values)?,
                        };
                        Ok(vec![unsigned(total)])
                    },
                )
                .await?;
            first_total(&totals)
        };
        Ok(KeyedPage { items, total, next })
    }

    /// One streamable track by id. None covers unknown ids and tracks
    /// whose file is missing or excluded.
    pub async fn get_track(&self, id: &str) -> Result<Option<TrackRecord>, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        self.read("library.tracks.get", move |conn| {
            first(conn, sql::TRACK_BY_ID.text(), &values, track_row)
        })
        .await
    }

    /// Whether one streamable track's file carries lyrics, as the lyrics
    /// sweep last found. False for unknown ids.
    pub async fn track_has_lyrics(&self, id: &str) -> Result<bool, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        self.read("library.tracks.lyrics", move |conn| {
            scalar(conn, sql::TRACK_HAS_LYRICS.text(), &values)
        })
        .await
    }

    /// Library totals.
    pub async fn stats(&self) -> Result<StatsRecord, StoreError> {
        // Track totals come from the maintained per-format rows
        // (migration 0005); the zero-count rows they keep stay out of
        // the breakdown, matching GROUP BY exactly.
        let (albums, artists, tracks, total_size_bytes): (i64, i64, i64, i64) = self
            .read("library.stats", |conn| {
                let mut statement = conn.prepare_cached(sql::STATS.text())?;
                statement.query_row([], |row| {
                    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                })
            })
            .await?;
        let formats = self
            .read("library.stats.formats", |conn| {
                rows(conn, sql::STATS_FORMATS.text(), &[], |row| {
                    Ok((row.get::<_, String>(0)?, unsigned(row.get(1)?)))
                })
            })
            .await?;
        Ok(StatsRecord {
            total_albums: unsigned(albums),
            total_artists: unsigned(artists),
            total_tracks: unsigned(tracks),
            total_size_bytes,
            format_breakdown: formats.into_iter().collect(),
        })
    }

    /// Playable tracks and the folders holding at least one, across every
    /// library folder. A track is playable while its file is indexed.
    /// Counting the folders groups every indexed track and player apps poll
    /// it, so the counts are kept until the catalog changes.
    pub async fn playable_counts(&self) -> Result<(i64, i64), StoreError> {
        let counted = self
            .totals(
                "library.playable_counts".to_owned(),
                "library.playable_counts",
                |conn| {
                    let mut statement = conn.prepare_cached(sql::PLAYABLE_COUNTS.text())?;
                    statement.query_row([], |row| {
                        Ok(vec![unsigned(row.get(0)?), unsigned(row.get(1)?)])
                    })
                },
            )
            .await?;
        match counted.as_slice() {
            [tracks, folders] => Ok((
                i64::try_from(*tracks).unwrap_or(i64::MAX),
                i64::try_from(*folders).unwrap_or(i64::MAX),
            )),
            _ => Ok((0, 0)),
        }
    }

    /// Newest owned albums first, capped: the ids from the creation index,
    /// then the projection for those albums only.
    pub async fn recently_added(&self, limit: u64) -> Result<Vec<AlbumRecord>, StoreError> {
        let values = vec![count(limit)];
        let ids: Vec<String> = self
            .read("library.albums.recent", move |conn| {
                rows(conn, sql::RECENT_ALBUM_IDS.text(), &values, |row| {
                    row.get(0)
                })
            })
            .await?;
        let mut albums = self.albums_by_id(&ids).await?;
        Ok(ids.iter().filter_map(|id| albums.remove(id)).collect())
    }

    /// Genres by descending track count, name tiebreak.
    pub async fn genres(&self) -> Result<Vec<GenreRecord>, StoreError> {
        self.read("library.genres", |conn| {
            rows(conn, sql::GENRES.text(), &[], |row| {
                Ok(GenreRecord {
                    name: row.get("name")?,
                    folded_name: row.get("folded_name")?,
                    track_count: unsigned(row.get("track_count")?),
                    album_count: unsigned(row.get("album_count")?),
                })
            })
        })
        .await
    }

    /// One page of a genre's streamable tracks, artist/album/disc/track
    /// order, plus total. Empty when the genre is unknown.
    pub async fn genre_tracks(
        &self,
        genre_folded: &str,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<TrackRecord>, u64), StoreError> {
        let folded = Value::Text(droppedneedle_kernel::db::fold_text(genre_folded.trim()));
        let filter = vec![folded.clone(), folded];
        let mut page = filter.clone();
        page.extend([count(limit), count(offset)]);
        let tracks = self
            .read("library.genres.tracks", move |conn| {
                rows(conn, sql::GENRE_TRACKS.text(), &page, track_row)
            })
            .await?;
        let total: i64 = self
            .read("library.genres.tracks.count", move |conn| {
                scalar(conn, sql::GENRE_TRACK_COUNT.text(), &filter)
            })
            .await?;
        Ok((tracks, unsigned(total)))
    }

    /// Decade shelves of owned albums, ascending. Albums without a year
    /// sit outside.
    pub async fn decades(&self) -> Result<Vec<DecadeRecord>, StoreError> {
        self.read("library.decades", |conn| {
            rows(conn, sql::DECADES.text(), &[], |row| {
                Ok(DecadeRecord {
                    decade: row.get("decade")?,
                    album_count: unsigned(row.get("album_count")?),
                })
            })
        })
        .await
    }

    /// Suggestion candidates: newest imports.
    pub async fn newest_tracks(&self, limit: u64) -> Result<Vec<TrackRecord>, StoreError> {
        let values = vec![count(limit)];
        self.read("library.tracks.newest", move |conn| {
            rows(conn, sql::NEWEST_TRACKS.text(), &values, track_row)
        })
        .await
    }

    /// Suggestion candidates: oldest imports.
    pub async fn oldest_tracks(&self, limit: u64) -> Result<Vec<TrackRecord>, StoreError> {
        let values = vec![count(limit)];
        self.read("library.tracks.oldest", move |conn| {
            rows(conn, sql::OLDEST_TRACKS.text(), &values, track_row)
        })
        .await
    }

    /// Suggestion candidates: random streamable tracks, optionally from
    /// one decade.
    pub async fn random_tracks(
        &self,
        limit: u64,
        decade: Option<i64>,
    ) -> Result<Vec<TrackRecord>, StoreError> {
        // Two phases: a uniform random pick over the narrow rows, then
        // the wide columns for the picks alone.
        let decade = int(decade);
        let values = vec![decade.clone(), decade.clone(), decade, count(limit)];
        let ids: Vec<String> = self
            .read("library.tracks.random", move |conn| {
                rows(conn, sql::RANDOM_TRACK_IDS.text(), &values, |row| {
                    row.get(0)
                })
            })
            .await?;
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let values = texts(&ids);
        let tracks = self
            .read("library.tracks.random", move |conn| {
                rows(
                    conn,
                    &sql::tracks_in(ListLen::new(values.len())),
                    &values,
                    track_row,
                )
            })
            .await?;
        let mut by_id: HashMap<String, TrackRecord> = tracks
            .into_iter()
            .map(|track| (track.id.clone(), track))
            .collect();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// Which of the ids the user favorited, for one kind
    /// (`library_user_favorites.item_kind`: `album`, `artist` or `track`).
    pub async fn filter_favorites(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashSet<String>, StoreError> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut values = vec![
            Value::Text(user_id.to_owned()),
            Value::Text(kind.to_owned()),
        ];
        values.extend(texts(ids));
        let found = self
            .read("library.favorites.filter", move |conn| {
                rows(
                    conn,
                    &sql::favorites_in(ListLen::new(values.len() - 2)),
                    &values,
                    |row| row.get::<_, String>(0),
                )
            })
            .await?;
        Ok(found.into_iter().collect())
    }

    /// Favorite counts for the caller: albums, artists, tracks.
    pub async fn favorite_counts(&self, user_id: &str) -> Result<(u64, u64, u64), StoreError> {
        let values = vec![Value::Text(user_id.to_owned())];
        let kinds = self
            .read("library.favorites.counts", move |conn| {
                rows(conn, sql::FAVORITE_COUNTS.text(), &values, |row| {
                    Ok((row.get::<_, String>(0)?, unsigned(row.get(1)?)))
                })
            })
            .await?;
        let (mut albums, mut artists, mut tracks) = (0, 0, 0);
        for (kind, total) in kinds {
            match kind.as_str() {
                "album" => albums = total,
                "artist" => artists = total,
                "track" => tracks = total,
                _ => {}
            }
        }
        Ok((albums, artists, tracks))
    }
}
