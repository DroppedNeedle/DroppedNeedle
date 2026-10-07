//! Direct library edits an admin (or a curator, for single tracks) asks
//! for: remove an album or a track, rescan one album's folders, and let
//! management touch an album again after it was excluded.
//!
//! Removing never deletes audio. Without `delete_files` the tracks only
//! leave the catalog (marked missing, rows and references kept, as v2).
//! With it, every file first moves into the recycle bin, each into its
//! own `<stamp>-<id>` entry so equal names never collide; if any move
//! fails, the files already moved go back and nothing in the catalog
//! changes. The bin is the download settings' `recycle_bin_path` (the one
//! upgrades use, pruned on the same retention window), else `.recycle`
//! at the top of the file's library root, which the scanner skips and
//! which keeps the move a rename on the same filesystem.
//!
//! Every refusal carries one [`Reason`]: a stable code, a plain sentence
//! and what to do next. All functions here block; callers run them off
//! the async workers.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use super::operations::reasons::Reason;
use super::scan::coordinator::ScanRequestError;
use super::scan::models::{ScanKind, ScanRequest, ScanRequestResult, ScanScope, ScanTrigger};
use super::scan::roots::RootRegistry;
use super::wiring::LibrarySetup;
use crate::runtime_config::sections::DownloadPolicy;

/// Most hops followed through merged (retired) albums.
const MAX_RETIRED_HOPS: usize = 8;

const fn reason(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

pub const ALBUM_NOT_FOUND: Reason = reason(
    "ALBUM_NOT_FOUND",
    "This album is not in the library.",
    "Rescan the library, then open the album again.",
);
pub const TRACK_NOT_FOUND: Reason = reason(
    "TRACK_NOT_FOUND",
    "This track is not in the library any more.",
    "Refresh the album page; it may already have been removed.",
);
pub const ALBUM_HAS_NO_FILES: Reason = reason(
    "ALBUM_HAS_NO_FILES",
    "None of this album's files are in the library, so there is nothing to rescan.",
    "Run a full library scan to pick the files up again.",
);
pub const FILE_OUTSIDE_ROOT: Reason = reason(
    "FILE_OUTSIDE_ROOT",
    "A file of this album is not inside a configured library folder, so it was left alone.",
    "Check the library folders under Settings > Library, then rescan.",
);
pub const NO_RECYCLE_BIN: Reason = reason(
    "NO_RECYCLE_BIN",
    "There is no recycle bin to move the files into.",
    "Set an absolute recycle bin path under Settings > Library management, or remove without deleting files.",
);
pub const RECYCLE_FAILED: Reason = reason(
    "RECYCLE_FAILED",
    "A file could not be moved into the recycle bin, so nothing was removed.",
    "Check that the server can write to the library folder and the recycle bin, then try again.",
);
pub const RESTORE_FAILED: Reason = reason(
    "RESTORE_FAILED",
    "A file could not be moved into the recycle bin, and some files already moved could not be put back.",
    "Look in the recycle bin for this album's files and move them back by hand, then rescan.",
);
pub const LIBRARY_BUSY: Reason = reason(
    "LIBRARY_BUSY",
    "A scan or a management run is working in this library folder right now.",
    "Wait for it to finish, then try again.",
);
pub const STALE_EXCLUSION: Reason = reason(
    "STALE_REVISION",
    "The album's management exclusion changed before it could be removed.",
    "Reload the album page and try again.",
);
pub const SCAN_REFUSED: Reason = reason(
    "SCAN_REFUSED",
    "The library folders changed while the rescan was being set up.",
    "Reload the album page and try again.",
);
pub const LIBRARY_DISABLED: Reason = reason(
    "LIBRARY_DISABLED",
    "The local library is switched off, so nothing can be rescanned.",
    "Turn the library on under Settings > Library, then try again.",
);

/// Why a mutation did not happen.
#[derive(Debug)]
pub enum MutationError {
    /// The album or track is unknown.
    NotFound(Reason),
    /// The request cannot run against the current state.
    Conflict(Reason),
    /// A file move failed; nothing in the catalog changed.
    Files(Reason),
    /// A store fault (logged by the handler).
    Store(String),
}

impl From<rusqlite::Error> for MutationError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(error.to_string())
    }
}

/// One album or track removal's result: the track ids that left the
/// catalog.
#[derive(Debug, Clone, Default)]
pub struct Removed {
    /// The canonical local album or the track id.
    pub id: String,
    /// Track ids now marked missing.
    pub track_ids: Vec<String>,
    /// The removed album's release group, when it was identified.
    pub release_group_mbid: Option<String>,
}

/// What happens outside the library once an album is removed: its
/// download records and wanted watch are cleaned up. Acquisition fills the
/// slot at boot; failures are logged there, never surfaced, because the
/// removal itself already happened.
pub trait AlbumRemovalHook: Send + Sync {
    /// The album `release_group_mbid` left the library. `stop_wanted`
    /// stops its watch; otherwise a fulfilled watch looks for a
    /// replacement again.
    fn album_removed<'a>(
        &'a self,
        release_group_mbid: &'a str,
        stop_wanted: bool,
    ) -> futures_util::future::BoxFuture<'a, ()>;
}

/// The slot the removal hook goes into.
pub type AlbumRemovalSlot =
    std::sync::Arc<std::sync::OnceLock<std::sync::Arc<dyn AlbumRemovalHook>>>;

/// One track row a removal or rescan works on.
#[derive(Debug, Clone)]
struct TrackFile {
    id: String,
    album_id: String,
    root_id: String,
    relative_path: String,
}

/// Library mutations over the setup's store, roots and scan coordinator.
pub struct Mutations<'a> {
    setup: &'a LibrarySetup,
}

impl<'a> Mutations<'a> {
    /// Mutations over one library setup.
    pub fn new(setup: &'a LibrarySetup) -> Self {
        Self { setup }
    }

    /// Remove an album from the catalog, moving its files into the
    /// recycle bin when `delete_files` is set. `album_id` is a local album
    /// id or a release-group MBID.
    pub fn remove_album(
        &self,
        album_id: &str,
        delete_files: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        let canonical = self
            .canonical_album(album_id)?
            .ok_or(MutationError::NotFound(ALBUM_NOT_FOUND))?;
        let tracks = self.album_tracks(&canonical)?;
        let reason_code = if delete_files {
            "ALBUM_FILES_RECYCLED"
        } else {
            "CATALOG_REMOVAL"
        };
        let release_group_mbid = self.read(|conn| {
            conn.query_row(
                "SELECT release_group_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND release_group_mbid IS NOT NULL LIMIT 1",
                [&canonical],
                |row| row.get::<_, String>(0),
            )
            .optional()
        })?;
        let track_ids = self.remove_tracks(&tracks, delete_files, reason_code, actor)?;
        Ok(Removed {
            id: canonical,
            track_ids,
            release_group_mbid,
        })
    }

    /// Remove one track, moving its file into the recycle bin when
    /// `delete_file` is set.
    pub fn remove_track(
        &self,
        track_id: &str,
        delete_file: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        let track = self
            .read(|conn| {
                conn.query_row(
                    "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'",
                    [track_id],
                    track_row,
                )
                .optional()
            })?
            .ok_or(MutationError::NotFound(TRACK_NOT_FOUND))?;
        let reason_code = if delete_file {
            "FILE_RECYCLED"
        } else {
            "CATALOG_REMOVAL"
        };
        let track_ids = self.remove_tracks(&[track], delete_file, reason_code, actor)?;
        Ok(Removed {
            id: track_id.to_owned(),
            track_ids,
            release_group_mbid: None,
        })
    }

    /// Queue a file rescan over the folders holding the album's files.
    pub fn rescan_album(
        &self,
        album_id: &str,
        actor: &str,
    ) -> Result<ScanRequestResult, MutationError> {
        let canonical = self
            .canonical_album(album_id)?
            .ok_or(MutationError::NotFound(ALBUM_NOT_FOUND))?;
        let registry = self.setup.live_registry();
        let scopes = rescan_scopes(&canonical, &self.album_tracks(&canonical)?, &registry);
        if scopes.is_empty() {
            return Err(MutationError::NotFound(ALBUM_HAS_NO_FILES));
        }
        self.setup
            .coordinator
            .request_run(&ScanRequest {
                kind: ScanKind::RescanFiles,
                trigger: ScanTrigger::Manual,
                scopes,
                requested_by_user_id: Some(actor.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
            .map_err(|error| match error {
                ScanRequestError::Disabled => MutationError::Conflict(LIBRARY_DISABLED),
                ScanRequestError::StalePolicy | ScanRequestError::UnknownRoots => {
                    MutationError::Conflict(SCAN_REFUSED)
                }
                other => MutationError::Store(other.to_string()),
            })
    }

    /// Clear an album's management exclusion if it still has
    /// `expected_revision`. False when the album was not excluded.
    pub fn reenable_management(
        &self,
        album_id: &str,
        expected_revision: i64,
        actor: &str,
    ) -> Result<bool, MutationError> {
        let canonical = self
            .canonical_album(album_id)?
            .ok_or(MutationError::NotFound(ALBUM_NOT_FOUND))?;
        self.write(|tx| {
            let removed: Option<(String, String, f64, i64)> = tx
                .query_row(
                    "DELETE FROM library_management_exclusions \
                     WHERE local_album_id = ?1 AND row_revision = ?2 \
                     RETURNING reason, excluded_by_user_id, excluded_at, row_revision",
                    params![canonical, expected_revision],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .optional()?;
            let Some((reason, by, at, revision)) = removed else {
                let exists: Option<i64> = tx
                    .query_row(
                        "SELECT row_revision FROM library_management_exclusions \
                         WHERE local_album_id = ?1",
                        [&canonical],
                        |row| row.get(0),
                    )
                    .optional()?;
                return match exists {
                    Some(_) => Err(MutationError::Conflict(STALE_EXCLUSION)),
                    None => Ok(false),
                };
            };
            let before = serde_json::json!({
                "local_album_id": canonical,
                "reason": reason,
                "excluded_by_user_id": by,
                "excluded_at": at,
                "row_revision": revision,
            });
            record_action(
                tx,
                actor,
                "management_reenabled",
                &canonical,
                None,
                &before,
                &serde_json::json!({}),
                "MANAGEMENT_REENABLED",
            )?;
            bump_catalog(tx)?;
            Ok(true)
        })
    }

    /// Recycle (when asked) then mark missing. Files already gone from
    /// disk are not a failure: their rows still leave the catalog.
    fn remove_tracks(
        &self,
        tracks: &[TrackFile],
        recycle: bool,
        reason_code: &str,
        actor: &str,
    ) -> Result<Vec<String>, MutationError> {
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut guards = Vec::new();
        if recycle {
            let registry = self.setup.live_registry();
            let configured = self.configured_bin();
            let mut planned = Vec::new();
            for track in tracks {
                let root = registry
                    .resolve(&track.root_id)
                    .ok_or(MutationError::Conflict(FILE_OUTSIDE_ROOT))?;
                let path = join_inside(&root.path, &track.relative_path)
                    .ok_or(MutationError::Conflict(FILE_OUTSIDE_ROOT))?;
                let bin = configured
                    .clone()
                    .unwrap_or_else(|| root.path.join(super::scan::fs::RECYCLE_BIN_DIRECTORY_NAME));
                planned.push((track.root_id.clone(), path, bin));
            }
            for root_id in planned
                .iter()
                .map(|(root_id, _, _)| root_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
            {
                guards.push(
                    self.setup
                        .fs
                        .try_write(&root_id)
                        .ok_or(MutationError::Conflict(LIBRARY_BUSY))?,
                );
            }
            for (_, path, bin) in planned {
                if !path.exists() {
                    tracing::warn!(
                        file = %path.file_name().unwrap_or_default().to_string_lossy(),
                        "file already absent; removing its catalog entry"
                    );
                    continue;
                }
                match recycle_file(&path, &bin) {
                    Ok(destination) => moved.push((path, destination)),
                    Err(error) => {
                        tracing::warn!(%error, "recycle move failed; restoring");
                        return Err(MutationError::Files(restore(&moved)));
                    }
                }
            }
        }
        let ids: Vec<String> = tracks.iter().map(|track| track.id.clone()).collect();
        let marked = self.write(|tx| mark_missing(tx, tracks, reason_code, actor));
        drop(guards);
        match marked {
            Ok(changed) => {
                self.setup.events.poke_activity();
                tracing::info!(
                    tracks = ids.len(),
                    recycled = moved.len(),
                    "library removal"
                );
                Ok(changed)
            }
            Err(error) => {
                if !moved.is_empty() {
                    let _ = restore(&moved);
                }
                Err(error)
            }
        }
    }

    /// The configured recycle bin, when it is an absolute path.
    fn configured_bin(&self) -> Option<PathBuf> {
        let configured = match self.setup.config.get::<DownloadPolicy>() {
            Ok(section) => section.recycle_bin_path,
            Err(error) => {
                tracing::warn!(%error, "download settings unreadable; using root bins");
                return None;
            }
        };
        let path = PathBuf::from(configured.trim());
        if configured.trim().is_empty() {
            return None;
        }
        if !path.is_absolute() {
            tracing::warn!("recycle_bin_path is not absolute; using the root's .recycle");
            return None;
        }
        Some(path)
    }

    /// Resolve a local album id (following merges) or a release-group
    /// MBID to the live local album.
    fn canonical_album(&self, album_id: &str) -> Result<Option<String>, MutationError> {
        self.read(|conn| {
            let mut current = album_id.to_owned();
            for _ in 0..MAX_RETIRED_HOPS {
                let row: Option<Option<String>> = conn
                    .query_row(
                        "SELECT retired_into_album_id FROM local_albums WHERE id = ?1",
                        [&current],
                        |row| row.get(0),
                    )
                    .optional()?;
                match row {
                    Some(None) => return Ok(Some(current)),
                    Some(Some(next)) => current = next,
                    None => break,
                }
            }
            conn.query_row(
                "SELECT b.id FROM local_album_external_identities e \
                 JOIN local_albums b ON b.id = e.local_album_id \
                 WHERE lower(e.release_group_mbid) = lower(?1) \
                 AND b.retired_into_album_id IS NULL ORDER BY b.created_at LIMIT 1",
                [album_id],
                |row| row.get(0),
            )
            .optional()
        })
    }

    fn album_tracks(&self, album_id: &str) -> Result<Vec<TrackFile>, MutationError> {
        self.read(|conn| {
            let mut statement = conn.prepare(
                "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed' \
                 ORDER BY disc_number, track_number, id",
            )?;
            let rows = statement.query_map([album_id], track_row)?;
            rows.collect()
        })
    }

    fn read<T>(
        &self,
        op: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<T>,
    ) -> Result<T, MutationError> {
        self.setup
            .identify_store
            .with_connection(|conn| op(conn))
            .map_err(MutationError::from)
    }

    fn write<T>(
        &self,
        op: impl FnOnce(&Transaction<'_>) -> Result<T, MutationError>,
    ) -> Result<T, MutationError> {
        self.setup.identify_store.with_connection(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            tx.commit()?;
            Ok(value)
        })
    }
}

fn track_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TrackFile> {
    Ok(TrackFile {
        id: row.get(0)?,
        album_id: row.get(1)?,
        root_id: row.get(2)?,
        relative_path: row.get(3)?,
    })
}

/// One scope per (root, folder) holding the album's files.
fn rescan_scopes(album_id: &str, tracks: &[TrackFile], registry: &RootRegistry) -> Vec<ScanScope> {
    let mut scopes: BTreeMap<(String, String), ScanScope> = BTreeMap::new();
    for track in tracks {
        let Some(root) = registry.resolve(&track.root_id) else {
            continue;
        };
        let parent = Path::new(&track.relative_path)
            .parent()
            .map(|parent| parent.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let relative = if parent.is_empty() || parent == "." {
            ".".to_owned()
        } else {
            parent
        };
        let full = root.path.join(&relative);
        scopes
            .entry((track.root_id.clone(), relative.clone()))
            .or_insert_with(|| ScanScope {
                root_id: track.root_id.clone(),
                scope_id: Some(format!("album:{album_id}:{relative}")),
                relative_path: relative,
                root_path: Some(root.path.to_string_lossy().into_owned()),
                effective_policy: root.policy_for(&full),
                policy_revision: registry.policy_revision().to_owned(),
                estimated_count: None,
            });
    }
    scopes.into_values().collect()
}

/// Join a stored relative path under its root, refusing anything that
/// could step outside it.
fn join_inside(root: &Path, relative: &str) -> Option<PathBuf> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(relative))
}

/// Move one file into its own bin entry; returns where it went.
fn recycle_file(path: &Path, bin: &Path) -> std::io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| std::io::Error::other("file has no name"))?;
    let entry = bin.join(format!(
        "{}-{}",
        utc_stamp(SystemTime::now()),
        &uuid::Uuid::new_v4().simple().to_string()[..8]
    ));
    std::fs::create_dir_all(&entry)?;
    let destination = entry.join(name);
    move_file(path, &destination)?;
    Ok(destination)
}

/// Rename, or copy then unlink when the bin is on another filesystem.
fn move_file(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(18) => {
            std::fs::copy(from, to)?;
            std::fs::remove_file(from)
        }
        Err(error) => Err(error),
    }
}

/// Put moved files back, newest first. Answers the reason to report.
fn restore(moved: &[(PathBuf, PathBuf)]) -> Reason {
    let mut failed = false;
    for (original, destination) in moved.iter().rev() {
        let back = original
            .parent()
            .map(std::fs::create_dir_all)
            .transpose()
            .and_then(|_| move_file(destination, original));
        if let Err(error) = back {
            failed = true;
            tracing::error!(%error, "could not restore a recycled file");
        }
    }
    if failed {
        RESTORE_FAILED
    } else {
        RECYCLE_FAILED
    }
}

/// Mark indexed tracks missing, one catalog action each; rows and
/// references stay (v2 `mark_target_tracks_missing`).
fn mark_missing(
    tx: &Transaction<'_>,
    tracks: &[TrackFile],
    reason_code: &str,
    actor: &str,
) -> Result<Vec<String>, MutationError> {
    let now = now_secs();
    let mut changed = Vec::new();
    for track in tracks {
        let revision: Option<i64> = tx
            .query_row(
                "SELECT row_revision FROM local_tracks WHERE id = ?1 AND availability = 'indexed'",
                [&track.id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(revision) = revision else {
            continue;
        };
        tx.execute(
            "UPDATE local_tracks SET availability = 'missing', missing_since = ?1, \
             row_revision = row_revision + 1 WHERE id = ?2",
            params![now, track.id],
        )?;
        record_action(
            tx,
            actor,
            "remove_track",
            &track.album_id,
            Some(&track.id),
            &serde_json::json!({"availability": "indexed", "row_revision": revision}),
            &serde_json::json!({"availability": "missing", "row_revision": revision + 1}),
            reason_code,
        )?;
        changed.push(track.id.clone());
    }
    if !changed.is_empty() {
        bump_catalog(tx)?;
    }
    Ok(changed)
}

#[allow(clippy::too_many_arguments)]
fn record_action(
    tx: &Transaction<'_>,
    actor: &str,
    kind: &str,
    album_id: &str,
    track_id: Option<&str>,
    before: &serde_json::Value,
    after: &serde_json::Value,
    reason_code: &str,
) -> rusqlite::Result<()> {
    // The actor column references auth_users; an unknown id stays NULL.
    tx.execute(
        "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         local_track_id, before_json, after_json, reason_code, created_at) VALUES \
         (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            uuid::Uuid::new_v4().to_string(),
            actor,
            kind,
            album_id,
            track_id,
            before.to_string(),
            after.to_string(),
            reason_code,
            now_secs(),
        ],
    )?;
    Ok(())
}

fn bump_catalog(tx: &Transaction<'_>) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0) \
         ON CONFLICT (singleton) DO NOTHING",
        [],
    )?;
    tx.execute(
        "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
        [],
    )?;
    Ok(())
}

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

/// `YYYYMMDDTHHMMSS` in UTC, the stamp the bin prune reads.
fn utc_stamp(now: SystemTime) -> String {
    let secs = now
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rest = secs % 86_400;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}",
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamp_and_path_guard() {
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(utc_stamp(at), "20231114T221320");
        assert!(join_inside(Path::new("/lib"), "a/../../etc").is_none());
        assert!(join_inside(Path::new("/lib"), "/etc/passwd").is_none());
        assert_eq!(
            join_inside(Path::new("/lib"), "A/B/01.flac"),
            Some(PathBuf::from("/lib/A/B/01.flac"))
        );
    }
}
