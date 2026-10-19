//! In-place upgrade of a v2 data directory to v3.
//!
//! v2 and v3 keep their data in the same two folders, `config/` and
//! `cache/`, which are separate mounts in a container. [`run`] turns a v2
//! pair into a v3 pair without the user moving anything:
//!
//! 1. [`detect`] classifies the folders. Only a v2 layout is upgraded.
//! 2. The exclusive [`DataLock`] is taken (so a running server refuses the
//!    run), a v2 database with unmerged writes is refused, and free disk
//!    is checked.
//! 3. A marker is written, then every v2 entry is renamed into a
//!    `v2-backup/` folder inside its own mount. A rename stays on one
//!    filesystem, so the move is instant and needs no extra disk.
//! 4. The backup is exported into a work folder, sealed with a passphrase
//!    that only ever exists in memory, then validated, dry-run and imported
//!    into the now-empty folders.
//! 5. The work folder and the marker are removed.
//!
//! Any failure after step 3 rolls back: what the import created is deleted
//! and the v2 entries are renamed back. A killed run leaves the marker, and
//! the next run rolls back before it starts again. The server boots only
//! on an empty or v3 layout
//! ([`check_bootable`](droppedneedle::bootstrap::data_dirs::check_bootable)), so it
//! never migrates over a v2 database and never serves a half-imported one.
//!
//! The marker records one fact: whether anything outside the backup may be
//! deleted. While entries move between the folders and the backup
//! (`moving`), nothing may, because an entry still in place is the user's
//! v2 data. Once every entry is inside the backup (`upgrading`), whatever
//! else sits in the two folders was created by the upgrade. The rollback
//! deletes first, flips the marker back to `moving`, and only then renames
//! entries back, so a rollback that is itself interrupted can never delete
//! a v2 file it already restored.
//!
//! [`run_offline_import`] is the import both this upgrade and the tool's
//! `import` and `dry-run` commands run.

use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OpenFlags};
use sqlx::SqlitePool;
use thiserror::Error;

use crate::export::v2dir::{self, V2Dirs};
use crate::export::{ExportError, ExportRequest, export_v2_to_file};
use crate::import::{
    ExitCode, ExportFile, ImportReport, ImportRequest, ValidationIssue, check_attachments,
    run_import, validate_export,
};
use droppedneedle::bootstrap::data_dirs::{
    BACKUP_DIR_NAME, BootRefusal, DataDirs, Layout, MARKER_FILE_NAME, UnknownLayout, detect,
    key_refusal, refuse_lost_key, wal_bytes, wal_path,
};
use droppedneedle_kernel::db::datalock::{DataLock, DataLockError, lock_path, server_lock_path};
use droppedneedle_kernel::db::{DbConfig, open_runtime};
use droppedneedle_kernel::runtime_config::crypto::{CryptoError, KEY_FILE_NAME};
use droppedneedle_kernel::runtime_config::ingest::{LEGACY_CONFIG_FILE_NAME, mark_skip};
use droppedneedle_kernel::runtime_config::key_check::{Assessment, KeyCheck, assess, enforce};
use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto, KeyOrigin, LegacyConfig, Secret};
use droppedneedle_kernel::schema::SchemaError;

/// Work folder in `cache/` for the export file and its bundle.
const WORK_DIR_NAME: &str = ".upgrade-work";
const EXPORT_FILE_NAME: &str = "export.json";
const MARKER_FORMAT: &str = "droppedneedle-upgrade-marker";
/// The validator's code for a follow or approval of a deleted user.
const DANGLING_USER_REF: &str = "DANGLING_USER_REF";
const MIB: u64 = 1024 * 1024;
/// Free disk asked for on top of the estimate.
const DISK_HEADROOM_BYTES: u64 = 64 * MIB;
/// v2 cache folders whose files travel inside the export bundle.
const CARRIED_FILE_DIRS: &[&str] = &[
    "avatars",
    "covers/playlists",
    "held",
    "library-management/blobs/objects",
];

fn work_dir(dirs: &DataDirs) -> PathBuf {
    dirs.cache_dir.join(WORK_DIR_NAME)
}

/// The same layout inside the two backup folders.
fn backup_side(dirs: &DataDirs) -> DataDirs {
    let config_dir = dirs.config_backup();
    let cache_dir = dirs.cache_backup();
    DataDirs {
        db_path: cache_dir.join(dirs.db_path.file_name().unwrap_or_default()),
        config_dir,
        cache_dir,
        plugins_dir: dirs.plugins_dir.clone(),
    }
}

/// The two folders the upgrade works in, each with its backup and the
/// entries it never moves or deletes.
fn folders(dirs: &DataDirs) -> [Folder; 2] {
    let marker = OsString::from(MARKER_FILE_NAME);
    let mut cache_kept = vec![OsString::from(BACKUP_DIR_NAME), tmp_name(&marker), marker];
    // The locks are held for the whole run and have to stay where the
    // server looks for them, so the upgrade never moves or deletes them.
    for lock in [lock_path(&dirs.db_path), server_lock_path(&dirs.db_path)] {
        if let Some(name) = lock.file_name() {
            cache_kept.push(name.to_owned());
        }
    }
    [
        Folder {
            dir: dirs.config_dir.clone(),
            backup: dirs.config_backup(),
            kept: vec![OsString::from(BACKUP_DIR_NAME)],
        },
        Folder {
            dir: dirs.cache_dir.clone(),
            backup: dirs.cache_backup(),
            kept: cache_kept,
        },
    ]
}

/// One data folder, its backup folder, and the entry names left alone.
struct Folder {
    dir: PathBuf,
    backup: PathBuf,
    kept: Vec<OsString>,
}

/// One upgrade run's inputs.
#[derive(Debug, Clone)]
pub struct UpgradeRequest {
    /// The folders to upgrade.
    pub dirs: DataDirs,
    /// Keep the v2 files in the backup folders once the import is done.
    pub keep_backup: bool,
    /// Best-effort v2 provenance, carried into the export when known.
    pub v2_commit: Option<String>,
    /// Test-only failure point. The tool never sets one.
    pub fault: Option<Fault>,
}

/// Test-only failure points, to prove the rollback and the recovery of a
/// killed run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// The import fails before it commits, so the run rolls back.
    ImportFails,
    /// The process dies after the config folder moved aside and before the
    /// cache folder did.
    DiesWhileMoving,
    /// The process dies right after the import.
    DiesAfterImport,
    /// The import fails before it commits, and the process dies during the
    /// rollback, after the config folder went back and before the cache
    /// folder did.
    DiesWhileRollingBack,
}

/// Progress, for the tool to print as the run moves along.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// An earlier run did not finish; its changes are being rolled back.
    Recovering,
    /// The v2 entries are moving into the backup folders.
    MovingAside,
    /// The backup is being exported.
    Exporting,
    /// This many follows and approvals of deleted users were removed from
    /// the v2 database; the export runs again.
    OrphansRemoved(usize),
    /// The import is being rehearsed without writing records.
    DryRun,
    /// The import is running.
    Importing,
    /// This run failed; the v2 entries are going back.
    RollingBack,
}

/// How an upgrade run ended.
#[derive(Debug)]
pub enum Upgraded {
    /// The folders held nothing to upgrade: an empty or v3 layout.
    NothingToDo(Layout),
    /// The v2 data is now v3 data.
    Done(Box<Summary>),
}

/// What a finished upgrade did.
#[derive(Debug)]
pub struct Summary {
    /// The import report, also saved as
    /// [`REPORT_FILE_NAME`](droppedneedle::bootstrap::data_dirs::REPORT_FILE_NAME).
    pub report: ImportReport,
    /// Follows and approvals of deleted users removed from the v2 database.
    pub orphans_removed: usize,
    /// False when the v2 files were deleted after the import.
    pub backup_kept: bool,
}

/// Every way the upgrade, its undo, or the backup removal can fail.
#[derive(Debug, Error)]
pub enum UpgradeError {
    /// A server or another tool run holds the data lock.
    #[error("{0}. Stop the server, then try again")]
    Locked(DataLockError),
    /// An upgrade or import is running right now.
    #[error("an upgrade or import is running on this data directory; wait for it to finish")]
    ToolRunning,
    /// The folders hold something that is neither v2 nor v3.
    #[error("cannot tell what this data directory holds: {0}. Nothing was changed")]
    UnknownLayout(UnknownLayout),
    /// The paths are valid for the server but not for an in-place upgrade.
    #[error(
        "{reason}, which the in-place upgrade does not handle. Use the export and import \
         commands instead"
    )]
    UnsupportedPaths {
        /// What is unusual about the paths.
        reason: &'static str,
    },
    /// A backup folder is already there.
    #[error(
        "{} already exists. Move it away or delete it, then run the upgrade again",
        .0.display()
    )]
    BackupExists(PathBuf),
    /// The v2 database was not closed cleanly.
    #[error(
        "V2_WAL_PRESENT: v2 database {} has unmerged writes in its -wal file. If v2 is still \
         running, stop it first. Otherwise start v2 and stop it cleanly, then run the upgrade \
         again",
        .path.display()
    )]
    WalPresent {
        /// The `-wal` file.
        path: PathBuf,
    },
    /// The export refused. The cause keeps its own code.
    #[error("{code}: {0}", code = .0.code())]
    Export(#[from] ExportError),
    /// Not enough free disk for the export bundle and the new database.
    #[error(
        "the upgrade needs about {needed_mib} MiB free in {} and only {free_mib} MiB is free",
        .dir.display()
    )]
    NotEnoughDisk {
        /// The cache folder.
        dir: PathBuf,
        /// Estimated need, in MiB.
        needed_mib: u64,
        /// Free space, in MiB.
        free_mib: u64,
    },
    /// A file or folder operation failed.
    #[error("cannot {action} {}: {reason}", .path.display())]
    Io {
        /// What was being done.
        action: &'static str,
        /// The file or folder.
        path: PathBuf,
        /// OS reason.
        reason: String,
    },
    /// A v2 entry cannot go back because its place is taken.
    #[error("cannot put {} back: something else is in its place", .0.display())]
    Occupied(PathBuf),
    /// The system random source failed.
    #[error("cannot make a passphrase: the system random source failed")]
    RandomFailed,
    /// The export did not pass validation.
    #[error("the export did not pass validation: {}", issue_list(.0))]
    Invalid(Vec<ValidationIssue>),
    /// Removing orphan rows from the v2 database failed.
    #[error("cannot remove orphan rows from the v2 database: {0}")]
    OrphanRepair(String),
    /// The import could not start.
    #[error(transparent)]
    ImportSetup(#[from] OfflineImportError),
    /// The import or its dry run reported a failure.
    #[error("the {} failed: {}", import_kind(*.dry_run), .report.exit.message)]
    ImportFailed {
        /// True when the dry run failed, before any record was written.
        dry_run: bool,
        /// The failed run's report.
        report: Box<ImportReport>,
    },
    /// There is no complete v2 backup to go back to.
    #[error(
        "no complete v2 backup in {} and {}, so nothing was deleted or moved",
        .config.display(),
        .cache.display()
    )]
    NoBackup {
        /// Where the config backup was expected.
        config: PathBuf,
        /// Where the cache backup was expected.
        cache: PathBuf,
    },
    /// Undo was asked on folders that already hold v2 data.
    #[error("this data directory already holds v2 data; there is nothing to undo")]
    AlreadyV2,
    /// A backup path is a link or a file, which is never followed or deleted.
    #[error("{} is not a folder; it was left alone", .0.display())]
    NotAFolder(PathBuf),
    /// The backup is the only complete copy of the data.
    #[error(
        "this data directory is not on v3, so the v2 backup may be the only complete copy \
         of your data and was not removed. Finish the upgrade with `droppedneedle-tool \
         upgrade`, or go back to v2 with `droppedneedle-tool upgrade --undo --yes`"
    )]
    BackupStillNeeded,
    /// The run failed and putting the v2 entries back failed too.
    #[error(
        "{cause}. Putting the v2 files back also failed: {rollback}. Run `droppedneedle-tool \
         upgrade` again to retry"
    )]
    RollbackFailed {
        /// Why the run failed.
        cause: Box<UpgradeError>,
        /// Why the rollback failed.
        rollback: Box<UpgradeError>,
    },
    /// The upgrade finished but the backup is still there.
    #[error(
        "the upgrade is done, but the v2 backup could not be removed: {0}. Run \
         `droppedneedle-tool remove-v2-backup` to try again"
    )]
    BackupNotRemoved(Box<UpgradeError>),
    /// Injected crash for the tests. The tool never produces it.
    #[error("injected crash")]
    InjectedCrash,
}

fn failed(action: &'static str, path: &Path, error: impl std::fmt::Display) -> UpgradeError {
    UpgradeError::Io {
        action,
        path: path.to_owned(),
        reason: error.to_string(),
    }
}

fn issue_list(issues: &[ValidationIssue]) -> String {
    issues
        .iter()
        .map(|issue| format!("{} at {}: {}", issue.code, issue.path, issue.message))
        .collect::<Vec<_>>()
        .join("; ")
}

fn import_kind(dry_run: bool) -> &'static str {
    if dry_run { "dry run" } else { "import" }
}

/// Upgrade a v2 layout in place. An empty or v3 layout is left alone.
pub async fn run(
    request: &UpgradeRequest,
    progress: &mut dyn FnMut(Step),
) -> Result<Upgraded, UpgradeError> {
    let dirs = &request.dirs;
    // Look before locking: taking the lock creates files, and a layout
    // that is not upgraded has to stay exactly as it was found.
    match detect(dirs) {
        layout @ (Layout::Empty | Layout::V3) => return Ok(Upgraded::NothingToDo(layout)),
        Layout::Unknown(reason) => return Err(UpgradeError::UnknownLayout(reason)),
        Layout::V2 | Layout::Interrupted => {}
    }
    let lock = lock_exclusive(dirs)?;
    // Look again under the lock: another run may have finished meanwhile.
    if detect(dirs) == Layout::Interrupted {
        progress(Step::Recovering);
        roll_back(dirs, None)?;
    }
    match detect(dirs) {
        Layout::V2 => {}
        layout @ (Layout::Empty | Layout::V3) => return Ok(Upgraded::NothingToDo(layout)),
        Layout::Unknown(reason) => return Err(UpgradeError::UnknownLayout(reason)),
        Layout::Interrupted => {
            return Err(failed("clear", &dirs.marker(), "the marker is still there"));
        }
    }
    preflight(dirs)?;
    progress(Step::MovingAside);
    let converted = match move_aside(dirs, request.fault) {
        Ok(()) => convert(request, &lock, progress).await,
        Err(error) => Err(error),
    };
    let (report, orphans_removed) = match converted {
        Ok(done) => done,
        // A killed process rolls nothing back; the next run does.
        Err(UpgradeError::InjectedCrash) => return Err(UpgradeError::InjectedCrash),
        Err(cause) => {
            progress(Step::RollingBack);
            return Err(match roll_back(dirs, request.fault) {
                Ok(()) => cause,
                Err(UpgradeError::InjectedCrash) => UpgradeError::InjectedCrash,
                Err(rollback) => UpgradeError::RollbackFailed {
                    cause: Box::new(cause),
                    rollback: Box::new(rollback),
                },
            });
        }
    };
    if !request.keep_backup {
        // The marker's removal has to be on disk first. A marker that came
        // back after a power cut, beside a half-deleted backup, would make
        // the next run delete the v3 data.
        sync_dir(&dirs.cache_dir)
            .and_then(|()| delete_backups(dirs))
            .map_err(|error| UpgradeError::BackupNotRemoved(Box::new(error)))?;
    }
    Ok(Upgraded::Done(Box::new(Summary {
        report,
        orphans_removed,
        backup_kept: request.keep_backup,
    })))
}

/// Go back to v2: delete the v3 data and put the v2 entries back from the
/// backup folders. Also finishes the rollback of an interrupted upgrade.
pub fn undo(dirs: &DataDirs) -> Result<(), UpgradeError> {
    // No lock file is created unless there is something to undo.
    match detect(dirs) {
        Layout::Interrupted => {}
        Layout::V2 => return Err(UpgradeError::AlreadyV2),
        _ => require_backup(dirs)?,
    }
    let _lock = lock_exclusive(dirs)?;
    match detect(dirs) {
        Layout::Interrupted => roll_back(dirs, None),
        Layout::Empty | Layout::V3 => {
            require_backup(dirs)?;
            write_marker(dirs, Phase::Upgrading)?;
            roll_back(dirs, None)
        }
        Layout::V2 => Err(UpgradeError::AlreadyV2),
        Layout::Unknown(reason) => Err(UpgradeError::UnknownLayout(reason)),
    }
}

/// Delete the v2 backup folders of a finished upgrade. Returns the folders
/// removed, none when there was no backup.
pub fn remove_backup(dirs: &DataDirs) -> Result<Vec<PathBuf>, UpgradeError> {
    let backups = [dirs.config_backup(), dirs.cache_backup()];
    if !backups
        .iter()
        .any(|backup| std::fs::symlink_metadata(backup).is_ok())
    {
        return Ok(Vec::new());
    }
    // Shared, so this works beside a running server and still waits out an
    // upgrade, which holds the lock exclusively.
    let _lock = DataLock::shared(&dirs.db_path).map_err(|error| match error {
        DataLockError::Held { .. } => UpgradeError::ToolRunning,
        DataLockError::Io { path, reason } => failed("lock", &path, reason),
    })?;
    if detect(dirs) != Layout::V3 {
        return Err(UpgradeError::BackupStillNeeded);
    }
    delete_backups(dirs)
}

/// Drop follows and approvals whose user is gone from a v2 database, and
/// return how many rows went. v2 cannot hold such rows while its foreign
/// keys are on, and the import refuses an export that names an unknown
/// user.
pub fn delete_orphan_rows(db_path: &Path) -> rusqlite::Result<usize> {
    let mut db = Connection::open_with_flags(db_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    // v2 opens these tables with foreign keys on, so do the same here.
    db.pragma_update(None, "foreign_keys", true)?;
    let tx = db.transaction()?;
    let mut removed = 0;
    for table in ["user_followed_artists", "auto_download_approvals"] {
        removed += tx.execute(
            &format!("DELETE FROM {table} WHERE user_id NOT IN (SELECT id FROM auth_users)"),
            [],
        )?;
    }
    tx.commit()?;
    db.close().map_err(|(_, error)| error)?;
    Ok(removed)
}

fn lock_exclusive(dirs: &DataDirs) -> Result<DataLock, UpgradeError> {
    DataLock::exclusive(&dirs.db_path).map_err(|error| match error {
        held @ DataLockError::Held { .. } => UpgradeError::Locked(held),
        DataLockError::Io { path, reason } => failed("lock", &path, reason),
    })
}

/// Refuse, before anything moves, what the run could not finish.
fn preflight(dirs: &DataDirs) -> Result<(), UpgradeError> {
    if dirs.db_path.parent() != Some(dirs.cache_dir.as_path()) {
        return Err(UpgradeError::UnsupportedPaths {
            reason: "the database is not directly inside the cache folder",
        });
    }
    if let (Ok(config), Ok(cache)) = (
        std::fs::canonicalize(&dirs.config_dir),
        std::fs::canonicalize(&dirs.cache_dir),
    ) && (config.starts_with(&cache) || cache.starts_with(&config))
    {
        return Err(UpgradeError::UnsupportedPaths {
            reason: "the config and cache folders are inside one another",
        });
    }
    for backup in [dirs.config_backup(), dirs.cache_backup()] {
        if std::fs::symlink_metadata(&backup).is_ok() {
            return Err(UpgradeError::BackupExists(backup));
        }
    }
    // The export reads the database file alone, so writes still sitting in
    // the -wal file would be left out.
    if wal_bytes(&dirs.db_path) > 0 {
        return Err(UpgradeError::WalPresent {
            path: wal_path(&dirs.db_path),
        });
    }
    // The two files every export needs. Checked here so a missing one is
    // reported at its real path, with nothing moved.
    v2dir::read_data_enc_key(&dirs.config_dir)?;
    v2dir::read_config(&dirs.config_dir)?;
    check_free_disk(dirs)
}

/// The export bundle and the new database both land in the cache folder
/// while the v2 files are still there. Each is at most the size of the v2
/// database plus the files the bundle carries.
fn check_free_disk(dirs: &DataDirs) -> Result<(), UpgradeError> {
    let database = std::fs::metadata(&dirs.db_path).map_or(0, |meta| meta.len());
    let carried: u64 = CARRIED_FILE_DIRS
        .iter()
        .map(|dir| dir_bytes(&dirs.cache_dir.join(dir)))
        .sum();
    let needed = database
        .saturating_add(carried)
        .saturating_mul(2)
        .saturating_add(DISK_HEADROOM_BYTES);
    let free = droppedneedle_library::adapters::free_bytes_for(&dirs.cache_dir)
        .map_err(|error| failed("measure free space in", &dirs.cache_dir, error))?;
    if free >= needed {
        return Ok(());
    }
    Err(UpgradeError::NotEnoughDisk {
        dir: dirs.cache_dir.clone(),
        needed_mib: needed.div_ceil(MIB),
        free_mib: free / MIB,
    })
}

/// Bytes held by the regular files under `dir`. Links are not followed and
/// an unreadable entry counts as nothing: this only feeds an estimate.
fn dir_bytes(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    let mut total: u64 = 0;
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            total = total.saturating_add(dir_bytes(&entry.path()));
        } else if meta.is_file() {
            total = total.saturating_add(meta.len());
        }
    }
    total
}

/// Rename every v2 entry into its folder's backup.
fn move_aside(dirs: &DataDirs, fault: Option<Fault>) -> Result<(), UpgradeError> {
    write_marker(dirs, Phase::Moving)?;
    for (index, folder) in folders(dirs).iter().enumerate() {
        if index > 0 && fault == Some(Fault::DiesWhileMoving) {
            return Err(UpgradeError::InjectedCrash);
        }
        std::fs::create_dir_all(&folder.dir)
            .map_err(|error| failed("create", &folder.dir, error))?;
        std::fs::create_dir(&folder.backup)
            .map_err(|error| failed("create", &folder.backup, error))?;
        for name in entry_names(&folder.dir, &folder.kept)? {
            let from = folder.dir.join(&name);
            std::fs::rename(&from, folder.backup.join(&name))
                .map_err(|error| failed("move", &from, error))?;
        }
        // Everything else in the folder is deleted on rollback from here
        // on, so nothing of v2's may be left in it.
        if let Some(stray) = entry_names(&folder.dir, &folder.kept)?.first() {
            return Err(failed(
                "move",
                &folder.dir.join(stray),
                "it appeared while the folder was being moved",
            ));
        }
        sync_dir(&folder.backup)?;
        sync_dir(&folder.dir)?;
    }
    write_marker(dirs, Phase::Upgrading)
}

/// Export the backup, check it, and import it into the emptied folders.
/// Returns the import report and how many orphan rows were removed.
async fn convert(
    request: &UpgradeRequest,
    lock: &DataLock,
    progress: &mut dyn FnMut(Step),
) -> Result<(ImportReport, usize), UpgradeError> {
    let dirs = &request.dirs;
    let backup = backup_side(dirs);
    let work = work_dir(dirs);
    create_private_dir(&work)?;
    let passphrase = fresh_passphrase()?;
    let export_path = work.join(EXPORT_FILE_NAME);
    let export = ExportRequest {
        v2: V2Dirs {
            config_dir: backup.config_dir.clone(),
            cache_dir: backup.cache_dir.clone(),
            plugins_dir: dirs.plugins_dir.clone(),
        },
        db_path: Some(backup.db_path.clone()),
        passphrase: passphrase.clone(),
        exported_at: None,
        v2_commit: request.v2_commit.clone(),
    };

    progress(Step::Exporting);
    let mut orphans_removed = 0;
    let export_bytes = match export_checked(&export, &export_path)? {
        Checked::Valid(bytes) => bytes,
        Checked::Refused(issues) if issues.iter().all(|issue| issue.code == DANGLING_USER_REF) => {
            orphans_removed = delete_orphan_rows(&backup.db_path)
                .map_err(|error| UpgradeError::OrphanRepair(error.to_string()))?;
            if orphans_removed == 0 {
                return Err(UpgradeError::Invalid(issues));
            }
            progress(Step::OrphansRemoved(orphans_removed));
            match export_checked(&export, &export_path)? {
                Checked::Valid(bytes) => bytes,
                Checked::Refused(issues) => return Err(UpgradeError::Invalid(issues)),
            }
        }
        Checked::Refused(issues) => return Err(UpgradeError::Invalid(issues)),
    };

    let job = |dry_run: bool| OfflineImport {
        export_bytes: export_bytes.clone(),
        passphrase: passphrase.clone(),
        db_path: dirs.db_path.clone(),
        config_dir: dirs.config_dir.clone(),
        cache_dir: dirs.cache_dir.clone(),
        attachments_dir: work.clone(),
        v2_config: Some(backup.legacy_config_file()),
        dry_run,
        fault_before_commit: !dry_run
            && matches!(
                request.fault,
                Some(Fault::ImportFails | Fault::DiesWhileRollingBack)
            ),
    };
    progress(Step::DryRun);
    require_success(run_offline_import(lock, job(true)).await?)?;
    progress(Step::Importing);
    let report = require_success(run_offline_import(lock, job(false)).await?)?;
    if request.fault == Some(Fault::DiesAfterImport) {
        return Err(UpgradeError::InjectedCrash);
    }

    write_durably(&dirs.report_file(), report.to_json().as_bytes())?;
    remove_entry(&work)?;
    remove_marker(dirs)?;
    Ok((report, orphans_removed))
}

/// An export that either passed the validator or was refused by it.
enum Checked {
    Valid(Vec<u8>),
    Refused(Vec<ValidationIssue>),
}

/// Export to `export_path`, then run the checks the `validate` command
/// runs: the validator, then the bundle against the hash the export names.
fn export_checked(request: &ExportRequest, export_path: &Path) -> Result<Checked, UpgradeError> {
    export_v2_to_file(request, export_path)?;
    let bytes = std::fs::read(export_path).map_err(|error| failed("read", export_path, error))?;
    let parsed = ExportFile::parse(&bytes).map_err(|error| failed("read", export_path, error))?;
    let mut report = validate_export(&parsed.root);
    if report.valid() {
        let folder = export_path.parent().unwrap_or(Path::new("."));
        report
            .errors
            .extend(check_attachments(&parsed.root, folder));
    }
    Ok(if report.valid() {
        Checked::Valid(bytes)
    } else {
        Checked::Refused(report.errors)
    })
}

fn require_success(report: ImportReport) -> Result<ImportReport, UpgradeError> {
    match report.exit.code {
        ExitCode::Ok | ExitCode::OkWithDrops => Ok(report),
        _ => Err(UpgradeError::ImportFailed {
            dry_run: report.dry_run,
            report: Box::new(report),
        }),
    }
}

/// A random passphrase for one run. It seals the export in the work folder
/// and is never written anywhere.
fn fresh_passphrase() -> Result<Secret, UpgradeError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| UpgradeError::RandomFailed)?;
    Ok(Secret::new(STANDARD.encode(bytes)))
}

/// Undo whatever the marker says is under way, then remove the marker.
/// Without a marker there is nothing that is safe to assume, so nothing is
/// touched.
fn roll_back(dirs: &DataDirs, fault: Option<Fault>) -> Result<(), UpgradeError> {
    let Some(phase) = read_marker(dirs) else {
        return Ok(());
    };
    let folders = folders(dirs);
    if phase == Phase::Upgrading {
        // What is about to be deleted may be the only other copy.
        require_backup(dirs)?;
        for folder in &folders {
            for name in entry_names(&folder.dir, &folder.kept)? {
                remove_entry(&folder.dir.join(name))?;
            }
            sync_dir(&folder.dir)?;
        }
        // From here on entries go back into place, and nothing in place
        // may be deleted again.
        write_marker(dirs, Phase::Moving)?;
    }
    for (index, folder) in folders.iter().enumerate() {
        if index > 0 && fault == Some(Fault::DiesWhileRollingBack) {
            return Err(UpgradeError::InjectedCrash);
        }
        for name in entry_names(&folder.backup, &[])? {
            let home = folder.dir.join(&name);
            if std::fs::symlink_metadata(&home).is_ok() {
                return Err(UpgradeError::Occupied(home));
            }
            let from = folder.backup.join(&name);
            std::fs::rename(&from, &home).map_err(|error| failed("move", &from, error))?;
        }
        match std::fs::remove_dir(&folder.backup) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed("remove", &folder.backup, error)),
        }
        if folder.dir.is_dir() {
            sync_dir(&folder.dir)?;
        }
    }
    remove_marker(dirs)
}

/// Both backup folders are there and together hold a v2 instance.
fn require_backup(dirs: &DataDirs) -> Result<(), UpgradeError> {
    let config = dirs.config_backup();
    let cache = dirs.cache_backup();
    let both = [&config, &cache]
        .into_iter()
        .all(|backup| std::fs::symlink_metadata(backup).is_ok_and(|meta| meta.is_dir()));
    if both && detect(&backup_side(dirs)) == Layout::V2 {
        Ok(())
    } else {
        Err(UpgradeError::NoBackup { config, cache })
    }
}

fn delete_backups(dirs: &DataDirs) -> Result<Vec<PathBuf>, UpgradeError> {
    let mut removed = Vec::new();
    for backup in [dirs.config_backup(), dirs.cache_backup()] {
        match std::fs::symlink_metadata(&backup) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed("read", &backup, error)),
            Ok(meta) if meta.is_dir() => {
                std::fs::remove_dir_all(&backup)
                    .map_err(|error| failed("delete", &backup, error))?;
                removed.push(backup);
            }
            Ok(_) => return Err(UpgradeError::NotAFolder(backup)),
        }
    }
    Ok(removed)
}

/// Names in `dir` other than `kept`, sorted. A missing folder is empty.
fn entry_names(dir: &Path, kept: &[OsString]) -> Result<Vec<OsString>, UpgradeError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(failed("list", dir, error)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let name = entry
            .map_err(|error| failed("list", dir, error))?
            .file_name();
        if !kept.contains(&name) {
            names.push(name);
        }
    }
    names.sort();
    Ok(names)
}

/// Delete one entry without following links: a link is removed, never what
/// it points at.
fn remove_entry(path: &Path) -> Result<(), UpgradeError> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(failed("read", path, error)),
    };
    let removed = if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    removed.map_err(|error| failed("delete", path, error))
}

/// What the rollback may do, as the marker records it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// Entries are moving between the folders and the backup. Nothing
    /// outside the backup may be deleted.
    Moving,
    /// Every v2 entry is inside the backup. Anything else in the folders
    /// was created after the move and may be deleted.
    Upgrading,
}

impl Phase {
    fn name(self) -> &'static str {
        match self {
            Self::Moving => "moving",
            Self::Upgrading => "upgrading",
        }
    }
}

fn write_marker(dirs: &DataDirs, phase: Phase) -> Result<(), UpgradeError> {
    let body = serde_json::json!({
        "format": MARKER_FORMAT,
        "phase": phase.name(),
        "note": "An upgrade from v2 is under way or stopped halfway. Run \
                 `droppedneedle-tool upgrade` to finish it. Do not delete the v2-backup folders.",
    });
    write_durably(&dirs.marker(), body.to_string().as_bytes())
}

/// The marker's phase, `None` without a marker. A marker that cannot be
/// read counts as `Moving`, the phase that deletes nothing.
fn read_marker(dirs: &DataDirs) -> Option<Phase> {
    let text = match std::fs::read_to_string(dirs.marker()) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => return Some(Phase::Moving),
    };
    let upgrading = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|marker| {
            marker
                .get("phase")
                .and_then(serde_json::Value::as_str)
                .map(|phase| phase == Phase::Upgrading.name())
        })
        .unwrap_or(false);
    Some(if upgrading {
        Phase::Upgrading
    } else {
        Phase::Moving
    })
}

fn remove_marker(dirs: &DataDirs) -> Result<(), UpgradeError> {
    let marker = dirs.marker();
    for path in [tmp_path(&marker), marker] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(failed("remove", &path, error)),
        }
    }
    // The marker is gone, so the work is done. A flush that fails now must
    // not turn a finished run into a failed one.
    let _ = sync_dir(&dirs.cache_dir);
    Ok(())
}

fn tmp_name(name: &OsStr) -> OsString {
    let mut tmp = name.to_owned();
    tmp.push(".tmp");
    tmp
}

fn tmp_path(path: &Path) -> PathBuf {
    path.with_file_name(tmp_name(path.file_name().unwrap_or_default()))
}

/// Write an owner-only file through a temporary name and a rename, and
/// flush the file and its folder to disk. A failed write leaves no
/// temporary file behind.
fn write_durably(path: &Path, bytes: &[u8]) -> Result<(), UpgradeError> {
    let tmp = tmp_path(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let written = options
        .open(&tmp)
        .and_then(|mut file| {
            file.write_all(bytes)?;
            file.sync_all()
        })
        .and_then(|()| std::fs::rename(&tmp, path));
    if let Err(error) = written {
        // The write error is the one to report, whatever the cleanup does.
        let _ = std::fs::remove_file(&tmp);
        return Err(failed("write", path, error));
    }
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

/// Flush a folder's entries (renames, new files, deletions) to disk.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> Result<(), UpgradeError> {
    std::fs::File::open(dir)
        .and_then(|handle| handle.sync_all())
        .map_err(|error| failed("sync", dir, error))
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> Result<(), UpgradeError> {
    Ok(())
}

/// The work folder holds password hashes and listening history.
fn create_private_dir(dir: &Path) -> Result<(), UpgradeError> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|error| failed("create", dir, error))
}

/// One offline import or dry run.
pub struct OfflineImport {
    /// The export file's bytes.
    pub export_bytes: Vec<u8>,
    /// Passphrase that opens the export.
    pub passphrase: Secret,
    /// Target v3 database, created and migrated when missing.
    pub db_path: PathBuf,
    /// Target v3 config folder: the key file, and maybe a `config.json`
    /// from before settings moved into the database.
    pub config_dir: PathBuf,
    /// Target v3 cache folder, where avatars and held downloads land.
    pub cache_dir: PathBuf,
    /// Folder holding the export's bundle.
    pub attachments_dir: PathBuf,
    /// v2 `config.json` for the one-shot scan-schedule carry.
    pub v2_config: Option<PathBuf>,
    /// Decide and count everything, write no records.
    pub dry_run: bool,
    /// Test-only: fail the import before it commits.
    pub fault_before_commit: bool,
}

/// Why an offline import could not start. The messages are the ones the
/// `import` and `dry-run` commands print.
#[derive(Debug, Error)]
pub enum OfflineImportError {
    /// The target database cannot be opened.
    #[error("cannot open target database: {0}")]
    Open(sqlx::Error),
    /// The target schema could not be migrated.
    #[error(transparent)]
    Schema(#[from] SchemaError),
    /// Another writer holds the target database.
    #[error("target database is locked (stop the server before importing)")]
    DatabaseLocked,
    /// The write-lock probe could not be released.
    #[error("cannot probe target database: {0}")]
    Probe(sqlx::Error),
    /// The v3 key could not be loaded or created.
    #[error(transparent)]
    Key(#[from] CryptoError),
    /// The target holds v3 data whose key file is missing, or is not the
    /// key the data was sealed with.
    #[error(transparent)]
    LostKey(BootRefusal),
    /// The target's settings could not be opened (a `config.json` from
    /// before settings moved into the database that does not read).
    #[error("cannot open the target's settings: {0}")]
    Settings(String),
}

/// Open and migrate the target, make sure nobody else writes to it, and
/// run the import pipeline. The caller holds the data lock for the whole
/// run; `_lock` is the proof.
///
/// The pool is writable and not the serving runtime's, which turns
/// read-only after boot: an offline import owns its writes. It is closed
/// before this returns, so the database is left cleanly closed.
pub async fn run_offline_import(
    _lock: &DataLock,
    job: OfflineImport,
) -> Result<ImportReport, OfflineImportError> {
    refuse_lost_key(&job.config_dir, &job.db_path).map_err(OfflineImportError::LostKey)?;
    let (crypto, legacy) = if job.dry_run {
        // Throwaway in-memory key: a dry run re-encrypts into settings it
        // discards, so no key file may be minted here. Without a key file
        // the real run would mint one, and so would leave config.json
        // unread.
        let key_absent = !job.config_dir.join(KEY_FILE_NAME).exists();
        // A real key beside a database is checked now, writing nothing, so
        // a key that does not belong shows before the real run.
        if !key_absent && job.db_path.is_file() {
            prepare_target(&job.db_path, &job.config_dir, None, false).await?;
        }
        (
            Crypto::from_key_bytes(&[42u8; 32])?,
            LegacyConfig::at(&job.config_dir, key_absent),
        )
    } else {
        if !job.config_dir.join(KEY_FILE_NAME).exists() {
            mark_skip(&job.config_dir)
                .map_err(|error| OfflineImportError::Settings(error.to_string()))?;
        }
        let (crypto, origin) = Crypto::load_or_generate(&job.config_dir)?;
        let legacy = LegacyConfig::at(&job.config_dir, origin == KeyOrigin::Minted);
        prepare_target(&job.db_path, &job.config_dir, Some(&legacy), false).await?;
        (crypto, legacy)
    };
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite:{}?mode=rwc", job.db_path.display()))
        .await
        .map_err(OfflineImportError::Open)?;
    let (dry_run, db_path, config_dir) = (job.dry_run, job.db_path.clone(), job.config_dir.clone());
    let outcome = import_into(&pool, job, crypto, legacy).await;
    pool.close().await;
    // A finished import stores the key's check value; a refused one
    // leaves the settings as they were.
    if let Ok(report) = &outcome
        && !dry_run
        && matches!(report.exit.code, ExitCode::Ok | ExitCode::OkWithDrops)
    {
        prepare_target(&db_path, &config_dir, None, true).await?;
    }
    outcome
}

/// Ready the target the way the server's start does, with no other pool
/// open on it: migrate it, copy in a `config.json` it never started on
/// (when `legacy` is given and the file exists), then check that the key
/// file belongs to the database. With `record`, a key that belongs has its
/// check value stored when none is yet.
async fn prepare_target(
    db_path: &Path,
    config_dir: &Path,
    legacy: Option<&LegacyConfig>,
    record: bool,
) -> Result<(), OfflineImportError> {
    let failed = |error: &dyn std::fmt::Display| OfflineImportError::Settings(error.to_string());
    let runtime = open_runtime(&DbConfig::new(db_path))
        .await
        .map_err(|error| failed(&error))?;
    let outcome = async {
        if let Some(legacy) = legacy
            && config_dir.join(LEGACY_CONFIG_FILE_NAME).exists()
        {
            let crypto = Crypto::load(config_dir)?;
            ConfigStore::open(
                runtime.db().clone(),
                crypto,
                std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
                legacy.clone(),
            )
            .await
            .map_err(|error| failed(&error))?;
        }
        let (db, dir) = (runtime.db().clone(), config_dir.to_path_buf());
        let belongs = tokio::task::spawn_blocking(move || {
            let crypto = Crypto::load(&dir)?;
            if record {
                enforce(&db, &crypto, &droppedneedle_kernel::clock::SystemClock)
                    .map(|outcome| outcome != KeyCheck::Mismatch)
                    .map_err(|error| failed(&error))
            } else {
                db.read_blocking("import.key_check", |conn| assess(conn, &crypto))
                    .map(|outcome| outcome != Assessment::Mismatch)
                    .map_err(|error| failed(&error))
            }
        })
        .await
        .map_err(|error| failed(&error))??;
        if belongs {
            Ok(())
        } else {
            Err(OfflineImportError::LostKey(key_refusal(
                config_dir, db_path, false,
            )))
        }
    }
    .await;
    runtime.shutdown().await;
    outcome
}

async fn import_into(
    pool: &SqlitePool,
    job: OfflineImport,
    crypto: Crypto,
    legacy: LegacyConfig,
) -> Result<ImportReport, OfflineImportError> {
    droppedneedle_kernel::schema::apply_migrations(pool).await?;
    probe_write_lock(pool).await?;
    Ok(run_import(ImportRequest {
        export_bytes: job.export_bytes,
        passphrase: job.passphrase.expose().to_owned(),
        pool: pool.clone(),
        legacy,
        crypto,
        v2_config_path: job.v2_config,
        attachments_dir: Some(job.attachments_dir),
        cache_dir: Some(job.cache_dir),
        dry_run: job.dry_run,
        fault_before_commit: job.fault_before_commit,
        fault_after_commit: false,
        fault_after_sections: None,
    })
    .await)
}

/// Refuse a database another writer holds: one connection takes an
/// immediate lock and releases it, so a live server fails the import
/// before anything is decided.
async fn probe_write_lock(pool: &SqlitePool) -> Result<(), OfflineImportError> {
    let mut connection = pool.acquire().await.map_err(OfflineImportError::Open)?;
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .map_err(|_| OfflineImportError::DatabaseLocked)?;
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .map_err(OfflineImportError::Probe)?;
    Ok(())
}
