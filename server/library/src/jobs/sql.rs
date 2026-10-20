//! The statements of `library_operation_jobs` and `library_operation_work`.
//! The five lease statements follow the named-parameter contract of the
//! kernel's lease core: times arrive there as integer milliseconds and are
//! stored as seconds. The rest are this module's own.

droppedneedle_kernel::sql! {
    /// One job row; the column order [`super::map_job`] reads.
    pub(super) fragment JOB_ROW = "SELECT id, kind, state, requested_by_user_id, \
             expected_work_count, completed_count, succeeded_count, failed_count, skipped_count, \
             control_request, terminal_code, reidentification_attempt_count, row_revision, \
             event_revision, created_at, updated_at FROM library_operation_jobs";
    /// One job by id (`?1`).
    pub(super) JOB_BY_ID = JOB_ROW + " WHERE id = ?1";
    /// One job by its row number (`?1`), after a claim.
    pub(super) JOB_BY_ROWID = JOB_ROW + " WHERE rowid = ?1";
    /// The job an idempotency key (`?1`) made.
    pub(super) JOB_BY_KEY = JOB_ROW + " WHERE idempotency_key = ?1";
    /// A job's row number (`?1` is its id), for the lease core.
    pub(super) ROWID = "SELECT rowid FROM library_operation_jobs WHERE id = ?1";
    /// A new queued job.
    pub(super) INSERT_JOB = "INSERT INTO library_operation_jobs (id, kind, state, \
             requested_by_user_id, input_catalog_revision, expected_work_count, idempotency_key, \
             created_at, updated_at) \
             VALUES (:id, :kind, 'queued', :requested_by, :catalog_revision, :work_count, :key, \
             :now, :now)";
    /// A job of a kind that is already done, under an idempotency key;
    /// nothing when the key is taken.
    pub(super) INSERT_DONE = "INSERT INTO library_operation_jobs (id, kind, state, \
             terminal_code, idempotency_key, created_at, updated_at, terminal_at) \
             VALUES (:id, :kind, 'succeeded', :code, :key, :now, :now, :now) \
             ON CONFLICT DO NOTHING";
    /// A new pending work item.
    pub(super) INSERT_WORK = "INSERT INTO library_operation_work (job_id, ordinal, \
             local_album_id, local_track_id, expected_subject_revision, expected_input_revision, \
             action, idempotency_key, state, updated_at) \
             VALUES (:job, :ordinal, :album, :track, :subject_revision, :input_revision, :action, \
             :key, 'pending', :now)";

    /// Take the next due queued job of a kind. A management job waits in
    /// the queue only while it has files to plan or write.
    pub(super) CLAIM = "UPDATE library_operation_jobs SET state = 'running', \
             started_at = COALESCE(started_at, :now / 1000.0), lease_owner = :owner, \
             lease_expires_at = :lease_until / 1000.0, heartbeat_at = :now / 1000.0, \
             updated_at = :now / 1000.0, next_attempt_at = NULL, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = (SELECT j.id FROM library_operation_jobs j \
                 WHERE j.kind = :scope AND j.state = 'queued' \
                 AND (j.next_attempt_at IS NULL OR j.next_attempt_at <= :now / 1000.0) \
                 AND (j.kind <> 'library_management' OR EXISTS (SELECT 1 \
                     FROM library_management_job_snapshots s WHERE s.job_id = j.id \
                     AND s.phase IN ('planning', 'applying', 'undoing', 'restoring'))) \
                 ORDER BY j.updated_at, j.created_at, j.id LIMIT 1) \
             RETURNING rowid";
    /// Extend a lease, for its owner only.
    pub(super) HEARTBEAT = "UPDATE library_operation_jobs \
             SET lease_expires_at = :lease_until / 1000.0, heartbeat_at = :now / 1000.0 \
             WHERE rowid = :id AND state = 'running' AND lease_owner = :owner";
    /// Put a running job back in the queue, for its owner only.
    pub(super) RELEASE = "UPDATE library_operation_jobs SET state = 'queued', \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = :now / 1000.0, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE rowid = :id AND state = 'running' AND lease_owner = :owner";
    /// Requeue the jobs of a kind whose lease ran out.
    pub(super) RECOVER_EXPIRED = "UPDATE library_operation_jobs SET state = 'queued', \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = :now / 1000.0, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE kind = :scope AND state = 'running' AND lease_expires_at <= :now / 1000.0";
    /// Requeue every running job of a kind: an earlier process held them.
    pub(super) RECOVER_AT_START = "UPDATE library_operation_jobs SET state = 'queued', \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = :now / 1000.0, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE kind = :scope AND state = 'running'";
    /// The running work of the jobs [`RECOVER_EXPIRED`] is about to
    /// requeue goes back to pending.
    pub(super) WORK_RECOVER_EXPIRED = "UPDATE library_operation_work SET state = 'pending', \
             updated_at = :now, row_revision = row_revision + 1 \
             WHERE state = 'running' AND job_id IN (SELECT id FROM library_operation_jobs \
                 WHERE kind = :kind AND state = 'running' AND lease_expires_at <= :now)";
    /// The running work of the jobs [`RECOVER_AT_START`] is about to
    /// requeue goes back to pending.
    pub(super) WORK_RECOVER_AT_START = "UPDATE library_operation_work SET state = 'pending', \
             updated_at = :now, row_revision = row_revision + 1 \
             WHERE state = 'running' AND job_id IN (SELECT id FROM library_operation_jobs \
                 WHERE kind = :kind AND state = 'running')";

    /// Whether an owner holds a running job.
    pub(super) HOLDS = "SELECT EXISTS (SELECT 1 FROM library_operation_jobs \
             WHERE id = :id AND state = 'running' AND lease_owner = :owner)";
    /// The control an owner's running job carries.
    pub(super) HELD_CONTROL = "SELECT control_request FROM library_operation_jobs \
             WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// Honour a pause or a stop: the job leaves `running` and its lease.
    pub(super) HALT = "UPDATE library_operation_jobs SET state = :state, \
             control_request = 'none', terminal_code = COALESCE(terminal_code, :code), \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = :now, \
             terminal_at = CASE WHEN :state = 'stopped' THEN :now ELSE terminal_at END, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :id";
    /// A job's running work goes back to pending, with `:code` as its
    /// reason when one is given.
    pub(super) WORK_BACK_TO_PENDING = "UPDATE library_operation_work SET state = 'pending', \
             failure_code = COALESCE(:code, failure_code), updated_at = :now, \
             row_revision = row_revision + 1 \
             WHERE job_id = :job AND state = 'running'";
    /// End an owner's running job.
    pub(super) FINISH = "UPDATE library_operation_jobs SET state = :state, \
             terminal_code = :code, terminal_at = :now, updated_at = :now, \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             completed_count = CASE WHEN :tally = 0 THEN completed_count \
                 ELSE expected_work_count END, \
             succeeded_count = CASE WHEN :tally = 2 THEN expected_work_count \
                 ELSE succeeded_count END, \
             failed_count = CASE WHEN :tally = 1 THEN expected_work_count ELSE failed_count END, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// An owner's running job has planned its work and waits for a person.
    pub(super) PARK_READY = "UPDATE library_operation_jobs SET state = 'ready', \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = :now, expected_work_count = 0, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// An owner's running job has checked its work and waits for a
    /// person, its counts and code kept.
    pub(super) REPORT_READY = "UPDATE library_operation_jobs SET state = 'ready', \
             terminal_code = :code, terminal_at = NULL, lease_owner = NULL, \
             lease_expires_at = NULL, heartbeat_at = NULL, updated_at = :now, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :id AND state = 'running' AND lease_owner = :owner";
    /// Set a ready job aside without doing its work.
    pub(super) CANCEL_READY = "UPDATE library_operation_jobs SET state = 'cancelled', \
             terminal_code = :code, terminal_at = :now, control_request = 'none', \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, \
             updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND state = 'ready'";
    /// A released job waits until `:at` before a claim takes it again.
    pub(super) RETRY_AT = "UPDATE library_operation_jobs SET next_attempt_at = :at \
             WHERE id = :id AND state = 'queued'";
    /// End a ready job that has nothing to write, as done.
    pub(super) COMPLETE_READY = "UPDATE library_operation_jobs SET state = 'succeeded', \
             terminal_code = :code, terminal_at = :now, expected_work_count = 0, \
             updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND state = 'ready'";
    /// Queue a job again with fresh counts for `:work_count` items.
    pub(super) QUEUE_WORK = "UPDATE library_operation_jobs SET state = 'queued', \
             expected_work_count = :work_count, completed_count = 0, succeeded_count = 0, \
             failed_count = 0, skipped_count = 0, terminal_code = NULL, terminal_at = NULL, \
             updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id";
    /// Set a management preview aside: one still planning or waiting to be
    /// applied, under the revision the caller saw.
    pub(super) DISCARD_PREVIEW = "UPDATE library_operation_jobs SET state = 'cancelled', \
             terminal_code = :code, terminal_at = :now, updated_at = :now, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :id AND kind = 'library_management' AND row_revision = :revision \
             AND state IN ('ready', 'queued', 'paused', 'stopped', 'failed') \
             AND id IN (SELECT job_id FROM library_management_job_snapshots \
                 WHERE phase IN ('planning', 'ready'))";

    /// Ask the running worker to pause or stop.
    pub(super) CONTROL_REQUEST = "UPDATE library_operation_jobs SET control_request = :request, \
             updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND row_revision = :revision";
    /// Stop a job nothing is running.
    pub(super) CONTROL_STOP_NOW = "UPDATE library_operation_jobs SET state = 'stopped', \
             control_request = 'none', terminal_code = 'STOPPED', terminal_at = :now, \
             lease_owner = NULL, lease_expires_at = NULL, heartbeat_at = NULL, updated_at = :now, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :id AND row_revision = :revision";
    /// Queue a job again from the start.
    pub(super) CONTROL_RESTART = "UPDATE library_operation_jobs SET state = 'queued', \
             control_request = 'none', terminal_code = NULL, terminal_at = NULL, \
             next_attempt_at = NULL, completed_count = 0, succeeded_count = 0, failed_count = 0, \
             skipped_count = 0, updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND row_revision = :revision";
    /// Queue a job again for its failed work.
    pub(super) CONTROL_RETRY_FAILED = "UPDATE library_operation_jobs SET state = 'queued', \
             control_request = 'none', terminal_code = NULL, terminal_at = NULL, \
             next_attempt_at = NULL, reidentification_attempt_count = 0, completed_count = 0, \
             failed_count = 0, updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND row_revision = :revision";
    /// Queue a job again where it left off.
    pub(super) CONTROL_CONTINUE = "UPDATE library_operation_jobs SET state = 'queued', \
             control_request = 'none', terminal_code = NULL, terminal_at = NULL, \
             updated_at = :now, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 \
             WHERE id = :id AND row_revision = :revision";
    /// Every work item of a job back to pending, its outcome and bundle
    /// cleared.
    pub(super) WORK_RESET_ALL = "UPDATE library_operation_work SET state = 'pending', \
             failure_code = NULL, result_json = NULL, bundle_id = NULL, updated_at = :now, \
             row_revision = row_revision + 1 WHERE job_id = :job";
    /// A job's failed and running work back to pending, its outcome and
    /// bundle cleared.
    pub(super) WORK_RESET_FAILED = "UPDATE library_operation_work SET state = 'pending', \
             failure_code = NULL, result_json = NULL, bundle_id = NULL, updated_at = :now, \
             row_revision = row_revision + 1 \
             WHERE job_id = :job AND state IN ('failed', 'running')";

    /// A job's next pending work item.
    pub(super) NEXT_WORK = "SELECT ordinal FROM library_operation_work \
             WHERE job_id = :job AND state = 'pending' ORDER BY ordinal LIMIT 1";
    /// Start one work item.
    pub(super) START_WORK = "UPDATE library_operation_work SET state = 'running', \
             updated_at = :now, row_revision = row_revision + 1 \
             WHERE job_id = :job AND ordinal = :ordinal";
    /// Record one work item's outcome.
    pub(super) FINISH_WORK = "UPDATE library_operation_work SET state = :state, \
             failure_code = :code, result_json = :result, updated_at = :now, \
             row_revision = row_revision + 1 WHERE job_id = :job AND ordinal = :ordinal";
    /// Count one finished work item on its job.
    pub(super) COUNT_WORK = "UPDATE library_operation_jobs \
             SET completed_count = completed_count + 1, \
             succeeded_count = succeeded_count + (:state = 'succeeded'), \
             failed_count = failed_count + (:state = 'failed'), \
             skipped_count = skipped_count + (:state = 'skipped'), updated_at = :now, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = :job";
    /// Record the outcome of a job's running work.
    pub(super) FINISH_RUNNING_WORK = "UPDATE library_operation_work SET state = :state, \
             failure_code = :code, result_json = COALESCE(:result, result_json), \
             updated_at = :now, row_revision = row_revision + 1 \
             WHERE job_id = :job AND state = 'running'";

    /// The publish bundle a work item writes under, once it has one.
    pub(super) WORK_BUNDLE = "SELECT bundle_id FROM library_operation_work \
             WHERE job_id = :job AND ordinal = :ordinal";
    /// Record the publish bundle a work item is about to write under.
    pub(super) SET_WORK_BUNDLE = "UPDATE library_operation_work SET bundle_id = :bundle, \
             row_revision = row_revision + 1 WHERE job_id = :job AND ordinal = :ordinal";
    /// The work item (job, ordinal) that wrote under a bundle.
    pub(super) WORK_OF_BUNDLE = "SELECT job_id, ordinal FROM library_operation_work \
             WHERE bundle_id = :bundle";
}
