//! The statements of the drop imports (`drop_import_jobs`,
//! `drop_import_items`).

use crate::held::sql::OPEN;

droppedneedle_kernel::sql! {
    /// Every job column, in the order `JobRecord::from_row` reads them.
    pub(super) fragment JOB_ROW = "SELECT id, user_id, user_name, status, created_at, \
             upload_name, staging_dir, error FROM drop_import_jobs";
    /// Every item column, in the order `ItemRecord::from_row` reads them.
    pub(super) fragment ITEM_ROW = "SELECT id, job_id, folder_name, status, updated_at, \
             release_group_mbid, album_title, artist_name, files_total, files_imported, detail, \
             reason_code, staging_paths FROM drop_import_items";
    /// A new job, processing.
    pub(super) INSERT_JOB = "INSERT INTO drop_import_jobs (id, user_id, user_name, status, \
             created_at, upload_name, staging_dir) \
             VALUES (?1, ?2, ?3, 'processing', ?4, ?5, ?6)";
    /// Settle job `?1` with status `?2` and note `?3`.
    pub(super) SET_JOB_STATUS = "UPDATE drop_import_jobs SET status = ?2, error = ?3 \
             WHERE id = ?1";
    /// A new processing item.
    pub(super) INSERT_ITEM = "INSERT INTO drop_import_items (job_id, folder_name, status, \
             files_total, staging_paths, updated_at) VALUES (?1, ?2, 'processing', ?3, ?4, ?5)";
    /// Write item `?1`'s settled state while it is still in status `?2`.
    pub(super) SAVE_ITEM = "UPDATE drop_import_items SET status = ?3, release_group_mbid = ?4, \
             album_title = ?5, artist_name = ?6, files_total = ?7, files_imported = ?8, \
             detail = ?9, reason_code = ?10, staging_paths = ?11, \
             updated_at = ?12 WHERE id = ?1 AND status = ?2";
    /// Claim review item `?1` for a person's match.
    pub(super) CLAIM_REVIEW = "UPDATE drop_import_items SET status = 'processing', \
             updated_at = ?2 WHERE id = ?1 AND status = 'needs_review'";
    /// One job (`?1`).
    pub(super) JOB_BY_ID = JOB_ROW + " WHERE id = ?1";
    /// The newest `?2` jobs of user `?1`, or everyone's when `?1` is NULL.
    pub(super) LIST_JOBS = JOB_ROW + " WHERE (?1 IS NULL OR user_id = ?1) \
             ORDER BY created_at DESC, id DESC LIMIT ?2";
    /// The items of job `?1`, in the order they were added.
    pub(super) ITEMS_OF_JOB = ITEM_ROW + " WHERE job_id = ?1 ORDER BY id";
    /// One item (`?1`).
    pub(super) ITEM_BY_ID = ITEM_ROW + " WHERE id = ?1";
    /// The items a restart cut off, with what their held entry needs and
    /// whether their job was still being worked on and its owner exists.
    pub(super) CUT_OFF_ITEMS = "SELECT i.id, i.staging_paths, i.reason_code, \
             i.release_group_mbid, i.artist_name, i.album_title, j.user_id, \
             j.status = 'processing', \
             EXISTS (SELECT 1 FROM auth_users u WHERE u.id = j.user_id) \
             FROM drop_import_items i JOIN drop_import_jobs j ON j.id = i.job_id \
             WHERE i.status = 'processing'";
    /// Send cut-off item `?5` back to review with reason `?2`. A hand
    /// match (`?4`) keeps its own detail, if it has one, over `?1`.
    pub(super) RETURN_TO_REVIEW = "UPDATE drop_import_items SET status = 'needs_review', \
             detail = CASE WHEN ?4 THEN COALESCE(detail, ?1) ELSE ?1 END, \
             reason_code = ?2, updated_at = ?3 WHERE id = ?5";
    /// Fail every job still processing, with note `?1`.
    pub(super) FAIL_PROCESSING_JOBS = "UPDATE drop_import_jobs SET status = 'failed', \
             error = ?1 WHERE status = 'processing'";
    /// Jobs still processing that have no items yet, with their staging
    /// folders.
    pub(super) JOBS_WITHOUT_ITEMS = "SELECT j.id, j.staging_dir FROM drop_import_jobs j \
             WHERE j.status = 'processing' \
               AND NOT EXISTS (SELECT 1 FROM drop_import_items i WHERE i.job_id = j.id)";
    /// Every job's staging folder and whether it is kept: the job or one
    /// of its items is still being worked on, or an item waits in an open
    /// entry of the held list.
    pub(super) STAGING_DIRS = "SELECT j.staging_dir, (j.status = 'processing' OR EXISTS ( \
             SELECT 1 FROM drop_import_items i WHERE i.job_id = j.id \
             AND (i.status = 'processing' OR EXISTS (SELECT 1 FROM acquire_held_entries e \
                 WHERE e.drop_item_id = i.id AND e.state IN " + OPEN + ")))) \
             FROM drop_import_jobs j";
}
