//! Management's statements over its applied work: each album's outcome
//! and the publish bundles a job wrote, for its results and its undo.

droppedneedle_kernel::sql! {
    /// One album's work outcome (`?1` job, `?2` ordinal): state, failure
    /// code, result and bundle.
    pub(super) WORK_RESULT = "SELECT state, failure_code, result_json, bundle_id \
             FROM library_operation_work WHERE job_id = ?1 AND ordinal = ?2";
    /// The bundles of job `?1` that count as applied: (ordinal, bundle id).
    /// A bundle counts when its album succeeded (`?2`) or its publish
    /// committed after the job stopped.
    pub(super) APPLIED_BUNDLES = "SELECT ordinal, bundle_id FROM library_operation_work \
             WHERE job_id = ?1 AND bundle_id IS NOT NULL \
             AND (state = ?2 OR bundle_id IN (SELECT bundle_id FROM library_publish_journal \
                 WHERE state IN ('committed', 'cleanup_pending', 'cleaned'))) \
             ORDER BY ordinal";
    /// Undo counts of job `?1` on day `?2`, its applied bundles chosen as in
    /// [`APPLIED_BUNDLES`] (`?3` the succeeded state): files with a
    /// before-state still kept, files whose before-state is gone, and the
    /// first day one runs out.
    pub(super) UNDO_COUNTS = "WITH bundles AS (SELECT bundle_id FROM library_operation_work \
               WHERE job_id = ?1 AND bundle_id IS NOT NULL \
               AND (state = ?3 OR bundle_id IN (SELECT bundle_id FROM library_publish_journal \
                   WHERE state IN ('committed', 'cleanup_pending', 'cleaned')))), \
             written AS (SELECT DISTINCT j.bundle_id, j.track_id FROM library_publish_journal j \
               JOIN bundles b ON b.bundle_id = j.bundle_id \
               WHERE j.kind = 'audio' AND j.track_id IS NOT NULL \
               AND j.state IN ('committed', 'cleaned', 'cleanup_pending')) \
             SELECT \
               (SELECT COUNT(*) FROM library_publish_snapshots s JOIN bundles b \
                 ON b.bundle_id = s.bundle_id WHERE s.expires_day > ?2), \
               (SELECT COUNT(*) FROM written w WHERE NOT EXISTS (SELECT 1 \
                 FROM library_publish_snapshots s WHERE s.bundle_id = w.bundle_id \
                 AND s.track_id = w.track_id AND s.expires_day > ?2)), \
               (SELECT MIN(s.expires_day) FROM library_publish_snapshots s JOIN bundles b \
                 ON b.bundle_id = s.bundle_id WHERE s.expires_day > ?2)";
}
