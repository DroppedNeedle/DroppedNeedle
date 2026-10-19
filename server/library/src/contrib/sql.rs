//! The contribution store's newer statements.

droppedneedle_kernel::sql! {
    /// Record on contribution `?1` the code its verification ended with
    /// (`?2`, NULL when it linked).
    pub(super) RECORD_VERIFICATION_FAILURE = "UPDATE library_contribution_drafts \
             SET last_verification_failure = ?2 WHERE id = ?1";

    /// Verifying contributions with no queued or running verification job.
    pub(super) VERIFYING_WITHOUT_JOB = "SELECT d.id FROM library_contribution_drafts d \
             WHERE d.state = 'verifying' AND NOT EXISTS (SELECT 1 FROM durable_jobs j \
             WHERE j.kind = 'library.contribution_verify' AND j.idempotency_key = d.id \
             AND j.state IN ('queued', 'running')) ORDER BY d.id";
}
