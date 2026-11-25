//! Backup UX: list, run, pre-restore reports, and the nightly loop.
//!
//! The mechanism lives in [`droppedneedle_kernel::db::backup`]; this module is the admin
//! face over it. Each kind of backup is kept by its own rule
//! ([`RETENTION`]), and verification always precedes rotation, so a failed
//! run removes nothing.
//!
//! Two simplifications on purpose: backup runs are synchronous (a
//! catalog-sized backup finishes in seconds, and no client needs progress
//! polling yet; revisit if slow devices show otherwise), and the restore
//! itself stays an offline CLI (`droppedneedle-tool restore`). The
//! restore-report route verifies read-only and never moves the live
//! database.

use std::{
    path::Path,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use rusqlite::Connection;

use super::{
    error::AdminError,
    models::{
        BackupKeep, BackupListResponse, BackupRunResponse, BackupView, RestoreCheck, RestoreReport,
    },
};
use droppedneedle_kernel::db::{
    BackupError, BackupKind, BackupManifest, BackupReport, BackupService, RETENTION, SchemaError,
    Stamp, UndoDir,
    backup::{
        PACKED_SUFFIX, PLAIN_SUFFIX, backup_order, backup_undo_names, is_backup_name,
        is_packed_name, manifest_path, open_immutable,
    },
    undo_dir::UNDO_DIR_NAME,
    writer::CancelFlag,
};
use droppedneedle_kernel::jobs::{
    AlreadyRunning, CancellationToken, Next, Schedule, Supervisor, Tick, TickCtx, TickError,
    TickResult,
};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::Backups;

/// The restore report's check on the backup's undo copies. A restore
/// does not stop on it.
pub const UNDO_COPIES_CHECK: &str = "undo-copies";

/// The restore report's check that the schema runner would open the
/// backup at all: not an older pre-release stamp, not tables with no stamp.
/// Unlike `schema-version`, `--allow-downgrade` never overrides it.
pub const SCHEMA_FLOOR_CHECK: &str = "schema-floor";

/// The nightly backup loop's name in the jobs view.
pub const BACKUP_JOB: &str = "database-backup";
/// How often the loop looks whether a nightly backup is due.
const BACKUP_TICK: Duration = Duration::from_secs(30 * 60);
/// The loop's first look after boot.
const BACKUP_FIRST_TICK: Duration = Duration::from_secs(10 * 60);
/// A nightly backup is due once the newest scheduled one is this old.
const NIGHTLY_EVERY_MS: i64 = 24 * 60 * 60 * 1000;
/// After an integrity failure the loop tries again only this much later.
const INTEGRITY_HOLD_MS: i64 = 6 * 60 * 60 * 1000;

/// The loop's codes for a failed pass.
pub const BACKUP_INTEGRITY_FAILED: &str = "BACKUP_INTEGRITY_FAILED";
/// The backups folder has no room for a backup.
pub const BACKUP_NO_ROOM: &str = "BACKUP_NO_ROOM";
/// A backup failed otherwise; the log says why.
pub const BACKUP_FAILED: &str = "BACKUP_FAILED";

/// List the backups on disk, oldest first, with whether the boot check found the database
/// held in memory. Blocking file IO. Files without a manifest sidecar still list with their
/// on-disk size and as manual; only the manifest fields stay empty.
pub fn list_backups(
    backup_dir: &Path,
    database_in_memory: bool,
) -> Result<BackupListResponse, AdminError> {
    let keep = BackupKeep {
        daily: RETENTION.daily as u32,
        weekly: RETENTION.weekly as u32,
        pre_upgrade: RETENTION.pre_upgrade as u32,
        manual: RETENTION.manual as u32,
    };
    let mut names: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(backup_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BackupListResponse {
                backups: Vec::new(),
                keep,
                database_in_memory,
            });
        }
        Err(error) => {
            return Err(AdminError::internal(&format_args!(
                "backup listing failed: {error}"
            )));
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_backup_name(&name) {
            names.push(name);
        }
    }
    names.sort_by_cached_key(|name| backup_order(Path::new(name)));
    let mut backups = Vec::with_capacity(names.len());
    for name in names {
        let path = backup_dir.join(&name);
        let size_bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let manifest = read_manifest(&path);
        backups.push(BackupView {
            name,
            size_bytes,
            kind: manifest
                .as_ref()
                .map(|found| found.kind)
                .unwrap_or_default(),
            sha256: manifest.as_ref().map(|found| found.sha256.clone()),
            user_version: manifest.as_ref().map(|found| found.user_version),
            created_at_unix: manifest
                .as_ref()
                .and_then(|found| unix_secs(found.created_at)),
            undo_bytes: manifest.as_ref().map(|found| found.undo_bytes),
        });
    }
    Ok(BackupListResponse {
        backups,
        keep,
        database_in_memory,
    })
}

/// Run one backup now, as a person asked, and report what it produced.
pub async fn run_backup(service: &BackupService) -> Result<BackupRunResponse, BackupError> {
    let report = service
        .backup(BackupKind::Manual, None, &CancelFlag::never())
        .await?;
    Ok(run_response(&report))
}

/// The nightly backup loop: every half hour, a scheduled backup once the
/// newest one in the folder is a day old. What is due is read from the
/// backups folder alone, so a restart never doubles a backup.
pub struct NightlyBackup {
    service: Arc<BackupService>,
    settings: Live<Backups>,
    /// No backup is tried before this time (kernel clock milliseconds): a
    /// failed integrity check holds the loop off. In memory only.
    held_until: Option<i64>,
}

impl NightlyBackup {
    /// The loop over the one backup service and its settings section.
    pub fn new(service: Arc<BackupService>, settings: Live<Backups>) -> Self {
        Self {
            service,
            settings,
            held_until: None,
        }
    }

    /// Start the loop on the supervisor.
    pub fn spawn_on(self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        let schedule = Schedule::new(BACKUP_TICK)
            .with_jitter(Duration::from_secs(5 * 60))
            .with_initial_delay(BACKUP_FIRST_TICK);
        supervisor.periodic(BACKUP_JOB, schedule, self)
    }

    /// One pass: take a scheduled backup when nightly backups are on and
    /// one is due. An unreadable settings section skips the pass. An
    /// integrity failure answers its code and holds the loop off for
    /// [`INTEGRITY_HOLD_MS`], answering the same code meanwhile.
    pub async fn pass(&mut self, token: &CancellationToken) -> TickResult {
        let Some(settings) = self.settings.read() else {
            return Ok(Next::Continue);
        };
        if !settings.nightly {
            return Ok(Next::Continue);
        }
        drop(settings);
        let now = self.service.now_ms();
        if let Some(until) = self.held_until {
            if now < until {
                return Err(TickError::new(
                    BACKUP_INTEGRITY_FAILED,
                    "the last nightly backup found the database damaged; the next try waits",
                ));
            }
            self.held_until = None;
        }
        let service = Arc::clone(&self.service);
        let newest = tokio::task::spawn_blocking(move || service.newest_scheduled_at())
            .await
            .map_err(|error| TickError::new(BACKUP_FAILED, error))?
            .map_err(|error| TickError::new(BACKUP_FAILED, error))?;
        if newest.is_some_and(|at| now.saturating_sub(at) < NIGHTLY_EVERY_MS) {
            return Ok(Next::Continue);
        }
        let cancel = CancelFlag::never();
        let run = self.service.backup(BackupKind::Scheduled, None, &cancel);
        tokio::pin!(run);
        let outcome = tokio::select! {
            outcome = &mut run => outcome,
            () = token.cancelled() => {
                cancel.cancel();
                run.await
            }
        };
        match outcome {
            Ok(report) => {
                tracing::info!(backup = %report.path.display(), "nightly backup complete");
                Ok(Next::Continue)
            }
            // A backup someone started is running: the next pass looks again.
            Err(BackupError::Running) => Ok(Next::Continue),
            Err(BackupError::Db(droppedneedle_kernel::db::DbError::Cancelled { .. }))
                if token.is_cancelled() =>
            {
                Ok(Next::Finish)
            }
            Err(error @ BackupError::NoRoom { .. }) => Err(TickError::new(BACKUP_NO_ROOM, error)),
            Err(BackupError::Db(droppedneedle_kernel::db::DbError::IntegrityFailed)) => {
                self.held_until = Some(now.saturating_add(INTEGRITY_HOLD_MS));
                Err(TickError::new(
                    BACKUP_INTEGRITY_FAILED,
                    "the live database failed its integrity check; previous backups are kept",
                ))
            }
            Err(error) => Err(TickError::new(BACKUP_FAILED, error)),
        }
    }
}

impl Tick for NightlyBackup {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        self.pass(ctx.token()).await
    }
}

/// Verify a backup read-only and report whether an offline restore should
/// succeed. Blocking (it hashes and opens the file): handlers call it on a
/// blocking thread. `ok` means every check passed; `restorable` means the file is
/// intact and its schema is not newer than this binary (a missing manifest
/// fails `ok` but a hand-placed backup can still restore).
///
/// How deep a restore report looks at a gzip backup.
#[derive(Clone, Copy)]
pub enum ReportDepth<'a> {
    /// Unpack it through the server's backup service into a temporary file
    /// in the backups folder, and run the database checks on that.
    Unpacked(&'a BackupService),
    /// Check only the stored file against its manifest and write nothing
    /// beside it: the offline restore's preflight, whose restore unpacks
    /// the backup and runs the database checks itself.
    StoredFile,
}

/// A gzip backup is looked at as `depth` says; a plain one always gets the
/// database checks.
pub fn restore_report(
    backup_dir: &Path,
    name: &str,
    depth: ReportDepth<'_>,
) -> Result<RestoreReport, AdminError> {
    let safe = safe_backup_name(name).ok_or_else(|| AdminError::InvalidInput {
        message: "Backup name must be a plain file name ending in .db or .db.gz".to_owned(),
    })?;
    let path = backup_dir.join(safe);
    let mut checks: Vec<RestoreCheck> = Vec::new();

    let present = path.is_file();
    checks.push(RestoreCheck {
        name: "file-present".to_owned(),
        passed: present,
        detail: if present {
            format!("{safe} is on disk")
        } else {
            format!("{safe} is not in the backups directory")
        },
    });
    if !present {
        return Ok(RestoreReport {
            backup: safe.to_owned(),
            ok: false,
            restorable: false,
            checks,
        });
    }

    let manifest = read_manifest(&path);
    checks.push(RestoreCheck {
        name: "manifest-present".to_owned(),
        passed: manifest.is_some(),
        detail: manifest.as_ref().map_or_else(
            || "No manifest sits beside this backup; identity checks are skipped".to_owned(),
            |found| {
                format!(
                    "Manifest records {} bytes at schema {}",
                    found.size_bytes, found.user_version
                )
            },
        ),
    });

    let mut identity_ok = true;
    if let Some(found) = &manifest {
        let size_bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let size_ok = size_bytes == found.size_bytes;
        checks.push(RestoreCheck {
            name: "manifest-size".to_owned(),
            passed: size_ok,
            detail: if size_ok {
                format!("On-disk size matches the manifest ({size_bytes} bytes)")
            } else {
                format!(
                    "On-disk size {size_bytes} differs from the manifest {}",
                    found.size_bytes
                )
            },
        });
        let sha_ok = droppedneedle_kernel::db::backup::file_sha256(&path)
            .is_ok_and(|(sha256, _)| sha256 == found.sha256);
        checks.push(RestoreCheck {
            name: "manifest-sha256".to_owned(),
            passed: sha_ok,
            detail: if sha_ok {
                "SHA-256 matches the manifest".to_owned()
            } else {
                "SHA-256 differs from the manifest; the file changed after the backup".to_owned()
            },
        });
        identity_ok = size_ok && sha_ok;
    }

    let packed = is_packed_name(&path);
    let unpacked = match (packed, depth) {
        (false, _) => None,
        (true, ReportDepth::Unpacked(service)) => Some(service.unpack_for_check(&path)),
        (true, ReportDepth::StoredFile) => {
            return Ok(RestoreReport {
                backup: safe.to_owned(),
                ok: checks.iter().all(|check| check.passed),
                restorable: identity_ok,
                checks,
            });
        }
    };
    let database: Option<&Path> = match &unpacked {
        None => Some(&path),
        Some(Ok(copy)) => Some(copy.path()),
        Some(Err(_)) => None,
    };

    let integrity = database.is_some_and(integrity_clean);
    checks.push(RestoreCheck {
        name: "integrity-check".to_owned(),
        passed: integrity,
        detail: if integrity {
            "integrity_check reads ok with no foreign-key violations".to_owned()
        } else if database.is_none() {
            "The backup does not unpack; the file is damaged or cut short".to_owned()
        } else {
            "The backup fails integrity_check or has foreign-key violations".to_owned()
        },
    });

    // Two checks: newer than the binary (which `--allow-downgrade` may
    // override) and the floor, a stamp the runner refuses at any binary.
    let stamp = database.and_then(|database| {
        open_immutable(database)
            .ok()
            .and_then(|connection| Stamp::read(&connection).ok())
    });
    let (version_ok, floor_ok) = match stamp {
        Some(stamp) => {
            let expected = droppedneedle_kernel::schema::latest_version();
            let found = stamp.version;
            let current = found <= expected;
            checks.push(RestoreCheck {
                name: "schema-version".to_owned(),
                passed: current,
                detail: if current {
                    format!("Backup schema {found} restores under binary schema {expected}")
                } else {
                    format!(
                        "Backup schema {found} is newer than binary schema {expected}; \
                         restore needs --allow-downgrade"
                    )
                },
            });
            let floor = match droppedneedle_kernel::schema::CHAIN.plan(stamp) {
                Ok(_) | Err(SchemaError::TooNew { .. }) => None,
                Err(refused) => Some(refused),
            };
            checks.push(RestoreCheck {
                name: SCHEMA_FLOOR_CHECK.to_owned(),
                passed: floor.is_none(),
                detail: match &floor {
                    None => format!("Backup schema {found} is one this binary can open"),
                    Some(refused) => refused.to_string(),
                },
            });
            (current, floor.is_none())
        }
        None => {
            for name in ["schema-version", SCHEMA_FLOOR_CHECK] {
                checks.push(RestoreCheck {
                    name: name.to_owned(),
                    passed: false,
                    detail: "The backup stamp could not be read".to_owned(),
                });
            }
            (false, false)
        }
    };
    drop(unpacked);

    checks.push(undo_copies_check(backup_dir, &path, manifest.as_ref()));

    let ok = checks.iter().all(|check| check.passed);
    Ok(RestoreReport {
        backup: safe.to_owned(),
        ok,
        restorable: present && integrity && version_ok && floor_ok,
        checks,
    })
}

/// Whether the pool beside the backups holds every undo copy the backup
/// names. For information only: a restore goes ahead without a missing
/// copy and lists it, and Undo then skips only the files that need it.
fn undo_copies_check(
    backup_dir: &Path,
    path: &Path,
    manifest: Option<&BackupManifest>,
) -> RestoreCheck {
    let name = UNDO_COPIES_CHECK.to_owned();
    let named = match backup_undo_names(path) {
        Ok(named) => named,
        Err(_) => {
            return RestoreCheck {
                name,
                passed: false,
                detail: "The undo copies this backup names could not be read".to_owned(),
            };
        }
    };
    let pool = UndoDir::new(backup_dir.join(UNDO_DIR_NAME));
    let known_missing = manifest.map_or(&[][..], |found| found.undo_missing.as_slice());
    let missing = named
        .iter()
        .filter(|sha256| known_missing.contains(*sha256) || !pool.contains(sha256))
        .count();
    RestoreCheck {
        name,
        passed: missing == 0,
        detail: if missing == 0 {
            format!("All {} undo copies are beside the backup", named.len())
        } else {
            format!(
                "{missing} of {} undo copies are missing; undo skips the files that need them",
                named.len()
            )
        },
    }
}

/// Accept only plain backup file names: no separators, no parent climbs,
/// no hidden files, ending in `.db` or `.db.gz`.
fn safe_backup_name(name: &str) -> Option<&str> {
    if name.is_empty()
        || !(name.ends_with(PLAIN_SUFFIX) || name.ends_with(PACKED_SUFFIX))
        || name.starts_with('.')
    {
        return None;
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    let path = Path::new(name);
    if path.file_name().is_some_and(|base| base == name) {
        Some(name)
    } else {
        None
    }
}

/// Best-effort manifest read: a missing or corrupt sidecar is `None`, never
/// an error; the listing still shows the file.
fn read_manifest(backup: &Path) -> Option<BackupManifest> {
    let bytes = std::fs::read(manifest_path(backup)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// System time as unix seconds, or `None` before the epoch.
fn unix_secs(at: SystemTime) -> Option<u64> {
    at.duration_since(UNIX_EPOCH)
        .ok()
        .map(|span| span.as_secs())
}

/// `integrity_check` must read `ok` and `foreign_key_check` must be empty.
/// Opens read-only and immutable, so nothing is written beside the file;
/// any failure reads as unclean.
fn integrity_clean(path: &Path) -> bool {
    let connection: Connection = match open_immutable(path) {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let integrity: Result<String, _> =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0));
    if integrity.as_deref() != Ok("ok") {
        return false;
    }
    let violations: Result<i64, _> =
        connection.query_row(super::sql::FOREIGN_KEY_VIOLATIONS.text(), [], |row| {
            row.get(0)
        });
    violations == Ok(0)
}

/// Map a run report onto the wire shape.
fn run_response(report: &BackupReport) -> BackupRunResponse {
    BackupRunResponse {
        name: report
            .path
            .file_name()
            .map(|base| base.to_string_lossy().into_owned())
            .unwrap_or_else(|| report.path.display().to_string()),
        sha256: report.manifest.sha256.clone(),
        size_bytes: report.manifest.size_bytes,
        user_version: report.manifest.user_version,
        duration_ms: report.duration.as_millis().min(u128::from(u64::MAX)) as u64,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A route takes a plain backup file name only, and the listing counts
    /// finished backups only.
    #[test]
    fn backup_names_and_listing() {
        assert_eq!(safe_backup_name("library-1-0.db"), Some("library-1-0.db"));
        assert_eq!(
            safe_backup_name("library-1-0.db.gz"),
            Some("library-1-0.db.gz")
        );
        for refused in [
            "",
            "library.db.bak",
            ".staging-1.db",
            "../library.db",
            "sub/library.db",
            "..\\library.db",
            "library..db",
        ] {
            assert_eq!(safe_backup_name(refused), None, "name {refused:?}");
        }

        assert!(is_backup_name("library-1-0.db"));
        assert!(is_backup_name("library-1-0.db.gz"));
        for unfinished in [
            "library-1-0.db.manifest.json",
            "library-1-0.db.gz.manifest.json",
            ".staging-1-2-3.db",
            ".staging-1-2-3.db.gz",
            ".restore-1-2.tmp",
        ] {
            assert!(!is_backup_name(unfinished), "listed {unfinished:?}");
        }
    }
}
