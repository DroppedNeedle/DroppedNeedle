//! The SQL of a re-identification's snapshot.

droppedneedle_kernel::sql! {
    pub(super) SNAPSHOT = "SELECT local_album_id, expected_album_revision, expected_input_revision, \
         expected_identity_revision, requested_release_mbid, selected_candidate_key, \
         result_json, one_off_local_metadata FROM library_reidentification_snapshots \
         WHERE job_id = ?1";
    pub(super) LIVE_ALBUM_REVISION = "SELECT row_revision FROM local_albums WHERE id = ?1 \
             AND retired_into_album_id IS NULL";
    pub(super) ALBUM_APPLIED_POLICIES = "SELECT DISTINCT applied_policy FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY applied_policy";
    pub(super) LOCAL_METADATA_MANAGED = "SELECT DISTINCT root_id FROM local_tracks WHERE local_album_id = ?1 \
         AND availability = 'indexed' AND applied_policy = 'local_metadata'";
    pub(super) CREATE_REIDENTIFICATION = "INSERT INTO library_reidentification_snapshots (job_id, local_album_id, \
         expected_album_revision, expected_input_revision, expected_identity_revision, \
         one_off_local_metadata, requested_release_mbid, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)";
    pub(super) LIVE_REIDENTIFICATION = "SELECT id FROM library_operation_jobs \
             WHERE kind = ?1 AND state IN (?3, ?4) AND id IN \
             (SELECT job_id FROM library_reidentification_snapshots WHERE local_album_id = ?2 \
              AND one_off_local_metadata >= ?5) \
             ORDER BY created_at LIMIT 1";
    pub(super) row SnapshotRow for [SNAPSHOT] {
        local_album_id: String,
        expected_album_revision: i64,
        expected_input_revision: String,
        expected_identity_revision: String,
        requested_release_mbid: Option<String>,
        selected_candidate_key: Option<String>,
        result_json: Option<String>,
        one_off_local_metadata: bool,
    }
}
