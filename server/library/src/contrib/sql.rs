//! The contribution store's newer statements.

droppedneedle_kernel::sql! {
    /// Put verification job `?1`, held by worker `?2`, back in the queue
    /// from `?3` (at `?4`) without counting the attempt it claimed.
    pub(super) RELEASE_VERIFICATION = "UPDATE library_contribution_verification_jobs \
             SET state = 'queued', attempt_count = MAX(attempt_count - 1, 0), not_before = ?3, \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = ?4, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = ?1 AND state = 'running' AND lease_owner = ?2";
}
