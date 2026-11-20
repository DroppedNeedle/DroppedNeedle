//! Chunked catalog cleanup for tracks belonging to a removed library root.
//!
//! Removing a library folder from Settings > Library leaves its tracks indexed
//! so the admin can restore the root without rescanning. When the admin instead
//! wants to drop those lingering catalog tracks (for example after replacing a
//! root with a new one at an overlapping path), this service marks only that
//! removed root's indexed tracks as `missing` in bounded transactions, firing
//! all read-model triggers and preserving play history, favorites, ratings, and
//! playlists without touching any files on disk.

use std::sync::Arc;

use rusqlite::params;

use crate::clock::LibraryClock as _;
use crate::sql;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::writer::Lane;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_kernel::runtime_config::settings_error::SettingsError;

/// Maximum tracks updated per foreground transaction.
const CHUNK: usize = 500;

/// Outcome of dropping indexed tracks for a removed root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropRemovedRootResult {
    pub marked_missing_count: u64,
    pub policy_revision: String,
}

enum DropChunkError {
    ScanActive,
    Db(DbError),
}

impl std::fmt::Display for DropChunkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ScanActive => f.write_str("a library scan is active"),
            Self::Db(error) => error.fmt(f),
        }
    }
}

impl From<DbError> for DropChunkError {
    fn from(error: DbError) -> Self {
        Self::Db(error)
    }
}

impl From<rusqlite::Error> for DropChunkError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

/// Owns the `[library]` writes that drop lingering catalog tracks for a
/// removed library root.
#[derive(Clone)]
pub struct RemovedRootCleanup {
    db: Db,
    clock: Arc<dyn Clock>,
    library_settings: Live<TypedLibrary>,
}

impl RemovedRootCleanup {
    pub fn new(db: Db, clock: Arc<dyn Clock>, library_settings: Live<TypedLibrary>) -> Self {
        Self {
            db,
            clock,
            library_settings,
        }
    }

    fn verify_removed_root(
        &self,
        root_id: &str,
        expected_policy_revision: &str,
    ) -> Result<(), SettingsError> {
        let stored = self
            .library_settings
            .get()
            .map_err(|error| SettingsError::internal(&error))?;
        crate::policy::check_revision(&stored, expected_policy_revision)?;
        if stored.library_roots.iter().any(|root| root.id == root_id) {
            return Err(SettingsError::InvalidInput {
                message: "That library folder is currently configured. \
                          Remove it from Library settings first."
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// Mark every indexed track under removed root `root_id` as `missing` in
    /// chunks of 500 and clear residual scan state for that root.
    pub async fn drop_removed_root_tracks(
        &self,
        root_id: &str,
        expected_policy_revision: &str,
    ) -> Result<DropRemovedRootResult, SettingsError> {
        if root_id.trim().is_empty() {
            return Err(SettingsError::InvalidInput {
                message: "A removed library root id is required.".to_owned(),
            });
        }

        let mut total_marked: u64 = 0;
        loop {
            self.verify_removed_root(root_id, expected_policy_revision)?;
            let now_ms = crate::clock::ms(self.clock.now_secs());
            let target_root_id = root_id.to_owned();
            let (chunk_marked, done) = self
                .db
                .write(Lane::Foreground, "drop_removed_root_tracks", move |tx| {
                    let active: bool =
                        tx.query_row(sql::SCAN_ACTIVE_OR_FINISHING.text(), [], |row| row.get(0))?;
                    if active {
                        return Err(DropChunkError::ScanActive);
                    }
                    let ids: Vec<String> = {
                        let mut stmt =
                            tx.prepare_cached(sql::REMOVED_ROOT_INDEXED_TRACK_IDS.text())?;
                        stmt.query_map(params![target_root_id, CHUNK as i64], |row| row.get(0))?
                            .collect::<rusqlite::Result<Vec<_>>>()?
                    };
                    let mut marked = 0usize;
                    if !ids.is_empty() {
                        let mut mark =
                            tx.prepare_cached(sql::MARK_REMOVED_ROOT_TRACK_MISSING.text())?;
                        for id in &ids {
                            marked += mark.execute(params![id, now_ms])?;
                        }
                    }
                    let is_last = ids.len() < CHUNK;
                    let mut cleaned = 0usize;
                    if is_last {
                        cleaned +=
                            tx.execute(sql::DROP_ROOT_HELD_SCOPES.text(), params![target_root_id])?;
                        cleaned += tx.execute(
                            sql::DROP_ROOT_UNJUDGED_NON_INDEXED.text(),
                            params![target_root_id],
                        )?;
                        cleaned +=
                            tx.execute(sql::DROP_ROOT_MOUNTS.text(), params![target_root_id])?;
                        cleaned += tx.execute(
                            sql::DROP_ROOT_FOLDER_DIGESTS.text(),
                            params![target_root_id],
                        )?;
                    }
                    if marked > 0 || cleaned > 0 {
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Ok((marked as u64, is_last))
                })
                .await
                .map_err(|error| match error {
                    DropChunkError::ScanActive => SettingsError::Conflict {
                        message: "Wait for the active library scan to finish before dropping \
                                  removed folder tracks."
                            .to_owned(),
                    },
                    DropChunkError::Db(DbError::Busy { operation }) => SettingsError::Busy {
                        cause: format!("database busy during {operation}"),
                    },
                    DropChunkError::Db(other) => SettingsError::internal(&other),
                })?;

            total_marked += chunk_marked;
            if done {
                break;
            }
        }

        tracing::info!(
            root_id,
            marked_missing_count = total_marked,
            "dropped lingering catalog tracks for removed library root"
        );
        Ok(DropRemovedRootResult {
            marked_missing_count: total_marked,
            policy_revision: expected_policy_revision.to_owned(),
        })
    }
}
