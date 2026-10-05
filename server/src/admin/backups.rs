//! Backup UX: list, run, pre-restore reports, pre-upgrade safety nets.
//!
//! The mechanism lives in [`crate::db::backup`]; this module is the admin
//! face over it. Rotation stays at five (`BACKUP_KEEP`): verification
//! always precedes rotation, so a failed run leaves the previous five
//! untouched.
//!
//! Two simplifications on purpose: backup runs are synchronous (a
//! catalog-sized backup finishes in seconds, and no client needs progress
//! polling yet; revisit if slow devices show otherwise), and the restore
//! itself stays an offline CLI (`droppedneedle-tool restore`). The
//! restore-report route verifies read-only and never moves the live
//! database.

use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use rusqlite::{Connection, OpenFlags};

use super::{
    error::AdminError,
    models::{BackupListResponse, BackupRunResponse, BackupView, RestoreCheck, RestoreReport},
};
use crate::db::{
    BACKUP_KEEP, BackupManifest, BackupReport, BackupService, backup::MANIFEST_SUFFIX,
};

/// List the backups on disk, oldest first. Blocking file IO. Files without a manifest sidecar
/// still list with their on-disk size; only the manifest fields stay empty.
pub fn list_backups(backup_dir: &Path) -> Result<BackupListResponse, AdminError> {
    let mut names: Vec<String> = Vec::new();
    let entries = match std::fs::read_dir(backup_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BackupListResponse {
                backups: Vec::new(),
                keep: BACKUP_KEEP as u32,
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
        if is_backup_file(&name) {
            names.push(name);
        }
    }
    names.sort();
    let mut backups = Vec::with_capacity(names.len());
    for name in names {
        let path = backup_dir.join(&name);
        let size_bytes = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
        let manifest = read_manifest(&path);
        backups.push(BackupView {
            name,
            size_bytes,
            sha256: manifest.as_ref().map(|found| found.sha256.clone()),
            user_version: manifest.as_ref().map(|found| found.user_version),
            created_at_unix: manifest
                .as_ref()
                .and_then(|found| unix_secs(found.created_at)),
        });
    }
    Ok(BackupListResponse {
        backups,
        keep: BACKUP_KEEP as u32,
    })
}

/// Run one backup now and report what it produced.
pub async fn run_backup(service: &BackupService) -> Result<BackupRunResponse, AdminError> {
    let cancel = crate::db::writer::CancelFlag::never();
    let report = service
        .backup(None, &cancel)
        .await
        .map_err(|error| AdminError::internal(&format_args!("backup run failed: {error}")))?;
    Ok(run_response(&report))
}

/// Take a safety backup before a schema upgrade. Reads the live stamp: when
/// it already matches the binary, there is nothing to protect and this
/// returns `None` without touching the backups directory. A fresh database
/// (stamp 0 with no tables yet) also skips: there is nothing to lose.
/// Otherwise it runs one verified backup (rotating at five) and returns its
/// report. Boot treats a failure here as fatal: migrating without a safety
/// net risks the catalog.
///
/// The stamp alone does not prove freshness: a legacy database could carry
/// stamp 0 with real tables, and that still gets its backup. Only the
/// stamp-0-plus-no-tables shape skips.
pub async fn ensure_pre_upgrade_backup(
    db_path: &Path,
    backup_dir: &Path,
) -> Result<Option<BackupReport>, crate::db::error::DbError> {
    let found = read_user_version(db_path)?;
    let expected = crate::schema::latest_version();
    if found >= expected {
        return Ok(None);
    }
    if found == 0 && !has_user_tables(db_path)? {
        tracing::info!("fresh database; skipping the pre-upgrade backup");
        return Ok(None);
    }
    tracing::info!(
        found,
        expected,
        "schema upgrade pending; taking a pre-upgrade backup"
    );
    let service = BackupService::new(db_path, backup_dir);
    let cancel = crate::db::writer::CancelFlag::never();
    let report = service.backup(None, &cancel).await?;
    tracing::info!(
        backup = report.path.display().to_string(),
        "pre-upgrade backup complete"
    );
    Ok(Some(report))
}

/// Verify a backup read-only and report whether an offline restore should
/// succeed. Blocking (it hashes and opens the file): handlers call it on a
/// blocking thread. `ok` means every check passed; `restorable` means the file is
/// intact and its schema is not newer than this binary (a missing manifest
/// fails `ok` but a hand-placed backup can still restore).
pub fn restore_report(backup_dir: &Path, name: &str) -> Result<RestoreReport, AdminError> {
    let safe = safe_backup_name(name).ok_or_else(|| AdminError::InvalidInput {
        message: "Backup name must be a plain file name ending in .db".to_owned(),
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
        let sha_ok =
            crate::db::backup::file_sha256(&path).is_ok_and(|(sha256, _)| sha256 == found.sha256);
        checks.push(RestoreCheck {
            name: "manifest-sha256".to_owned(),
            passed: sha_ok,
            detail: if sha_ok {
                "SHA-256 matches the manifest".to_owned()
            } else {
                "SHA-256 differs from the manifest; the file changed after the backup".to_owned()
            },
        });
    }

    let integrity = integrity_clean(&path);
    checks.push(RestoreCheck {
        name: "integrity-check".to_owned(),
        passed: integrity,
        detail: if integrity {
            "integrity_check reads ok with no foreign-key violations".to_owned()
        } else {
            "The backup fails integrity_check or has foreign-key violations".to_owned()
        },
    });

    let version_ok = match read_user_version(&path) {
        Ok(found) => {
            let expected = crate::schema::latest_version();
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
            current
        }
        Err(_) => {
            checks.push(RestoreCheck {
                name: "schema-version".to_owned(),
                passed: false,
                detail: "The backup stamp could not be read".to_owned(),
            });
            false
        }
    };

    let ok = checks.iter().all(|check| check.passed);
    Ok(RestoreReport {
        backup: safe.to_owned(),
        ok,
        restorable: present && integrity && version_ok,
        checks,
    })
}

/// Accept only plain backup file names: no separators, no parent climbs,
/// no hidden files, ending in `.db`.
fn safe_backup_name(name: &str) -> Option<&str> {
    if name.is_empty() || !name.ends_with(".db") || name.starts_with('.') {
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

/// Directory entries that count as backups: finished `.db` files, never
/// staging or restore temp files.
fn is_backup_file(name: &str) -> bool {
    name.ends_with(".db") && !name.starts_with(".staging-") && !name.starts_with(".restore-")
}

/// Manifest sidecar path for a backup file.
fn manifest_path(backup: &Path) -> PathBuf {
    let mut name = backup.as_os_str().to_owned();
    name.push(MANIFEST_SUFFIX);
    PathBuf::from(name)
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
/// Opens read-only; any failure reads as unclean.
fn integrity_clean(path: &Path) -> bool {
    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        Err(_) => return false,
    };
    let integrity: Result<String, _> =
        connection.query_row("PRAGMA integrity_check", [], |row| row.get(0));
    if integrity.as_deref() != Ok("ok") {
        return false;
    }
    let violations: Result<i64, _> =
        connection.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
            row.get(0)
        });
    violations == Ok(0)
}

/// Whether a database file holds any user tables yet. Opens read-only; a
/// file that cannot be read counts as non-empty, so the safety net stays.
fn has_user_tables(path: &Path) -> Result<bool, crate::db::error::DbError> {
    let connection = match Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(connection) => connection,
        // Unreadable after a successful stamp read is bizarre; take the
        // backup rather than trust the emptiness.
        Err(_) => return Ok(true),
    };
    let tables: i64 = connection.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    Ok(tables > 0)
}

/// `user_version` stamped in a database file.
fn read_user_version(path: &Path) -> Result<i64, crate::db::error::DbError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let found: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(found)
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

    #[test]
    fn backup_names_accept_plain_files_only() {
        assert_eq!(safe_backup_name("library-1-0.db"), Some("library-1-0.db"));
        assert_eq!(safe_backup_name(""), None);
        assert_eq!(safe_backup_name("library.db.bak"), None);
        assert_eq!(safe_backup_name(".staging-1.db"), None);
        assert_eq!(safe_backup_name("../library.db"), None);
        assert_eq!(safe_backup_name("sub/library.db"), None);
        assert_eq!(safe_backup_name("..\\library.db"), None);
        assert_eq!(safe_backup_name("library..db"), None);
    }

    #[test]
    fn listing_counts_finished_backups_only() {
        assert!(is_backup_file("library-1-0.db"));
        assert!(!is_backup_file("library-1-0.db.manifest.json"));
        assert!(!is_backup_file(".staging-1-2-3.db"));
        assert!(!is_backup_file(".restore-1-2.tmp"));
    }
}
