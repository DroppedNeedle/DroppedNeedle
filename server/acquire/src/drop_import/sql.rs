//! The statements of the drop imports (`drop_import_jobs`,
//! `drop_import_items`).

use crate::held::sql::OPEN;

droppedneedle_kernel::sql! {
    /// Every job column, in the order `JobRecord::from_row` reads them.
    pub(super) fragment JOB_ROW = "SELECT id, user_id, user_name, status, created_at, \
             upload_name, staging_dir, error, source FROM drop_import_jobs";
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
    /// The newest `?2` uploads of user `?1`, or everyone's when `?1` is
    /// NULL. The import folder's jobs are not uploads: its leftovers are in
    /// the held list.
    pub(super) LIST_JOBS = JOB_ROW + " WHERE source = 'upload' AND (?1 IS NULL OR user_id = ?1) \
             ORDER BY created_at DESC, id DESC LIMIT ?2";
    /// The items of job `?1`, in the order they were added.
    pub(super) ITEMS_OF_JOB = ITEM_ROW + " WHERE job_id = ?1 ORDER BY id";
    /// One item (`?1`).
    pub(super) ITEM_BY_ID = ITEM_ROW + " WHERE id = ?1";
    /// The items a restart cut off (an upload's, or a person's match of an
    /// import-folder album), with what their held entry needs and whether
    /// their upload was still being worked on and its owner exists.
    pub(super) CUT_OFF_ITEMS = "SELECT i.id, i.staging_paths, i.reason_code, \
             i.release_group_mbid, i.artist_name, i.album_title, j.user_id, \
             j.status = 'processing' AND j.source = 'upload', \
             EXISTS (SELECT 1 FROM auth_users u WHERE u.id = j.user_id) \
             FROM drop_import_items i JOIN drop_import_jobs j ON j.id = i.job_id \
             WHERE i.status = 'processing' \
               AND (j.source = 'upload' OR i.reason_code IS NOT NULL)";
    /// Send cut-off item `?5` back to review with reason `?2`. A hand
    /// match (`?4`) keeps its own detail, if it has one, over `?1`.
    pub(super) RETURN_TO_REVIEW = "UPDATE drop_import_items SET status = 'needs_review', \
             detail = CASE WHEN ?4 THEN COALESCE(detail, ?1) ELSE ?1 END, \
             reason_code = ?2, updated_at = ?3 WHERE id = ?5";
    /// Fail every upload still processing, with note `?1`.
    pub(super) FAIL_PROCESSING_JOBS = "UPDATE drop_import_jobs SET status = 'failed', \
             error = ?1 WHERE status = 'processing' AND source = 'upload'";
    /// Jobs still processing that have no items yet, with their staging
    /// folders.
    pub(super) JOBS_WITHOUT_ITEMS = "SELECT j.id, j.staging_dir FROM drop_import_jobs j \
             WHERE j.status = 'processing' AND j.source = 'upload' \
               AND NOT EXISTS (SELECT 1 FROM drop_import_items i WHERE i.job_id = j.id)";
    /// Every job's staging folder and whether it is kept: the job or one
    /// of its items is still being worked on, or an item waits in an open
    /// entry of the held list.
    pub(super) STAGING_DIRS = "SELECT j.staging_dir, (j.status = 'processing' OR EXISTS ( \
             SELECT 1 FROM drop_import_items i WHERE i.job_id = j.id \
             AND (i.status = 'processing' OR EXISTS (SELECT 1 FROM acquire_held_entries e \
                 WHERE e.drop_item_id = i.id AND e.state IN " + OPEN + ")))) \
             FROM drop_import_jobs j WHERE j.source = 'upload'";
    /// A new import-folder job, processing: no owner, staged in folder `?3`.
    pub(super) INSERT_FOLDER_JOB = "INSERT INTO drop_import_jobs (id, user_id, user_name, \
             status, created_at, upload_name, staging_dir, source) \
             VALUES (?1, NULL, '', 'processing', ?2, 'Import folder', ?3, 'import_folder')";
    /// The import-folder jobs of folder `?1`.
    fragment FOLDER_JOBS = "(SELECT id FROM drop_import_jobs \
             WHERE source = 'import_folder' AND staging_dir = ?1)";
    /// The import-folder items of folder `?1` still to run, oldest first. An
    /// item with a reason is a person's Match running on it, never the loop's.
    pub(super) FOLDER_PROCESSING = ITEM_ROW + " WHERE status = 'processing' \
             AND reason_code IS NULL AND job_id IN " + FOLDER_JOBS + " ORDER BY id";
    /// Settled import-folder items of folder `?1` whose files still wait to
    /// leave the folder.
    pub(super) FOLDER_REMOVALS = ITEM_ROW + " WHERE status != 'processing' \
             AND staging_paths != '[]' AND job_id IN " + FOLDER_JOBS + " ORDER BY id";
    /// The files of folder `?1` an import-folder item is working on or has
    /// still to remove.
    pub(super) FOLDER_TAKEN = "SELECT staging_paths FROM drop_import_items \
             WHERE staging_paths != '[]' AND job_id IN " + FOLDER_JOBS;
    /// How many import-folder items of folder `?1` are still to run.
    pub(super) FOLDER_WAITING = "SELECT COUNT(*) FROM drop_import_items \
             WHERE status = 'processing' AND reason_code IS NULL AND job_id IN " + FOLDER_JOBS;
    /// Settled item `?1`'s files are out of the folder, or stay for a reason
    /// logged: its removal list empties at `?2`.
    pub(super) CLEAR_REMOVALS = "UPDATE drop_import_items SET staging_paths = '[]', \
             updated_at = ?2 WHERE id = ?1 AND status != 'processing'";
    /// Import-folder jobs none of whose items is still to run complete.
    pub(super) SETTLE_FOLDER_JOBS = "UPDATE drop_import_jobs SET status = 'completed' \
             WHERE source = 'import_folder' AND status = 'processing' \
             AND NOT EXISTS (SELECT 1 FROM drop_import_items i \
                 WHERE i.job_id = drop_import_jobs.id AND i.status = 'processing')";
    /// The import-folder jobs of any folder but `?1` (NULL: of every folder).
    fragment OTHER_FOLDER_JOBS = "(SELECT id FROM drop_import_jobs \
             WHERE source = 'import_folder' AND staging_dir IS NOT ?1)";
    /// Items of another folder not yet started fail with sentence `?2` and
    /// reason `?3` at `?4`; their files are left where they are. One a
    /// person's Match runs on (it has a reason) is left to finish.
    pub(super) FAIL_OTHER_FOLDER_ITEMS = "UPDATE drop_import_items SET status = 'failed', \
             detail = ?2, reason_code = ?3, staging_paths = '[]', updated_at = ?4 \
             WHERE status = 'processing' AND reason_code IS NULL \
             AND job_id IN " + OTHER_FOLDER_JOBS;
    /// Another folder's jobs still processing fail with note `?2`.
    pub(super) FAIL_OTHER_FOLDER_JOBS = "UPDATE drop_import_jobs SET status = 'failed', \
             error = ?2 WHERE source = 'import_folder' AND status = 'processing' \
             AND staging_dir IS NOT ?1";
    /// The folder of the import-folder job drop item `?1` belongs to.
    pub(crate) FOLDER_OF_ITEM = "SELECT j.staging_dir FROM drop_import_items i \
             JOIN drop_import_jobs j ON j.id = i.job_id \
             WHERE i.id = ?1 AND j.source = 'import_folder'";
}
