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
//! The flow is stage into the backups directory, fold the staging WAL into
//! the main image, `integrity_check` plus `foreign_key_check` on the staging
//! copy, atomic rename into place, then rotate older backups out.
//! Verification always precedes rotation: a failed backup leaves the previous
//! five untouched. Restore takes a backup back into an empty directory and
//! refuses occupied targets and version-newer backups, mirroring the v2
//! restore guard.
//!
//! A backup carries the undo folder too. The copies a backup's database
//! names are copied into one pool beside the backups, `backups/undo`,
//! shared by every kept backup, so each backup adds only the copies that
//! are new since the last one; after rotation the pool keeps only what a
//! kept backup names. A restore puts the copies back first and the
//! database last, so a crash in between never leaves a database naming
//! copies that are not there.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags, backup::Backup};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    error::DbError,
    factory::sqlite_uri_path,
    fs::same_filesystem,
    sql,
    undo_dir::{self, Copied, Hold, Stored, UNDO_DIR_NAME, UndoDir, shard_names, sync_dir},
    writer::CancelFlag,
};
use crate::clock::Clock;

/// Pages copied per backup step. The v2 `pages=256` value, unchanged.
pub const BACKUP_STEP_PAGES: i32 = 256;
/// Pause between steps so live writers slip through.
const BACKUP_STEP_PAUSE: Duration = Duration::from_millis(10);
/// Rolling retention in the backups directory.
pub const BACKUP_KEEP: usize = 5;
/// Manifest sidecar suffix: `<backup>.manifest.json`.
pub const MANIFEST_SUFFIX: &str = ".manifest.json";
/// Left in a restore's target until the restored database has its name.
pub const RESTORE_MARKER: &str = ".restore-in-progress";
/// The manifest format this build writes.
const MANIFEST_FORMAT: u32 = 2;

/// Progress callback: `(copied_pages, total_pages)`.
pub type BackupProgress = Arc<dyn Fn(u64, u64) + Send + Sync>;

/// Recorded with every backup: identity, size, and schema stamp.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupManifest {
    /// Format marker for future readers.
    pub format_version: u32,
    /// Hex SHA-256 of the backup file.
    pub sha256: String,
    /// Backup file size in bytes.
    pub size_bytes: u64,
    /// `user_version` stamped in the backup file.
    pub user_version: i64,
    /// When the backup was taken.
    pub created_at: SystemTime,
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
}

/// Backup and restore against one live database file.
pub struct BackupService {
    db_path: PathBuf,
    backup_dir: PathBuf,
    clock: Arc<dyn Clock>,
    staging_seq: AtomicU64,
    live: Mutex<HashSet<PathBuf>>,
}

impl std::fmt::Debug for BackupService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupService")
            .field("db_path", &self.db_path)
            .field("backup_dir", &self.backup_dir)
            .finish_non_exhaustive()
    }
}

impl BackupService {
    /// Wire the service; `clock` dates and names each backup. The
    /// directory is created on the first backup.
    pub fn new(db_path: &Path, backup_dir: &Path, clock: Arc<dyn Clock>) -> Self {
        Self {
            db_path: db_path.to_owned(),
            backup_dir: backup_dir.to_owned(),
            clock,
            staging_seq: AtomicU64::new(0),
            live: Mutex::new(HashSet::new()),
        }
    }

    /// Now, as wall-clock milliseconds, by the service's clock.
    fn now_millis(&self) -> u64 {
        u64::try_from(self.clock.now_ms()).unwrap_or(0)
    }

    /// Backups directory this service rotates within.
    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// Copy the live database out: sweep stale staging, stage, verify,
    /// copy the undo copies it names, rename, rotate. The progress
    /// callback fires after every step with copied and total page counts.
    /// Blocking SQLite and file work runs off the async runtime. A tripped
    /// `cancel` stops the copy between steps and removes the staging file;
    /// nothing renames or rotates.
    pub async fn backup(
        &self,
        progress: Option<BackupProgress>,
        cancel: &CancelFlag,
    ) -> Result<BackupReport, DbError> {
        std::fs::create_dir_all(&self.backup_dir)?;
        if !same_filesystem(&self.db_path, &self.backup_dir)? {
            return Err(DbError::StagingCrossFilesystem {
                path: self.backup_dir.clone(),
            });
        }
        self.sweep_stale_staging();
        let (staging, seq) = self.staging_path();
        self.mark_live(&staging);
        let outcome = self
            .backup_inner(&staging, seq, progress, cancel)
            .await
            .inspect_err(|_| {
                remove_staging_tree(&staging);
            });
        self.unmark_live(&staging);
        outcome
    }

    /// Stage, verify, rename, and rotate one backup into its final name.
    async fn backup_inner(
        &self,
        staging: &Path,
        seq: u64,
        progress: Option<BackupProgress>,
        cancel: &CancelFlag,
    ) -> Result<BackupReport, DbError> {
        let source_path = self.db_path.clone();
        let created_at = UNIX_EPOCH + Duration::from_millis(self.now_millis());
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
        let copied = tokio::task::spawn_blocking(move || {
            copy_live(&source_path, &staging_for_copy, progress, &cancel_for_copy)
        })
        .await
        .map_err(|_| DbError::WriteFailed {
            operation: "backup".to_owned(),
            cause: "backup task failed to join".to_owned(),
        })??;
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled {
                operation: "backup".to_owned(),
                chunks: copied as usize,
            });
        }
        // Verify, hash, copy the undo copies, publish and rotate are file
        // and SQLite work: keep them off the async workers like the copy
        // itself.
        let staging = staging.to_owned();
        let backup_dir = self.backup_dir.clone();
        let cancel = cancel.clone();
        let (path, manifest) = tokio::task::spawn_blocking(move || {
            let (_live_hold, pool_hold) = holds;
            fold_staging(&staging)?;
            compact_staging(&staging)?;
            let mut manifest = verify_staging(&staging, created_at)?;
            let undo = copy_undo(&staging, &live, &pool, &pool_hold, &cancel)?;
            manifest.undo_blobs = undo.blobs;
            manifest.undo_bytes = undo.bytes;
            manifest.undo_missing = undo.missing;
            remove_staging_sidecars(&staging);
            std::fs::rename(&staging, &final_path)?;
            let sidecar = manifest_path(&final_path);
            std::fs::write(&sidecar, serde_json::to_string_pretty(&manifest)?)?;
            rotate_backups(&backup_dir, BACKUP_KEEP)?;
            sync_dir(&backup_dir)?;
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

    /// Restore a backup file into an empty directory as `library.db`,
    /// with the undo copies it names beside it in `undo`.
    ///
    /// Refuses occupied targets and, unless `allow_downgrade`, backups
    /// newer than this binary. A target holding only what an interrupted
    /// restore left (its marker, its temporary files, `undo`, and no
    /// `library.db`) is cleared and the restore starts over. A copy the
    /// backup lacks does not stop the restore: it is listed in
    /// `undo_missing`, and Undo then skips only the files that need it.
    /// The restored database passes `integrity_check` before it takes its
    /// name.
    pub fn restore(
        &self,
        backup_db: &Path,
        target_dir: &Path,
        allow_downgrade: bool,
    ) -> Result<RestoredBackup, DbError> {
        clear_unfinished_restore(target_dir)?;
        let source = Connection::open_with_flags(backup_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let found = user_version(&source)?;
        let expected = crate::schema::latest_version();
        if found > expected && !allow_downgrade {
            return Err(DbError::BackupTooNew { found, expected });
        }
        check_clean(&source)?;
        drop(source);

        std::fs::create_dir_all(target_dir)?;
        let marker = target_dir.join(RESTORE_MARKER);
        std::fs::File::create(&marker)?.sync_all()?;
        sync_dir(target_dir)?;
        match self.restore_files(backup_db, target_dir) {
            Ok(restored) => {
                std::fs::remove_file(&marker)?;
                sync_dir(target_dir)?;
                Ok(restored)
            }
            Err(error) => {
                if let Err(cleanup) = clear_unfinished_restore(target_dir) {
                    tracing::warn!(%cleanup, "a failed restore left files in its target");
                }
                Err(error)
            }
        }
    }

    /// The copying half of [`restore`](Self::restore), under its marker:
    /// the database to a temporary name, the undo copies into a temporary
    /// folder, then the folder and last the database to their names.
    fn restore_files(
        &self,
        backup_db: &Path,
        target_dir: &Path,
    ) -> Result<RestoredBackup, DbError> {
        let seq = self.staging_seq.fetch_add(1, Ordering::Relaxed);
        let stem = format!(
            ".restore-{}-{}-{}",
            std::process::id(),
            self.now_millis(),
            seq
        );
        let temp = target_dir.join(format!("{stem}.tmp"));
        let bytes = std::fs::copy(backup_db, &temp)?;
        std::fs::File::open(&temp)?.sync_all()?;
        let (user_version, named) = {
            let restored = open_immutable(&temp)?;
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
        std::fs::rename(staged.path(), target_dir.join(UNDO_DIR_NAME))?;
        sync_dir(target_dir)?;
        let target = target_dir.join("library.db");
        std::fs::rename(&temp, &target)?;
        sync_dir(target_dir)?;
        Ok(RestoredBackup {
            path: target,
            bytes,
            user_version,
            undo_blobs,
            undo_missing,
        })
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
    /// backup owns. A poisoned lock skips the sweep rather than blocking.
    fn sweep_stale_staging(&self) {
        let live = self
            .live
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default();
        let entries = match std::fs::read_dir(&self.backup_dir) {
            Ok(entries) => entries,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let leftover = name.starts_with(".staging-")
                || (name.starts_with(".restore-") && name.ends_with(".tmp"));
            if !leftover || live.contains(&entry.path()) {
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

/// The undo copies a database file names that live in the undo folder
/// rather than in its own table. A database from before the undo tables
/// names none. The file is opened read-only and immutable, so nothing is
/// written beside it.
pub fn named_undo_copies(path: &Path) -> Result<Vec<String>, DbError> {
    named_in(&open_immutable(path)?)
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
fn open_immutable(path: &Path) -> rusqlite::Result<Connection> {
    Connection::open_with_flags(
        format!("file:{}?immutable=1", sqlite_uri_path(path)),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )
}

/// What a backup's copy of the undo folder holds.
#[derive(Debug, Default)]
struct UndoCopied {
    blobs: u64,
    bytes: u64,
    missing: Vec<String>,
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
    let mut copied = UndoCopied::default();
    for sha256 in named_undo_copies(staging)? {
        if cancel.is_cancelled() {
            return Err(DbError::Cancelled {
                operation: "backup".to_owned(),
                chunks: copied.blobs as usize,
            });
        }
        match live.copy_to(pool, pool_hold, &sha256)? {
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
                copied.missing.push(sha256);
            }
        }
    }
    Ok(copied)
}

/// Remove from the pool what no kept backup names, and files a copy left
/// mid-write. Fails before removing anything when a kept backup cannot
/// be read: its names are unknown, so nothing is guessed away.
fn clean_pool(backup_dir: &Path, pool: &UndoDir, hold: &Hold) -> Result<(), DbError> {
    let mut keep = HashSet::new();
    for backup in finished_backups(backup_dir)? {
        keep.extend(named_undo_copies(&backup)?);
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

/// Clear what an interrupted restore left in `target_dir`, or refuse a
/// target that holds anything else. The marker, the `.restore-` files and
/// `undo` are an interrupted restore's own only while no `library.db`
/// sits beside them; a marker beside a database is stale and clears
/// nothing.
fn clear_unfinished_restore(target_dir: &Path) -> Result<(), DbError> {
    if !target_dir.exists() {
        return Ok(());
    }
    let lock_name = crate::db::datalock::lock_file_name("library.db");
    let server_lock_name = crate::db::datalock::server_lock_file_name("library.db");
    let mut names = Vec::new();
    for entry in target_dir.read_dir()? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name != lock_name && name != server_lock_name {
            names.push(name);
        }
    }
    if names.is_empty() {
        return Ok(());
    }
    let unfinished = names.iter().any(|name| name == RESTORE_MARKER)
        && names
            .iter()
            .all(|name| name.starts_with(".restore-") || name == UNDO_DIR_NAME);
    if !unfinished {
        return Err(DbError::RestoreTargetNotEmpty {
            path: target_dir.to_owned(),
        });
    }
    for name in names.iter().filter(|name| name.as_str() != RESTORE_MARKER) {
        let path = target_dir.join(name);
        if path.is_dir() {
            std::fs::remove_dir_all(&path)?;
        } else {
            std::fs::remove_file(&path)?;
        }
    }
    std::fs::remove_file(target_dir.join(RESTORE_MARKER))?;
    sync_dir(target_dir)?;
    Ok(())
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
/// self-contained. Runs before verification so the manifest hashes the
/// final bytes; verification itself recreates empty sidecars (even a
/// read-only open does), so the caller drops them again before the rename.
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
    let mut name = staging.as_os_str().to_owned();
    name.push(".compact");
    let compact = PathBuf::from(name);
    remove_staging_tree(&compact);
    connection.execute("VACUUM INTO ?1", [compact.to_string_lossy()])?;
    drop(connection);
    remove_staging_sidecars(staging);
    std::fs::rename(&compact, staging)?;
    remove_staging_sidecars(&compact);
    Ok(())
}

/// Remove a staging file and any of its sidecars. Best effort.
fn remove_staging_tree(staging: &Path) {
    let _ = std::fs::remove_file(staging);
    remove_staging_sidecars(staging);
}

/// Remove the `-wal`/`-shm`/`-journal` sidecars of a staging file.
fn remove_staging_sidecars(staging: &Path) {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = staging.as_os_str().to_owned();
        sidecar.push(suffix);
        let _ = std::fs::remove_file(Path::new(&sidecar));
    }
}

/// Verify a staging copy: integrity plus foreign keys, then identity.
fn verify_staging(staging: &Path, created_at: SystemTime) -> Result<BackupManifest, DbError> {
    let connection = Connection::open_with_flags(staging, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    if check_clean(&connection).is_err() {
        drop(connection);
        let _ = std::fs::remove_file(staging);
        return Err(DbError::IntegrityFailed);
    }
    let user_version = user_version(&connection)?;
    drop(connection);
    let (sha256, size_bytes) = file_sha256(staging)?;
    Ok(BackupManifest {
        format_version: MANIFEST_FORMAT,
        sha256,
        size_bytes,
        user_version,
        created_at,
        undo_blobs: 0,
        undo_bytes: 0,
        undo_missing: Vec::new(),
    })
}

/// Lowercase hex SHA-256 and byte length of a file, read in 64 KiB chunks
/// so a catalog-sized database never sits in memory whole. Blocking: call
/// from a blocking thread.
pub fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    let mut file = std::io::BufReader::with_capacity(64 * 1024, std::fs::File::open(path)?);
    let mut hasher = Sha256::new();
    let total = std::io::copy(&mut file, &mut hasher)?;
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

/// `user_version` stamped in a database file.
fn user_version(connection: &Connection) -> rusqlite::Result<i64> {
    connection.query_row("PRAGMA user_version", [], |row| row.get(0))
}

/// Keep the newest `keep` backups; older files and sidecars rotate out only
/// after the new backup verified.
fn rotate_backups(backup_dir: &Path, keep: usize) -> Result<(), DbError> {
    let mut backups = finished_backups(backup_dir)?;
    while backups.len() > keep {
        let oldest = backups.remove(0);
        let _ = std::fs::remove_file(manifest_path(&oldest));
        let _ = std::fs::remove_file(&oldest);
    }
    Ok(())
}

/// The finished backup files in `backup_dir`, oldest first.
fn finished_backups(backup_dir: &Path) -> Result<Vec<PathBuf>, DbError> {
    let mut backups: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(backup_dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".db") && !name.starts_with(".staging-") && !name.starts_with(".restore-")
        {
            backups.push(path);
        }
    }
    backups.sort();
    Ok(backups)
}

/// Manifest sidecar path for a backup file.
fn manifest_path(backup: &Path) -> PathBuf {
    let mut name = backup.as_os_str().to_owned();
    name.push(MANIFEST_SUFFIX);
    PathBuf::from(name)
}

/// Sortable backup file name from wall-clock milliseconds plus the staging
/// sequence, so two backups in the same millisecond never collide.
fn backup_file_name(now: SystemTime, seq: u64) -> String {
    let millis = now
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis())
        .unwrap_or(0);
    format!("library-{millis}-{seq}.db")
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

    #[test]
    fn backup_names_sort_in_creation_order() {
        let first = backup_file_name(UNIX_EPOCH + Duration::from_millis(10), 0);
        let second = backup_file_name(UNIX_EPOCH + Duration::from_millis(11), 1);
        assert!(first < second);
        assert!(first.ends_with(".db"));
    }

    #[test]
    fn backup_names_share_no_millisecond_collision() {
        let same_ms = UNIX_EPOCH + Duration::from_millis(10);
        let first = backup_file_name(same_ms, 0);
        let second = backup_file_name(same_ms, 1);
        assert_ne!(first, second);
        assert!(first < second);
    }

    #[test]
    fn rotation_keeps_the_newest_five() {
        let dir = std::env::temp_dir().join(format!("db-rotate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for index in 0..7 {
            let name = format!("library-{index}.db");
            std::fs::write(dir.join(&name), b"x").unwrap();
            std::fs::write(dir.join(format!("{name}{MANIFEST_SUFFIX}")), b"{}").unwrap();
        }
        rotate_backups(&dir, 5).unwrap();
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            vec![
                "library-2.db",
                "library-2.db.manifest.json",
                "library-3.db",
                "library-3.db.manifest.json",
                "library-4.db",
                "library-4.db.manifest.json",
                "library-5.db",
                "library-5.db.manifest.json",
                "library-6.db",
                "library-6.db.manifest.json",
            ]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
