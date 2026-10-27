//! The statements of the manual search jobs (`search_jobs`) and of the
//! download a job parks or starts.

droppedneedle_kernel::sql! {
    /// A new job.
    pub(super) INSERT_JOB = "INSERT INTO search_jobs (id, user_id, artist_name, album_title, \
             year, release_group_mbid, search_query, status, candidates_blob, created_at, \
             updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10)";
    /// A job the automatic downloader's search finished, in one go.
    pub(super) INSERT_REVIEWED_JOB = "INSERT INTO search_jobs (id, user_id, artist_name, \
             album_title, year, release_group_mbid, search_query, status, candidates_blob, \
             created_at, updated_at, completed_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?10, ?10)";
    /// One job (`?1`), in the order `JobRow::from_row` reads it.
    pub(super) JOB_BY_ID = "SELECT id, user_id, artist_name, album_title, year, \
             release_group_mbid, status, candidates_blob, error_message \
             FROM search_jobs WHERE id = ?1";
    /// One job's (`?1`) status.
    pub(super) JOB_STATUS = "SELECT status FROM search_jobs WHERE id = ?1";
    /// The newest download a pick started from a job (`?1`).
    pub(super) PICKED_TASK = "SELECT id FROM download_tasks WHERE search_job_id = ?1 \
             ORDER BY created_at DESC LIMIT 1";
    /// The download parked on a job (`?1`): queued, linked to the job, no
    /// candidate picked yet.
    pub(super) PARKED_TASK = "SELECT id FROM download_tasks WHERE search_job_id = ?1 \
             AND candidate_index IS NULL AND status = 'queued' ORDER BY created_at LIMIT 1";
    /// Whether a task (`?1`) is still queued and linked to no job.
    pub(super) TASK_OPEN_FOR_REVIEW = "SELECT EXISTS(SELECT 1 FROM download_tasks \
             WHERE id = ?1 AND status = 'queued' AND search_job_id IS NULL)";
    /// Park a task (`?1`) on a job (`?2`).
    pub(super) PARK_TASK = "UPDATE download_tasks SET search_job_id = ?2, updated_at = ?3 \
             WHERE id = ?1";
    /// Store a search's outcome while the job (`?1`) is still `?6`.
    pub(super) FINISH_JOB = "UPDATE search_jobs SET status = ?2, candidates_blob = ?3, \
             error_message = ?4, completed_at = ?5, updated_at = ?5 \
             WHERE id = ?1 AND status = ?6";
    /// Close a job (`?1`) from any of three statuses.
    pub(super) CLOSE_JOB = "UPDATE search_jobs SET status = ?2, updated_at = ?3 \
             WHERE id = ?1 AND status IN (?4, ?5, ?6)";
    /// Move a job (`?1`) from one status (`?5`) to another.
    pub(super) TRANSITION_JOB = "UPDATE search_jobs SET status = ?2, \
             error_message = COALESCE(?3, error_message), updated_at = ?4 \
             WHERE id = ?1 AND status = ?5";
    /// Set a job's (`?1`) status.
    pub(super) SET_JOB_STATUS = "UPDATE search_jobs SET status = ?2, updated_at = ?3 \
             WHERE id = ?1";
    /// Unpicked jobs last touched before `?5`, unless a download is parked
    /// on them.
    pub(super) PRUNE_STALE = "DELETE FROM search_jobs WHERE status IN (?1, ?2, ?3, ?4) \
             AND updated_at < ?5 AND NOT EXISTS ( \
             SELECT 1 FROM download_tasks t WHERE t.search_job_id = search_jobs.id \
             AND t.status = 'queued')";
    /// Picked jobs whose download finished or was removed.
    pub(super) PRUNE_PICKED = "DELETE FROM search_jobs WHERE status = ?1 AND NOT EXISTS ( \
             SELECT 1 FROM download_tasks t WHERE t.search_job_id = search_jobs.id \
             AND t.status IN ('queued', 'downloading', 'processing'))";
}
