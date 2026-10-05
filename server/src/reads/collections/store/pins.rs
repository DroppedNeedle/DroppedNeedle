//! Edition pins over `library_album_release_pins`.
//!
//! A pin is a display preference for one local album, never catalog
//! identity: this store reads `local_album_external_identities` and never
//! writes it.

use rusqlite::params;
use sqlx::Row as _;

use super::super::db::{CollectionsDb, StoreError, now_epoch};

/// One album's identity facts the pin lane needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlbumEditions {
    /// Linked release group, when the album is identified.
    pub release_group_mbid: Option<String>,
    /// The album's own release, when known: the default display pick.
    pub release_mbid: Option<String>,
    /// The current pin, when set.
    pub pinned_release_mbid: Option<String>,
}

/// The pin store.
#[derive(Clone, Debug)]
pub struct PinStore {
    db: CollectionsDb,
}

impl PinStore {
    /// Store over one database.
    pub fn new(db: CollectionsDb) -> Self {
        Self { db }
    }

    /// Identity and pin facts for one album, or None when the album is not
    /// in the library.
    pub async fn album(&self, album_id: &str) -> Result<Option<AlbumEditions>, StoreError> {
        let pool = self.db.pool()?;
        let row = sqlx::query(
            "SELECT e.release_group_mbid AS release_group_mbid, \
             e.release_mbid AS release_mbid, p.release_mbid AS pinned \
             FROM local_albums a \
             LEFT JOIN local_album_external_identities e ON e.local_album_id = a.id \
             LEFT JOIN library_album_release_pins p ON p.local_album_id = a.id \
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
                pinned_release_mbid: row.try_get("pinned")?,
            })
        })
        .transpose()
        .map_err(|error| StoreError::read("pins.album", error))
    }

    /// Releases of one group the library knows: the identities of every
    /// local copy.
    pub async fn known_editions(
        &self,
        release_group_mbid: &str,
    ) -> Result<Vec<String>, StoreError> {
        let pool = self.db.pool()?;
        sqlx::query_scalar(
            "SELECT DISTINCT release_mbid FROM local_album_external_identities \
             WHERE lower(release_group_mbid) = lower(?) AND release_mbid IS NOT NULL \
             ORDER BY release_mbid",
        )
        .bind(release_group_mbid)
        .fetch_all(pool)
        .await
        .map_err(|error| StoreError::read("pins.known_editions", error))
    }

    /// Pin one release for one album, replacing any earlier pin.
    pub async fn set(
        &self,
        album_id: &str,
        release_group_mbid: &str,
        release_mbid: &str,
        user_id: &str,
    ) -> Result<(), StoreError> {
        let (album_id, group, release, user_id) = (
            album_id.to_owned(),
            release_group_mbid.to_owned(),
            release_mbid.to_owned(),
            user_id.to_owned(),
        );
        self.db
            .write("pins.set", move |tx| {
                tx.execute(
                    "INSERT INTO library_album_release_pins (local_album_id, \
                     release_group_mbid, release_mbid, set_by_user_id, set_at) \
                     VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%SZ', ?5, 'unixepoch')) \
                     ON CONFLICT (local_album_id) DO UPDATE SET \
                     release_group_mbid = excluded.release_group_mbid, \
                     release_mbid = excluded.release_mbid, \
                     set_by_user_id = excluded.set_by_user_id, set_at = excluded.set_at",
                    params![album_id, group, release, user_id, now_epoch() as i64],
                )?;
                Ok(())
            })
            .await
    }

    /// Clear one album's pin. Clearing an unpinned album is a no-op.
    pub async fn clear(&self, album_id: &str) -> Result<(), StoreError> {
        let album_id = album_id.to_owned();
        self.db
            .write("pins.clear", move |tx| {
                tx.execute(
                    "DELETE FROM library_album_release_pins WHERE local_album_id = ?1",
                    params![album_id],
                )?;
                Ok(())
            })
            .await
    }
}
