//! The album edition facts the per-album edition route shows.
//!
//! The album's identity row is its edition; this store only reads it. A
//! person's choice is a `manual` row.

use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError};

/// One album's edition, as its identity row holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEditions {
    /// Linked release group, when the album is identified.
    pub release_group_mbid: Option<String>,
    /// The album's edition, when known.
    pub release_mbid: Option<String>,
    /// True when a person chose that edition.
    pub chosen: bool,
}

/// Reads one album's edition.
#[derive(Clone, Debug)]
pub struct PinStore {
    db: CollectionsDb,
}

impl PinStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// The album's edition facts, or None when the album is not in the
    /// library.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumEditions>, StoreError> {
        let pool = self.db.pool()?;
        let row = sqlx::query(
            "SELECT e.release_group_mbid AS release_group_mbid, \
             e.release_mbid AS release_mbid, \
             COALESCE(e.decision_source IN ('manual', 'legacy_import'), 0) AS chosen \
             FROM local_albums a \
             LEFT JOIN local_album_external_identities e ON e.local_album_id = a.id \
             WHERE a.id = ? AND a.retired_into_album_id IS NULL",
        )
        .bind(album_id)
        .fetch_optional(pool)
        .await
        .map_err(|error| StoreError::read("pins.album", error))?;
        row.map(|row| {
            Ok(AlbumEditions {
                release_group_mbid: row.try_get("release_group_mbid")?,
                release_mbid: row.try_get("release_mbid")?,
                chosen: row.try_get::<i64, _>("chosen")? != 0,
            })
        })
        .transpose()
        .map_err(|error| StoreError::read("pins.album", error))
    }
}
