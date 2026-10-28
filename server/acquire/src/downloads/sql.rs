//! The statements of the download journal (`download_tasks`,
//! `download_attempts`, `download_idempotency_keys`, `download_quarantine`)
//! and of the queue reads built on it.

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    /// Every task column, for `TaskRow::from_row` and the
    /// queue view's extras.
    pub(super) fragment TASK_ROW = "SELECT * FROM download_tasks";
    /// One task by id (`?1`).
    pub(super) TASK_BY_ID = TASK_ROW + " WHERE id = ?1";
    /// A new queued album task.
    pub(super) INSERT_TASK = "INSERT INTO download_tasks \
             (id, user_id, artist_name, album_title, release_group_mbid, origin, \
              retry_count, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'queued', ?8, ?8)";
    /// A new queued exact-track task.
    pub(super) INSERT_TRACK_TASK = "INSERT INTO download_tasks \
             (id, user_id, artist_name, album_title, release_group_mbid, recording_mbid, \
              download_type, origin, retry_count, status, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'track', ?7, ?8, 'queued', ?9, ?9)";
    /// The newest live task for one album (`?1`).
    pub(super) NEWEST_ACTIVE_FOR_ALBUM = TASK_ROW + " WHERE release_group_mbid = ?1 \
             AND download_type = 'album' AND status IN ('queued', 'downloading', 'processing') \
             ORDER BY created_at DESC, rowid DESC LIMIT 1";
    /// Stamp the optional detail columns; NULL keeps a column.
    pub(super) SET_TASK_DETAILS = "UPDATE download_tasks \
             SET release_mbid = COALESCE(?1, release_mbid), \
                 artist_mbid = COALESCE(?2, artist_mbid), \
                 year = COALESCE(?3, year), \
                 track_title = COALESCE(?4, track_title), \
                 updated_at = ?5 \
             WHERE id = ?6";
    /// The album a single-track task resolved to.
    pub(super) SET_TRACK_ALBUM = "UPDATE download_tasks \
             SET release_group_mbid = ?1, release_mbid = ?2, album_title = ?3, \
                 release_track_mbid = ?4, track_number = ?5, disc_number = ?6, \
                 track_count = ?7, track_duration_seconds = COALESCE(?8, track_duration_seconds), \
                 year = COALESCE(?9, year), updated_at = ?10 \
             WHERE id = ?11 AND download_type = 'track'";
    /// The optional detail columns of one task (`?1`).
    pub(super) TASK_DETAILS = "SELECT release_mbid, artist_mbid, year, track_title \
             FROM download_tasks WHERE id = ?1";
    /// One task's stored plan (`?1`).
    pub(super) TASK_PLAN = "SELECT plan_json FROM download_tasks WHERE id = ?1";
    /// Replace one task's plan.
    pub(super) SET_TASK_PLAN = "UPDATE download_tasks SET plan_json = ?1, updated_at = ?2 \
             WHERE id = ?3";
    /// Tasks in any of the given statuses.
    family TASKS_IN = tasks_in;
    /// Move a live task to a status. Terminal rows never move: the guard is
    /// in the WHERE clause so a concurrent settle cannot slip in between.
    pub(super) TRANSITION_TASK = "UPDATE download_tasks \
             SET status = ?1, error_message = COALESCE(?2, error_message), \
                 outcome_code = CASE WHEN ?2 IS NULL THEN outcome_code ELSE ?6 END, \
                 last_polled_at = ?3, \
                 completed_at = CASE WHEN ?4 THEN ?3 ELSE completed_at END, \
                 updated_at = ?3 \
             WHERE id = ?5 AND status NOT IN ('completed', 'partial', 'failed', 'cancelled')";
    /// Record a poll without a status change.
    pub(super) TOUCH_POLL = "UPDATE download_tasks SET last_polled_at = ?1, updated_at = ?1 \
             WHERE id = ?2";
    /// Link a task to a picked candidate, saying whether a person picked it.
    pub(super) LINK_CANDIDATE = "UPDATE download_tasks SET source = ?1, source_username = ?2, \
             search_job_id = ?3, candidate_index = ?4, updated_at = ?5, \
             picked_by_person = ?7 WHERE id = ?6";
    /// Reimport guard: a failed or partial task with an attempt whose handle
    /// still names its files and whose cleanup has not run.
    pub(super) REIMPORTABLE = "SELECT EXISTS (SELECT 1 FROM download_tasks t \
             WHERE t.id = ?1 AND t.status IN ('failed', 'partial') \
               AND EXISTS (SELECT 1 FROM download_attempts a WHERE a.task_id = t.id \
                 AND a.handle_json != '' \
                 AND a.state IN ('preserved', 'cleanup_pending')))";
    /// Take a reimport's attempt (`?2`) back from cleanup: kept files
    /// stay kept while the reimport reads them. Nothing when cleanup holds
    /// its lease or moved it on.
    pub(super) CLAIM_REIMPORT = "UPDATE download_attempts \
             SET state = 'preserved', disposition = 'preserve', \
                 lease_owner = NULL, lease_expires_at = NULL, \
                 updated_at = ?1, row_revision = row_revision + 1 \
             WHERE id = ?2 \
               AND state IN ('preserved', 'cleanup_pending') \
               AND (lease_owner IS NULL OR lease_expires_at IS NULL \
                    OR lease_expires_at <= ?1)";
    /// Hand kept files to cleanup: those of a cancelled or removed task,
    /// and those kept since before `?2` whose task is not running. Each
    /// moved attempt as (id, task id).
    pub(super) RELEASE_KEPT = "UPDATE download_attempts \
             SET state = 'cleanup_pending', disposition = 'discard', \
                 lease_owner = NULL, lease_expires_at = NULL, next_retry_at = ?1, \
                 updated_at = ?1, row_revision = row_revision + 1 \
             WHERE state = 'preserved' \
               AND (NOT EXISTS (SELECT 1 FROM download_tasks t \
                      WHERE t.id = download_attempts.task_id AND t.status != 'cancelled') \
                    OR (COALESCE(completed_at, updated_at) < ?2 \
                        AND NOT EXISTS (SELECT 1 FROM download_tasks t \
                          WHERE t.id = download_attempts.task_id \
                            AND t.status IN ('queued', 'downloading', 'processing')))) \
             RETURNING id, task_id";
    /// Hand one task's (`?2`) kept files to cleanup.
    pub(super) RELEASE_TASK_KEPT = "UPDATE download_attempts \
             SET state = 'cleanup_pending', disposition = 'discard', \
                 lease_owner = NULL, lease_expires_at = NULL, next_retry_at = ?1, \
                 updated_at = ?1, row_revision = row_revision + 1 \
             WHERE task_id = ?2 AND state = 'preserved'";
    /// A failed or partial task back to processing for a reimport.
    pub(super) BEGIN_REIMPORT = "UPDATE download_tasks SET status = 'processing', \
             error_message = NULL, outcome_code = NULL, completed_at = NULL, last_polled_at = ?1, \
             updated_at = ?1 WHERE id = ?2 AND status IN ('failed', 'partial')";
    /// Claim an idempotency key; nothing when it is taken.
    pub(super) CLAIM_KEY = "INSERT OR IGNORE INTO download_idempotency_keys \
             (key, task_id, operation, created_at) VALUES (?1, ?2, ?3, ?4)";
    /// The task behind a claimed key (`?1`).
    pub(super) TASK_FOR_KEY = "SELECT task_id FROM download_idempotency_keys WHERE key = ?1";
    /// Release a key whose task was never written.
    pub(super) RELEASE_KEY = "DELETE FROM download_idempotency_keys WHERE key = ?1";
    /// A new attempt-journal row.
    pub(super) INSERT_ATTEMPT = "INSERT INTO download_attempts \
             (id, task_id, source, candidate_index, job_name, handle_json, \
              state, created_at, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)";
    /// Every attempt column, for `AttemptRow::from_row`.
    pub(super) fragment ATTEMPT_ROW = "SELECT * FROM download_attempts";
    /// One attempt by id (`?1`).
    pub(super) ATTEMPT_BY_ID = ATTEMPT_ROW + " WHERE id = ?1";
    /// One attempt's stored client handle (`?1`).
    pub(super) ATTEMPT_HANDLE = "SELECT handle_json FROM download_attempts WHERE id = ?1";
    /// A task's attempts (`?1`), oldest first.
    pub(super) ATTEMPTS_FOR_TASK = ATTEMPT_ROW + " WHERE task_id = ?1 \
             ORDER BY candidate_index, created_at";
    /// Stored handles of a task's (`?1`) attempts on one source (`?2`).
    pub(super) SOURCE_HANDLES = "SELECT handle_json FROM download_attempts \
             WHERE task_id = ?1 AND source = ?2 AND handle_json != ''";
    /// The newest handle a task's (`?1`) attempts recorded.
    pub(super) NEWEST_HANDLE = "SELECT handle_json FROM download_attempts \
             WHERE task_id = ?1 AND handle_json != '' \
             ORDER BY candidate_index DESC, created_at DESC LIMIT 1";
    /// Move an attempt behind its revision; every move bumps the revision.
    pub(super) TRANSITION_ATTEMPT = "UPDATE download_attempts \
             SET state = ?1, \
                 disposition = COALESCE(?2, disposition), \
                 error_code = ?3, \
                 lease_owner = CASE WHEN ?4 THEN NULL ELSE lease_owner END, \
                 lease_expires_at = CASE WHEN ?4 THEN NULL ELSE lease_expires_at END, \
                 completed_at = CASE WHEN ?1 IN ('complete', 'preserved', 'needs_attention') \
                                     THEN ?5 ELSE completed_at END, \
                 updated_at = ?5, row_revision = row_revision + 1 \
             WHERE id = ?6 AND row_revision = ?7";
    /// Lease due cleanup attempts to one worker (`?1`) until `?2`.
    pub(super) CLAIM_CLEANUP = "UPDATE download_attempts \
             SET lease_owner = ?1, lease_expires_at = ?2, updated_at = ?3, \
                 row_revision = row_revision + 1 \
             WHERE id IN ( \
               SELECT id FROM download_attempts \
               WHERE state IN ('cleanup_pending', 'workspace_removed', 'needs_attention') \
                 AND next_retry_at <= ?3 \
                 AND (lease_owner IS NULL OR lease_expires_at IS NULL \
                      OR lease_expires_at <= ?3) \
               ORDER BY next_retry_at, created_at, id LIMIT ?4)";
    /// The attempts one claim leased.
    pub(super) CLAIMED_ATTEMPTS = ATTEMPT_ROW + " WHERE lease_owner = ?1 AND lease_expires_at = ?2 \
             ORDER BY next_retry_at, created_at, id";
    /// Defer a cleanup behind its revision.
    pub(super) RECORD_CLEANUP_FAILURE = "UPDATE download_attempts \
             SET cleanup_failures = cleanup_failures + 1, error_code = ?1, \
                 next_retry_at = ?2, lease_owner = NULL, lease_expires_at = NULL, \
                 updated_at = ?3, row_revision = row_revision + 1 \
             WHERE id = ?4 AND row_revision = ?5";
    /// Tasks whose files cleanup could not remove.
    pub(super) CLEANUP_WARNING_COUNT = "SELECT COUNT(DISTINCT task_id) FROM download_attempts \
             WHERE state = 'needs_attention' \
                OR (state IN ('cleanup_pending', 'workspace_removed') \
                    AND cleanup_failures >= 3)";
    /// Live cleanup debt for one client job; kept files count.
    pub(super) CLEANUP_DEBT = "SELECT COUNT(*) FROM download_attempts \
             WHERE source = ?1 AND task_id = ?2 AND job_name = ?3 \
               AND state != 'complete'";
    /// Cancel a live task.
    pub(super) CANCEL_TASK = "UPDATE download_tasks \
             SET status = 'cancelled', last_polled_at = ?1, completed_at = ?1, \
                 cancelled_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status NOT IN ('completed', 'partial', 'failed', 'cancelled')";
    /// Hand a cancelled task's live attempts to cleanup.
    pub(super) CANCEL_ATTEMPTS = "UPDATE download_attempts \
             SET state = 'cleanup_pending', disposition = 'discard', \
                 lease_owner = NULL, lease_expires_at = NULL, next_retry_at = ?1, \
                 updated_at = ?1, row_revision = row_revision + 1 \
             WHERE task_id = ?2 AND state IN ('acquiring', 'in_use')";
    /// Drop quarantine rows older than the cutoff.
    pub(super) PRUNE_QUARANTINE = "DELETE FROM download_quarantine WHERE quarantined_at < ?1";
    /// Blocklist one release; nothing when it already is.
    pub(super) INSERT_QUARANTINE = "INSERT OR IGNORE INTO download_quarantine \
             (source, identity, release_group_mbid, reason, quarantined_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)";
    /// Live quarantine pairs at or after a cutoff.
    pub(super) QUARANTINE_SET = "SELECT source, identity FROM download_quarantine \
             WHERE quarantined_at >= ?1";
    /// Every quarantine column, for `QuarantineRow::from_row`.
    pub(super) fragment QUARANTINE_ROW = "SELECT id, source, identity, release_group_mbid, \
             reason, quarantined_at FROM download_quarantine";
    /// Every quarantine row, newest first.
    pub(super) QUARANTINE_ALL = QUARANTINE_ROW + " ORDER BY quarantined_at DESC";
    /// One page of quarantine rows (`?1` rows from `?2`), newest first.
    pub(super) QUARANTINE_PAGE = QUARANTINE_ROW + " ORDER BY quarantined_at DESC, id DESC \
             LIMIT ?1 OFFSET ?2";
    /// Clear an album's (`?1`) blocklist.
    pub(super) DELETE_QUARANTINE_FOR_ALBUM = "DELETE FROM download_quarantine \
             WHERE release_group_mbid = ?1";
    /// Retryable tasks: failed or partial, under the retry ceiling (`?1`),
    /// with no newer task for the same ask.
    pub(super) RETRYABLE = TASK_ROW + " t \
             WHERE t.status IN ('failed', 'partial') \
               AND t.origin != 'upgrade' \
               AND t.retry_count < ?1 \
               AND NOT EXISTS ( \
                 SELECT 1 FROM download_tasks n \
                 WHERE n.user_id = t.user_id \
                   AND n.download_type = t.download_type \
                   AND (t.download_type = 'track' \
                        OR n.release_group_mbid = t.release_group_mbid) \
                   AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
                   AND n.origin != 'upgrade' \
                   AND (n.created_at > t.created_at \
                        OR (n.created_at = t.created_at AND n.rowid > t.rowid))) \
             ORDER BY t.completed_at ASC NULLS LAST";
    /// The retry anchor of one task (`?1`) while it is still the newest task
    /// for its ask.
    pub(super) RETRY_ANCHOR = "SELECT t.status, t.retry_count, t.completed_at, t.updated_at \
             FROM download_tasks t WHERE t.id = ?1 AND t.origin != 'upgrade' \
             AND NOT EXISTS (SELECT 1 FROM download_tasks n \
               WHERE n.user_id = t.user_id AND n.download_type = t.download_type \
                 AND (t.download_type = 'track' \
                      OR n.release_group_mbid = t.release_group_mbid) \
                 AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
                 AND n.origin != 'upgrade' \
                 AND (n.created_at > t.created_at \
                      OR (n.created_at = t.created_at AND n.rowid > t.rowid)))";
    /// The newest task one owner (`?1`) started for an album or recording
    /// (`?2` kind, `?3` key) at or after `?4`.
    pub(super) TASK_SINCE = "SELECT id FROM download_tasks WHERE user_id = ?1 \
             AND ((?2 = 'track' AND recording_mbid = ?3) \
                  OR (?2 != 'track' AND download_type = 'album' \
                      AND release_group_mbid = ?3)) \
             AND created_at >= ?4 ORDER BY created_at DESC, rowid DESC LIMIT 1";
    /// Live tasks no poller touched since `?1`.
    pub(super) UNPOLLED_ACTIVE = TASK_ROW + " WHERE status IN ('downloading', 'processing') \
             AND COALESCE(last_polled_at, started_at, created_at, 0) < ?1";
    /// A viewer's (`?1`, NULL for all) tasks, filtered by status (`?2`) and
    /// album (`?3`), one page (`?4` rows from `?5`), newest first.
    pub(super) QUEUE_PAGE = TASK_ROW + " WHERE (?1 IS NULL OR user_id = ?1) \
             AND (?2 IS NULL OR status = ?2) AND (?3 IS NULL OR release_group_mbid = ?3) \
             ORDER BY created_at DESC, rowid DESC LIMIT ?4 OFFSET ?5";
    /// Attempt counts and cleanup summaries per task.
    family ATTEMPT_FACTS = attempt_facts;
    /// The newest import decision per task.
    family DECISION_FACTS = decision_facts;
    /// The admins' activity revision.
    pub(super) GLOBAL_REVISION = "SELECT revision FROM download_activity_global_revision \
             WHERE singleton = 1";
    /// One user's (`?1`) activity revision.
    pub(super) USER_REVISION = "SELECT revision FROM download_activity_user_revisions \
             WHERE user_id = ?1";
    /// Live and failed task counts for a viewer (`?1`, NULL for all).
    pub(super) ACTIVITY_COUNTS = "SELECT \
             COALESCE(SUM(status IN ('queued', 'downloading', 'processing')), 0), \
             COALESCE(SUM(status = 'failed'), 0) \
             FROM download_tasks WHERE (?1 IS NULL OR user_id = ?1)";
    /// The 20 albums that landed most recently for a viewer (`?1`).
    pub(super) LANDED_ALBUMS = "SELECT release_group_mbid FROM download_tasks \
             WHERE status IN ('completed', 'partial') AND release_group_mbid != '' \
               AND (?1 IS NULL OR user_id = ?1) \
             GROUP BY release_group_mbid \
             ORDER BY MAX(COALESCE(completed_at, updated_at)) DESC, release_group_mbid ASC \
             LIMIT 20";
    /// A viewer's (`?1`) failed and short tasks that are still the newest for
    /// their album or track, oldest first.
    pub(super) NEWEST_UNSETTLED = TASK_ROW + " t \
             WHERE t.status IN ('failed', 'partial') AND (?1 IS NULL OR t.user_id = ?1) \
               AND NOT EXISTS ( \
                 SELECT 1 FROM download_tasks n \
                 WHERE n.user_id = t.user_id AND n.download_type = t.download_type \
                   AND n.release_group_mbid = t.release_group_mbid \
                   AND COALESCE(n.recording_mbid, '') = COALESCE(t.recording_mbid, '') \
                   AND (n.created_at > t.created_at \
                        OR (n.created_at = t.created_at AND n.rowid > t.rowid))) \
             ORDER BY t.created_at ASC";
    /// Files imported over every landing of a task (`?1`).
    pub(super) FILES_IMPORTED = "SELECT COALESCE(SUM(files_imported), 0) \
             FROM download_import_decisions WHERE task_id = ?1";
    /// Every decision column, in the order `ImportDecisionRow::from_row`
    /// reads them.
    pub(super) fragment DECISION_ROW = "SELECT task_id, attempt_id, outcome, reason_code, \
             detail, release_mbid, distance, files_total, files_imported, files_held, \
             checks_json, missing_positions, decided_at \
             FROM download_import_decisions";
    /// The newest decision of a task (`?1`).
    pub(super) LATEST_DECISION = DECISION_ROW + " WHERE task_id = ?1 \
             ORDER BY decided_at DESC, id DESC LIMIT 1";
    /// The newest decision of a task (`?1`) that imported files.
    pub(super) LATEST_IMPORT = DECISION_ROW + " WHERE task_id = ?1 \
             AND outcome IN ('imported', 'partial') \
             ORDER BY decided_at DESC, id DESC LIMIT 1";
    /// The newest decision of a task (`?1`) for one attempt (`?2`, NULL for
    /// none) that is not a deferral.
    pub(super) LAST_LANDING_OF_ATTEMPT = DECISION_ROW + " WHERE task_id = ?1 \
             AND attempt_id IS ?2 AND outcome != 'deferred' \
             ORDER BY decided_at DESC, id DESC LIMIT 1";
    /// Every distinct reason spelling stored on decisions, task outcomes
    /// and drop items.
    pub(super) STORED_REASONS = "SELECT reason_code FROM download_import_decisions \
               WHERE reason_code IS NOT NULL \
             UNION SELECT outcome_code FROM download_tasks WHERE outcome_code IS NOT NULL \
             UNION SELECT reason_code FROM drop_import_items WHERE reason_code IS NOT NULL";
    /// Record one landing's decision.
    pub(super) INSERT_DECISION = "INSERT INTO download_import_decisions (task_id, attempt_id, \
             outcome, reason_code, detail, release_mbid, distance, files_total, files_imported, \
             files_held, checks_json, missing_positions, decided_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)";
    /// Stamp a task's (`?3`) wrong-product verdict; the first one stands.
    pub(super) SET_WRONG_PRODUCT = "UPDATE download_tasks \
             SET wrong_product_verdict_at = ?1, wrong_product_detail = ?2 \
             WHERE id = ?3 AND wrong_product_verdict_at IS NULL";
    /// Clear a task's (`?1`) wrong-product verdict.
    pub(super) CLEAR_WRONG_PRODUCT = "UPDATE download_tasks \
             SET wrong_product_verdict_at = NULL, wrong_product_detail = NULL WHERE id = ?1";
    /// A failed or short task (`?2`) a person made whole by hand completes.
    pub(super) COMPLETE_AFTER_MANUAL_IMPORT = "UPDATE download_tasks \
             SET status = 'completed', error_message = NULL, outcome_code = NULL, \
                 completed_at = ?1, updated_at = ?1 \
             WHERE id = ?2 AND status IN ('failed', 'partial')";
    /// Stop retrying an album's (`?1`) failed and short tasks made at or
    /// before `?3`; answers their ids.
    pub(super) PURGE_ALBUM_TASKS = "UPDATE download_tasks \
             SET status = 'cancelled', cancelled_at = ?2, remote_queued = 0, updated_at = ?2 \
             WHERE lower(release_group_mbid) = lower(?1) AND status IN ('failed', 'partial') \
               AND created_at <= ?3 RETURNING id";
    /// Clear an album's (`?1`) blocklist entries made at or before `?2`.
    pub(super) PURGE_ALBUM_QUARANTINE = "DELETE FROM download_quarantine \
             WHERE lower(release_group_mbid) = lower(?1) AND quarantined_at <= ?2";
    /// One poll's progress on a live task (`?1`).
    pub(super) RECORD_PROGRESS = "UPDATE download_tasks SET downloaded_bytes = ?2, \
             total_size_bytes = COALESCE(?3, total_size_bytes), progress_percent = ?4, \
             files_total = ?5, files_completed = ?6, files_failed = ?7, \
             queue_position_start = ?8, queue_position_end = ?9, remote_queued = ?10, \
             last_polled_at = ?11 \
             WHERE id = ?1 AND status IN ('downloading', 'processing')";
    /// Stop retrying one failed or short task (`?1`).
    pub(super) STOP_RETRIES = "UPDATE download_tasks \
             SET status = 'cancelled', cancelled_at = ?2, remote_queued = 0, updated_at = ?2 \
             WHERE id = ?1 AND status IN ('failed', 'partial')";
    /// Stop an owner's (`?2`) wanted watch on an album (`?1`).
    pub(super) STOP_WATCH = "UPDATE wanted_watches SET state = 'stopped' \
             WHERE release_group_mbid_lower = lower(?1) AND user_id = ?2 \
               AND state = 'watching'";
    /// Delete finished tasks of one owner (`?1`, NULL for everyone).
    pub(super) CLEAR_FINISHED = "DELETE FROM download_tasks \
             WHERE status IN ('completed', 'cancelled') AND (?1 IS NULL OR user_id = ?1)";
    /// Remove one quarantine entry (`?1`).
    pub(super) DELETE_QUARANTINE = "DELETE FROM download_quarantine WHERE id = ?1";
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// Tasks in any of `statuses` statuses.
pub(super) fn tasks_in(statuses: ListLen) -> String {
    format!("{TASK_ROW} WHERE status IN ({})", marks(statuses))
}

/// Per task: attempts that reached a client, and the cleanup summary.
pub(super) fn attempt_facts(tasks: ListLen) -> String {
    format!(
        "SELECT task_id, SUM(handle_json != ''), \
           CASE \
             WHEN MAX(state = 'needs_attention') THEN 'needs_attention' \
             WHEN MAX(state = 'preserved') THEN 'preserved' \
             WHEN MAX(state IN ('cleanup_pending', 'workspace_removed')) THEN 'pending' \
             WHEN MAX(state IN ('acquiring', 'in_use')) THEN 'in_use' \
             WHEN MAX(state = 'complete') THEN 'complete' \
             ELSE 'not_tracked' \
           END \
         FROM download_attempts WHERE task_id IN ({}) GROUP BY task_id",
        marks(tasks)
    )
}

/// The newest import decision of each task in the list.
pub(super) fn decision_facts(tasks: ListLen) -> String {
    format!(
        "SELECT d.task_id, d.outcome, d.reason_code, d.detail, \
                d.files_total, d.files_imported, d.files_held, d.decided_at \
         FROM download_import_decisions d \
         WHERE d.task_id IN ({}) AND d.id = ( \
           SELECT n.id FROM download_import_decisions n WHERE n.task_id = d.task_id \
           ORDER BY n.decided_at DESC, n.id DESC LIMIT 1)",
        marks(tasks)
    )
}
