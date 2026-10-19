//! The statements of the jobs table. The five lease statements follow the
//! named-parameter contract in [`super::lease`]; the rest are the table's
//! own.

crate::sql! {
    /// Add a queued row unless the key already has one.
    pub(super) ENQUEUE = "INSERT INTO durable_jobs (kind, idempotency_key, state, attempts, not_before_at, payload_json, created_at, updated_at)
                 VALUES (:kind, :key, 'queued', 0, :due, :payload, :now, :now)
                 ON CONFLICT (kind, idempotency_key) WHERE state = 'queued' DO NOTHING
                 RETURNING id";
    /// The queued row a joined enqueue found.
    pub(super) QUEUED_ID = "SELECT id FROM durable_jobs WHERE kind = :kind AND idempotency_key = :key AND state = 'queued'";
    /// Claim the next due queued row whose key has no running row.
    pub(super) CLAIM = "UPDATE durable_jobs
                 SET state = 'running', attempts = attempts + 1, lease_owner = :owner,
                     lease_expires_at = :lease_until, updated_at = :now
                 WHERE id = (
                     SELECT j.id FROM durable_jobs j
                     WHERE j.kind = :scope AND j.state = 'queued' AND j.not_before_at <= :now
                       AND NOT EXISTS (SELECT 1 FROM durable_jobs r
                                       WHERE r.kind = j.kind AND r.idempotency_key = j.idempotency_key
                                         AND r.state = 'running')
                     ORDER BY j.not_before_at, j.id LIMIT 1)
                 RETURNING id";
    /// Extend a lease, for its owner only.
    pub(super) HEARTBEAT = "UPDATE durable_jobs SET lease_expires_at = :lease_until, updated_at = :now
                 WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// Put a running row back without spending its attempt, due now.
    pub(super) RELEASE = "UPDATE durable_jobs
                 SET state = 'queued', attempts = max(attempts - 1, 0), lease_owner = NULL,
                     lease_expires_at = NULL, not_before_at = :now, updated_at = :now
                 WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// Requeue the rows whose lease ran out.
    pub(super) RECOVER_EXPIRED = "UPDATE durable_jobs
                 SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL,
                     not_before_at = :now, last_error = 'LEASE_EXPIRED', updated_at = :now
                 WHERE kind = :scope AND state = 'running' AND lease_expires_at <= :now";
    /// Requeue every running row: an earlier process held them.
    pub(super) RECOVER_AT_START = "UPDATE durable_jobs
                 SET state = 'queued', lease_owner = NULL, lease_expires_at = NULL,
                     not_before_at = :now, last_error = 'INTERRUPTED', updated_at = :now
                 WHERE kind = :scope AND state = 'running'";
    /// Running rows recovery is about to requeue: every one at start, the
    /// expired ones later.
    pub(super) RECOVERABLE = "SELECT id, idempotency_key, attempts FROM durable_jobs
                 WHERE kind = :kind AND state = 'running' AND (:all = 1 OR lease_expires_at <= :now)";
    /// One running row as its owner holds it.
    pub(super) HELD = "SELECT kind, idempotency_key, attempts FROM durable_jobs
                 WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// Whether a row is done by this owner already.
    pub(super) DONE_BY = "SELECT EXISTS(SELECT 1 FROM durable_jobs
                 WHERE id = :id AND state = 'done' AND lease_owner = :owner)";
    /// The queued row sharing a key.
    pub(super) SIBLING = "SELECT id, attempts, not_before_at FROM durable_jobs
                 WHERE kind = :kind AND idempotency_key = :key AND state = 'queued'";
    /// Fold a put-back into its queued sibling.
    pub(super) FOLD_INTO = "UPDATE durable_jobs
                 SET attempts = max(attempts, :attempts), not_before_at = max(not_before_at, :due),
                     last_error = coalesce(:code, last_error), updated_at = :now
                 WHERE id = :id";
    /// Remove one row by id.
    pub(super) DELETE_ROW = "DELETE FROM durable_jobs WHERE id = :id";
    /// Put a running row back with its new attempts, due time and code.
    pub(super) REQUEUE = "UPDATE durable_jobs
                 SET state = 'queued', attempts = :attempts, not_before_at = :due,
                     last_error = coalesce(:code, last_error), lease_owner = NULL,
                     lease_expires_at = NULL, updated_at = :now
                 WHERE id = :id";
    /// Fail a row for good.
    pub(super) FAIL = "UPDATE durable_jobs
                 SET state = 'failed', last_error = :code, lease_expires_at = NULL,
                     finished_at = :now, updated_at = :now
                 WHERE id = :id";
    /// Finish a row, for its owner only.
    pub(super) COMPLETE = "UPDATE durable_jobs
                 SET state = 'done', last_error = NULL, lease_expires_at = NULL,
                     finished_at = :now, updated_at = :now
                 WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// End every live row of a key.
    pub(super) CANCEL = "DELETE FROM durable_jobs
                 WHERE kind = :kind AND idempotency_key = :key AND state IN ('queued', 'running')";
    /// A claimed row's key, attempt count and payload.
    pub(super) CLAIMED = "SELECT idempotency_key, attempts, payload_json FROM durable_jobs WHERE id = :id";
    /// When this kind next has work: the earliest claimable due time, or the
    /// earliest lease to run out.
    pub(super) NEXT_DUE = "SELECT min(at) FROM (
                     SELECT min(j.not_before_at) AS at FROM durable_jobs j
                     WHERE j.kind = :kind AND j.state = 'queued'
                       AND NOT EXISTS (SELECT 1 FROM durable_jobs r
                                       WHERE r.kind = j.kind AND r.idempotency_key = j.idempotency_key
                                         AND r.state = 'running')
                     UNION ALL
                     SELECT min(lease_expires_at) FROM durable_jobs WHERE kind = :kind AND state = 'running')";
    /// One row's status.
    pub(super) STATUS_BY_ID = "SELECT id, state, attempts, last_error, not_before_at, created_at, finished_at
                 FROM durable_jobs WHERE id = :id";
    /// The newest row of a key.
    pub(super) LATEST_BY_KEY = "SELECT id, state, attempts, last_error, not_before_at, created_at, finished_at
                 FROM durable_jobs WHERE kind = :kind AND idempotency_key = :key ORDER BY id DESC LIMIT 1";
    /// Live and failed rows per kind, for the admin view.
    pub(super) COUNTS = "SELECT kind,
                     sum(state = 'queued'), sum(state = 'running'), sum(state = 'failed')
                 FROM durable_jobs WHERE state <> 'done' GROUP BY kind ORDER BY kind";
    /// Kinds that have queued or running rows.
    pub(super) LIVE_KINDS = "SELECT DISTINCT kind FROM durable_jobs WHERE state IN ('queued', 'running') ORDER BY kind";
    /// Remove finished rows of one state older than a cut-off, a batch at a time.
    pub(super) PRUNE = "DELETE FROM durable_jobs WHERE id IN (
                     SELECT id FROM durable_jobs
                     WHERE kind = :kind AND state = :state AND finished_at < :before
                     LIMIT 500)";
}
