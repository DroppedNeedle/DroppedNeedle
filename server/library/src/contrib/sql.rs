//! The contribution store's newer statements.

droppedneedle_kernel::sql! {
    /// Verifying contributions with no queued or running verification job.
    pub(super) VERIFYING_WITHOUT_JOB = "SELECT d.id FROM library_contribution_drafts d \
             WHERE d.state = 'verifying' AND NOT EXISTS (SELECT 1 FROM durable_jobs j \
             WHERE j.kind = 'library.contribution_verify' AND j.idempotency_key = d.id \
             AND j.state IN ('queued', 'running')) ORDER BY d.id";
}
