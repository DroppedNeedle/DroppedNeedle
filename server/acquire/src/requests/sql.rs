//! The statements that move a request with its download: settling rows
//! when their task finishes or their album reaches the library, linking
//! rows to a new download, and the sweep that finds rows left behind.

droppedneedle_kernel::sql! {
    /// Settle the rows linked to one task (`?1`) as `?2`, stamping
    /// `completed_at` with `?3` unless it is NULL. Live rows always match;
    /// with `?4` set, failed and incomplete rows match too (a completed
    /// task imports them).
    pub(crate) SETTLE_TASK_ROWS = "UPDATE request_history SET status = ?2, \
             completed_at = COALESCE(?3, completed_at), generation = generation + 1 \
         WHERE download_task_id = ?1 \
           AND (status IN ('pending', 'queued', 'downloading') \
                OR (?4 AND status IN ('failed', 'incomplete'))) \
         RETURNING musicbrainz_id_lower, request_kind, user_id, track_release_group_mbid";
    /// An album row (`?1`, lowercased) whose album the library holds:
    /// imported at `?2` unless a person decided it or it is mid-cancel.
    pub(crate) SETTLE_ALBUM_ROW = "UPDATE request_history SET status = 'imported', \
             completed_at = ?2, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1 AND request_kind = 'album' \
           AND status IN ('pending', 'queued', 'downloading', 'awaiting_approval', \
                          'failed', 'incomplete') \
         RETURNING musicbrainz_id_lower, request_kind, user_id, track_release_group_mbid";
    /// What a settled task says about its album: `(download_type,
    /// release_group_mbid)`.
    pub(crate) TASK_ALBUM = "SELECT download_type, release_group_mbid FROM download_tasks \
         WHERE id = ?1";
    /// The album's watch (`?1`) is fulfilled, with outcome `?2` at `?3`.
    pub(crate) FULFIL_WATCH = "UPDATE wanted_watches SET state = 'fulfilled', \
             last_outcome = ?2, last_checked_at = ?3, new_candidate_count = 0 \
         WHERE release_group_mbid_lower = lower(?1)";
    /// Link one row (`?1`) to a new download (`?2`) behind the optional
    /// generation guard `?3`. Only a row a download may serve moves; it
    /// then reads downloading.
    pub(crate) LINK_ROW = "UPDATE request_history SET download_task_id = ?2, \
             status = 'downloading', completed_at = NULL, generation = generation + 1 \
         WHERE musicbrainz_id_lower = ?1 AND (?3 IS NULL OR generation = ?3) \
           AND status IN ('pending', 'queued', 'downloading', 'failed', 'incomplete', \
                          'cancelled')";
    /// Move the rows that followed task `?1` onto its retry `?2`.
    pub(crate) RELINK_ROWS = "UPDATE request_history SET download_task_id = ?2, \
             status = 'downloading', completed_at = NULL, generation = generation + 1 \
         WHERE download_task_id = ?1 \
           AND status IN ('pending', 'queued', 'downloading', 'failed', 'incomplete', \
                          'cancelled') \
         RETURNING musicbrainz_id_lower";
    /// The request a task (`?1`) serves (`?2`), the link's other end.
    pub(crate) MARK_TASK_REQUEST = "UPDATE download_tasks SET request_history_mbid = ?2 \
         WHERE id = ?1";
    /// Finished tasks whose linked rows did not follow them:
    /// `(task id, task status)`.
    pub(crate) UNSETTLED_TASKS = "SELECT DISTINCT dt.id, dt.status FROM request_history rh \
         JOIN download_tasks dt ON dt.id = rh.download_task_id \
         WHERE dt.status IN ('completed', 'partial', 'failed', 'cancelled') \
           AND (rh.status IN ('pending', 'queued', 'downloading') \
                OR (dt.status = 'completed' AND rh.status IN ('failed', 'incomplete')))";
    /// Album rows only the library can settle: open rows with no task left
    /// to follow, and failed rows whose task is gone.
    pub(crate) ALBUMS_WITHOUT_TASK = "SELECT rh.musicbrainz_id_lower FROM request_history rh \
         WHERE rh.request_kind = 'album' \
           AND NOT EXISTS (SELECT 1 FROM download_tasks dt WHERE dt.id = rh.download_task_id) \
           AND (rh.status IN ('pending', 'queued', 'downloading', 'awaiting_approval') \
                OR (rh.status = 'failed' AND rh.download_task_id IS NOT NULL))";
}
