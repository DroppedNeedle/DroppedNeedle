//! Playlist rows over `playlists`, `playlist_tracks` and `playlist_covers`.
//!
//! One playlist model serves the native routes, both compat protocols and
//! remote playlist imports. Entries that point at a library file carry its
//! local track id in `library_file_id`; compat clients see only those.
//!
//! Every call names whose it is. [`PlaylistStore::owned_by`] reads and
//! writes one user's own playlists: each write checks the owner in its own
//! statement or transaction, so a write to another user's playlist changes
//! nothing and answers [`Written::Missing`]. [`PlaylistStore::visible_to`]
//! reads what one user may see: their own playlists and public ones, with
//! the line drawn in SQL.
//!
//! Positions are dense and zero based. `UNIQUE(playlist_id, position)`
//! is checked row by row during an UPDATE, so every reorder writes the new
//! order in two passes: first to negative slots, then to the final ones.

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::super::db::{CollectionsDb, StoreError};
use super::super::models::PlaylistTrack;
use super::super::sql;
use droppedneedle_kernel::db::DbError;

/// How many track covers a summary carries.
const SUMMARY_COVER_COUNT: i64 = 4;

/// One playlist with its list aggregates.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistRow {
    /// Playlist id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Owning user id. v2 rows from before ownership may have none.
    pub owner_id: Option<String>,
    /// Owner display name, when the owner still exists.
    pub owner_name: Option<String>,
    /// Public playlists are visible to every user.
    pub is_public: bool,
    /// Import provenance (`<source>:<id>`).
    pub source_ref: Option<String>,
    /// Creation time, epoch seconds.
    pub created_at: u64,
    /// Last mutation time, epoch seconds.
    pub updated_at: u64,
    /// True when an uploaded cover exists.
    pub has_cover: bool,
    /// Every entry.
    pub track_count: usize,
    /// Summed entry durations, seconds, when any entry has one.
    pub total_duration: Option<f64>,
    /// Entries pointing at a library file.
    pub streamable_count: usize,
    /// Summed durations of those entries, seconds.
    pub streamable_duration: f64,
    /// Up to four entry covers in position order.
    pub cover_urls: Vec<String>,
}

/// One row of the full list: a playlist the caller may read, or only the
/// existence of another user's private one.
#[derive(Debug, Clone, PartialEq)]
pub enum Listed {
    /// A playlist the caller may read.
    Readable(PlaylistRow),
    /// Another user's private playlist.
    Hidden {
        /// Playlist id.
        id: String,
        /// Every entry.
        track_count: usize,
        /// Owner display name, when the owner still exists.
        owner_name: Option<String>,
    },
}

/// One entry to insert.
#[derive(Debug, Clone, Default)]
pub struct NewEntry {
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Album name.
    pub album_name: String,
    /// Album id.
    pub album_id: Option<String>,
    /// Artist id.
    pub artist_id: Option<String>,
    /// Source-specific track id.
    pub track_source_id: Option<String>,
    /// Cover URL.
    pub cover_url: Option<String>,
    /// Preferred source type.
    pub source_type: String,
    /// Sources carrying the track.
    pub available_sources: Option<Vec<String>>,
    /// Audio format.
    pub format: Option<String>,
    /// Track number.
    pub track_number: Option<i32>,
    /// Disc number.
    pub disc_number: Option<i32>,
    /// Duration, seconds.
    pub duration: Option<f64>,
    /// Plex rating key.
    pub plex_rating_key: Option<String>,
    /// Local track id, when the entry is a library file.
    pub library_file_id: Option<String>,
}

/// Uploaded cover bytes.
#[derive(Debug, Clone)]
pub struct CoverImage {
    /// Mime type.
    pub content_type: String,
    /// Image bytes.
    pub bytes: Vec<u8>,
}

/// A playlist as one write left it, without its entries. Times are epoch
/// seconds.
#[derive(Debug, Clone)]
pub struct SavedPlaylist {
    /// Playlist id.
    pub id: String,
    /// Name as stored.
    pub name: String,
    /// Public flag.
    pub is_public: bool,
    /// When it was made.
    pub created_at: u64,
    /// When it last changed.
    pub updated_at: u64,
}

/// One track a membership check asks about, by exact names.
#[derive(Debug, Clone)]
pub struct EntryNames<'a> {
    /// Track title.
    pub track_name: &'a str,
    /// Artist name.
    pub artist_name: &'a str,
    /// Album name.
    pub album_name: &'a str,
}

/// Outcome of a write that names a playlist or entry by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written<T> {
    /// The write happened.
    Done(T),
    /// The playlist is gone or not the caller's, or the entry is gone.
    Missing,
}

/// The playlist store. Every read and write goes through
/// [`Self::owned_by`] or [`Self::visible_to`].
#[derive(Clone, Debug)]
pub struct PlaylistStore {
    db: CollectionsDb,
}

/// One user's own playlists.
#[derive(Clone, Copy, Debug)]
pub struct OwnedPlaylists<'a> {
    store: &'a PlaylistStore,
    user_id: &'a str,
}

/// What one user may read: their own playlists and public ones.
#[derive(Clone, Copy, Debug)]
pub struct VisiblePlaylists<'a> {
    store: &'a PlaylistStore,
    user_id: &'a str,
}

fn epoch(value: i64) -> u64 {
    value.max(0) as u64
}

fn count(value: i64) -> usize {
    value.max(0) as usize
}

fn playlist_from_row(row: &Row<'_>) -> rusqlite::Result<PlaylistRow> {
    Ok(PlaylistRow {
        id: row.get(0)?,
        name: row.get(1)?,
        owner_id: row.get(2)?,
        owner_name: row.get(3)?,
        is_public: row.get::<_, i64>(4)? != 0,
        source_ref: row.get(5)?,
        created_at: epoch(row.get(6)?),
        updated_at: epoch(row.get(7)?),
        has_cover: row.get::<_, i64>(8)? != 0,
        track_count: count(row.get(9)?),
        total_duration: row.get(10)?,
        streamable_count: count(row.get(11)?),
        streamable_duration: row.get(12)?,
        cover_urls: Vec::new(),
    })
}

fn track_from_row(row: &Row<'_>) -> rusqlite::Result<PlaylistTrack> {
    let sources: Option<String> = row.get(10)?;
    Ok(PlaylistTrack {
        id: row.get(0)?,
        position: count(row.get(1)?),
        track_name: row.get(2)?,
        artist_name: row.get(3)?,
        album_name: row.get(4)?,
        album_id: row.get(5)?,
        artist_id: row.get(6)?,
        track_source_id: row.get(7)?,
        cover_url: row.get(8)?,
        source_type: row.get(9)?,
        available_sources: sources.as_deref().and_then(decode_sources),
        format: row.get(11)?,
        track_number: row.get(12)?,
        disc_number: row.get(13)?,
        duration: row.get(14)?,
        created_at: epoch(row.get(15)?),
        plex_rating_key: row.get(16)?,
        library_file_id: row.get(17)?,
    })
}

fn cover_from_row(row: &Row<'_>) -> rusqlite::Result<CoverImage> {
    Ok(CoverImage {
        content_type: row.get(0)?,
        bytes: row.get(1)?,
    })
}

/// Stored sources are a JSON string list (v2 shape). A malformed value
/// reads as unknown rather than failing the playlist.
fn decode_sources(raw: &str) -> Option<Vec<String>> {
    match serde_json::from_str::<Vec<String>>(raw) {
        Ok(sources) => Some(sources),
        Err(error) => {
            tracing::warn!(%error, "playlist entry has unreadable available_sources");
            None
        }
    }
}

fn encode_sources(sources: Option<&Vec<String>>) -> Option<String> {
    sources.map(|list| serde_json::Value::from(list.clone()).to_string())
}

/// One playlist row and its first entry covers, or None.
fn one_playlist(
    conn: &Connection,
    statement: droppedneedle_kernel::db::statement::Statement,
    user_id: &str,
    playlist_id: &str,
) -> Result<Option<PlaylistRow>, DbError> {
    let Some(mut row) = conn
        .query_row(
            statement.text(),
            params![user_id, playlist_id],
            playlist_from_row,
        )
        .optional()?
    else {
        return Ok(None);
    };
    row.cover_urls = conn
        .prepare(sql::PLAYLIST_COVERS.text())?
        .query_map(params![playlist_id, SUMMARY_COVER_COUNT], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(row))
}

/// Fill each row's cover tiles from one read of the caller's readable
/// playlists' first entry covers.
fn fill_covers<'a>(
    conn: &Connection,
    user_id: &str,
    rows: impl Iterator<Item = &'a mut PlaylistRow>,
) -> Result<(), DbError> {
    let mut by_playlist: std::collections::HashMap<String, Vec<String>> =
        std::collections::HashMap::new();
    let mut stmt = conn.prepare(sql::LIST_COVERS.text())?;
    let covers = stmt.query_map(params![user_id, SUMMARY_COVER_COUNT], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for cover in covers {
        let (id, url) = cover?;
        by_playlist.entry(id).or_default().push(url);
    }
    for row in rows {
        if let Some(urls) = by_playlist.remove(&row.id) {
            row.cover_urls = urls;
        }
    }
    Ok(())
}

/// Whether `playlist_id` is `user_id`'s.
fn owns(tx: &Transaction, playlist_id: &str, user_id: &str) -> rusqlite::Result<bool> {
    Ok(tx
        .query_row(
            sql::OWNED_GUARD.text(),
            params![playlist_id, user_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Bump `updated_at` of the user's playlist; false when it is gone or not
/// theirs.
fn touch(tx: &Transaction, playlist_id: &str, user_id: &str, now: u64) -> rusqlite::Result<bool> {
    let changed = tx.execute(sql::TOUCH.text(), params![now as i64, playlist_id, user_id])?;
    Ok(changed > 0)
}

/// Insert one empty playlist row made at `now`, epoch seconds.
fn insert_playlist(
    tx: &Transaction,
    id: &str,
    name: &str,
    now: i64,
    source_ref: Option<&str>,
    owner_id: &str,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_PLAYLIST.text(),
        params![id, name, now, source_ref, owner_id],
    )?;
    Ok(())
}

/// Entry ids in position order.
fn entry_order(tx: &Transaction, playlist_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt = tx.prepare(sql::ENTRY_ORDER.text())?;
    let rows = stmt.query_map(params![playlist_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Write `order` as the dense positions 0..n, in two passes so the unique
/// position index never sees a collision.
fn write_order(tx: &Transaction, order: &[String]) -> rusqlite::Result<()> {
    let mut park = tx.prepare(sql::SET_POSITION.text())?;
    for (index, id) in order.iter().enumerate() {
        park.execute(params![-(index as i64) - 1, id])?;
    }
    for (index, id) in order.iter().enumerate() {
        park.execute(params![index as i64, id])?;
    }
    Ok(())
}

/// Insert `entries` at `position` (clamped; None appends) and renumber.
/// The caller has checked the playlist is the user's.
fn insert_entries(
    tx: &Transaction,
    playlist_id: &str,
    position: Option<usize>,
    entries: &[NewEntry],
    now: u64,
) -> rusqlite::Result<Vec<String>> {
    let mut order = entry_order(tx, playlist_id)?;
    let at = position.unwrap_or(order.len()).min(order.len());
    let tail = order.len() as i64;
    let mut stmt = tx.prepare(sql::INSERT_ENTRY.text())?;
    let mut added = Vec::with_capacity(entries.len());
    for (offset, entry) in entries.iter().enumerate() {
        let id = uuid::Uuid::new_v4().to_string();
        // Parked past the tail; `write_order` renumbers below.
        stmt.execute(params![
            id,
            playlist_id,
            tail + offset as i64,
            entry.track_name,
            entry.artist_name,
            entry.album_name,
            entry.album_id,
            entry.artist_id,
            entry.track_source_id,
            entry.cover_url,
            entry.source_type,
            encode_sources(entry.available_sources.as_ref()),
            entry.format,
            entry.track_number,
            entry.disc_number,
            entry.duration,
            now as i64,
            entry.plex_rating_key,
            entry.library_file_id,
        ])?;
        added.push(id);
    }
    drop(stmt);
    order.splice(at..at, added.iter().cloned());
    write_order(tx, &order)?;
    Ok(added)
}

impl PlaylistStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// One user's own playlists, to read and change.
    pub fn owned_by<'a>(&'a self, user_id: &'a str) -> OwnedPlaylists<'a> {
        OwnedPlaylists {
            store: self,
            user_id,
        }
    }

    /// What one user may read.
    pub fn visible_to<'a>(&'a self, user_id: &'a str) -> VisiblePlaylists<'a> {
        VisiblePlaylists {
            store: self,
            user_id,
        }
    }
}

impl VisiblePlaylists<'_> {
    /// Every playlist in name order: the readable ones with aggregates and
    /// cover tiles, other users' private ones as existence only.
    pub async fn list(&self) -> Result<Vec<Listed>, StoreError> {
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.list", move |conn| {
                let mut stmt = conn.prepare(sql::LIST_ALL.text())?;
                let mut listed = stmt
                    .query_map(params![user_id], |row| {
                        if row.get::<_, i64>(13)? != 0 {
                            Ok(Listed::Readable(playlist_from_row(row)?))
                        } else {
                            Ok(Listed::Hidden {
                                id: row.get(0)?,
                                track_count: count(row.get(9)?),
                                owner_name: row.get(3)?,
                            })
                        }
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                fill_covers(
                    conn,
                    &user_id,
                    listed.iter_mut().filter_map(|item| match item {
                        Listed::Readable(row) => Some(row),
                        Listed::Hidden { .. } => None,
                    }),
                )?;
                Ok(listed)
            })
            .await
    }

    /// The playlists the caller may read, name order, with aggregates and
    /// cover tiles.
    pub async fn readable(&self) -> Result<Vec<PlaylistRow>, StoreError> {
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.readable", move |conn| {
                let mut stmt = conn.prepare(sql::LIST_READABLE.text())?;
                let mut rows = stmt
                    .query_map(params![user_id], playlist_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                fill_covers(conn, &user_id, rows.iter_mut())?;
                Ok(rows)
            })
            .await
    }

    /// One playlist the caller may read, with aggregates, or None.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<PlaylistRow>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.get", move |conn| {
                one_playlist(conn, sql::GET_READABLE, &user_id, &playlist_id)
            })
            .await
    }

    /// Whether the caller owns a playlist they may read; None when they
    /// may not read it or it is gone.
    pub async fn is_owner(&self, playlist_id: &str) -> Result<Option<bool>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.is_owner", move |conn| {
                Ok(conn
                    .query_row(
                        sql::READABLE_OWNER.text(),
                        params![user_id, playlist_id],
                        |row| Ok(row.get::<_, i64>(0)? != 0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Entries of a playlist the caller may read, in position order; none
    /// when they may not read it.
    pub async fn tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.tracks", move |conn| {
                Ok(conn
                    .prepare(sql::TRACKS_READABLE.text())?
                    .query_map(params![user_id, playlist_id], track_from_row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Uploaded cover bytes of a playlist the caller may read, or None.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<CoverImage>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.cover", move |conn| {
                Ok(conn
                    .query_row(
                        sql::COVER_READABLE.text(),
                        params![user_id, playlist_id],
                        cover_from_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// For each asked track, by its index, the ids of the playlists the
    /// caller may read holding an entry with exactly its names, in id
    /// order. One statement for the whole list.
    pub async fn holding(
        &self,
        tracks: &[EntryNames<'_>],
    ) -> Result<Vec<(usize, String)>, StoreError> {
        if tracks.is_empty() {
            return Ok(Vec::new());
        }
        let asked = serde_json::Value::from(
            tracks
                .iter()
                .map(|track| {
                    serde_json::json!([track.track_name, track.artist_name, track.album_name])
                })
                .collect::<Vec<_>>(),
        )
        .to_string();
        let user_id = self.user_id.to_owned();
        self.store
            .db
            .read("playlists.holding", move |conn| {
                Ok(conn
                    .prepare(sql::MEMBERSHIP.text())?
                    .query_map(params![user_id, asked], |row| {
                        Ok((count(row.get(0)?), row.get::<_, String>(1)?))
                    })?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }
}

impl OwnedPlaylists<'_> {
    /// One of the user's playlists with aggregates, or None.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<PlaylistRow>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.get_owned", move |conn| {
                one_playlist(conn, sql::GET_OWNED, &user_id, &playlist_id)
            })
            .await
    }

    /// Entries of one of the user's playlists, in position order; none
    /// when it is not theirs.
    pub async fn tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.tracks_owned", move |conn| {
                Ok(conn
                    .prepare(sql::TRACKS_OWNED.text())?
                    .query_map(params![user_id, playlist_id], track_from_row)?
                    .collect::<rusqlite::Result<_>>()?)
            })
            .await
    }

    /// Uploaded cover bytes of one of the user's playlists, or None.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<CoverImage>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .read("playlists.cover_owned", move |conn| {
                Ok(conn
                    .query_row(
                        sql::COVER_OWNED.text(),
                        params![user_id, playlist_id],
                        cover_from_row,
                    )
                    .optional()?)
            })
            .await
    }

    /// The playlist the user imported from one source, or None.
    pub async fn find_by_source(&self, source_ref: &str) -> Result<Option<String>, StoreError> {
        let (user_id, source_ref) = (self.user_id.to_owned(), source_ref.to_owned());
        self.store
            .db
            .read("playlists.find_by_source", move |conn| {
                Ok(conn
                    .query_row(
                        sql::FIND_BY_SOURCE.text(),
                        params![user_id, source_ref],
                        |row| row.get(0),
                    )
                    .optional()?)
            })
            .await
    }

    /// Create a playlist; returns its id. A duplicate import provenance for
    /// the same owner reports `None`.
    pub async fn create(
        &self,
        name: &str,
        source_ref: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name, source_ref) = (
            self.user_id.to_owned(),
            name.to_owned(),
            source_ref.map(str::to_owned),
        );
        let now = self.store.db.now_epoch() as i64;
        self.store
            .db
            .write("playlists.create", move |tx| {
                if let Some(source_ref) = &source_ref {
                    let taken: Option<String> = tx
                        .query_row(
                            sql::FIND_BY_SOURCE.text(),
                            params![owner_id, source_ref],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if taken.is_some() {
                        return Ok(None);
                    }
                }
                insert_playlist(tx, &id, &name, now, source_ref.as_deref(), &owner_id)?;
                Ok(Some(id))
            })
            .await
    }

    /// Create a playlist holding `entries`, in order, in one transaction:
    /// a write that fails leaves nothing behind. Returns its id and when it
    /// was made, epoch seconds.
    pub async fn create_with(
        &self,
        name: &str,
        entries: Vec<NewEntry>,
    ) -> Result<(String, u64), StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name) = (self.user_id.to_owned(), name.to_owned());
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.create_with", move |tx| {
                insert_playlist(tx, &id, &name, now as i64, None, &owner_id)?;
                insert_entries(tx, &id, None, &entries, now)?;
                Ok((id, now))
            })
            .await
    }

    /// Rename a playlist.
    pub async fn rename(&self, playlist_id: &str, name: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id, name) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            name.to_owned(),
        );
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.rename", move |tx| {
                let changed =
                    tx.execute(sql::RENAME.text(), params![name, playlist_id, user_id])?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Flip visibility.
    pub async fn set_public(
        &self,
        playlist_id: &str,
        public: bool,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.set_public", move |tx| {
                let changed = tx.execute(
                    sql::SET_PUBLIC.text(),
                    params![i64::from(public), playlist_id, user_id],
                )?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete a playlist with its entries and cover.
    pub async fn delete(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        self.store
            .db
            .write("playlists.delete", move |tx| {
                let changed =
                    tx.execute(sql::DELETE_PLAYLIST.text(), params![playlist_id, user_id])?;
                Ok(if changed == 0 {
                    Written::Missing
                } else {
                    Written::Done(())
                })
            })
            .await
    }

    /// Insert entries at `position` (clamped; None appends). Returns the
    /// new entry ids.
    pub async fn insert(
        &self,
        playlist_id: &str,
        position: Option<usize>,
        entries: Vec<NewEntry>,
    ) -> Result<Written<Vec<String>>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.insert", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                let added = insert_entries(tx, &playlist_id, position, &entries, now)?;
                Ok(Written::Done(added))
            })
            .await
    }

    /// Replace every entry with `entries`, in order, in one transaction: a
    /// write that fails leaves the old entries in place.
    pub async fn replace_entries(
        &self,
        playlist_id: &str,
        entries: Vec<NewEntry>,
    ) -> Result<Written<()>, StoreError> {
        Ok(
            match self.replace_named(playlist_id, None, entries).await? {
                Written::Done(_) => Written::Done(()),
                Written::Missing => Written::Missing,
            },
        )
    }

    /// Replace every entry with `entries` and, when `name` is given, rename
    /// the playlist, in one transaction. Returns the playlist as the write
    /// left it.
    pub async fn replace_named(
        &self,
        playlist_id: &str,
        name: Option<&str>,
        entries: Vec<NewEntry>,
    ) -> Result<Written<SavedPlaylist>, StoreError> {
        let (user_id, playlist_id, name) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            name.map(str::to_owned),
        );
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.replace_entries", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                if let Some(name) = &name {
                    tx.execute(sql::RENAME.text(), params![name, playlist_id, user_id])?;
                }
                tx.execute(sql::CLEAR_ENTRIES.text(), params![playlist_id])?;
                insert_entries(tx, &playlist_id, None, &entries, now)?;
                let (name, is_public, created_at) =
                    tx.query_row(sql::SAVED.text(), params![playlist_id], |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)? != 0,
                            epoch(row.get(2)?),
                        ))
                    })?;
                Ok(Written::Done(SavedPlaylist {
                    id: playlist_id,
                    name,
                    is_public,
                    created_at,
                    updated_at: now,
                }))
            })
            .await
    }

    /// Remove entries by id, skipping unknown ids. Returns how many went.
    pub async fn remove(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<Written<usize>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let entry_ids = entry_ids.to_vec();
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.remove", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                let mut removed = 0;
                {
                    let mut stmt = tx.prepare(sql::REMOVE_ENTRY.text())?;
                    for id in &entry_ids {
                        removed += stmt.execute(params![playlist_id, id])?;
                    }
                }
                let order = entry_order(tx, &playlist_id)?;
                write_order(tx, &order)?;
                Ok(Written::Done(removed))
            })
            .await
    }

    /// Move one entry to `position`, clamped. Returns where it landed, or
    /// `Missing` when the playlist or entry is gone.
    pub async fn move_entry(
        &self,
        playlist_id: &str,
        entry_id: &str,
        position: usize,
    ) -> Result<Written<usize>, StoreError> {
        let (user_id, playlist_id, entry_id) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            entry_id.to_owned(),
        );
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.move", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let mut order = entry_order(tx, &playlist_id)?;
                let Some(from) = order.iter().position(|id| *id == entry_id) else {
                    return Ok(Written::Missing);
                };
                let id = order.remove(from);
                let at = position.min(order.len());
                order.insert(at, id);
                write_order(tx, &order)?;
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(at))
            })
            .await
    }

    /// Replace one entry's source fields.
    pub async fn update_sources(
        &self,
        playlist_id: &str,
        entry_id: &str,
        source_type: Option<String>,
        available_sources: Option<Vec<String>>,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id, entry_id) = (
            self.user_id.to_owned(),
            playlist_id.to_owned(),
            entry_id.to_owned(),
        );
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.update_sources", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let exists = tx
                    .query_row(
                        sql::ENTRY_EXISTS.text(),
                        params![playlist_id, entry_id],
                        |_| Ok(()),
                    )
                    .optional()?;
                if exists.is_none() {
                    return Ok(Written::Missing);
                }
                if let Some(source_type) = &source_type {
                    tx.execute(
                        sql::SET_SOURCE_TYPE.text(),
                        params![source_type, entry_id, playlist_id],
                    )?;
                }
                if let Some(sources) = &available_sources {
                    tx.execute(
                        sql::SET_AVAILABLE_SOURCES.text(),
                        params![encode_sources(Some(sources)), entry_id, playlist_id],
                    )?;
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Store or replace the uploaded cover.
    pub async fn set_cover(
        &self,
        playlist_id: &str,
        cover: CoverImage,
    ) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.set_cover", move |tx| {
                if !touch(tx, &playlist_id, &user_id, now)? {
                    return Ok(Written::Missing);
                }
                tx.execute(
                    sql::UPSERT_COVER.text(),
                    params![playlist_id, cover.content_type, cover.bytes, now as f64],
                )?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete the uploaded cover. `Missing` when there was none.
    pub async fn remove_cover(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let (user_id, playlist_id) = (self.user_id.to_owned(), playlist_id.to_owned());
        let now = self.store.db.now_epoch();
        self.store
            .db
            .write("playlists.remove_cover", move |tx| {
                if !owns(tx, &playlist_id, &user_id)? {
                    return Ok(Written::Missing);
                }
                let changed = tx.execute(sql::DELETE_COVER.text(), params![playlist_id])?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, &user_id, now)?;
                Ok(Written::Done(()))
            })
            .await
    }
}
