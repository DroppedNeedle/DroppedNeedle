//! Direct library edits an admin (or a curator, for single tracks) asks
//! for: remove an album or a track, rescan one album's folders, and let
//! management touch an album again after it was excluded.
//!
//! Removing never deletes audio. Without `delete_files` the tracks only
//! leave the catalog (marked missing, rows and references kept, as v2).
//! With it, the removal goes through the publisher as one bundle
//! (`publish::remove`): each file is set aside under the journal, the
//! catalog marks the tracks missing in one commit, and only then does each
//! file move into its own `<stamp>-<id>` recycle bin entry, so equal names
//! never collide. A removal that stops before its commit puts every file
//! back; one that stops after it finishes on restart. This module decides
//! what to remove and never moves a file itself. The bin is the download
//! settings' `recycle_bin_path` (the one upgrades use, pruned on the same
//! retention window) when it overlaps no library or download folder, else
//! `.recycle` at the top of the file's library root, which the scanner
//! skips and which keeps the move a rename on the same filesystem.
//!
//! Every refusal carries one [`Reason`]: a stable code, a plain sentence
//! and what to do next. All functions here block; callers run them off
//! the async workers.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use droppedneedle_kernel::db::Lane;
use rusqlite::{OptionalExtension, Transaction, params};

use super::catalog_revision::bump_catalog;
use super::manage::RemovalEntry;
use super::management::takeover;
use super::operations::reasons::Reason;
use super::publish::PublishError;
use super::publish::bin::{fresh_entry, serving};
use super::publish::paths::Root;
use super::publish::planner::ExtrasAction;
use super::publish::remove::{self, Refusal, Target};
use super::scan::coordinator::ScanRequestError;
use super::scan::models::{ScanKind, ScanRequest, ScanRequestResult, ScanScope, ScanTrigger};
use super::scan::roots::RootRegistry;
use super::wiring::LibrarySetup;

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
pub const LIBRARY_FOLDER_MISSING: Reason = reason(
    "LIBRARY_FOLDER_MISSING",
    "The library folder isn't there, so nothing was removed.",
    "Check that its drive or share is mounted, then try again.",
);
pub const RECYCLE_FAILED: Reason = reason(
    "RECYCLE_FAILED",
    "A file could not be moved into the recycle bin, so nothing was removed.",
    "Check that the server can write to the library folder and the recycle bin, then try again.",
);
pub const RESTORE_FAILED: Reason = reason(
    "RESTORE_FAILED",
    "A file could not be moved into the recycle bin, and another could not be put back. It is kept under a hidden name beside where it was.",
    "Look for a hidden .droppedneedle-management file in the album's folder, move it back by hand, then rescan.",
);
pub const REMOVAL_CHANGED: Reason = reason(
    "REMOVAL_CHANGED",
    "A file or its track changed while it was being removed, so nothing was removed.",
    "Reload the page and try again.",
);
pub const FILE_NOT_LINKABLE: Reason = reason(
    "FILE_NOT_LINKABLE",
    "A file could not be moved safely, so nothing was removed: the server can't link it to keep a way back.",
    "Check the file's owner and permissions, or that its disk supports hard links, then try again.",
);
pub const NO_RECYCLE_BIN: Reason = reason(
    "NO_RECYCLE_BIN",
    "This library folder has no usable recycle bin, so nothing was removed: its .recycle links somewhere it may not.",
    "Make .recycle a plain folder, or set a recycle bin in the download settings.",
);
pub const RECYCLE_BIN_UNKNOWN: Reason = reason(
    "RECYCLE_BIN_UNKNOWN",
    "No recycle bin is known while the download or SABnzbd settings can't be read, so nothing was removed.",
    "Open those settings and save them again, then try again.",
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

pub const HAND_BACK_UNREADABLE: Reason = reason(
    "HAND_BACK_UNREADABLE",
    "The album's files could not be read, so it was not handed back and stays unmanaged.",
    "Check that the library folder is mounted and readable, then try again.",
);

pub const NOTHING_TO_RESTORE: Reason = reason(
    "NOTHING_TO_RESTORE",
    "There is no removal of this album's extra files to take back, or it was already taken back.",
    "Refresh the album page to see its files.",
);
pub const RESTORE_PATH_OCCUPIED: Reason = reason(
    "RESTORE_PATH_OCCUPIED",
    "Another file now sits where a removed file was, so nothing was put back.",
    "Move or rename the file that is in the way, then put the files back again. The removed files are still in the recycle bin.",
);

/// The `before_json` a restore records, naming the removal it undid.
fn restored_marker(action_id: &str) -> String {
    serde_json::json!({ "undoes": action_id }).to_string()
}

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
    /// The database was too busy to answer, or is shutting down; the
    /// cause is for the log.
    Busy(String),
}

impl std::fmt::Display for MutationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(reason) | Self::Conflict(reason) | Self::Files(reason) => {
                write!(f, "{}: {}", reason.code, reason.message)
            }
            Self::Store(cause) | Self::Busy(cause) => write!(f, "{cause}"),
        }
    }
}

impl From<droppedneedle_kernel::db::DbError> for MutationError {
    fn from(error: droppedneedle_kernel::db::DbError) -> Self {
        use droppedneedle_kernel::db::DbError;
        match error {
            DbError::Busy { .. } | DbError::LaneClosed => Self::Busy(error.to_string()),
            other => Self::Store(other.to_string()),
        }
    }
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
    /// Files moved into the recycle bin.
    pub recycled: Recycled,
}

/// Files moved into the recycle bin, as (where it was, where it is now).
pub type Recycled = Vec<(PathBuf, PathBuf)>;

/// What happens outside the library once an album is removed: its
/// download records and wanted watch are cleaned up. The library is built
/// with acquisition's; failures are logged there, never surfaced, because
/// the removal itself already happened.
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
    /// id or a release-group MBID. Background work: it waits for a write
    /// in flight instead of answering busy.
    pub fn remove_album(
        &self,
        album_id: &str,
        delete_files: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        self.remove_album_with(album_id, delete_files, actor, RemovalEntry::Wait)
    }

    /// [`Self::remove_album`] for a person waiting on the answer: a write
    /// in flight answers busy at once.
    pub fn remove_album_now(
        &self,
        album_id: &str,
        delete_files: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        self.remove_album_with(album_id, delete_files, actor, RemovalEntry::Now)
    }

    fn remove_album_with(
        &self,
        album_id: &str,
        delete_files: bool,
        actor: &str,
        entry: RemovalEntry,
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
        let release_group_mbid = self
            .setup
            .db
            .read_blocking("mutations.remove_album", |conn| {
                conn.query_row(
                    "SELECT release_group_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND release_group_mbid IS NOT NULL LIMIT 1",
                    [&canonical],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(MutationError::from)
            })?;
        let removal = Removal {
            reason_code,
            actor,
            entry,
            expect_sha256: None,
            extras: None,
        };
        let (track_ids, recycled) = self.remove_tracks(&tracks, delete_files, &removal)?;
        Ok(Removed {
            id: canonical,
            track_ids,
            release_group_mbid,
            recycled,
        })
    }

    /// Remove one track, moving its file into the recycle bin when
    /// `delete_file` is set. Background work: it waits for a write in
    /// flight instead of answering busy.
    pub fn remove_track(
        &self,
        track_id: &str,
        delete_file: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        self.remove_track_with(track_id, delete_file, actor, RemovalEntry::Wait)
    }

    /// [`Self::remove_track`] for a person waiting on the answer: a write
    /// in flight answers busy at once.
    pub fn remove_track_now(
        &self,
        track_id: &str,
        delete_file: bool,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        self.remove_track_with(track_id, delete_file, actor, RemovalEntry::Now)
    }

    fn remove_track_with(
        &self,
        track_id: &str,
        delete_file: bool,
        actor: &str,
        entry: RemovalEntry,
    ) -> Result<Removed, MutationError> {
        let track = self.indexed_track(track_id, None)?;
        let reason_code = if delete_file {
            "FILE_RECYCLED"
        } else {
            "CATALOG_REMOVAL"
        };
        let removal = Removal {
            reason_code,
            actor,
            entry,
            expect_sha256: None,
            extras: None,
        };
        let (track_ids, recycled) = self.remove_tracks(&[track], delete_file, &removal)?;
        Ok(Removed {
            id: track_id.to_owned(),
            track_ids,
            release_group_mbid: None,
            recycled,
        })
    }

    /// Move the file a duplicate resolution sends to the recycle bin, and
    /// take its track out of the catalog. The track must still be at
    /// `root_id:relative_path` and its file must still hold `sha256`, the
    /// bytes the preview read; otherwise nothing is removed.
    pub(crate) fn recycle_duplicate(
        &self,
        track_id: &str,
        (root_id, relative_path): (&str, &str),
        sha256: &str,
        actor: &str,
    ) -> Result<Removed, MutationError> {
        let track = self.indexed_track(track_id, None)?;
        if track.root_id != root_id || track.relative_path != relative_path {
            return Err(MutationError::Conflict(REMOVAL_CHANGED));
        }
        let removal = Removal {
            reason_code: "FILE_RECYCLED",
            actor,
            entry: RemovalEntry::Now,
            expect_sha256: Some(sha256),
            extras: None,
        };
        let (track_ids, recycled) = self.remove_tracks(&[track], true, &removal)?;
        Ok(Removed {
            id: track_id.to_owned(),
            track_ids,
            release_group_mbid: None,
            recycled,
        })
    }

    /// Move some of one album's files into the recycle bin: the files its
    /// edition has no place for, which a person picked from the edition
    /// plan. The undo record is written in the same commit that takes the
    /// tracks out, so [`Self::restore_recycled`] can put them back. Answers
    /// that record's id and the track ids that left the catalog.
    pub fn recycle_album_files(
        &self,
        album_id: &str,
        track_ids: &[String],
        actor: &str,
    ) -> Result<(String, Vec<String>), MutationError> {
        let mut tracks = Vec::with_capacity(track_ids.len());
        for track_id in track_ids {
            tracks.push(self.indexed_track(track_id, Some(album_id))?);
        }
        let action_id = uuid::Uuid::new_v4().to_string();
        let removal = Removal {
            reason_code: "EDITION_EXTRA_RECYCLED",
            actor,
            entry: RemovalEntry::Now,
            expect_sha256: None,
            extras: Some(ExtrasAction {
                id: action_id.clone(),
                album_id: album_id.to_owned(),
            }),
        };
        let (removed, _) = self.remove_tracks(&tracks, true, &removal)?;
        Ok((action_id, removed))
    }

    /// The indexed track `track_id`, on `album_id` when one is given.
    fn indexed_track(
        &self,
        track_id: &str,
        album_id: Option<&str>,
    ) -> Result<TrackFile, MutationError> {
        self.setup
            .db
            .read_blocking("mutations.indexed_track", |conn| {
                conn.query_row(
                    "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed' \
                     AND (?2 IS NULL OR local_album_id = ?2)",
                    params![track_id, album_id],
                    track_row,
                )
                .optional()
                .map_err(MutationError::from)
            })?
            .ok_or(MutationError::NotFound(TRACK_NOT_FOUND))
    }

    /// Put back the files one [`Self::recycle_album_files`] moved, then
    /// rescan their folders. Refused as a whole, and still there to take
    /// back later, when another file now sits at one of the old paths,
    /// unless that file's recycled copy is gone from the bin (someone
    /// moved it back by hand). Answers false when a file could not be put
    /// back (it stays in the recycle bin).
    pub fn restore_recycled(
        &self,
        album_id: &str,
        action_id: &str,
        actor: &str,
    ) -> Result<bool, MutationError> {
        let after: String = self
            .setup
            .db
            .read_blocking("mutations.recycled_moves", |conn| {
                conn.query_row(
                    "SELECT after_json FROM library_catalog_actions WHERE id = ?1 \
                     AND local_album_id = ?2 AND action_kind = 'edition_extras_recycled' \
                     AND NOT EXISTS (SELECT 1 FROM library_catalog_actions r \
                       WHERE r.action_kind = 'edition_extras_restored' AND r.before_json = ?3)",
                    params![action_id, album_id, restored_marker(action_id)],
                    |row| row.get(0),
                )
                .optional()
                .map_err(MutationError::from)
            })?
            .ok_or(MutationError::NotFound(NOTHING_TO_RESTORE))?;
        let moves: Vec<(PathBuf, PathBuf)> = serde_json::from_str::<serde_json::Value>(&after)
            .ok()
            .and_then(|value| value.get("moves").cloned())
            .and_then(|moves| serde_json::from_value::<Vec<[String; 2]>>(moves).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|[from, to]| (PathBuf::from(from), PathBuf::from(to)))
            .collect();
        // A file the person already moved back by hand has no copy left in
        // the bin, and must not block the undo.
        if let Some((original, _)) = moves
            .iter()
            .find(|(original, recycled)| recycled.exists() && occupied(original))
        {
            tracing::warn!(
                file = %original.file_name().unwrap_or_default().to_string_lossy(),
                unreadable = ?std::fs::symlink_metadata(original).err().map(|error| error.kind()),
                "a file sits where a recycled file was; nothing was put back"
            );
            return Err(MutationError::Conflict(RESTORE_PATH_OCCUPIED));
        }
        self.setup
            .db
            .write_blocking(Lane::Foreground, "mutations.record_restored", |tx| {
                tx.execute(
                    "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, \
                 local_album_id, before_json, after_json, reason_code, created_at) VALUES \
                 (?1, (SELECT id FROM auth_users WHERE id = ?2), 'edition_extras_restored', ?3, \
                 ?4, '{}', 'EDITION_EXTRA_RESTORED', ?5)",
                    params![
                        uuid::Uuid::new_v4().to_string(),
                        actor,
                        album_id,
                        restored_marker(action_id),
                        now_secs(),
                    ],
                )?;
                Ok::<_, MutationError>(())
            })?;
        Ok(self.put_back(&moves, actor))
    }

    /// The recycle bin a file of this library folder goes to: the
    /// configured bin when it passes its checks, else the folder's own
    /// `.recycle`. None while the download settings do not read, so
    /// nothing is recycled.
    pub fn bin_for(&self, root_id: &str) -> Option<PathBuf> {
        let bins = self.bins(&self.setup.live_registry()).ok()?;
        bin_dir(&bins, root_id).ok()
    }

    /// A fresh recycle-bin entry for a file named `name` from this
    /// library folder: `<bin>/<stamp>-<id>/<name>`, not created yet.
    /// `None` for a name that cannot be one. `Err` while no bin is known
    /// ([`RECYCLE_BIN_UNKNOWN`]) or the folder has no usable one
    /// ([`NO_RECYCLE_BIN`]).
    pub fn recycle_entry(
        &self,
        root_id: &str,
        name: &str,
    ) -> Result<Option<PathBuf>, MutationError> {
        let bins = self.bins(&self.setup.live_registry())?;
        Ok(fresh_entry(
            &bin_dir(&bins, root_id)?,
            name,
            SystemTime::now(),
        ))
    }

    /// Undo recycle moves (an upgrade whose new file could not be placed):
    /// each file goes back where it was, then the folders are rescanned so
    /// the catalog lists them again. Answers false when a file could not
    /// be put back, or another file now sits at its old path; it stays in
    /// the recycle bin and the error is logged.
    pub fn put_back(&self, moved: &[(PathBuf, PathBuf)], actor: &str) -> bool {
        let restored = restore(moved);
        let registry = self.setup.live_registry();
        let mut scopes: BTreeMap<(String, String), ScanScope> = BTreeMap::new();
        for (original, _) in moved {
            let Some(root) = registry
                .roots()
                .iter()
                .find(|root| original.starts_with(&root.path))
            else {
                continue;
            };
            let relative = original
                .parent()
                .and_then(|parent| parent.strip_prefix(&root.path).ok())
                .map(|parent| parent.to_string_lossy().replace('\\', "/"))
                .filter(|parent| !parent.is_empty())
                .unwrap_or_else(|| ".".to_owned());
            let full = root.path.join(&relative);
            scopes
                .entry((root.id.clone(), relative.clone()))
                .or_insert_with(|| ScanScope {
                    root_id: root.id.clone(),
                    scope_id: Some(format!("restore:{relative}")),
                    relative_path: relative,
                    root_path: Some(root.path.to_string_lossy().into_owned()),
                    effective_policy: root.policy_for(&full),
                    policy_revision: registry.policy_revision().to_owned(),
                    estimated_count: None,
                });
        }
        if !scopes.is_empty()
            && let Err(error) = self.setup.coordinator.request_run(&ScanRequest {
                kind: ScanKind::RescanFiles,
                trigger: ScanTrigger::Manual,
                scopes: scopes.into_values().collect(),
                requested_by_user_id: Some(actor.to_owned()),
                policy_revision: registry.policy_revision().to_owned(),
            })
        {
            tracing::warn!(%error, "rescan after putting files back was refused; run a scan");
        }
        restored
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
        self.setup
            .db
            .write_blocking(Lane::Foreground, "mutations.reenable_management", |tx| {
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

    /// Hand an album the app stepped back from to automatic management
    /// again: its files are recorded as they are now, and the next
    /// automatic run writes them from the album's edition. False when the
    /// album is not marked. Reads the album's files.
    pub fn hand_back_management(&self, album_id: &str, actor: &str) -> Result<bool, MutationError> {
        let canonical = self
            .canonical_album(album_id)?
            .ok_or(MutationError::NotFound(ALBUM_NOT_FOUND))?;
        takeover::hand_back(self.setup, &canonical, Some(actor)).map_err(|error| match error {
            super::management::models::ManagementError::Conflict(_) => {
                MutationError::Conflict(HAND_BACK_UNREADABLE)
            }
            super::management::models::ManagementError::Busy(cause) => MutationError::Busy(cause),
            other => MutationError::Store(other.to_string()),
        })
    }

    /// Ask for every album the app stepped back from to be handed back.
    /// The management worker does it, one album at a time. Returns how
    /// many albums now wait for their turn. Reads no file.
    pub fn hand_back_all(&self, actor: &str) -> Result<usize, MutationError> {
        self.setup
            .db
            .write_blocking(Lane::Foreground, "mutations.hand_back_all", |tx| {
                Ok(takeover::stamp_all(tx, actor, now_secs())?)
            })
    }

    /// Take `tracks` out of the catalog. Without `recycle` this is one
    /// catalog write. With it, each file is read once here, before the
    /// publish lock, and the removal is published as one bundle: files
    /// already gone from disk are not a failure, and their rows still leave
    /// the catalog.
    fn remove_tracks(
        &self,
        tracks: &[TrackFile],
        recycle: bool,
        removal: &Removal<'_>,
    ) -> Result<(Vec<String>, Recycled), MutationError> {
        if !recycle {
            let changed = self.setup.db.write_blocking(
                Lane::Foreground,
                "mutations.remove_tracks",
                |tx| mark_missing(tx, tracks, removal.reason_code, removal.actor),
            )?;
            tracing::info!(tracks = changed.len(), "library removal");
            return Ok((changed, Vec::new()));
        }
        let registry = self.setup.live_registry();
        let mut targets = Vec::with_capacity(tracks.len());
        for track in tracks {
            let root = registry
                .resolve(&track.root_id)
                .ok_or(MutationError::Conflict(FILE_OUTSIDE_ROOT))?;
            let path = join_inside(&root.path, &track.relative_path)
                .ok_or(MutationError::Conflict(FILE_OUTSIDE_ROOT))?;
            let probe =
                remove::probe(&root.path, &path).map_err(|error| refusal(Refusal::from(error)))?;
            targets.push(Target {
                track_id: track.id.clone(),
                root_id: track.root_id.clone(),
                relative_path: track.relative_path.clone(),
                probe,
                expect_sha256: removal.expect_sha256.map(str::to_owned),
            });
        }
        let done = self
            .setup
            .publish_removal(
                &targets,
                removal.reason_code,
                Some(removal.actor),
                removal.extras.clone(),
                removal.entry,
            )
            .map_err(refusal)?;
        tracing::info!(
            tracks = done.track_ids.len(),
            recycled = done.recycled.len(),
            "library removal"
        );
        Ok((done.track_ids, done.recycled))
    }

    /// The recycle bins now, chosen as the publisher's sessions choose
    /// them. `Err` while a setting they come from does not read: removals
    /// then wait rather than pick another bin.
    fn bins(&self, registry: &RootRegistry) -> Result<Vec<Root>, MutationError> {
        self.setup
            .bins
            .current(registry)
            .map(|choice| choice.roots)
            .map_err(|cause| {
                tracing::debug!(cause, "no recycle bin is known");
                MutationError::Conflict(RECYCLE_BIN_UNKNOWN)
            })
    }
    /// Resolve a local album id (following merges) or a release-group
    /// MBID to the live local album.
    fn canonical_album(&self, album_id: &str) -> Result<Option<String>, MutationError> {
        self.setup
            .db
            .read_blocking("mutations.canonical_album", |conn| {
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
                .map_err(MutationError::from)
            })
    }

    fn album_tracks(&self, album_id: &str) -> Result<Vec<TrackFile>, MutationError> {
        self.setup
            .db
            .read_blocking("mutations.album_tracks", |conn| {
                let mut statement = conn.prepare(
                    "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed' \
                 ORDER BY disc_number, track_number, id",
                )?;
                let rows = statement.query_map([album_id], track_row)?;
                Ok::<_, MutationError>(rows.collect::<rusqlite::Result<_>>()?)
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

/// The folder of the bin that serves library root `root_id`. A root whose
/// `.recycle` failed its checks has none: its removals wait rather than
/// pick another bin.
fn bin_dir(bins: &[Root], root_id: &str) -> Result<PathBuf, MutationError> {
    serving(bins, root_id)
        .map(|bin| bin.dir.clone())
        .ok_or(MutationError::Conflict(NO_RECYCLE_BIN))
}

/// What one removal is, besides its tracks.
struct Removal<'a> {
    reason_code: &'a str,
    actor: &'a str,
    entry: RemovalEntry,
    /// The bytes every file must still hold (a duplicate resolution).
    expect_sha256: Option<&'a str>,
    /// Remove extras' undo record.
    extras: Option<ExtrasAction>,
}

/// A removal's refusal as the page reads it.
fn refusal(refused: Refusal) -> MutationError {
    match refused {
        Refusal::Busy => MutationError::Conflict(LIBRARY_BUSY),
        Refusal::Changed => MutationError::Conflict(REMOVAL_CHANGED),
        Refusal::OutsideRoot => MutationError::Conflict(FILE_OUTSIDE_ROOT),
        Refusal::BinsUnknown(cause) => {
            tracing::debug!(cause, "no recycle bin is known");
            MutationError::Conflict(RECYCLE_BIN_UNKNOWN)
        }
        Refusal::NoBin(root) => {
            tracing::warn!(root, "a removal found no usable recycle bin");
            MutationError::Conflict(NO_RECYCLE_BIN)
        }
        Refusal::Publish { error, kept: true } => {
            tracing::error!(%error, "a removal unwound and kept a file under a hidden name");
            MutationError::Files(RESTORE_FAILED)
        }
        Refusal::Publish { error, kept: false } => match error {
            PublishError::Interrupted(cause) => MutationError::Busy(cause),
            PublishError::Validation(_) | PublishError::Catalog(_) | PublishError::Collision(_) => {
                tracing::info!(%error, "a removal found its files or tracks changed");
                MutationError::Conflict(REMOVAL_CHANGED)
            }
            PublishError::UnsafePath(_) => {
                tracing::warn!(%error, "a removal refused a path");
                MutationError::Conflict(FILE_OUTSIDE_ROOT)
            }
            PublishError::FolderMissing => {
                tracing::warn!("a removal found its library folder missing");
                MutationError::Conflict(LIBRARY_FOLDER_MISSING)
            }
            PublishError::NotLinkable(_) => {
                tracing::warn!(%error, "a removal could not keep a way back");
                MutationError::Files(FILE_NOT_LINKABLE)
            }
            PublishError::NoSafePlacement(_)
            | PublishError::Cleanup(_)
            | PublishError::Space(_)
            | PublishError::Capability(_)
            | PublishError::Snapshot(_)
            | PublishError::Archive(_)
            | PublishError::Io(_) => {
                tracing::warn!(%error, "a removal moved nothing");
                MutationError::Files(RECYCLE_FAILED)
            }
            // The plan already waited out other writes on these tracks,
            // and each removal's hidden names are new, so a journal refusal
            // here is an internal fault.
            PublishError::Journal(_)
            | PublishError::Store(_)
            | PublishError::TimedOut(_)
            | PublishError::InjectedCrash(_) => MutationError::Store(error.to_string()),
        },
    }
}

/// Whether anything sits at `path`, a dangling link included. A path
/// that cannot be read counts as taken.
fn occupied(path: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    )
}

/// Put files back from the recycle bin, newest first, never onto a path
/// something else has taken: the placing call refuses one, and a copy to
/// another disk is verified before the bin copy goes. False when a file
/// could not be put back; it stays in the bin and the error is logged.
fn restore(moved: &[(PathBuf, PathBuf)]) -> bool {
    let mut restored = true;
    for (original, destination) in moved.iter().rev() {
        if occupied(original) {
            restored = false;
            tracing::error!(
                file = %original.file_name().unwrap_or_default().to_string_lossy(),
                "a file sits where a recycled file was; left it in the recycle bin"
            );
            continue;
        }
        match super::publish::paths::put_home(destination, original) {
            Ok(true) => {}
            Ok(false) => {
                restored = false;
                tracing::error!("a recycled file to put back is gone from the recycle bin");
            }
            Err(error) => {
                restored = false;
                tracing::error!(%error, "could not restore a recycled file");
            }
        }
    }
    restored
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
        if remove::mark_removed(
            tx,
            &track.id,
            &track.album_id,
            reason_code,
            Some(actor),
            now,
        )? {
            changed.push(track.id.clone());
        }
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

fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::publish::bin::utc_stamp;

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
