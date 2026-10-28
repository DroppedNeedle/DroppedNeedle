//! Orphan reconcile.
//!
//! See issue #131. A DN-named complete-dir
//! folder is debris only when no attempt journal owns it, no live task or
//! unsettled publisher bundle claims it, it is older than 6 hours, and
//! the client confirms the job is not active. Every ambiguous answer
//! keeps the folder: deletion decisions fail closed.

/// A folder qualifies as orphan debris only past this age (6 hours). The
/// floor comfortably exceeds any crash window between the client
/// materializing a job and its attempt row being journaled, so a live
/// download can never look abandoned just because its row landed late.
pub const ORPHAN_MIN_AGE_SECONDS: f64 = 6.0 * 3600.0;

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
    /// Any attempt journal still references this job, files kept for
    /// "Retry import" included.
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
}
