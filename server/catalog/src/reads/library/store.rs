//! [`LocalCatalog`]: the one store over the local catalog, the `local_*`
//! tables with their artwork and identity rows, the caller's favorites and
//! open requests. Reads only, through the database's request pool, one
//! pooled read per statement.
//!
//! Its methods sit beside what they serve: the library pages and favorites
//! in `sqlite.rs`, the identity lookups in `lookups.rs`, the catalog pages'
//! library reads in `reads::catalog::library`. Every statement is in
//! `sql.rs`, and each entity is read through its one projection and the row
//! mapper here.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use droppedneedle_kernel::db::{Db, DbError};
use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Row};

use super::sql;
use super::stores::{AlbumRecord, ArtistRecord, StoreError, TrackRecord};

/// The local catalog. `Clone` is cheap; clones share the database and the
/// remembered totals.
#[derive(Clone, Debug)]
pub struct LocalCatalog {
    db: Db,
    totals: Arc<Mutex<Totals>>,
}

/// List totals counted at one catalog revision, by list and filter. Every
/// catalog write moves the revision, which drops them all.
#[derive(Debug, Default)]
struct Totals {
    revision: i64,
    counted: HashMap<String, Vec<u64>>,
}

/// Most totals remembered at one revision; past this they are dropped.
const TOTALS_CAP: usize = 256;

impl LocalCatalog {
    /// Reads over `db`.
    pub fn new(db: Db) -> Self {
        Self {
            db,
            totals: Arc::default(),
        }
    }

    /// The totals `key` names at the catalog revision now: remembered from
    /// an earlier page, else counted by `count` in a read named `name`.
    pub(crate) async fn totals<F>(
        &self,
        key: String,
        name: &'static str,
        count: F,
    ) -> Result<Vec<u64>, StoreError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<Vec<u64>> + Send + 'static,
    {
        let revision: i64 = self
            .read("library.catalog.revision", |conn| {
                scalar(conn, sql::CATALOG_REVISION.text(), &[])
            })
            .await?;
        {
            let memo = self
                .totals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if memo.revision == revision
                && let Some(counted) = memo.counted.get(&key)
            {
                return Ok(counted.clone());
            }
        }
        let counted = self.read(name, count).await?;
        let mut memo = self
            .totals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if memo.revision != revision || memo.counted.len() >= TOTALS_CAP {
            memo.revision = revision;
            memo.counted.clear();
        }
        memo.counted.insert(key, counted.clone());
        Ok(counted)
    }

    /// One pooled read named `name`. A busy database answers
    /// [`StoreError::Busy`], any other failure [`StoreError::Internal`].
    pub(crate) async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Connection) -> rusqlite::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        self.db
            .read(name, move |conn| op(conn).map_err(DbError::from))
            .await
            .map_err(|error| StoreError::db(name, error))
    }
}

/// Every row of `text` over `values`, mapped.
pub(crate) fn rows<T>(
    conn: &Connection,
    text: &str,
    values: &[Value],
    map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut statement = conn.prepare_cached(text)?;
    statement
        .query_map(rusqlite::params_from_iter(values), map)?
        .collect()
}

/// The first row of `text` over `values`, mapped, or `None`.
pub(crate) fn first<T>(
    conn: &Connection,
    text: &str,
    values: &[Value],
    map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Option<T>> {
    let mut statement = conn.prepare_cached(text)?;
    statement
        .query_row(rusqlite::params_from_iter(values), map)
        .optional()
}

/// The first column of the one row `text` answers over `values`.
pub(crate) fn scalar<T: rusqlite::types::FromSql>(
    conn: &Connection,
    text: &str,
    values: &[Value],
) -> rusqlite::Result<T> {
    let mut statement = conn.prepare_cached(text)?;
    statement.query_row(rusqlite::params_from_iter(values), |row| row.get(0))
}

/// A text value, NULL when absent.
pub(crate) fn text(value: Option<&str>) -> Value {
    value.map_or(Value::Null, |value| Value::Text(value.to_owned()))
}

/// An integer value, NULL when absent.
pub(crate) fn int(value: Option<i64>) -> Value {
    value.map_or(Value::Null, Value::Integer)
}

/// A count or page bound as an integer.
pub(crate) fn count(value: u64) -> Value {
    Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
}

/// Text values for a list of ids.
pub(crate) fn texts<'a>(ids: impl IntoIterator<Item = &'a String>) -> Vec<Value> {
    ids.into_iter().map(|id| Value::Text(id.clone())).collect()
}

/// A count column as `u64`, negatives as zero.
pub(crate) fn unsigned(value: i64) -> u64 {
    value.max(0) as u64
}

/// The album projection's row.
pub(crate) fn album_row(row: &Row<'_>) -> rusqlite::Result<AlbumRecord> {
    Ok(AlbumRecord {
        id: row.get("id")?,
        title: row.get("title")?,
        artist_name: row.get("artist_name")?,
        artist_id: row.get("artist_id")?,
        release_group_mbid: row.get("release_group_mbid")?,
        release_mbid: row.get("release_mbid")?,
        artist_mbid: row.get("artist_mbid")?,
        linked: row.get("linked")?,
        track_count: unsigned(row.get("track_count")?),
        total_duration_seconds: row.get("total_duration_seconds")?,
        total_size_bytes: row.get("total_size_bytes")?,
        format: row.get("format")?,
        year: row.get("year")?,
        is_compilation: row.get("is_compilation")?,
        cover_available: row.get("cover_available")?,
        art_version: row.get("art_version")?,
        date_added: row.get("date_added")?,
        contribution_id: row.get("contribution_id")?,
        contribution_state: row.get("contribution_state")?,
    })
}

/// The track projection's row.
pub(crate) fn track_row(row: &Row<'_>) -> rusqlite::Result<TrackRecord> {
    Ok(TrackRecord {
        id: row.get("id")?,
        title: row.get("title")?,
        album_id: row.get("album_id")?,
        album_title: row.get("album_title")?,
        artist_name: row.get("artist_name")?,
        artist_id: row.get("artist_id")?,
        album_artist_name: row.get("album_artist_name")?,
        album_artist_id: row.get("album_artist_id")?,
        recording_mbid: row.get("recording_mbid")?,
        release_group_mbid: row.get("release_group_mbid")?,
        artist_mbid: row.get("artist_mbid")?,
        album_artist_mbid: row.get("album_artist_mbid")?,
        disc_number: row.get("disc_number")?,
        track_number: row.get("track_number")?,
        year: row.get("year")?,
        genre: row.get("genre")?,
        duration_seconds: row.get("duration_seconds")?,
        format: row.get("format")?,
        bit_rate: row.get("bit_rate")?,
        sample_rate: row.get("sample_rate")?,
        bit_depth: row.get("bit_depth")?,
        channels: row.get("channels")?,
        file_size_bytes: row.get("file_size_bytes")?,
        date_added: row.get("date_added")?,
        cover_available: row.get("cover_available")?,
    })
}

/// An artist's indexed credited tracks, appearance albums and appearance
/// tracks.
pub(crate) type Credits = (u64, u64, u64);

/// The artist projection's row. Its counts come from the grouped passes
/// and start at zero here.
pub(crate) fn artist_row(row: &Row<'_>) -> rusqlite::Result<ArtistRecord> {
    Ok(ArtistRecord {
        id: row.get("id")?,
        name: row.get("name")?,
        artist_mbid: row.get("artist_mbid")?,
        linked: row.get("linked")?,
        album_count: 0,
        track_count: 0,
        appearance_album_count: 0,
        appearance_track_count: 0,
        date_added: row.get("date_added")?,
    })
}
