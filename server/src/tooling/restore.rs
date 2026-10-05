//! Offline database restore: the CLI half of server backups.
//!
//! The mechanism is [`BackupService::restore`](crate::db::BackupService):
//! verify the backup (integrity plus schema stamp), copy it into an empty
//! directory as `library.db`, and verify the copy. This module adds the
//! operator face: a read-only preflight over the backup restore report
//! (manifest identity when a sidecar exists), fail-closed refusals with
//! plain messages, and a small JSON summary on success.
//!
//! There is no HTTP route for restore by decision: the server must be
//! stopped, and the target directory must be empty, so a restore can never
//! land on a live database.

use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use crate::admin::backups::restore_report;
use crate::db::BackupService;

/// Restore failures. Messages name the fix, never file contents.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RestoreError {
    /// The backup file is not where the operator pointed.
    #[error("backup file {0} is not readable")]
    BackupUnreadable(String),
    /// The preflight failed: integrity, version, or identity checks.
    #[error("restore refused: {0}")]
    Refused(String),
    /// The target directory is not empty.
    #[error("restore target {0} is not empty; restore into an empty directory")]
    TargetNotEmpty(String),
    /// The backup schema is newer than this binary.
    #[error(
        "backup schema {found} is newer than binary schema {expected}; pass --allow-downgrade to restore it anyway"
    )]
    BackupTooNew {
        /// Stamp read from the backup.
        found: i64,
        /// Stamp of this binary.
        expected: i64,
    },
    /// The copy or its verification failed.
    #[error("restore failed: {0}")]
    Failed(String),
}

/// What one offline restore produced. Printed as JSON by the CLI.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RestoreSummary {
    /// Backup file restored.
    pub backup: String,
    /// Restored database path.
    pub restored: String,
    /// Bytes copied.
    pub bytes: u64,
    /// Schema stamp of the restored file.
    pub user_version: i64,
    /// Preflight check names that passed.
    pub checks_passed: Vec<String>,
}

/// Restore `backup_db` into `target_dir` as `library.db`.
///
/// Runs the read-only preflight first (manifest identity, integrity,
/// schema stamp), then the verified copy. Refuses occupied targets and,
/// unless `allow_downgrade`, backups newer than this binary.
pub fn restore_backup(
    backup_db: &Path,
    target_dir: &Path,
    allow_downgrade: bool,
) -> Result<RestoreSummary, RestoreError> {
    if !backup_db.is_file() {
        return Err(RestoreError::BackupUnreadable(
            backup_db.display().to_string(),
        ));
    }
    let mut checks_passed = preflight(backup_db, allow_downgrade)?;
    let parent = backup_db
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let service = BackupService::new(backup_db, parent);
    let restored = service
        .restore(backup_db, target_dir, allow_downgrade)
        .map_err(|error| map_db_error(&error, target_dir))?;
    checks_passed.push("restore-copy-verified".to_owned());
    Ok(RestoreSummary {
        backup: backup_db.display().to_string(),
        restored: restored.path.display().to_string(),
        bytes: restored.bytes,
        user_version: restored.user_version,
        checks_passed,
    })
}

/// Read-only preflight over the backup restore report. Returns the names
/// of the checks that passed. Files that cannot take the preflight (names
/// outside the backups-dir convention) skip it; the restore itself still
/// verifies integrity and the schema stamp.
fn preflight(backup_db: &Path, allow_downgrade: bool) -> Result<Vec<String>, RestoreError> {
    let name = backup_db
        .file_name()
        .and_then(|base| base.to_str())
        .unwrap_or_default();
    let parent = backup_db
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let report = match restore_report(parent, name) {
        Ok(report) => report,
        Err(_) => return Ok(Vec::new()),
    };
    let mut failed: Vec<String> = Vec::new();
    let mut passed: Vec<String> = Vec::new();
    for check in &report.checks {
        if check.passed {
            passed.push(check.name.clone());
        } else if check.name == "schema-version" && allow_downgrade {
            passed.push(format!("{}-overridden", check.name));
        } else if check.name == "manifest-present" {
            // A hand-placed backup restores without a sidecar; the
            // integrity check below still guards the copy.
            passed.push(format!("{}-skipped", check.name));
        } else {
            failed.push(format!("{}: {}", check.name, check.detail));
        }
    }
    if !report.restorable && failed.is_empty() && !allow_downgrade {
        failed.push("backup is not restorable under this binary".to_owned());
    }
    if failed.is_empty() {
        Ok(passed)
    } else {
        Err(RestoreError::Refused(failed.join("; ")))
    }
}

/// Map the storage-layer restore failure onto operator wording.
fn map_db_error(error: &crate::db::DbError, target_dir: &Path) -> RestoreError {
    match error {
        crate::db::DbError::RestoreTargetNotEmpty { .. } => {
            RestoreError::TargetNotEmpty(target_dir.display().to_string())
        }
        crate::db::DbError::BackupTooNew { found, expected } => RestoreError::BackupTooNew {
            found: *found,
            expected: *expected,
        },
        other => RestoreError::Failed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_backup_refuses() {
        let missing = std::env::temp_dir().join(format!(
            "droppedneedle-restore-{}-missing.db",
            std::process::id()
        ));
        let target = std::env::temp_dir().join(format!(
            "droppedneedle-restore-{}-target",
            std::process::id()
        ));
        let error = restore_backup(&missing, &target, false).unwrap_err();
        assert!(matches!(error, RestoreError::BackupUnreadable(_)));
    }
}
