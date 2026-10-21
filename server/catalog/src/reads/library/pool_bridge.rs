//! What the player catalog, which still reads through the sqlx pool,
//! shares with the local catalog: the pool handle, the artist projection
//! and the current-id lookup, run over the store's registered statements.
//! The player catalog and collections' unwired database are its only
//! users; it goes when the player reads move to the request pool.

use std::collections::HashMap;

use droppedneedle_kernel::db::statement::ListLen;
use sqlx::{Row as _, SqlitePool};

use super::sql;
use super::stores::{ArtistRecord, StoreError};

/// The reader pool for the player catalog. Its unwired form and
/// [`LibraryDb::live`] remain only for collections' unwired database.
#[derive(Clone, Debug, Default)]
pub struct LibraryDb {
    inner: Option<SqlitePool>,
}

impl LibraryDb {
    /// Live handle over the runtime's reader pool.
    pub fn new(pool: &SqlitePool) -> Self {
        Self {
            inner: Some(pool.clone()),
        }
    }

    /// Skeleton-boot handle: every operation fails closed. Only pre-boot
    /// state builds this; production uses [`LibraryDb::new`].
    pub fn unwired() -> Self {
        Self::default()
    }

    pub(crate) fn live(&self) -> Option<&SqlitePool> {
        self.inner.as_ref()
    }

    /// The handle over `db`'s pool, for the read benchmarks.
    #[cfg(test)]
    pub(crate) fn of(db: &droppedneedle_kernel::db::Db) -> Self {
        Self::new(db.sqlx())
    }
}

impl StoreError {
    /// A failed sqlx read named `operation`: busy stays busy, anything else
    /// is internal.
    pub(crate) fn read(operation: &str, error: sqlx::Error) -> Self {
        Self::db(
            operation,
            droppedneedle_kernel::db::map_sqlx_busy(operation, error),
        )
    }
}

fn internal(operation: &str) -> impl Fn(sqlx::Error) -> StoreError + '_ {
    move |error| StoreError::read(operation, error)
}

/// One id-list statement's rows over the pool.
async fn id_rows(
    pool: &SqlitePool,
    text: &str,
    ids: &[String],
) -> Result<Vec<sqlx::sqlite::SqliteRow>, StoreError> {
    let mut query = sqlx::query(text);
    for id in ids {
        query = query.bind(id);
    }
    query
        .fetch_all(pool)
        .await
        .map_err(internal("library.artists.list"))
}

/// Artist rows with their counts for `ids`, in `ids` order; unknown ids
/// drop out.
pub(crate) async fn hydrate_artists(
    pool: &SqlitePool,
    ids: &[String],
) -> Result<Vec<ArtistRecord>, StoreError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let len = ListLen::new(ids.len());
    let rows = id_rows(pool, &sql::artist_rows(len), ids).await?;
    let mut album_counts = HashMap::new();
    for row in id_rows(pool, &sql::artist_album_counts(len), ids).await? {
        let total: i64 = row.get(1);
        album_counts.insert(row.get::<String, _>(0), total.max(0) as u64);
    }
    let mut credit_counts = HashMap::new();
    for row in id_rows(pool, &sql::artist_credit_counts(len), ids).await? {
        let (tracks, appearances, appearance_tracks): (i64, i64, i64) =
            (row.get(1), row.get(2), row.get(3));
        credit_counts.insert(
            row.get::<String, _>(0),
            (
                tracks.max(0) as u64,
                appearances.max(0) as u64,
                appearance_tracks.max(0) as u64,
            ),
        );
    }
    let mut by_id = HashMap::with_capacity(rows.len());
    for row in &rows {
        let id: String = row.get("id");
        let (track_count, appearance_album_count, appearance_track_count) =
            credit_counts.get(&id).copied().unwrap_or_default();
        by_id.insert(
            id.clone(),
            ArtistRecord {
                album_count: album_counts.get(&id).copied().unwrap_or(0),
                id,
                name: row.get("name"),
                artist_mbid: row.get("artist_mbid"),
                linked: row.get("linked"),
                track_count,
                appearance_album_count,
                appearance_track_count,
                date_added: row.get("date_added"),
            },
        );
    }
    Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

/// The live album or artist (`kind`) a local id or alias names now, if
/// any. A track id always answers: the track it names now, or itself.
pub(crate) async fn current_id(
    pool: &SqlitePool,
    kind: &str,
    identifier: &str,
) -> Result<Option<String>, StoreError> {
    let statement = match kind {
        "album" => sql::CURRENT_ALBUM,
        "artist" => sql::CURRENT_ARTIST,
        "track" => sql::CURRENT_TRACK_ID,
        _ => return Ok(None),
    };
    sqlx::query_scalar(statement.text())
        .bind(identifier)
        .fetch_optional(pool)
        .await
        .map_err(internal("library.lookups.direct"))
}
