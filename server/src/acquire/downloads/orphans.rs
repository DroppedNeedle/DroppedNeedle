//! Orphan reconcile and recycle-bin prune.
//!
//! Ports `reconcile_orphan_folders` (#131) and `recycle_bin.py`. A
//! DN-named complete-dir folder is debris only when no attempt journal
//! owns it, no live task or unsettled publisher bundle claims it, it is
//! older than 6 hours, and the client confirms the job is not active.
//! Every ambiguous answer keeps the folder: deletion decisions fail
//! closed. The recycle bin holds upgrade-replaced files for a retention
//! window so a bad swap stays recoverable.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A folder qualifies as orphan debris only past this age (6 hours). The
/// floor comfortably exceeds any crash window between the client
/// materializing a job and its attempt row being journaled, so a live
/// download can never look abandoned just because its row landed late.
pub const ORPHAN_MIN_AGE_SECONDS: f64 = 6.0 * 3600.0;

/// Entry stamp format for recycle entries (`<stamp>-<unique>`).
pub const RECYCLE_STAMP_FORMAT_LEN: usize = 15;

/// Split a DN job directory name into `(task_id, job_name)`.
///
/// Journal rows carry the unsuffixed name
/// (`droppedneedle-<32 hex>-<n>`); SABnzbd renames colliding complete-dir
/// entries to `<job>.<N>`, so reconciliation accepts the suffixed
/// variants while journal identity keeps the strict shape. Returns `None`
/// for anything that is not a DN-named folder.
pub fn job_name_parts(dir_name: &str) -> Option<(String, String)> {
    let base = match dir_name.split_once('.') {
        Some((head, suffix))
            if !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()) =>
        {
            // A `.0` suffix is never a SABnzbd collision marker; treat it
            // as a foreign name rather than debris.
            if suffix.bytes().all(|b| b == b'0') {
                return None;
            }
            head
        }
        Some(_) => return None,
        None => dir_name,
    };
    let rest = base.strip_prefix("droppedneedle-")?;
    let (task_id, counter) = rest.split_once('-')?;
    if task_id.len() != 32 || !task_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if counter.is_empty() || !counter.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if counter.len() > 1 && counter.starts_with('0') {
        return None;
    }
    Some((task_id.to_string(), base.to_string()))
}

/// Ownership evidence for one candidate folder.
#[derive(Debug, Clone, Copy)]
pub struct OrphanEvidence {
    /// Any attempt journal still references this job.
    pub has_cleanup_debt: bool,
    /// Owning task is still active (`queued`/`downloading`/`processing`).
    pub task_active: bool,
    /// Every publisher bundle settled (or none exist).
    pub bundles_settled: bool,
    /// The client's storage is reachable and healthy.
    pub mount_healthy: bool,
    /// The client still owns the job.
    pub client_job_active: bool,
    /// Folder age in seconds.
    pub age_seconds: f64,
}

/// Reconcile verdict for one folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrphanDecision {
    /// Not a DN-named folder, or a symlink: never touch it.
    Ignore,
    /// Owned, young, ambiguous, or the client still has it: keep it.
    Keep,
    /// Proven debris: safe to discard client records, then remove.
    Remove,
}

/// Pure orphan policy: name match plus ownership evidence in, verdict out.
/// The filesystem walk and client calls live with the caller; this keeps
/// the fail-closed rules testable without I/O.
#[derive(Debug, Clone, Copy)]
pub struct OrphanPolicy {
    /// Minimum debris age in seconds.
    pub min_age_seconds: f64,
}

impl Default for OrphanPolicy {
    fn default() -> Self {
        Self {
            min_age_seconds: ORPHAN_MIN_AGE_SECONDS,
        }
    }
}

impl OrphanPolicy {
    /// Judge one directory entry. `is_symlink` short-circuits to
    /// [`OrphanDecision::Ignore`] before any other check.
    pub fn evaluate(
        &self,
        dir_name: &str,
        is_symlink: bool,
        evidence: Option<OrphanEvidence>,
    ) -> OrphanDecision {
        if is_symlink || job_name_parts(dir_name).is_none() {
            return OrphanDecision::Ignore;
        }
        let Some(evidence) = evidence else {
            // Ownership lookup failed: fail closed, keep the folder.
            return OrphanDecision::Keep;
        };
        if evidence.has_cleanup_debt || evidence.task_active || !evidence.bundles_settled {
            return OrphanDecision::Keep;
        }
        if evidence.age_seconds < self.min_age_seconds {
            return OrphanDecision::Keep;
        }
        if !evidence.mount_healthy || evidence.client_job_active {
            return OrphanDecision::Keep;
        }
        OrphanDecision::Remove
    }
}

/// Judge one directory entry with the default policy.
pub fn evaluate_orphan(
    dir_name: &str,
    is_symlink: bool,
    evidence: Option<OrphanEvidence>,
) -> OrphanDecision {
    OrphanPolicy::default().evaluate(dir_name, is_symlink, evidence)
}

/// Upgrade-only recycle bin: replaced files move here instead of being
/// deleted, so a bad swap stays recoverable for the retention window.
/// User-initiated deletes stay hard deletes outside this bin.
#[derive(Debug, Clone)]
pub struct RecycleBin {
    root: PathBuf,
    retention_days: i64,
}

impl RecycleBin {
    /// Resolve the effective bin directory: the configured path, else
    /// `.recycle` under the first library path, else `None` when no
    /// library is configured yet. A relative configured path is ignored
    /// (it would resolve against the server's working directory and
    /// scatter recycled files who-knows-where).
    pub fn resolve(configured: &str, library_paths: &[String]) -> Option<PathBuf> {
        let trimmed = configured.trim();
        if !trimmed.is_empty() {
            let path = PathBuf::from(trimmed);
            if path.is_absolute() {
                return Some(path);
            }
        }
        library_paths
            .first()
            .map(|first| PathBuf::from(first).join(".recycle"))
    }

    /// Open a bin at an explicit root with a retention window in days.
    pub fn at(root: PathBuf, retention_days: i64) -> Self {
        Self {
            root,
            retention_days,
        }
    }

    /// Open a bin only when the root cannot eat a real tree: `/`, empty,
    /// and relative roots are refused, as is any root equal to or an
    /// ancestor of a protected path (library, download, staging dirs). A
    /// refused root answers `None` so the caller skips pruning; the bin
    /// itself is never created.
    pub fn guarded(root: PathBuf, retention_days: i64, protected: &[PathBuf]) -> Option<Self> {
        if root.as_os_str().is_empty() || !root.is_absolute() || root.parent().is_none() {
            tracing::warn!(root = %root.display(), "recycle bin root refused: not absolute");
            return None;
        }
        for keep in protected {
            if root == *keep || keep.starts_with(&root) {
                tracing::warn!(
                    root = %root.display(),
                    protected = %keep.display(),
                    "recycle bin root refused: overlaps a protected tree"
                );
                return None;
            }
        }
        Some(Self::at(root, retention_days))
    }

    /// Bin root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Delete entries older than the retention window; returns how many
    /// entries were removed. Never touches anything outside the root.
    /// Entry age comes from the directory name stamp; unparseable names
    /// fall back to mtime.
    pub fn prune(&self, now: SystemTime) -> Result<usize, RecycleError> {
        // Belt and suspenders behind `guarded`: a `/` root never prunes.
        if self.root.parent().is_none() {
            tracing::warn!("recycle bin prune skipped: root is filesystem root");
            return Ok(0);
        }
        if !self.root.is_dir() {
            return Ok(0);
        }
        let cutoff_secs = self.retention_days.max(0) as u64 * 86_400;
        let mut removed = 0;
        let entries = std::fs::read_dir(&self.root).map_err(|source| RecycleError::Io {
            path: self.root.clone(),
            detail: source.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| RecycleError::Io {
                path: self.root.clone(),
                detail: source.to_string(),
            })?;
            let path = entry.path();
            let expired =
                entry_expired(&path, now, cutoff_secs).map_err(|detail| RecycleError::Io {
                    path: path.clone(),
                    detail,
                })?;
            if !expired {
                continue;
            }
            if path.is_dir() {
                std::fs::remove_dir_all(&path).map_err(|source| RecycleError::Io {
                    path: path.clone(),
                    detail: source.to_string(),
                })?;
            } else {
                std::fs::remove_file(&path).map_err(|source| RecycleError::Io {
                    path: path.clone(),
                    detail: source.to_string(),
                })?;
            }
            removed += 1;
        }
        Ok(removed)
    }
}

/// True when a recycle entry is older than the cutoff. The stamp is
/// authoritative: a cross-filesystem move rewrites mtimes, so mtime
/// alone would lie about an entry's real age.
fn entry_expired(path: &Path, now: SystemTime, cutoff_secs: u64) -> Result<bool, String> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let stamp = name.split('-').next().unwrap_or_default();
    if stamp.len() == RECYCLE_STAMP_FORMAT_LEN
        && let Some(created) = parse_stamp(stamp)
    {
        let age = now.duration_since(created).unwrap_or_default().as_secs();
        return Ok(age > cutoff_secs);
    }
    let mtime = std::fs::symlink_metadata(path)
        .map_err(|err| err.to_string())?
        .modified()
        .map_err(|err| err.to_string())?;
    Ok(now.duration_since(mtime).unwrap_or_default().as_secs() > cutoff_secs)
}

/// Parse a `YYYYMMDDTHHMMSS` UTC stamp without external date crates.
fn parse_stamp(stamp: &str) -> Option<SystemTime> {
    let year: i32 = stamp.get(0..4)?.parse().ok()?;
    let month: i32 = stamp.get(4..6)?.parse().ok()?;
    let day: i32 = stamp.get(6..8)?.parse().ok()?;
    if stamp.as_bytes().get(8) != Some(&b'T') {
        return None;
    }
    let hour: i32 = stamp.get(9..11)?.parse().ok()?;
    let minute: i32 = stamp.get(11..13)?.parse().ok()?;
    let second: i32 = stamp.get(13..15)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let days = days_since_epoch(year, month, day)?;
    let secs = days * 86_400 + i64::from(hour) * 3600 + i64::from(minute) * 60 + i64::from(second);
    SystemTime::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(secs as u64))
}

/// Civil date to days since the unix epoch (Howard Hinnant's algorithm).
fn days_since_epoch(year: i32, month: i32, day: i32) -> Option<i64> {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400) as i64;
    let mp = ((month + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + i64::from(day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(i64::from(era) * 146_097 + doe - 719_468)
}

/// Recycle-bin failures.
#[derive(Debug, thiserror::Error)]
pub enum RecycleError {
    /// Directory iteration or removal failed.
    #[error("recycle bin I/O failed for {}: {detail}", path.display())]
    Io {
        /// Path being read or removed.
        path: PathBuf,
        /// Underlying OS error text.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settled_old() -> OrphanEvidence {
        OrphanEvidence {
            has_cleanup_debt: false,
            task_active: false,
            bundles_settled: true,
            mount_healthy: true,
            client_job_active: false,
            age_seconds: 7.0 * 3600.0,
        }
    }

    #[test]
    fn job_names_split_task_and_job() {
        let (task, job) =
            job_name_parts("droppedneedle-0123456789abcdef0123456789abcdef-3").unwrap();
        assert_eq!(task, "0123456789abcdef0123456789abcdef");
        assert_eq!(job, "droppedneedle-0123456789abcdef0123456789abcdef-3");
        // SABnzbd collision suffix still resolves to the journaled job.
        let (_, job) =
            job_name_parts("droppedneedle-0123456789abcdef0123456789abcdef-3.2").unwrap();
        assert_eq!(job, "droppedneedle-0123456789abcdef0123456789abcdef-3");
        assert!(job_name_parts("random-folder").is_none());
        assert!(job_name_parts("droppedneedle-short-1").is_none());
    }

    #[test]
    fn orphan_rules_fail_closed() {
        let policy = OrphanPolicy::default();
        let name = "droppedneedle-0123456789abcdef0123456789abcdef-3";
        assert_eq!(
            policy.evaluate(name, false, Some(settled_old())),
            OrphanDecision::Remove
        );
        assert_eq!(policy.evaluate(name, false, None), OrphanDecision::Keep);
        assert_eq!(
            policy.evaluate(name, true, Some(settled_old())),
            OrphanDecision::Ignore
        );
        assert_eq!(
            policy.evaluate("other", false, Some(settled_old())),
            OrphanDecision::Ignore
        );
        let young = OrphanEvidence {
            age_seconds: 60.0,
            ..settled_old()
        };
        assert_eq!(
            policy.evaluate(name, false, Some(young)),
            OrphanDecision::Keep
        );
        let owned = OrphanEvidence {
            has_cleanup_debt: true,
            ..settled_old()
        };
        assert_eq!(
            policy.evaluate(name, false, Some(owned)),
            OrphanDecision::Keep
        );
    }

    #[test]
    fn relative_recycle_path_falls_back_to_library_default() {
        let bin = RecycleBin::resolve("relative/bin", &["/lib".to_string()]);
        assert_eq!(bin, Some(PathBuf::from("/lib/.recycle")));
        let bin = RecycleBin::resolve("/abs/bin", &["/lib".to_string()]);
        assert_eq!(bin, Some(PathBuf::from("/abs/bin")));
        assert_eq!(RecycleBin::resolve("", &[]), None);
    }
}
