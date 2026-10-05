//! Online backup and restore: verified copies, then rotation.
//!
//! Backup uses the SQLite Online Backup API stepped at 256 pages, matching
//! the v2 capture path: each step holds the source lock briefly and yields
//! between steps, so foreground writes keep their lane while a backup runs.
//! `VACUUM INTO` is rejected as the mechanism on purpose: it holds a shared
//! lock for its whole run and would stall the writer lane for seconds on a
//! large catalog.
//!
//! The flow is stage into the backups directory, fold the staging WAL into
//! the main image, `integrity_check` plus `foreign_key_check` on the staging
//! copy, atomic rename into place, then rotate older backups out.
//! Verification always precedes rotation: a failed backup leaves the previous
//! five untouched. Restore takes a backup back into an empty directory and
//! refuses occupied targets and version-newer backups, mirroring the v2
//! restore guard.

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

use super::{error::DbError, fs::same_filesystem, writer::CancelFlag};

/// Pages copied per backup step. The v2 `pages=256` value, unchanged.
pub const BACKUP_STEP_PAGES: i32 = 256;
/// Pause between steps so live writers slip through.
const BACKUP_STEP_PAUSE: Duration = Duration::from_millis(10);
/// Rolling retention in the backups directory.
pub const BACKUP_KEEP: usize = 5;
/// Manifest sidecar suffix: `<backup>.manifest.json`.
pub const MANIFEST_SUFFIX: &str = ".manifest.json";

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
}

/// Backup and restore against one live database file.
#[derive(Debug)]
pub struct BackupService {
    db_path: PathBuf,
    backup_dir: PathBuf,
    staging_seq: AtomicU64,
    live: Mutex<HashSet<PathBuf>>,
}

impl BackupService {
    /// Wire the service. The directory is created on the first backup.
    pub fn new(db_path: &Path, backup_dir: &Path) -> Self {
        Self {
            db_path: db_path.to_owned(),
            backup_dir: backup_dir.to_owned(),
            staging_seq: AtomicU64::new(0),
            live: Mutex::new(HashSet::new()),
        }
    }

    /// Backups directory this service rotates within.
    pub fn backup_dir(&self) -> &Path {
        &self.backup_dir
    }

    /// Copy the live database out: sweep stale staging, stage, verify,
    /// rename, rotate. The progress callback fires after every step with
    /// copied and total page counts. Blocking SQLite work runs off the
    /// async runtime. A tripped `cancel` stops the copy between steps and
    /// removes the staging file; nothing renames or rotates.
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
        let final_name = backup_file_name(SystemTime::now(), seq);
        let final_path = self.backup_dir.join(&final_name);
        let started = Instant::now();
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
        // Verify, hash, publish and rotate are file and SQLite work: keep
        // them off the async workers like the copy itself.
        let staging = staging.to_owned();
        let backup_dir = self.backup_dir.clone();
        let (path, manifest) = tokio::task::spawn_blocking(move || {
            fold_staging(&staging)?;
            let manifest = verify_staging(&staging)?;
            remove_staging_sidecars(&staging);
            std::fs::rename(&staging, &final_path)?;
            let sidecar = manifest_path(&final_path);
            std::fs::write(&sidecar, serde_json::to_string_pretty(&manifest)?)?;
            rotate_backups(&backup_dir, BACKUP_KEEP)?;
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

    /// Restore a backup file into an empty directory as `library.db`.
    ///
    /// Refuses occupied targets and, unless `allow_downgrade`, backups
    /// newer than this binary. The restored copy passes `integrity_check`
    /// before this returns.
    pub fn restore(
        &self,
        backup_db: &Path,
        target_dir: &Path,
        allow_downgrade: bool,
    ) -> Result<RestoredBackup, DbError> {
        if target_dir.exists() && target_dir.read_dir()?.next().is_some() {
            return Err(DbError::RestoreTargetNotEmpty {
                path: target_dir.to_owned(),
            });
        }
        let source = Connection::open_with_flags(backup_db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        let found = user_version(&source)?;
        let expected = crate::schema::latest_version();
        if found > expected && !allow_downgrade {
            return Err(DbError::BackupTooNew { found, expected });
        }
        check_clean(&source)?;
        drop(source);

        std::fs::create_dir_all(target_dir)?;
        let target = target_dir.join("library.db");
        let temp = target_dir.join(format!(
            ".restore-{}-{}.tmp",
            std::process::id(),
            now_millis()
        ));
        let bytes = std::fs::copy(backup_db, &temp)?;
        std::fs::rename(&temp, &target)?;
        let restored = Connection::open_with_flags(&target, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        check_clean(&restored)?;
        let user_version = user_version(&restored)?;
        drop(restored);
        Ok(RestoredBackup {
            path: target,
            bytes,
            user_version,
        })
    }

    /// Unique staging path inside the backups directory, with its sequence
    /// number for the final name.
    fn staging_path(&self) -> (PathBuf, u64) {
        let seq = self.staging_seq.fetch_add(1, Ordering::Relaxed);
        let path = self.backup_dir.join(format!(
            ".staging-{}-{}-{}.db",
            std::process::id(),
            now_millis(),
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
fn verify_staging(staging: &Path) -> Result<BackupManifest, DbError> {
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
        format_version: 1,
        sha256,
        size_bytes,
        user_version,
        created_at: SystemTime::now(),
    })
}

/// Lowercase hex SHA-256 and byte length of a file, read in 64 KiB chunks
/// so a catalog-sized database never sits in memory whole. Blocking: call
/// from a blocking thread.
pub fn file_sha256(path: &Path) -> std::io::Result<(String, u64)> {
    use std::io::Read as _;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        total += read as u64;
    }
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
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        })
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
    while backups.len() > keep {
        let oldest = backups.remove(0);
        let _ = std::fs::remove_file(manifest_path(&oldest));
        let _ = std::fs::remove_file(&oldest);
    }
    Ok(())
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

/// Wall-clock milliseconds for unique temp names.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_millis() as u64)
        .unwrap_or(0)
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
