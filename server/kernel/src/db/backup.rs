//! Online backup and restore: verified copies, then rotation.
//!
//! Backup uses the SQLite Online Backup API stepped at 256 pages, matching
//! the v2 capture path: each step holds the source lock briefly and yields
//! between steps, so foreground writes keep their lane while a backup runs.
//! `VACUUM INTO` is rejected as the mechanism on purpose: it holds a shared
//! lock for its whole run and would stall the writer lane for seconds on a
//! large catalog. It is used only on the finished staging copy, which
//! nothing else reads.
//!
//! The flow is a room check, stage into the backups directory, fold the
//! staging WAL into the main image, switch the copy to rollback-journal
//! mode (so a read-only open of a backup never makes `-wal`/`-shm` files),
//! `integrity_check` plus `foreign_key_check` on the staging copy, gzip it,
//! read the stored file back and check it unpacks to the checked bytes,
//! atomic rename into place, then rotate older backups out. One backup runs
//! at a time. Verification always precedes rotation: a failed backup
//! removes nothing.
//!
//! Each backup has a kind: scheduled (the nightly loop), manual ("Back up
//! now") or before an upgrade. Each kind rotates only within itself (see
//! [`RETENTION`]), so a crash-looping upgrade never rotates away the
//! nightly ones. Retention counts the newest days and weeks that hold a
//! backup, never back from today.
//!
//! Restore takes a backup back into an empty directory and refuses occupied
//! targets, version-newer backups (unless downgrade is allowed) and,
//! always, any backup the schema runner would refuse at boot. A plain
//! `.db` backup is checked before anything is copied; a `.db.gz` one is
//! unpacked into the restore's temporary file and checked there.
//!
//! Every file a backup or restore writes is synced before it is renamed or
//! linked into place, and its folder after, so a power cut leaves either
//! the old name or the whole new file. A restore never replaces a database:
//! it places the copy with a hard link, which fails on a name that is
//! taken, and an entry in the target it cannot read counts as taken. The
//! folder syncs open a directory, so this module is for Unix hosts only.
//!
//! A backup carries the undo folder too. The copies a backup's database
//! names are copied into one pool beside the backups, `backups/undo`,
//! shared by every kept backup, so each backup adds only the copies that
//! are new since the last one; after rotation the pool keeps only what a
//! kept backup names, as its manifest lists them (a plain `.db` backup's
//! own database for older ones). A restore puts the copies back first and
//! the database last, so a crash in between never leaves a database naming
//! copies that are not there.
//!
//! A second backups folder, when an admin set one, gets a checked copy of
//! each kept backup and the undo copies it names, in the same layout, and
//! is pruned by the same retention over its own manifests. It is used only
//! while it holds the app's marker and sits on another filesystem. The key
//! file is never copied there.

use std::{
    collections::HashSet,
    ffi::OsString,
    io::{self, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use flate2::{Compression as Level, read::GzDecoder, write::GzEncoder};
use rusqlite::{Connection, OpenFlags, backup::Backup};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    datalock::{self, DataLock},
    error::DbError,
    factory::sqlite_uri_path,
    fs::{reject_remote_filesystem, same_filesystem},
    migrate::{SchemaError, Stamp},
    sql,
    undo_dir::{
        self, Copied, Hold, Stored, UNDO_DIR_NAME, UndoDir, ensure_dir, shard_names, sync_dir,
    },
    writer::CancelFlag,
};
use crate::clock::Clock;

/// Pages copied per backup step. The v2 `pages=256` value, unchanged.
pub const BACKUP_STEP_PAGES: i32 = 256;
/// Pause between steps so live writers slip through.
const BACKUP_STEP_PAUSE: Duration = Duration::from_millis(10);
/// Manifest sidecar suffix: `<backup>.manifest.json`.
pub const MANIFEST_SUFFIX: &str = ".manifest.json";
/// A backup this build writes: `library-<ms>-<seq>.db.gz`.
pub const PACKED_SUFFIX: &str = ".db.gz";
/// A backup from before compression, or one placed by hand.
pub const PLAIN_SUFFIX: &str = ".db";
/// Left in a restore's target until the restored database has its name.
pub const RESTORE_MARKER: &str = ".restore-in-progress";
/// The manifest format this build writes.
const MANIFEST_FORMAT: u32 = 3;
/// A backup dated further than this ahead of the clock is not counted as
/// done, not bucketed and never removed.
pub const AHEAD_TOLERANCE_MS: i64 = 60 * 60 * 1000;
/// Room kept free beyond what a backup needs.
const ROOM_MARGIN_BYTES: u64 = 64 * 1024 * 1024;
const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How many backups of each kind are kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// The newest scheduled backup of each of this many newest UTC days
    /// that hold one.
    pub daily: usize,
    /// The newest scheduled backup of each of this many newest ISO weeks
    /// that hold one.
    pub weekly: usize,
    /// The newest backup taken before an upgrade for each of this many
    /// newest schema versions.
    pub pre_upgrade: usize,
    /// The newest backups taken by hand.
    pub manual: usize,
}

/// What every backups folder keeps. Fixed, not a setting.
pub const RETENTION: Retention = Retention {
    daily: 7,
    weekly: 4,
    pre_upgrade: 3,
    manual: 3,
};

/// Why a backup was taken. A manifest without one, or a backup without a
/// manifest, reads as manual.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum BackupKind {
    /// The nightly loop.
    Scheduled,
    /// A person asked for it ("Back up now").
    #[default]
    Manual,
    /// Taken at boot before a schema upgrade.
    PreUpgrade,
}

/// How a backup file is stored. A manifest without one reads as plain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Compression {
    /// The database file as it is.
    #[default]
    None,
    /// gzip.
    Gzip,
}

/// Why a backup did not run.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The backups folder lacks the room a backup needs. Nothing was
    /// staged and nothing rotated.
    #[error("no room for a backup: it needs {needed} bytes free and {free} are")]
    NoRoom {
        /// Bytes the backup needs free.
        needed: u64,
        /// Bytes free.
        free: u64,
    },
    /// Another backup is running. Nothing was done.
    #[error("another backup is running")]
    Running,
    /// The backup itself failed; [`DbError::IntegrityFailed`] means the
    /// live database failed its check.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// Progress callback: `(copied_pages, total_pages)`.
pub type BackupProgress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// Recorded with every backup: identity, size, kind and schema stamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    /// Format marker for future readers.
    pub format_version: u32,
    /// Hex SHA-256 of the stored backup file: for a `.db.gz`, of its
    /// compressed bytes.
    pub sha256: String,
    /// Size of the stored backup file in bytes.
    pub size_bytes: u64,
    /// `user_version` stamped in the backup file.
    pub user_version: i64,
    /// When the backup was taken.
    pub created_at: SystemTime,
    /// Why it was taken.
    #[serde(default)]
    pub kind: BackupKind,
    /// How the file is stored.
    #[serde(default)]
    pub compression: Compression,
    /// Hex SHA-256 of the database the stored file holds once unpacked:
    /// the bytes the integrity check passed. Absent before format 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_sha256: Option<String>,
    /// That database's size in bytes. Absent before format 3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_bytes: Option<u64>,
    /// Undo copies the backup holds in the pool.
    #[serde(default)]
    pub undo_blobs: u64,
    /// Their size in bytes.
    #[serde(default)]
    pub undo_bytes: u64,
    /// Copies the database named that were already missing or damaged
    /// when the backup was taken.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub undo_missing: Vec<String>,
    /// Every undo copy the database names, the missing ones included. The
    /// pool keeps what a kept backup lists here. Absent before format 3,
    /// whose plain `.db` backups are read for their names instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_named: Option<Vec<String>>,
}

/// What one backup run produced.
#[derive(Debug, Clone)]
pub struct BackupReport {
    /// Final backup file path.
    pub path: PathBuf,
    /// Manifest written beside it.
    pub manifest: BackupManifest,
    /// How long the run took.
    pub duration: Duration,
}

/// What one restore produced.
#[derive(Debug, Clone)]
pub struct RestoredBackup {
    /// Restored database file path.
    pub path: PathBuf,
    /// Bytes copied.
    pub bytes: u64,
    /// `user_version` stamped in the restored file.
    pub user_version: i64,
    /// Undo copies put back.
    pub undo_blobs: u64,
    /// Undo copies the database names that the backup did not hold.
    pub undo_missing: Vec<String>,
    /// How the database took its name.
    pub placement: Placement,
}

/// How a restored database took its name, none of which replaces a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    /// A hard link from the synced copy.
    HardLink,
    /// The filesystem has no hard links: a rename that refuses a taken
    /// name (`renameat2` with `RENAME_NOREPLACE`).
    RenameNoReplace,
    /// The filesystem has neither: a plain rename, made only after the
    /// name was found free under the restore's exclusive lock.
    RenameAfterCheck,
}

/// What the `-wal` file beside a database holds.
#[derive(Debug)]
pub enum WalState {
    /// There is none.
    Absent,
    /// It is there and 0 bytes long.
    Empty,
    /// It holds this many bytes, which a read of the main file alone
    /// would leave out.
    HoldsData(u64),
    /// Its metadata cannot be read (any error but not found), so what it
    /// holds is unknown.
    Unreadable(io::Error),
}

/// The `-wal` file beside a database.
#[must_use]
pub fn wal_path(db_path: &Path) -> PathBuf {
    let mut name = db_path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// What the `-wal` file beside `db_path` holds. Only not-found counts as
/// absent and only length 0 as empty: a file that cannot be read is never
/// taken for an empty one.
#[must_use]
pub fn wal_state(db_path: &Path) -> WalState {
    match std::fs::metadata(wal_path(db_path)) {
        Ok(meta) if meta.len() == 0 => WalState::Empty,
        Ok(meta) => WalState::HoldsData(meta.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => WalState::Absent,
        Err(error) => WalState::Unreadable(error),
    }
}

/// Backup and restore against one live database file.
pub struct BackupService {
    db_path: PathBuf,
    backup_dir: PathBuf,
    clock: Arc<dyn Clock>,
    staging_seq: AtomicU64,
    live: Mutex<HashSet<PathBuf>>,
    database_in_memory: bool,
    /// Held by the one backup that runs, until its last file step ends.
    running: Arc<AtomicBool>,
    /// Free bytes on the filesystem holding a path.
    free_space: fn(&Path) -> io::Result<u64>,
}

impl std::fmt::Debug for BackupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupService")
            .field("db_path", &self.db_path)
            .field("backup_dir", &self.backup_dir)
            .finish_non_exhaustive()
    }
}

/// The one backup's turn: taken before it starts, given back when the last
/// step that holds it ends, even when its caller stopped waiting.
struct Turn(Arc<AtomicBool>);

impl Turn {
    fn take(running: &Arc<AtomicBool>) -> Option<Self> {
        running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| Self(Arc::clone(running)))
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl BackupService {
    /// Wire the service; `clock` dates and names each backup. The
    /// directory is created on the first backup. `database_in_memory` is
    /// what the boot check found: the database sits on a filesystem held in
    /// memory, so it and these backups are lost on restart.
    pub fn new(
        db_path: &Path,
        backup_dir: &Path,
        clock: Arc<dyn Clock>,
        database_in_memory: bool,
    ) -> Self {
        Self {
            db_path: db_path.to_owned(),
            backup_dir: backup_dir.to_owned(),
            clock,
            staging_seq: AtomicU64::new(0),
            live: Mutex::new(HashSet::new()),
            database_in_memory,
            running: Arc::new(AtomicBool::new(false)),
            free_space: crate::disk::free_bytes,
        }
    }

    /// Read free space through `probe` instead of the filesystem, so a test
    /// can pin what the room check sees.
    #[must_use]
    pub fn with_space_probe(mut self, probe: fn(&Path) -> io::Result<u64>) -> Self {
        self.free_space = probe;
        self
    }

    /// Now, in milliseconds, by the service's clock.
    pub fn now_ms(&self) -> i64 {
        self.clock.now_ms()
    }

    /// Now, as wall-clock milliseconds, by the service's clock.
    fn now_millis(&self) -> u64 {
        u64::try_from(self.clock.now_ms()).unwrap_or(0)
    }

    /// Backups directory this service rotates within.
    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// True when the database sits on a filesystem held in memory, as the
    /// boot check found.
    pub fn database_in_memory(&self) -> bool {
        self.database_in_memory
    }

    /// When the newest scheduled backup in the folder was taken, by its
    /// name, in milliseconds. One dated more than [`AHEAD_TOLERANCE_MS`]
    /// ahead of the clock does not count. Blocking.
    pub fn newest_scheduled_at(&self) -> Result<Option<i64>, DbError> {
        let entries = match listed_backups(&self.backup_dir) {
            Ok(entries) => entries,
            Err(DbError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let ahead = self.clock.now_ms().saturating_add(AHEAD_TOLERANCE_MS);
        Ok(entries
            .iter()
            .filter(|entry| entry.kind == BackupKind::Scheduled && entry.millis <= ahead)
            .map(|entry| entry.millis)
            .max())
    }

    /// Copy the live database out: check for room, sweep stale staging,
    /// stage, verify, copy the undo copies it names, compress, rename,
    /// rotate. One runs at a time: another answers
    /// [`BackupError::Running`]. The progress callback fires after every
    /// step with copied and total page counts. Blocking SQLite and file
    /// work runs off the async runtime. A tripped `cancel` stops the copy
    /// between steps and removes the staging file; nothing renames or
    /// rotates.
    pub async fn backup(
        &self,
        kind: BackupKind,
        progress: Option<BackupProgress>,
        cancel: &CancelFlag,
    ) -> Result<BackupReport, BackupError> {
        let turn = Turn::take(&self.running).ok_or(BackupError::Running)?;
        ensure_dir(&self.backup_dir).map_err(DbError::from)?;
        if !same_filesystem(&self.db_path, &self.backup_dir).map_err(DbError::from)? {
            return Err(DbError::StagingCrossFilesystem {
                path: self.backup_dir.clone(),
            }
            .into());
        }
        self.sweep_stale_staging();
        self.check_room().await?;
        let (staging, seq) = self.staging_path();
        self.mark_live(&staging);
        let outcome = self
            .backup_inner(turn, kind, &staging, seq, progress, cancel)
            .await
            .inspect_err(|_| {
                remove_staging_tree(&staging);
            });
        self.unmark_live(&staging);
        Ok(outcome?)
    }

    /// Refuse a backup the backups folder has no room for: 2.5 times the
    /// database's pages (the staging copy, its compacted copy and the
    /// compressed file), the undo copies the pool lacks, and a margin. A
    /// free space that cannot be read lets the backup go ahead.
    async fn check_room(&self) -> Result<(), BackupError> {
        let db_path = self.db_path.clone();
        let backup_dir = self.backup_dir.clone();
        let free_space = self.free_space;
        tokio::task::spawn_blocking(move || {
            let needed = room_needed(&db_path, &backup_dir)?;
            match free_space(&backup_dir) {
                Ok(free) if free < needed => Err(BackupError::NoRoom { needed, free }),
                Ok(_) => Ok(()),
                Err(error) => {
                    tracing::warn!(%error, "free space in the backups folder cannot be read; backing up anyway");
                    Ok(())
                }
            }
        })
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "backup".to_owned(),
            cause: "room check task failed to join".to_owned(),
        })?
    }

    /// Stage, verify, compress, rename, and rotate one backup into its
    /// final name. `turn` is held until the last file step ends.
    async fn backup_inner(
        &self,
        turn: Turn,
        kind: BackupKind,
        staging: &Path,
        seq: u64,
        progress: Option<BackupProgress>,
        cancel: &CancelFlag,
    ) -> Result<BackupReport, DbError> {
        let source_path = self.db_path.clone();
        let now_ms = self.now_millis();
        let created_at = UNIX_EPOCH + Duration::from_millis(now_ms);
        let final_name = backup_file_name(created_at, seq);
        let final_path = self.backup_dir.join(&final_name);
        let started = Instant::now();
        // Held from before the copy until the undo copies it names are in
        // the pool: no sweep removes one in between, and no other backup
        // cleans the pool under this one.
        let live = UndoDir::new(undo_dir::beside(&self.db_path));
        let pool = UndoDir::new(self.backup_dir.join(UNDO_DIR_NAME));
        let holds = {
            let (live, pool) = (live.clone(), pool.clone());
            tokio::task::spawn_blocking(move || {
                Ok::<_, DbError>((live.hold_shared()?, pool.hold_exclusive()?))
            })
            .await
            .map_err(|_| DbError::WriteFailed {
                operation: "backup".to_owned(),
                cause: "undo folder lock task failed to join".to_owned(),
            })??
        };
        let staging_for_copy = staging.to_owned();
        let cancel_for_copy = cancel.clone();
        let (copied, turn) = tokio::task::spawn_blocking(move || {
            let copied = copy_live(&source_path, &staging_for_copy, progress, &cancel_for_copy);
            (copied, turn)
        })
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "backup".to_owned(),
            cause: "backup task failed to join".to_owned(),
        })?;
        let copied = copied?;
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled {
                operation: "backup".to_owned(),
                chunks: copied as usize,
            });
        }
        // Verify, hash, copy the undo copies, compress, publish and rotate
        // are file and SQLite work: keep them off the async workers like
        // the copy itself.
        let staging = staging.to_owned();
        let backup_dir = self.backup_dir.clone();
        let cancel = cancel.clone();
        let (path, manifest) = tokio::task::spawn_blocking(move || {
            let _turn = turn;
            let (_live_hold, pool_hold) = holds;
            fold_staging(&staging)?;
            compact_staging(&staging)?;
            leave_wal(&staging)?;
            let checked = verify_staging(&staging)?;
            let undo = copy_undo(&staging, &live, &pool, &pool_hold, &cancel)?;
            remove_staging_sidecars(&staging);
            let packed = pack_staging(&staging, &checked.sha256)?;
            let manifest = BackupManifest {
                format_version: MANIFEST_FORMAT,
                sha256: packed.sha256,
                size_bytes: packed.size_bytes,
                user_version: checked.user_version,
                created_at,
                kind,
                compression: Compression::Gzip,
                db_sha256: Some(checked.sha256),
                db_bytes: Some(checked.size_bytes),
                undo_blobs: undo.blobs,
                undo_bytes: undo.bytes,
                undo_missing: undo.missing,
                undo_named: Some(undo.named),
            };
            // The manifest takes its name first and the backup last: a
            // finished backup always has its manifest beside it.
            write_synced(
                &manifest_path(&staging),
                &manifest_path(&final_path),
                serde_json::to_string_pretty(&manifest)?.as_bytes(),
            )?;
            sync_dir(&backup_dir)?;
            if let Err(error) = std::fs::rename(&packed.path, &final_path) {
                let _ = std::fs::remove_file(manifest_path(&final_path));
                return Err(error.into());
            }
            sync_dir(&backup_dir)?;
            // The checked copy lives on in the compressed file.
            if let Err(error) = std::fs::remove_file(&staging) {
                tracing::debug!(%error, "a backup's staging copy stays for the next sweep");
            }
            if let Err(error) = rotate_backups(&backup_dir, now_ms as i64, RETENTION) {
                tracing::warn!(%error, "older backups stay until the next backup rotates them");
            }
            if let Err(error) = clean_pool(&backup_dir, &pool, &pool_hold) {
                tracing::warn!(
                    %error,
                    "undo copies no backup names stay in the pool until the next backup"
                );
            }
            Ok::<_, DbError>((final_path, manifest))
        })
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "backup".to_owned(),
            cause: "backup verify task failed to join".to_owned(),
        })??;
        Ok(BackupReport {
            path,
            manifest,
            duration: started.elapsed(),
        })
    }

    /// Restore a backup file into an empty directory as `db_name`, with
    /// the undo copies it names beside it in `undo`.
    ///
    /// Refuses a backup with writes in its `-wal` file (the copy takes the
    /// main file only), occupied targets, targets on a network filesystem
    /// and, unless `allow_downgrade`, backups newer than this binary. A
    /// backup the schema runner would refuse at any binary (an older
    /// pre-release stamp, or tables with no stamp) is always refused. A
    /// plain backup is checked before anything is written; a gzip one is
    /// unpacked into the restore's temporary file and checked there, under
    /// the data lock, and a refusal clears what it wrote. The target is
    /// empty when it holds nothing but the restore's locks
    /// (`<db_name>.lock`, `<db_name>.server.lock`); one holding only what
    /// an interrupted restore left (its marker, its temporary files,
    /// `undo`, and no `db_name`) is cleared and the restore starts over.
    /// An entry it cannot read counts as occupied. The exclusive data lock
    /// is held for the run, so a server on the target refuses to start and
    /// a running one makes the restore refuse. A copy the backup lacks does
    /// not stop the restore: it is listed in `undo_missing`, and Undo then
    /// skips only the files that need it. The restored database passes
    /// `integrity_check` before it takes its name.
    pub fn restore(
        &self,
        backup_db: &Path,
        target_dir: &Path,
        db_name: &str,
        allow_downgrade: bool,
    ) -> Result<RestoredBackup, DbError> {
        let packed = is_gzip(backup_db)?;
        if !packed {
            match wal_state(backup_db) {
                WalState::Absent | WalState::Empty => {}
                WalState::HoldsData(_) => {
                    return Err(DbError::BackupWalPresent {
                        path: wal_path(backup_db),
                    });
                }
                WalState::Unreadable(reason) => {
                    return Err(DbError::Unreadable {
                        path: wal_path(backup_db),
                        reason,
                    });
                }
            }
            let source = open_immutable(backup_db)?;
            check_schema(&source, allow_downgrade)?;
            check_clean(&source)?;
            drop(source);
        }

        ensure_dir(target_dir)?;
        let target = target_dir.join(db_name);
        reject_remote_filesystem(&target)?;
        let lock = DataLock::exclusive(&target)?;
        clear_unfinished_restore(target_dir, db_name)?;
        let marker = target_dir.join(RESTORE_MARKER);
        std::fs::File::create(&marker)?.sync_all()?;
        sync_dir(target_dir)?;
        let seq = self.staging_seq.fetch_add(1, Ordering::Relaxed);
        let stem = format!(
            ".restore-{}-{}-{}",
            std::process::id(),
            self.now_millis(),
            seq
        );
        let temp = target_dir.join(format!("{stem}.tmp"));
        let source = Source {
            path: backup_db,
            packed,
            allow_downgrade,
        };
        match self.restore_files(&source, target_dir, &stem, &temp, &target, &lock) {
            Ok(restored) => {
                std::fs::remove_file(&marker)?;
                sync_dir(target_dir)?;
                Ok(restored)
            }
            Err(error) => {
                // Removed even when a database by the name turned up, so
                // nothing of this run is left for a retry to trip on.
                if let Err(cleanup) = std::fs::remove_file(&temp)
                    && cleanup.kind() != io::ErrorKind::NotFound
                {
                    tracing::warn!(%cleanup, "a failed restore left its copy in its target");
                }
                if let Err(cleanup) = clear_unfinished_restore(target_dir, db_name) {
                    tracing::warn!(%cleanup, "a failed restore left files in its target");
                }
                Err(error)
            }
        }
    }

    /// The copying half of [`restore`](Self::restore), under its marker:
    /// the database to `temp` (unpacked when the backup is gzip, and then
    /// checked against the schema runner), the undo copies into a
    /// temporary folder, then the folder and last the database to their
    /// names.
    fn restore_files(
        &self,
        source: &Source<'_>,
        target_dir: &Path,
        stem: &str,
        temp: &Path,
        target: &Path,
        lock: &DataLock,
    ) -> Result<RestoredBackup, DbError> {
        let backup_db = source.path;
        let bytes = if source.packed {
            unpack_to(backup_db, temp)?
        } else {
            std::fs::copy(backup_db, temp)?
        };
        std::fs::File::open(temp)?.sync_all()?;
        let (user_version, named) = {
            let restored = open_immutable(temp)?;
            if source.packed {
                check_schema(&restored, source.allow_downgrade)?;
            }
            check_clean(&restored)?;
            (user_version(&restored)?, named_in(&restored)?)
        };

        let known_missing: HashSet<String> = read_manifest(backup_db)
            .map(|manifest| manifest.undo_missing.into_iter().collect())
            .unwrap_or_default();
        let staged = UndoDir::new(target_dir.join(format!("{stem}-undo")));
        let staged_hold = staged.hold_shared()?;
        let pool = UndoDir::new(
            backup_db
                .parent()
                .filter(|dir| !dir.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."))
                .join(UNDO_DIR_NAME),
        );
        // The shared hold only keeps a backup's clean-up off the pool while
        // it is read. A pool that cannot be locked (a read-only backups
        // folder) is read without it: a restore always goes ahead.
        let pool_hold = match pool.path().is_dir().then(|| pool.hold_shared()) {
            Some(Err(error)) => {
                tracing::warn!(%error, "the undo copies beside the backup are read without their lock");
                None
            }
            hold => hold.and_then(Result::ok),
        };
        let mut undo_blobs = 0;
        let mut undo_missing = Vec::new();
        for sha256 in named {
            // A copy the pool cannot give (gone, damaged or unreadable) is
            // listed, never a failed restore; only writing the target fails.
            let found = if known_missing.contains(&sha256) {
                None
            } else {
                match pool.read(&sha256) {
                    Ok(Stored::Found(bytes)) => Some(bytes),
                    Ok(Stored::Missing | Stored::Corrupt) => None,
                    Err(error) => {
                        tracing::warn!(blob = %sha256, %error, "an undo copy beside the backup cannot be read");
                        None
                    }
                }
            };
            match found {
                Some(bytes) => {
                    staged.put(&staged_hold, &bytes)?;
                    undo_blobs += 1;
                }
                None => {
                    tracing::warn!(
                        blob = %sha256,
                        "the backup lacks an undo copy its database names; undo skips the files that need it"
                    );
                    undo_missing.push(sha256);
                }
            }
        }
        drop(pool_hold);
        drop(staged_hold);
        // A rename never replaces a folder that holds anything.
        std::fs::rename(staged.path(), target_dir.join(UNDO_DIR_NAME))?;
        sync_dir(target_dir)?;
        let placement = place_new(temp, target, lock)?;
        sync_dir(target_dir)?;
        // The database is whole under its name now, so a temporary name
        // left behind is only litter.
        if placement == Placement::HardLink
            && let Err(error) = std::fs::remove_file(temp)
        {
            tracing::warn!(%error, "the restore's temporary copy stays beside the database");
        }
        Ok(RestoredBackup {
            path: target.to_owned(),
            bytes,
            user_version,
            undo_blobs,
            undo_missing,
            placement,
        })
    }

    /// Unpack a gzip backup in this service's folder for a read-only check.
    /// The copy is a `.restore-*.tmp` file marked live, so a backup's sweep
    /// leaves it, and it is removed when the returned value is dropped.
    /// Blocking.
    pub fn unpack_for_check(&self, backup: &Path) -> Result<CheckCopy<'_>, DbError> {
        let seq = self.staging_seq.fetch_add(1, Ordering::Relaxed);
        let path = self.backup_dir.join(format!(
            ".restore-check-{}-{}-{}.tmp",
            std::process::id(),
            self.now_millis(),
            seq
        ));
        self.mark_live(&path);
        let copy = CheckCopy {
            service: self,
            path,
        };
        unpack_to(backup, &copy.path)?;
        Ok(copy)
    }

    /// Unique staging path inside the backups directory, with its sequence
    /// number for the final name.
    fn staging_path(&self) -> (PathBuf, u64) {
        let seq = self.staging_seq.fetch_add(1, Ordering::Relaxed);
        let path = self.backup_dir.join(format!(
            ".staging-{}-{}-{}.db",
            std::process::id(),
            self.now_millis(),
            seq
        ));
        (path, seq)
    }

    /// Remove crashed-run leftovers: staging and restore temp files no live
    /// backup or check owns. The live set is read after the folder is
    /// listed: a check marks its file live before it creates it, so any
    /// file the listing holds that a check owns is in the set. A poisoned
    /// lock skips the sweep rather than blocking.
    fn sweep_stale_staging(&self) {
        let entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&self.backup_dir) {
            Ok(entries) => entries.flatten().collect(),
            Err(_) => return,
        };
        let Ok(live) = self.live.lock().map(|guard| guard.clone()) else {
            return;
        };
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let leftover = name.starts_with(".staging-")
                || (name.starts_with(".restore-") && name.ends_with(".tmp"));
            // A live run's sidecars, compact and compressed copies and
            // manifest share its staging name as a prefix.
            let owned = live.iter().any(|path| {
                path.file_name()
                    .is_some_and(|live_name| name.starts_with(&*live_name.to_string_lossy()))
            });
            if !leftover || owned {
                continue;
            }
            if let Err(error) = std::fs::remove_file(entry.path()) {
                tracing::debug!(
                    file = name,
                    %error,
                    "stale backup staging survived the sweep and stays for the next run"
                );
            }
        }
    }

    /// Track a staging file as live so a concurrent sweep keeps it.
    fn mark_live(&self, staging: &Path) {
        if let Ok(mut live) = self.live.lock() {
            live.insert(staging.to_owned());
        }
    }

    /// Drop a staging file from the live set once it renamed or cleaned up.
    fn unmark_live(&self, staging: &Path) {
        if let Ok(mut live) = self.live.lock() {
            live.remove(staging);
        }
    }
}

/// The backup a restore reads.
struct Source<'a> {
    path: &'a Path,
    /// A gzip file, unpacked into the restore's temporary file.
    packed: bool,
    allow_downgrade: bool,
}

/// A gzip backup unpacked for a check, removed when dropped.
pub struct CheckCopy<'a> {
    service: &'a BackupService,
    path: PathBuf,
}

impl CheckCopy<'_> {
    /// The unpacked database.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for CheckCopy<'_> {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(%error, "a backup check's copy stays for the next sweep");
        }
        remove_staging_sidecars(&self.path);
        self.service.unmark_live(&self.path);
    }
}

/// The file that marks a second backups folder as the app's own. Only an
/// admin's save writes it, and only into an empty folder: an unmounted
/// share is an empty folder on the host's own disk, so the copy never
/// writes into one by itself.
pub const SECOND_FOLDER_MARKER: &str = ".droppedneedle-backups";

/// Why a second backups folder is not used.
#[derive(Debug, thiserror::Error)]
pub enum SecondFolderError {
    /// It is not there, is not a folder, or does not hold the marker.
    #[error("the second backups folder is not there or not marked as the app's")]
    Missing,
    /// It sits on the same filesystem as the backups folder, where a copy
    /// protects nothing.
    #[error("the second backups folder is on the same disk as the backups folder")]
    SameDisk,
    /// It holds files and no marker, so it may be someone else's folder.
    #[error("the second backups folder holds files and is not marked as the app's")]
    NotEmpty,
    /// The server cannot write in it.
    #[error("the second backups folder cannot be written")]
    ReadOnly,
    /// It lacks the room the copies need. Nothing was copied.
    #[error(
        "no room in the second backups folder: the copies need {needed} bytes and {free} are free"
    )]
    NoRoom {
        /// Bytes the copies need.
        needed: u64,
        /// Bytes free.
        free: u64,
    },
    /// A backup is running; the next pass copies.
    #[error("a backup is running")]
    Running,
    /// Reading or writing failed.
    #[error(transparent)]
    Db(#[from] DbError),
}

/// What one copy to the second folder did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SecondFolderCopy {
    /// Backups copied.
    pub copied: usize,
}

/// Check `folder` for an admin's save of it as the second backups folder
/// of `backup_dir`: it is a folder on another filesystem, the server can
/// write in it, and it is empty or already marked. Writes the marker into
/// an empty one. Answers its free bytes. Blocking.
pub fn mark_second_folder(backup_dir: &Path, folder: &Path) -> Result<u64, SecondFolderError> {
    if !std::fs::metadata(folder).is_ok_and(|meta| meta.is_dir()) {
        return Err(SecondFolderError::Missing);
    }
    ensure_dir(backup_dir).map_err(DbError::from)?;
    if same_filesystem(backup_dir, folder).map_err(DbError::from)? {
        return Err(SecondFolderError::SameDisk);
    }
    if is_marked(folder) {
        if !writable(folder) {
            return Err(SecondFolderError::ReadOnly);
        }
    } else {
        let holds_files = std::fs::read_dir(folder)
            .map_err(DbError::from)?
            .flatten()
            .any(|entry| !NAS_FOLDERS.contains(&&*entry.file_name().to_string_lossy()));
        if holds_files {
            return Err(SecondFolderError::NotEmpty);
        }
        let written = std::fs::File::create_new(folder.join(SECOND_FOLDER_MARKER))
            .and_then(|file| file.sync_all());
        if let Err(error) = written {
            return Err(match error.kind() {
                io::ErrorKind::PermissionDenied | io::ErrorKind::ReadOnlyFilesystem => {
                    SecondFolderError::ReadOnly
                }
                _ => DbError::from(error).into(),
            });
        }
        sync_dir(folder).map_err(DbError::from)?;
    }
    Ok(crate::disk::free_bytes(folder).map_err(DbError::from)?)
}

impl BackupService {
    /// Copy every backup in the backups folder that `folder` lacks and
    /// would keep, with the undo copies it names that `folder` lacks, then
    /// remove from `folder` what its own manifests say retention does not
    /// keep. The folder mirrors the backups folder's layout (`undo` beside
    /// the backups), so a restore reads from it as from the first. Each
    /// file goes to a temporary name, is synced and checked against its
    /// manifest's SHA-256, then renamed. Used only while `folder` holds the
    /// marker and sits on another filesystem. The key file is never copied.
    /// Blocking.
    pub fn copy_to_second(&self, folder: &Path) -> Result<SecondFolderCopy, SecondFolderError> {
        let _turn = Turn::take(&self.running).ok_or(SecondFolderError::Running)?;
        if !is_marked(folder) {
            return Err(SecondFolderError::Missing);
        }
        if same_filesystem(&self.backup_dir, folder).map_err(DbError::from)? {
            return Err(SecondFolderError::SameDisk);
        }
        sweep_copy_temps(folder);
        let now_ms = self.clock.now_ms();
        let here = listed_backups(&self.backup_dir)?;
        let there = listed_backups(folder)?;
        let present: HashSet<OsString> = there
            .iter()
            .filter_map(|entry| entry.path.file_name().map(ToOwned::to_owned))
            .collect();
        // Weighed together with what the folder holds, so a backup its own
        // retention would remove at once is never copied in.
        let candidates: Vec<(Listed, BackupManifest)> = here
            .into_iter()
            .filter(|entry| is_packed_name(&entry.path))
            .filter(|entry| {
                entry
                    .path
                    .file_name()
                    .is_some_and(|name| !present.contains(name))
            })
            .filter_map(|entry| read_manifest(&entry.path).map(|manifest| (entry, manifest)))
            .collect();
        let mut weighed: Vec<Listed> = there
            .into_iter()
            .chain(candidates.iter().map(|(entry, _)| entry.clone()))
            .collect();
        weighed.sort_by_cached_key(|entry| backup_order(&entry.path).0);
        let keep = kept(&weighed, now_ms, RETENTION);
        let kept_names: HashSet<OsString> = weighed
            .iter()
            .zip(keep)
            .filter(|(_, keep)| *keep)
            .filter_map(|(entry, _)| entry.path.file_name().map(ToOwned::to_owned))
            .collect();
        let wanted: Vec<&(Listed, BackupManifest)> = candidates
            .iter()
            .filter(|(entry, _)| {
                entry
                    .path
                    .file_name()
                    .is_some_and(|name| kept_names.contains(name))
            })
            .collect();

        let pool = UndoDir::new(self.backup_dir.join(UNDO_DIR_NAME));
        let second_pool = UndoDir::new(folder.join(UNDO_DIR_NAME));
        let pool_hold = pool.hold_shared().map_err(DbError::from)?;
        let second_hold = second_pool.hold_exclusive().map_err(DbError::from)?;
        let mut undo_needed: Vec<String> = Vec::new();
        let mut needed = ROOM_MARGIN_BYTES;
        for (_, manifest) in &wanted {
            needed = needed.saturating_add(manifest.size_bytes);
            for sha256 in manifest.undo_named.iter().flatten() {
                if !second_pool.contains(sha256) && !undo_needed.contains(sha256) {
                    needed = needed.saturating_add(pool.size_of(sha256).unwrap_or(0));
                    undo_needed.push(sha256.clone());
                }
            }
        }
        if !wanted.is_empty() {
            let free = (self.free_space)(folder).map_err(DbError::from)?;
            if free < needed {
                return Err(SecondFolderError::NoRoom { needed, free });
            }
        }

        let mut copied = SecondFolderCopy::default();
        for (entry, manifest) in wanted {
            for sha256 in manifest.undo_named.iter().flatten() {
                match pool
                    .copy_to(&second_pool, &second_hold, sha256)
                    .map_err(DbError::from)?
                {
                    Copied::Copied(_) | Copied::Already(_) => {}
                    Copied::Missing | Copied::Corrupt => {
                        tracing::debug!(blob = %sha256, "an undo copy a backup names is not in the pool, so the second folder lacks it too");
                    }
                }
            }
            copy_checked(&entry.path, folder, manifest)?;
            copied.copied += 1;
        }
        drop(pool_hold);
        if let Err(error) = rotate_backups(folder, now_ms, RETENTION) {
            tracing::warn!(%error, "older backups in the second folder stay until the next copy");
        }
        if let Err(error) = clean_pool(folder, &second_pool, &second_hold) {
            tracing::warn!(%error, "undo copies in the second folder no backup there names stay until the next copy");
        }
        Ok(copied)
    }
}

/// Folders a filesystem or NAS keeps in a share of its own accord. A
/// second folder holding only these counts as empty.
const NAS_FOLDERS: [&str; 3] = ["lost+found", "@eaDir", "#recycle"];

/// True when `folder` holds the second-folder marker.
fn is_marked(folder: &Path) -> bool {
    std::fs::symlink_metadata(folder.join(SECOND_FOLDER_MARKER)).is_ok_and(|meta| meta.is_file())
}

/// True when the server can write in `folder`: a file made there and
/// removed again.
fn writable(folder: &Path) -> bool {
    let probe = folder.join(format!(".droppedneedle-write-{}", std::process::id()));
    let made = std::fs::File::create_new(&probe).is_ok();
    let _ = std::fs::remove_file(&probe);
    made
}

/// The temporary name a copy into the second folder takes.
const COPY_TEMP_PREFIX: &str = ".copy-";

/// Remove what a copy into the second folder left mid-write.
fn sweep_copy_temps(folder: &Path) {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(COPY_TEMP_PREFIX)
            && name.ends_with(".tmp")
            && let Err(error) = std::fs::remove_file(entry.path())
        {
            tracing::debug!(file = name, %error, "a copy's leftover stays for the next pass");
        }
    }
}

/// Copy one backup and its manifest into `folder`: each to a temporary
/// name, synced, the backup checked against the manifest's SHA-256 and
/// size, then renamed, the manifest first.
fn copy_checked(backup: &Path, folder: &Path, manifest: &BackupManifest) -> Result<(), DbError> {
    let Some(name) = backup.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let target = folder.join(name);
    let temp = folder.join(format!("{COPY_TEMP_PREFIX}{name}.tmp"));
    let manifest_temp = folder.join(format!("{COPY_TEMP_PREFIX}{name}{MANIFEST_SUFFIX}.tmp"));
    let copied = (|| -> Result<(), DbError> {
        let mut source = std::fs::File::open(backup)?;
        let mut written = create_fresh(&temp)?;
        io::copy(&mut source, &mut written)?;
        written.sync_all()?;
        drop(written);
        let (sha256, size_bytes) = file_sha256(&temp)?;
        if sha256 != manifest.sha256 || size_bytes != manifest.size_bytes {
            return Err(DbError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "the copy in the second folder does not match its manifest",
            )));
        }
        write_synced(
            &manifest_temp,
            &manifest_path(&target),
            serde_json::to_string_pretty(manifest)?.as_bytes(),
        )?;
        sync_dir(folder)?;
        std::fs::rename(&temp, &target)?;
        sync_dir(folder)?;
        Ok(())
    })();
    if copied.is_err() {
        let _ = std::fs::remove_file(&temp);
        let _ = std::fs::remove_file(&manifest_temp);
    }
    copied
}
/// The undo copies a database file names that live in the undo folder
/// rather than in its own table. A database from before the undo tables
/// names none. The file is opened read-only and immutable, so nothing is
/// written beside it.
pub fn named_undo_copies(path: &Path) -> Result<Vec<String>, DbError> {
    named_in(&open_immutable(path)?)
}

/// The undo copies a kept backup names: a gzip backup's from its
/// manifest, a plain one's from its database. A gzip backup whose manifest
/// is missing, unreadable or lists none fails, so nothing is guessed.
pub fn backup_undo_names(backup: &Path) -> Result<Vec<String>, DbError> {
    if !is_packed_name(backup) {
        return named_undo_copies(backup);
    }
    let manifest: BackupManifest = serde_json::from_slice(&std::fs::read(manifest_path(backup))?)?;
    manifest.undo_named.ok_or_else(|| DbError::Unreadable {
        path: manifest_path(backup),
        reason: io::Error::new(
            io::ErrorKind::InvalidData,
            "the manifest does not list the undo copies",
        ),
    })
}

fn named_in(connection: &Connection) -> Result<Vec<String>, DbError> {
    let tables: i64 =
        connection.query_row(sql::UNDO_TABLES_PRESENT.text(), [], |row| row.get(0))?;
    if tables < 2 {
        return Ok(Vec::new());
    }
    let mut stmt = connection.prepare(sql::UNDO_NAMED_IN_FOLDER.text())?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Open a file nothing writes any more: read-only and immutable, so no
/// sidecar is made beside it.
pub fn open_immutable(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        format!("file:{}?immutable=1", sqlite_uri_path(path)),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// The bytes a backup needs free in the backups folder: 2.5 times the
/// live database's pages, the undo copies it names that the pool lacks,
/// and a margin.
fn room_needed(db_path: &Path, backup_dir: &Path) -> Result<u64, DbError> {
    let source = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let pages: i64 = source.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    let page_size: i64 = source.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let named = named_in(&source)?;
    drop(source);
    let page_bytes = u64::try_from(pages.saturating_mul(page_size)).unwrap_or(0);
    let live = UndoDir::new(undo_dir::beside(db_path));
    let pool = UndoDir::new(backup_dir.join(UNDO_DIR_NAME));
    let undo: u64 = named
        .iter()
        .filter(|sha256| !pool.contains(sha256))
        .filter_map(|sha256| live.size_of(sha256))
        .sum();
    Ok(page_bytes
        .saturating_mul(5)
        .div_ceil(2)
        .saturating_add(undo)
        .saturating_add(ROOM_MARGIN_BYTES))
}

/// What a backup's copy of the undo folder holds.
#[derive(Debug, Default)]
struct UndoCopied {
    blobs: u64,
    bytes: u64,
    missing: Vec<String>,
    /// Every copy the database names.
    named: Vec<String>,
}

/// Copy every undo copy the staged database names into the pool. One
/// missing or damaged in the live folder is listed, not fatal: the
/// database matters more than one undo.
fn copy_undo(
    staging: &Path,
    live: &UndoDir,
    pool: &UndoDir,
    pool_hold: &Hold,
    cancel: &CancelFlag,
) -> Result<UndoCopied, DbError> {
    let mut copied = UndoCopied {
        named: named_undo_copies(staging)?,
        ..UndoCopied::default()
    };
    for sha256 in &copied.named {
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled {
                operation: "backup".to_owned(),
                chunks: copied.blobs as usize,
            });
        }
        match live.copy_to(pool, pool_hold, sha256)? {
            Copied::Copied(size) | Copied::Already(size) => {
                copied.blobs += 1;
                copied.bytes += size;
            }
            outcome @ (Copied::Missing | Copied::Corrupt) => {
                tracing::warn!(
                    blob = %sha256,
                    damaged = outcome == Copied::Corrupt,
                    "an undo copy the database names is not in the undo folder; the backup goes on without it"
                );
                copied.missing.push(sha256.clone());
            }
        }
    }
    Ok(copied)
}

/// Remove from the pool what no kept backup names, and files a copy left
/// mid-write. Fails before removing anything when a kept backup's names
/// cannot be read: they are unknown, so nothing is guessed away.
fn clean_pool(backup_dir: &Path, pool: &UndoDir, hold: &Hold) -> Result<(), DbError> {
    let mut keep = HashSet::new();
    for backup in finished_backups(backup_dir)? {
        keep.extend(backup_undo_names(&backup)?);
    }
    for shard in shard_names() {
        let entries = pool.shard(&shard)?;
        for temp in &entries.temps {
            pool.remove_temp(hold, temp)?;
        }
        for sha256 in entries.blobs {
            if !keep.contains(&sha256) {
                pool.remove(hold, &sha256)?;
            }
        }
    }
    Ok(())
}

/// What a restore target holds, judged from its listing.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// Nothing, or only the restore's own locks.
    Empty,
    /// Only what an interrupted restore left: these names, marker included.
    Unfinished(Vec<OsString>),
}

/// Judge a restore target from the entries of its listing. The locks of
/// a database named `db_name` are the restore's own. The marker, the
/// `.restore-` files and `undo` are an interrupted restore's own only
/// while the marker is there and no `db_name` sits beside them; a marker
/// beside a database is stale and clears nothing. Anything else is
/// occupied, and so is an entry the listing could not read: what it is
/// cannot be known.
fn judge_target(
    target_dir: &Path,
    db_name: &str,
    listing: impl IntoIterator<Item = io::Result<OsString>>,
) -> Result<Target, DbError> {
    let locks = [
        datalock::lock_file_name(db_name),
        datalock::server_lock_file_name(db_name),
    ];
    let mut names = Vec::new();
    for entry in listing {
        let name = entry.map_err(|reason| DbError::Unreadable {
            path: target_dir.to_owned(),
            reason,
        })?;
        if !locks.iter().any(|lock| name == lock.as_str()) {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Ok(Target::Empty);
    }
    let leftover = |name: &OsString| {
        name.to_str().is_some_and(|name| {
            name != db_name
                && (name == RESTORE_MARKER
                    || name == UNDO_DIR_NAME
                    || name.starts_with(".restore-"))
        })
    };
    if names.iter().any(|name| name == RESTORE_MARKER) && names.iter().all(leftover) {
        Ok(Target::Unfinished(names))
    } else {
        Err(DbError::RestoreTargetNotEmpty {
            path: target_dir.to_owned(),
        })
    }
}

/// Clear what an interrupted restore left in `target_dir`, or refuse a
/// target that holds anything else (see [`judge_target`]). A target that
/// is not there is empty; one that cannot be listed is refused.
fn clear_unfinished_restore(target_dir: &Path, db_name: &str) -> Result<(), DbError> {
    let listing = match std::fs::read_dir(target_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(reason) => {
            return Err(DbError::Unreadable {
                path: target_dir.to_owned(),
                reason,
            });
        }
    };
    let listing = listing.map(|entry| entry.map(|entry| entry.file_name()));
    let Target::Unfinished(names) = judge_target(target_dir, db_name, listing)? else {
        return Ok(());
    };
    for name in names
        .iter()
        .filter(|name| name.as_os_str() != RESTORE_MARKER)
    {
        let path = target_dir.join(name);
        if std::fs::symlink_metadata(&path)?.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::remove_file(target_dir.join(RESTORE_MARKER))?;
    sync_dir(target_dir)?;
    Ok(())
}

/// Give the file at `temp` the name `target` without ever replacing a
/// file there: a hard link, which fails with `AlreadyExists` on a taken
/// name. The caller syncs the folder and, after a link, removes `temp`.
///
/// A filesystem with no hard links (vfat, exFAT) gets a rename that
/// refuses a taken name where the platform has one, else a plain rename
/// made after checking the name is free. The held `_lock` is what makes
/// that check hold: no server or other restore uses the folder meanwhile.
fn place_new(temp: &Path, target: &Path, _lock: &DataLock) -> io::Result<Placement> {
    let refused = match std::fs::hard_link(temp, target) {
        Ok(()) => return Ok(Placement::HardLink),
        Err(error) if error.raw_os_error().is_some_and(no_link_support) => error,
        Err(error) => return Err(error),
    };
    tracing::info!(%refused, "the restore target has no hard links; placing by rename");
    match rename_noreplace(temp, target) {
        Some(Ok(())) => return Ok(Placement::RenameNoReplace),
        Some(Err(error))
            if !error
                .raw_os_error()
                .is_some_and(|code| code == libc::EINVAL || code == libc::ENOSYS) =>
        {
            return Err(error);
        }
        _ => {}
    }
    match std::fs::symlink_metadata(target) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is already there", target.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::rename(temp, target)?;
            Ok(Placement::RenameAfterCheck)
        }
        Err(error) => Err(error),
    }
}

/// Error codes `link(2)` answers on a filesystem without hard links.
fn no_link_support(code: i32) -> bool {
    code == libc::EPERM || code == libc::EOPNOTSUPP || code == libc::ENOSYS
}

/// `renameat2(RENAME_NOREPLACE)`, or `None` where the platform has no
/// such call. Called through `syscall` because not every libc wraps it.
#[cfg(target_os = "linux")]
fn rename_noreplace(from: &Path, to: &Path) -> Option<io::Result<()>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let call = || {
        let from = CString::new(from.as_os_str().as_bytes())?;
        let to = CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: both paths are NUL-terminated strings that outlive the
        // call, and the descriptors are AT_FDCWD.
        let status = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                from.as_ptr(),
                libc::AT_FDCWD,
                to.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if status == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    };
    Some(call())
}

#[cfg(not(target_os = "linux"))]
fn rename_noreplace(_from: &Path, _to: &Path) -> Option<io::Result<()>> {
    None
}

/// Best-effort manifest read beside a backup.
fn read_manifest(backup: &Path) -> Option<BackupManifest> {
    let bytes = std::fs::read(manifest_path(backup)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Step the live database into the staging file with the backup API.
/// Returns the pages copied. A tripped `cancel` stops the loop between
/// steps; the caller removes the staging file.
fn copy_live(
    source_path: &Path,
    staging: &Path,
    progress: Option<BackupProgress>,
    cancel: &CancelFlag,
) -> Result<u64, DbError> {
    let _ = std::fs::remove_file(staging);
    let source = Connection::open_with_flags(source_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut target = Connection::open(staging)?;
    let backup = Backup::new(&source, &mut target)?;
    let mut copied = 0;
    loop {
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled {
                operation: "backup".to_owned(),
                chunks: copied as usize,
            });
        }
        match backup.step(BACKUP_STEP_PAGES)? {
            rusqlite::backup::StepResult::Done => break,
            rusqlite::backup::StepResult::More
            | rusqlite::backup::StepResult::Busy
            | rusqlite::backup::StepResult::Locked
            | _ => {
                let state = backup.progress();
                let total = state.pagecount.max(0) as u64;
                copied = total.saturating_sub(state.remaining.max(0) as u64);
                if let Some(report) = &progress {
                    report(copied, total);
                }
                std::thread::sleep(BACKUP_STEP_PAUSE);
            }
        }
    }
    let state = backup.progress();
    let total = state.pagecount.max(0) as u64;
    if let Some(report) = &progress {
        report(total, total);
    }
    Ok(total)
}

/// Fold any staging WAL frames into the main image, then drop the
/// sidecars. The backup target inherits WAL mode from the live database,
/// so without this the rename detaches a `-wal`/`-shm` pair that litters
/// the backups directory; folding first also keeps the renamed file
/// self-contained.
fn fold_staging(staging: &Path) -> Result<(), DbError> {
    let connection = Connection::open(staging)?;
    connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    drop(connection);
    remove_staging_sidecars(staging);
    Ok(())
}

/// Rewrite the staging copy without its free pages when more than a
/// quarter of it is free, as it is once after the undo copies move out
/// of the database. Only the copy is rewritten, never the live database.
fn compact_staging(staging: &Path) -> Result<(), DbError> {
    let connection = Connection::open(staging)?;
    let free: i64 = connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let pages: i64 = connection.query_row("PRAGMA page_count", [], |row| row.get(0))?;
    if free.saturating_mul(4) <= pages {
        return Ok(());
    }
    let compact = suffixed(staging, ".compact");
    remove_staging_tree(&compact);
    connection.execute("VACUUM INTO ?1", [compact.to_string_lossy()])?;
    drop(connection);
    remove_staging_sidecars(staging);
    std::fs::rename(&compact, staging)?;
    remove_staging_sidecars(&compact);
    Ok(())
}

/// Store the staging copy in rollback-journal mode, so a read-only open of
/// the backup, or of what it unpacks to, makes no `-wal` or `-shm` file
/// beside it. The server turns WAL mode back on when it opens a restored
/// database.
fn leave_wal(staging: &Path) -> Result<(), DbError> {
    let connection = Connection::open(staging)?;
    let mode: String = connection.query_row("PRAGMA journal_mode=DELETE", [], |row| row.get(0))?;
    drop(connection);
    remove_staging_sidecars(staging);
    if !mode.eq_ignore_ascii_case("delete") {
        return Err(DbError::Io(io::Error::other(format!(
            "the backup copy stayed in {mode} journal mode"
        ))));
    }
    Ok(())
}

/// Remove a staging file, its sidecars, its compacted and compressed
/// copies and its manifest. Best effort.
fn remove_staging_tree(staging: &Path) {
    for path in [
        staging.to_owned(),
        manifest_path(staging),
        suffixed(staging, ".gz"),
        suffixed(staging, ".compact"),
    ] {
        let _ = std::fs::remove_file(path);
    }
    remove_staging_sidecars(staging);
}

/// A new file at `temp`, made by the call that creates it, so a link or
/// a file someone placed there is never written through. A leftover of an
/// earlier run is removed first; removing a link removes only the link.
fn create_fresh(temp: &Path) -> io::Result<std::fs::File> {
    match std::fs::remove_file(temp) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    std::fs::File::create_new(temp)
}

/// Write `bytes` to `temp`, sync it, rename it to `path`. The caller
/// syncs the folder.
fn write_synced(temp: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let written = create_fresh(temp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(temp);
    }
    written
}

/// Remove the `-wal`/`-shm`/`-journal` sidecars of a staging file.
fn remove_staging_sidecars(staging: &Path) {
    for suffix in SIDECARS {
        let _ = std::fs::remove_file(suffixed(staging, suffix));
    }
}

/// The files SQLite may leave beside a database.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// What the integrity check passed.
struct Checked {
    user_version: i64,
    /// Hex SHA-256 of the checked database file.
    sha256: String,
    size_bytes: u64,
}

/// Verify a staging copy: integrity plus foreign keys, then identity.
fn verify_staging(staging: &Path) -> Result<Checked, DbError> {
    let connection = Connection::open_with_flags(staging, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if check_clean(&connection).is_err() {
        drop(connection);
        let _ = std::fs::remove_file(staging);
        return Err(DbError::IntegrityFailed);
    }
    let user_version = user_version(&connection)?;
    drop(connection);
    let (sha256, size_bytes) = file_sha256(staging)?;
    Ok(Checked {
        user_version,
        sha256,
        size_bytes,
    })
}

/// The compressed copy of a staging file, as stored.
struct Packed {
    path: PathBuf,
    /// Hex SHA-256 of the compressed bytes.
    sha256: String,
    size_bytes: u64,
}

/// Compress the checked staging copy to `<staging>.gz`, sync it, then read
/// the stored file back: it must unpack to exactly the bytes the integrity
/// check passed (`checked_sha256`), or nothing is published.
fn pack_staging(staging: &Path, checked_sha256: &str) -> Result<Packed, DbError> {
    let path = suffixed(staging, ".gz");
    let written = (|| -> io::Result<()> {
        let mut input = io::BufReader::with_capacity(64 * 1024, std::fs::File::open(staging)?);
        let output = io::BufWriter::with_capacity(64 * 1024, std::fs::File::create(&path)?);
        let mut encoder = GzEncoder::new(output, Level::default());
        io::copy(&mut input, &mut encoder)?;
        let file = encoder
            .finish()?
            .into_inner()
            .map_err(io::IntoInnerError::into_error)?;
        file.sync_all()
    })();
    written?;
    let (unpacked, _) = read_sha256(GzDecoder::new(std::fs::File::open(&path)?))?;
    if unpacked != checked_sha256 {
        return Err(DbError::Io(io::Error::new(
            io::ErrorKind::InvalidData,
            "the compressed backup does not unpack to the checked copy",
        )));
    }
    let (sha256, size_bytes) = file_sha256(&path)?;
    Ok(Packed {
        path,
        sha256,
        size_bytes,
    })
}

/// Unpack the gzip file at `packed` into a new file at `target`, which
/// must not exist. Returns the bytes written; the caller syncs them. A
/// damaged or cut-short file fails (gzip carries its own checksum).
fn unpack_to(packed: &Path, target: &Path) -> Result<u64, DbError> {
    let mut decoder = GzDecoder::new(io::BufReader::with_capacity(
        64 * 1024,
        std::fs::File::open(packed)?,
    ));
    let mut output = io::BufWriter::with_capacity(64 * 1024, std::fs::File::create_new(target)?);
    let bytes = io::copy(&mut decoder, &mut output)?;
    output
        .into_inner()
        .map_err(io::IntoInnerError::into_error)?;
    Ok(bytes)
}

/// True when the file starts with the gzip magic bytes.
fn is_gzip(path: &Path) -> Result<bool, DbError> {
    let mut magic = [0_u8; 2];
    match std::fs::File::open(path)?.read_exact(&mut magic) {
        Ok(()) => Ok(magic == [0x1f, 0x8b]),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// True for a backup named as this build stores them, `*.db.gz`.
#[must_use]
pub fn is_packed_name(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(PACKED_SUFFIX))
}

/// Lowercase hex SHA-256 and byte length of a file, read in 64 KiB chunks
/// so a catalog-sized database never sits in memory whole. Blocking: call
/// from a blocking thread.
pub fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    read_sha256(std::fs::File::open(path)?)
}

/// The SHA-256 (lower-case hex) of everything `reader` holds, and its
/// length.
pub fn read_sha256(reader: impl std::io::Read) -> std::io::Result<(String, u64)> {
    let mut reader = std::io::BufReader::with_capacity(64 * 1024, reader);
    let mut hasher = Sha256::new();
    let total = std::io::copy(&mut reader, &mut hasher)?;
    Ok((hex_digest(&hasher.finalize()), total))
}

/// `integrity_check` must read `ok` and `foreign_key_check` must be empty.
fn check_clean(connection: &Connection) -> Result<(), DbError> {
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .map_err(|_| DbError::IntegrityFailed)?;
    if integrity != "ok" {
        return Err(DbError::IntegrityFailed);
    }
    let violations: i64 = connection
        .query_row(sql::FOREIGN_KEY_VIOLATIONS.text(), [], |row| row.get(0))
        .map_err(|_| DbError::IntegrityFailed)?;
    if violations != 0 {
        return Err(DbError::IntegrityFailed);
    }
    Ok(())
}

/// Refuse a backup the schema runner would refuse at boot, and one newer
/// than this binary unless `allow_downgrade`.
fn check_schema(connection: &Connection, allow_downgrade: bool) -> Result<(), DbError> {
    match crate::schema::CHAIN.plan(Stamp::read(connection)?) {
        Ok(_) => Ok(()),
        Err(SchemaError::TooNew { .. }) if allow_downgrade => Ok(()),
        Err(SchemaError::TooNew { found, latest }) => Err(DbError::BackupTooNew {
            found,
            expected: latest,
        }),
        Err(refused) => Err(DbError::BackupRefused(refused)),
    }
}

/// `user_version` stamped in a database file.
fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// One of the app's own backups, as rotation weighs it.
#[derive(Debug, Clone)]
struct Listed {
    path: PathBuf,
    /// When it was taken, from its name.
    millis: i64,
    kind: BackupKind,
    user_version: Option<i64>,
    /// Made before backups recorded their kind: no manifest, or one older
    /// than format 3.
    kindless: bool,
}

/// Whether rotation leaves kindless backups alone: counted in no kind and
/// never removed, so the backups an older version kept stay until an admin
/// deletes them. With `false` they rotate as manual backups.
const ROTATION_SPARES_KINDLESS: bool = true;

/// The app's own `library-<ms>[-<seq>]` backups in `backup_dir`, oldest
/// first, each with the kind its manifest gives (manual without one).
fn listed_backups(backup_dir: &Path) -> Result<Vec<Listed>, DbError> {
    let mut listed = Vec::new();
    for path in finished_backups(backup_dir)? {
        let Some(millis) = backup_order(&path)
            .0
            .and_then(|(millis, _)| i64::try_from(millis).ok())
        else {
            continue;
        };
        let manifest = read_manifest(&path);
        listed.push(Listed {
            kind: manifest
                .as_ref()
                .map(|found| found.kind)
                .unwrap_or_default(),
            user_version: manifest.as_ref().map(|found| found.user_version),
            kindless: manifest
                .as_ref()
                .is_none_or(|found| found.format_version < MANIFEST_FORMAT),
            path,
            millis,
        });
    }
    Ok(listed)
}

/// Which of `listed` (oldest first) rotation keeps: per kind, by
/// `retention`. Scheduled: the newest of each of the newest days and the
/// newest of each of the newest ISO weeks that hold one (UTC, by the name's
/// millisecond; 1970-01-01 was a Thursday). Before an upgrade: the newest
/// for each of the newest schema versions. Manual: the newest. A backup
/// dated more than [`AHEAD_TOLERANCE_MS`] ahead of `now_ms` is kept and
/// counted in no slot.
fn kept(listed: &[Listed], now_ms: i64, retention: Retention) -> Vec<bool> {
    let mut keep = vec![false; listed.len()];
    let ahead = now_ms.saturating_add(AHEAD_TOLERANCE_MS);
    let (mut days, mut weeks, mut versions) = (HashSet::new(), HashSet::new(), HashSet::new());
    let mut manual = 0;
    for (index, entry) in listed.iter().enumerate().rev() {
        if entry.millis > ahead {
            tracing::warn!(
                backup = %entry.path.display(),
                "a backup is dated ahead of the clock; it is kept and rotates once the clock passes it"
            );
            keep[index] = true;
            continue;
        }
        if entry.kindless && ROTATION_SPARES_KINDLESS {
            keep[index] = true;
            continue;
        }
        keep[index] = match entry.kind {
            BackupKind::Scheduled => {
                let day = entry.millis.div_euclid(DAY_MS);
                let week = (day + 3).div_euclid(7);
                let daily = days.len() < retention.daily && days.insert(day);
                let weekly = weeks.len() < retention.weekly && weeks.insert(week);
                daily || weekly
            }
            BackupKind::PreUpgrade => {
                versions.len() < retention.pre_upgrade && versions.insert(entry.user_version)
            }
            BackupKind::Manual => {
                manual += 1;
                manual <= retention.manual
            }
        };
    }
    keep
}

/// Remove the backups `retention` does not keep, each with its manifest
/// and sidecars, and sidecars whose backup is gone. Runs only after the
/// new backup verified. Only files under the app's own
/// `library-<ms>[-<seq>]` names are weighed, and a file someone placed
/// under such a name counts as one of the app's: with no manifest it is
/// kindless, so [`ROTATION_SPARES_KINDLESS`] keeps it. A file under any
/// other name is never counted or deleted.
/// The backup goes before its manifest, so no finished backup is left
/// without one.
fn rotate_backups(backup_dir: &Path, now_ms: i64, retention: Retention) -> Result<(), DbError> {
    let listed = listed_backups(backup_dir)?;
    let keep = kept(&listed, now_ms, retention);
    for (entry, keep) in listed.iter().zip(keep) {
        if keep {
            continue;
        }
        let paths = [entry.path.clone(), manifest_path(&entry.path)]
            .into_iter()
            .chain(SIDECARS.map(|suffix| suffixed(&entry.path, suffix)));
        for path in paths {
            match std::fs::remove_file(&path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => {
                    tracing::warn!(file = %path.display(), %error, "an old backup could not be rotated out");
                }
                _ => {}
            }
        }
    }
    remove_orphan_sidecars(backup_dir)
}

/// Remove `-wal`, `-shm` and `-journal` files left beside one of the app's
/// own backup names after the backup itself is gone.
fn remove_orphan_sidecars(backup_dir: &Path) -> Result<(), DbError> {
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(base) = SIDECARS.iter().find_map(|suffix| name.strip_suffix(suffix)) else {
            continue;
        };
        let backup = backup_dir.join(base);
        if backup_order(&backup).0.is_none() {
            continue;
        }
        match std::fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                if let Err(error) = std::fs::remove_file(entry.path()) {
                    tracing::debug!(file = name, %error, "a stray backup sidecar stays for the next rotation");
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The finished backup files in `backup_dir`, oldest first: `.db` and
/// `.db.gz`, never staging or restore temp files. [`backup_order`] says
/// what oldest means.
fn finished_backups(backup_dir: &Path) -> Result<Vec<PathBuf>, DbError> {
    let mut backups: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_backup_name(&name) {
            backups.push(entry.path());
        }
    }
    backups.sort_by_cached_key(|path| backup_order(path));
    Ok(backups)
}

/// Folder entries that count as backups: finished `.db` and `.db.gz`
/// files, never staging or restore temp files.
#[must_use]
pub fn is_backup_name(name: &str) -> bool {
    (name.ends_with(PLAIN_SUFFIX) || name.ends_with(PACKED_SUFFIX))
        && !name.starts_with(".staging-")
        && !name.starts_with(".restore-")
}

/// Where a backup sorts: by the millisecond and then the sequence its
/// [`backup_file_name`] holds, read as numbers, so `-10` comes after `-9`.
/// `.db.gz` and `.db` names sort together. A name of another shape sorts
/// first, by name.
pub fn backup_order(path: &Path) -> (Option<(u128, u64)>, PathBuf) {
    let stamp = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("library-"))
        .and_then(|rest| {
            rest.strip_suffix(PACKED_SUFFIX)
                .or_else(|| rest.strip_suffix(PLAIN_SUFFIX))
        })
        .and_then(|stamp| {
            let (millis, seq) = stamp.split_once('-').unwrap_or((stamp, "0"));
            Some((millis.parse().ok()?, seq.parse().ok()?))
        });
    (stamp, path.to_owned())
}

/// `path` with `suffix` added to its file name.
fn suffixed(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Manifest sidecar path for a backup file.
#[must_use]
pub fn manifest_path(backup: &Path) -> PathBuf {
    suffixed(backup, MANIFEST_SUFFIX)
}

/// Sortable backup file name from wall-clock milliseconds plus the staging
/// sequence, so two backups in the same millisecond never collide.
fn backup_file_name(now: SystemTime, seq: u64) -> String {
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis())
        .unwrap_or(0);
    format!("library-{millis}-{seq}{PACKED_SUFFIX}")
}

/// Lowercase hex of a SHA-256 digest.
fn hex_digest(digest: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(digest.len() * 2);
    for byte in digest {
        text.push(HEX[(byte >> 4) as usize] as char);
        text.push(HEX[(byte & 0x0f) as usize] as char);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_hash_matches_the_whole_file_hash() {
        let path = std::env::temp_dir().join(format!("dn-hash-{}.bin", std::process::id()));
        let bytes: Vec<u8> = (0..200_000u32).map(|n| (n % 251) as u8).collect();
        std::fs::write(&path, &bytes).unwrap();
        let (sha256, size) = file_sha256(&path).unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(sha256, hex_digest(&Sha256::digest(&bytes)));
        let _ = std::fs::remove_file(path);
    }

    /// Rotation deletes by name order, so names sort in creation order and
    /// two backups in one millisecond never share a name.
    #[test]
    fn backup_names_sort_and_never_collide() {
        let first = backup_file_name(UNIX_EPOCH + Duration::from_millis(10), 0);
        let second = backup_file_name(UNIX_EPOCH + Duration::from_millis(11), 1);
        assert!(first < second, "later millisecond");
        assert!(first.ends_with(PACKED_SUFFIX), "extension");

        let same_ms = UNIX_EPOCH + Duration::from_millis(10);
        let first = backup_file_name(same_ms, 0);
        let second = backup_file_name(same_ms, 1);
        assert_ne!(first, second, "same millisecond");
        assert!(first < second, "same millisecond");
    }

    /// An entry the listing cannot read means occupied; nothing, or only
    /// the configured database's two locks, means empty.
    #[test]
    fn restore_target_with_an_unreadable_entry_is_occupied() {
        let dir = Path::new("/target");
        let unreadable = vec![
            Ok(OsString::from("music.db.lock")),
            Err(io::Error::from_raw_os_error(libc::EACCES)),
        ];
        assert!(matches!(
            judge_target(dir, "music.db", unreadable),
            Err(DbError::Unreadable { .. })
        ));
        assert_eq!(
            judge_target(dir, "music.db", Vec::new()).unwrap(),
            Target::Empty
        );
        let locks = vec![
            Ok(OsString::from("music.db.lock")),
            Ok(OsString::from("music.db.server.lock")),
        ];
        assert_eq!(judge_target(dir, "music.db", locks).unwrap(), Target::Empty);
        let stale_marker = vec![
            Ok(OsString::from(RESTORE_MARKER)),
            Ok(OsString::from("music.db")),
        ];
        assert!(matches!(
            judge_target(dir, "music.db", stale_marker),
            Err(DbError::RestoreTargetNotEmpty { .. })
        ));
    }

    /// Placing onto a name that is taken fails and leaves that file's
    /// bytes as they were.
    #[test]
    fn placing_onto_an_existing_database_never_replaces_it() {
        let dir = crate::test_support::ScratchDir::new("backup-place").unwrap();
        let temp = dir.join(".restore-1-2-3.tmp");
        let target = dir.join("library.db");
        std::fs::write(&temp, b"restored").unwrap();
        std::fs::write(&target, b"already here").unwrap();
        let lock = DataLock::exclusive(&target).unwrap();
        let error = place_new(&temp, &target, &lock).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&target).unwrap(), b"already here");
    }

    /// The copy takes the main file only, so a backup whose `-wal` holds
    /// writes is refused rather than restored without them.
    #[test]
    fn a_backup_with_writes_in_its_wal_is_refused() {
        let dir = crate::test_support::ScratchDir::new("backup-wal").unwrap();
        let backup = dir.join("library-1-0.db");
        // Held open so the writes stay in the -wal file.
        let writer = Connection::open(&backup).unwrap();
        writer
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE t (x INTEGER);")
            .unwrap();
        assert!(matches!(wal_state(&backup), WalState::HoldsData(_)));
        let service = BackupService::new(
            &backup,
            dir.path(),
            Arc::new(crate::clock::SystemClock),
            false,
        );
        let target = dir.join("restored");
        assert!(matches!(
            service.restore(&backup, &target, "library.db", false),
            Err(DbError::BackupWalPresent { .. })
        ));
        assert!(!target.exists(), "nothing was written for a refused backup");
        drop(writer);
    }

    /// Forty days of nightly backups, with ones taken by hand and before
    /// upgrades: a backup refused for room removes nothing, and the next
    /// one that verifies leaves exactly what each kind keeps. Days and
    /// weeks are the newest that hold a backup (day 40 and the week of
    /// January 19 hold none), and one dated a month ahead stays and counts
    /// for nothing.
    #[tokio::test]
    async fn retention_keeps_each_kind_by_its_own_rule() {
        // 2026-01-01, a Thursday, at midnight UTC.
        const DAY0: i64 = 1_767_225_600_000;
        const HOUR: i64 = 60 * 60 * 1000;
        let at = |day: i64, hour: i64| DAY0 + day * DAY_MS + hour * HOUR;
        let dir = crate::test_support::ScratchDir::new("backup-retention").unwrap();
        let db = dir.join("library.db");
        crate::test_support::template::copy_to(&db).unwrap();
        let backups = dir.join("backups");
        std::fs::create_dir_all(&backups).unwrap();
        let place = |millis: i64, kind: BackupKind, user_version: i64| {
            let path = backups.join(format!("library-{millis}-0{PACKED_SUFFIX}"));
            std::fs::write(&path, b"x").unwrap();
            let manifest = BackupManifest {
                format_version: MANIFEST_FORMAT,
                sha256: String::new(),
                size_bytes: 1,
                user_version,
                created_at: UNIX_EPOCH + Duration::from_millis(millis as u64),
                kind,
                compression: Compression::Gzip,
                db_sha256: None,
                db_bytes: None,
                undo_blobs: 0,
                undo_bytes: 0,
                undo_missing: Vec::new(),
                undo_named: Some(Vec::new()),
            };
            std::fs::write(manifest_path(&path), serde_json::to_vec(&manifest).unwrap()).unwrap();
            path.file_name().unwrap().to_string_lossy().into_owned()
        };
        let scheduled = |day| place(at(day, 2), BackupKind::Scheduled, 1);
        let nightly: Vec<(i64, String)> = (0..40)
            .filter(|day| !(18..=24).contains(day))
            .map(|day| (day, scheduled(day)))
            .collect();
        let late = place(at(36, 23), BackupKind::Scheduled, 1);
        let ahead = place(at(70, 2), BackupKind::Scheduled, 1);
        let upgrades: Vec<String> = [(5, 1), (12, 2), (20, 3), (21, 3), (22, 3), (30, 4)]
            .into_iter()
            .map(|(day, version)| place(at(day, 4), BackupKind::PreUpgrade, version))
            .collect();
        let manual: Vec<String> = [1, 2, 15, 33, 39]
            .into_iter()
            .map(|day| place(at(day, 12), BackupKind::Manual, 1))
            .collect();
        // Two backups an older version made, a manifest of format 2 and
        // none at all: spared, and counted in no kind.
        let old_format = place(at(3, 12), BackupKind::Manual, 1);
        let old_manifest = manifest_path(&backups.join(&old_format));
        let mut older: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&old_manifest).unwrap()).unwrap();
        older["format_version"] = 2.into();
        older.as_object_mut().unwrap().remove("kind");
        std::fs::write(&old_manifest, serde_json::to_vec(&older).unwrap()).unwrap();
        let no_manifest = format!("library-{}-0{PLAIN_SUFFIX}", at(4, 12));
        crate::test_support::template::copy_to(&backups.join(&no_manifest)).unwrap();
        let listing = || {
            let mut names: Vec<String> = std::fs::read_dir(&backups)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|name| name != UNDO_DIR_NAME)
                .collect();
            names.sort();
            names
        };
        let before = listing();

        let clock = crate::test_support::ManualClock::at(at(41, 3));
        let no_room = BackupService::new(&db, &backups, Arc::new(clock.clone()), false)
            .with_space_probe(|_| Ok(0));
        assert!(matches!(
            no_room
                .backup(BackupKind::Scheduled, None, &CancelFlag::never())
                .await,
            Err(BackupError::NoRoom { .. })
        ));
        assert_eq!(listing(), before, "a refused backup changed the folder");

        let service = BackupService::new(&db, &backups, Arc::new(clock), false)
            .with_space_probe(|_| Ok(u64::MAX));
        let report = service
            .backup(BackupKind::Scheduled, None, &CancelFlag::never())
            .await
            .unwrap();
        let newest = report
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();

        // Days 41 and 39 to 34, day 36 by its later backup; weeks: 41
        // (Feb 9-15), 38 (Feb 2-8), 31 (Jan 26 - Feb 1) and 17 (Jan 12-18).
        let days = [39, 38, 37, 35, 34, 31, 17];
        let mut kept: Vec<String> = nightly
            .iter()
            .filter(|(day, _)| days.contains(day))
            .map(|(_, name)| name.clone())
            .chain([late, newest, ahead])
            // The newest for each of the three newest versions.
            .chain([&upgrades[5], &upgrades[4], &upgrades[1]].map(Clone::clone))
            .chain(manual[2..].iter().cloned())
            .chain([old_format])
            .flat_map(|name| [format!("{name}{MANIFEST_SUFFIX}"), name])
            .chain([no_manifest])
            .collect();
        kept.sort();
        assert_eq!(listing(), kept);
    }
}
