//! Playlist rows over `playlists`, `playlist_tracks` and `playlist_covers`.
//!
//! One playlist model serves the native routes, both compat protocols and
//! remote playlist imports. Entries that point at a library file carry its
//! local track id in `library_file_id`; compat clients see only those.
//!
//! Positions are dense and zero based. `UNIQUE(playlist_id, position)`
//! is checked row by row during an UPDATE, so every reorder writes the new
//! order in two passes: first to negative slots, then to the final ones.

use rusqlite::{OptionalExtension, Transaction, params};
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, now_epoch, placeholders};
use super::super::models::PlaylistTrack;

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

/// Outcome of a write that names a playlist or entry by id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Written<T> {
    /// The write happened.
    Done(T),
    /// The playlist or entry is gone.
    Missing,
}

/// The playlist store.
#[derive(Clone, Debug)]
pub struct PlaylistStore {
    db: CollectionsDb,
}

const PLAYLIST_SELECT: &str = "SELECT p.id AS id, p.name AS name, p.user_id AS user_id, \
    COALESCE(u.display_name, u.username) AS owner_name, p.is_public AS is_public, \
    p.source_ref AS source_ref, \
    COALESCE(CAST(strftime('%s', p.created_at) AS INTEGER), 0) AS created_at, \
    COALESCE(CAST(strftime('%s', p.updated_at) AS INTEGER), 0) AS updated_at, \
    (c.playlist_id IS NOT NULL) AS has_cover, \
    (SELECT COUNT(*) FROM playlist_tracks t WHERE t.playlist_id = p.id) AS track_count, \
    (SELECT SUM(CAST(t.duration AS REAL)) FROM playlist_tracks t \
     WHERE t.playlist_id = p.id) AS total_duration, \
    (SELECT COUNT(*) FROM playlist_tracks t \
     WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL) AS streamable_count, \
    (SELECT COALESCE(SUM(CAST(t.duration AS REAL)), 0.0) FROM playlist_tracks t \
     WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL) AS streamable_duration \
    FROM playlists p \
    LEFT JOIN auth_users u ON u.id = p.user_id \
    LEFT JOIN playlist_covers c ON c.playlist_id = p.id";

const TRACK_SELECT: &str = "SELECT id, position, track_name, artist_name, album_name, \
    album_id, artist_id, track_source_id, cover_url, source_type, available_sources, format, \
    track_number, disc_number, CAST(duration AS REAL) AS duration, \
    COALESCE(CAST(strftime('%s', created_at) AS INTEGER), 0) AS created_at, \
    plex_rating_key, library_file_id FROM playlist_tracks";

fn playlist_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<PlaylistRow, sqlx::Error> {
    Ok(PlaylistRow {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        owner_id: row.try_get("user_id")?,
        owner_name: row.try_get("owner_name")?,
        is_public: row.try_get::<i64, _>("is_public")? != 0,
        source_ref: row.try_get("source_ref")?,
        created_at: row.try_get::<i64, _>("created_at")?.max(0) as u64,
        updated_at: row.try_get::<i64, _>("updated_at")?.max(0) as u64,
        has_cover: row.try_get::<i64, _>("has_cover")? != 0,
        track_count: row.try_get::<i64, _>("track_count")?.max(0) as usize,
        total_duration: row.try_get("total_duration")?,
        streamable_count: row.try_get::<i64, _>("streamable_count")?.max(0) as usize,
        streamable_duration: row.try_get("streamable_duration")?,
        cover_urls: Vec::new(),
    })
}

fn track_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<PlaylistTrack, sqlx::Error> {
    let sources: Option<String> = row.try_get("available_sources")?;
    Ok(PlaylistTrack {
        id: row.try_get("id")?,
        position: row.try_get::<i64, _>("position")?.max(0) as usize,
        track_name: row.try_get("track_name")?,
        artist_name: row.try_get("artist_name")?,
        album_name: row.try_get("album_name")?,
        album_id: row.try_get("album_id")?,
        artist_id: row.try_get("artist_id")?,
        track_source_id: row.try_get("track_source_id")?,
        cover_url: row.try_get("cover_url")?,
        source_type: row.try_get("source_type")?,
        available_sources: sources.as_deref().and_then(decode_sources),
        format: row.try_get("format")?,
        track_number: row.try_get("track_number")?,
        disc_number: row.try_get("disc_number")?,
        duration: row.try_get("duration")?,
        created_at: row.try_get::<i64, _>("created_at")?.max(0) as u64,
        plex_rating_key: row.try_get("plex_rating_key")?,
        library_file_id: row.try_get("library_file_id")?,
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

/// Bump `updated_at`; false when the playlist is gone.
fn touch(tx: &Transaction, playlist_id: &str, now: u64) -> rusqlite::Result<bool> {
    let changed = tx.execute(
        "UPDATE playlists SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch') \
         WHERE id = ?2",
        params![now as i64, playlist_id],
    )?;
    Ok(changed > 0)
}

/// Entry ids in position order.
fn entry_order(tx: &Transaction, playlist_id: &str) -> rusqlite::Result<Vec<String>> {
    let mut stmt =
        tx.prepare("SELECT id FROM playlist_tracks WHERE playlist_id = ?1 ORDER BY position")?;
    let rows = stmt.query_map(params![playlist_id], |row| row.get::<_, String>(0))?;
    rows.collect()
}

/// Write `order` as the dense positions 0..n, in two passes so the unique
/// position index never sees a collision.
fn write_order(tx: &Transaction, order: &[String]) -> rusqlite::Result<()> {
    let mut park = tx.prepare("UPDATE playlist_tracks SET position = ?1 WHERE id = ?2")?;
    for (index, id) in order.iter().enumerate() {
        park.execute(params![-(index as i64) - 1, id])?;
    }
    for (index, id) in order.iter().enumerate() {
        park.execute(params![index as i64, id])?;
    }
    Ok(())
}

/// Insert `entries` at `position` (clamped; None appends) and renumber.
/// The caller has checked the playlist exists.
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
    let mut stmt = tx.prepare(
        "INSERT INTO playlist_tracks (id, playlist_id, position, track_name, \
         artist_name, album_name, album_id, artist_id, track_source_id, cover_url, \
         source_type, available_sources, format, track_number, disc_number, \
         duration, created_at, plex_rating_key, library_file_id) VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
         strftime('%Y-%m-%dT%H:%M:%SZ', ?17, 'unixepoch'), ?18, ?19)",
    )?;
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

    /// Every playlist, name order, with aggregates and cover tiles.
    pub async fn list(&self) -> Result<Vec<PlaylistRow>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!(
            "{PLAYLIST_SELECT} ORDER BY p.name COLLATE NOCASE, p.id"
        ))
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("playlists.list", error))?;
        let mut playlists = rows
            .iter()
            .map(playlist_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("playlists.list", error))?;
        let covers = sqlx::query(
            "SELECT playlist_id, cover_url FROM (SELECT playlist_id, cover_url, \
             ROW_NUMBER() OVER (PARTITION BY playlist_id ORDER BY position) AS rank \
             FROM playlist_tracks WHERE cover_url IS NOT NULL AND cover_url != '') \
             WHERE rank <= ? ORDER BY playlist_id, rank",
        )
        .bind(SUMMARY_COVER_COUNT)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("playlists.list_covers", error))?;
        let mut by_playlist: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for row in &covers {
            let id: String = row
                .try_get("playlist_id")
                .map_err(|error| StoreError::read("playlists.list_covers", error))?;
            let url: String = row
                .try_get("cover_url")
                .map_err(|error| StoreError::read("playlists.list_covers", error))?;
            by_playlist.entry(id).or_default().push(url);
        }
        for playlist in &mut playlists {
            if let Some(urls) = by_playlist.remove(&playlist.id) {
                playlist.cover_urls = urls;
            }
        }
        Ok(playlists)
    }

    /// One playlist with aggregates, or None.
    pub async fn get(&self, playlist_id: &str) -> Result<Option<PlaylistRow>, StoreError> {
        let pool = self.db.pool()?;
        let row = sqlx::query(&format!("{PLAYLIST_SELECT} WHERE p.id = ?"))
            .bind(playlist_id)
            .fetch_optional(pool)
            .await
            .map_err(|error| StoreError::read("playlists.get", error))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let mut playlist =
            playlist_from_row(&row).map_err(|error| StoreError::read("playlists.get", error))?;
        playlist.cover_urls = self
            .tracks(playlist_id)
            .await?
            .into_iter()
            .filter_map(|track| track.cover_url.filter(|url| !url.is_empty()))
            .take(SUMMARY_COVER_COUNT as usize)
            .collect();
        Ok(Some(playlist))
    }

    /// The playlist a user imported from one source, or None.
    pub async fn find_by_source(
        &self,
        owner_id: &str,
        source_ref: &str,
    ) -> Result<Option<String>, StoreError> {
        let pool = self.db.pool()?;
        sqlx::query_scalar("SELECT id FROM playlists WHERE user_id = ? AND source_ref = ?")
            .bind(owner_id)
            .bind(source_ref)
            .fetch_optional(pool)
            .await
            .map_err(|error| StoreError::read("playlists.find_by_source", error))
    }

    /// Entries in position order.
    pub async fn tracks(&self, playlist_id: &str) -> Result<Vec<PlaylistTrack>, StoreError> {
        let pool = self.db.pool()?;
        let rows = sqlx::query(&format!(
            "{TRACK_SELECT} WHERE playlist_id = ? ORDER BY position"
        ))
        .bind(playlist_id)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("playlists.tracks", error))?;
        rows.iter()
            .map(track_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| StoreError::read("playlists.tracks", error))
    }

    /// Ids of the playlists in `visible` holding an entry with these exact
    /// names.
    pub async fn holding(
        &self,
        visible: &[String],
        track_name: &str,
        artist_name: &str,
        album_name: &str,
    ) -> Result<Vec<String>, StoreError> {
        if visible.is_empty() {
            return Ok(Vec::new());
        }
        let pool = self.db.pool()?;
        let sql = format!(
            "SELECT DISTINCT playlist_id FROM playlist_tracks WHERE track_name = ? \
             AND artist_name = ? AND album_name = ? AND playlist_id IN ({}) \
             ORDER BY playlist_id",
            placeholders(visible.len())
        );
        let mut query = sqlx::query_scalar::<_, String>(&sql)
            .bind(track_name)
            .bind(artist_name)
            .bind(album_name);
        for id in visible {
            query = query.bind(id);
        }
        query
            .fetch_all(pool)
            .await
            .map_err(|error| StoreError::read("playlists.holding", error))
    }

    /// Uploaded cover bytes, or None.
    pub async fn cover(&self, playlist_id: &str) -> Result<Option<CoverImage>, StoreError> {
        let pool = self.db.pool()?;
        let row =
            sqlx::query("SELECT content_type, image FROM playlist_covers WHERE playlist_id = ?")
                .bind(playlist_id)
                .fetch_optional(pool)
                .await
                .map_err(|error| StoreError::read("playlists.cover", error))?;
        row.map(|row| {
            Ok(CoverImage {
                content_type: row.try_get("content_type")?,
                bytes: row.try_get("image")?,
            })
        })
        .transpose()
        .map_err(|error| StoreError::read("playlists.cover", error))
    }

    /// Create a playlist; returns its id. A duplicate import provenance for
    /// the same owner reports `None`.
    pub async fn create(
        &self,
        owner_id: &str,
        name: &str,
        source_ref: Option<&str>,
    ) -> Result<Option<String>, StoreError> {
        let id = uuid::Uuid::new_v4().to_string();
        let (owner_id, name, source_ref) = (
            owner_id.to_owned(),
            name.to_owned(),
            source_ref.map(str::to_owned),
        );
        let new_id = id.clone();
        self.db
            .write("playlists.create", move |tx| {
                if let Some(source_ref) = &source_ref {
                    let taken: Option<String> = tx
                        .query_row(
                            "SELECT id FROM playlists WHERE user_id = ?1 AND source_ref = ?2",
                            params![owner_id, source_ref],
                            |row| row.get(0),
                        )
                        .optional()?;
                    if taken.is_some() {
                        return Ok(None);
                    }
                }
                let now = now_epoch() as i64;
                tx.execute(
                    "INSERT INTO playlists (id, name, cover_image_path, created_at, updated_at, \
                     source_ref, user_id, is_public) VALUES (?1, ?2, NULL, \
                     strftime('%Y-%m-%dT%H:%M:%SZ', ?3, 'unixepoch'), \
                     strftime('%Y-%m-%dT%H:%M:%SZ', ?3, 'unixepoch'), ?4, ?5, 0)",
                    params![new_id, name, now, source_ref, owner_id],
                )?;
                Ok(Some(new_id))
            })
            .await
    }

    /// Rename a playlist.
    pub async fn rename(&self, playlist_id: &str, name: &str) -> Result<Written<()>, StoreError> {
        let (playlist_id, name) = (playlist_id.to_owned(), name.to_owned());
        self.db
            .write("playlists.rename", move |tx| {
                let changed = tx.execute(
                    "UPDATE playlists SET name = ?1 WHERE id = ?2",
                    params![name, playlist_id],
                )?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, now_epoch())?;
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
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.set_public", move |tx| {
                let changed = tx.execute(
                    "UPDATE playlists SET is_public = ?1 WHERE id = ?2",
                    params![i64::from(public), playlist_id],
                )?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, now_epoch())?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete a playlist with its entries and cover.
    pub async fn delete(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.delete", move |tx| {
                let changed =
                    tx.execute("DELETE FROM playlists WHERE id = ?1", params![playlist_id])?;
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
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.insert", move |tx| {
                let now = now_epoch();
                if !touch(tx, &playlist_id, now)? {
                    return Ok(Written::Missing);
                }
                let added = insert_entries(tx, &playlist_id, position, &entries, now)?;
                Ok(Written::Done(added))
            })
            .await
    }

    /// Remove entries by id, skipping unknown ids. Returns how many went.
    pub async fn remove(
        &self,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<Written<usize>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        let entry_ids = entry_ids.to_vec();
        self.db
            .write("playlists.remove", move |tx| {
                let now = now_epoch();
                if !touch(tx, &playlist_id, now)? {
                    return Ok(Written::Missing);
                }
                let mut removed = 0;
                {
                    let mut stmt = tx.prepare(
                        "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND id = ?2",
                    )?;
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
        let (playlist_id, entry_id) = (playlist_id.to_owned(), entry_id.to_owned());
        self.db
            .write("playlists.move", move |tx| {
                let mut order = entry_order(tx, &playlist_id)?;
                let Some(from) = order.iter().position(|id| *id == entry_id) else {
                    return Ok(Written::Missing);
                };
                let id = order.remove(from);
                let at = position.min(order.len());
                order.insert(at, id);
                write_order(tx, &order)?;
                touch(tx, &playlist_id, now_epoch())?;
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
        let (playlist_id, entry_id) = (playlist_id.to_owned(), entry_id.to_owned());
        self.db
            .write("playlists.update_sources", move |tx| {
                let exists: Option<i64> = tx
                    .query_row(
                        "SELECT 1 FROM playlist_tracks WHERE playlist_id = ?1 AND id = ?2",
                        params![playlist_id, entry_id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if exists.is_none() {
                    return Ok(Written::Missing);
                }
                if let Some(source_type) = &source_type {
                    tx.execute(
                        "UPDATE playlist_tracks SET source_type = ?1 WHERE id = ?2",
                        params![source_type, entry_id],
                    )?;
                }
                if let Some(sources) = &available_sources {
                    tx.execute(
                        "UPDATE playlist_tracks SET available_sources = ?1 WHERE id = ?2",
                        params![encode_sources(Some(sources)), entry_id],
                    )?;
                }
                touch(tx, &playlist_id, now_epoch())?;
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
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.set_cover", move |tx| {
                let now = now_epoch();
                if !touch(tx, &playlist_id, now)? {
                    return Ok(Written::Missing);
                }
                tx.execute(
                    "INSERT INTO playlist_covers (playlist_id, content_type, image, updated_at) \
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT (playlist_id) DO UPDATE SET \
                     content_type = excluded.content_type, image = excluded.image, \
                     updated_at = excluded.updated_at",
                    params![playlist_id, cover.content_type, cover.bytes, now as f64],
                )?;
                Ok(Written::Done(()))
            })
            .await
    }

    /// Delete the uploaded cover. `Missing` when there was none.
    pub async fn remove_cover(&self, playlist_id: &str) -> Result<Written<()>, StoreError> {
        let playlist_id = playlist_id.to_owned();
        self.db
            .write("playlists.remove_cover", move |tx| {
                let changed = tx.execute(
                    "DELETE FROM playlist_covers WHERE playlist_id = ?1",
                    params![playlist_id],
                )?;
                if changed == 0 {
                    return Ok(Written::Missing);
                }
                touch(tx, &playlist_id, now_epoch())?;
                Ok(Written::Done(()))
            })
            .await
    }
}
