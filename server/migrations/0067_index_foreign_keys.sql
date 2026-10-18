-- 0067_index_foreign_keys.sql - index every foreign key.
--
-- Deleting or rekeying a parent row makes SQLite look up its children
-- through the foreign key. With no index whose leading columns are the
-- key, that is a scan of the child table for every parent row: purging
-- 10,000 saved originals scanned library_track_management_state 10,000
-- times and took about 4 s. These keys had no index; each gets one.
-- Indexes only: no table, column, key action or row changes.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX IF NOT EXISTS idx_library_artist_reconciliation_dismissals_right_artist_id
    ON library_artist_reconciliation_dismissals(right_artist_id);
CREATE INDEX IF NOT EXISTS idx_library_automatic_edition_undo_job_id
    ON library_automatic_edition_undo(job_id);
CREATE INDEX IF NOT EXISTS idx_library_catalog_actions_operation_job_id
    ON library_catalog_actions(operation_job_id);
CREATE INDEX IF NOT EXISTS idx_library_catalog_actions_local_track_id
    ON library_catalog_actions(local_track_id);
CREATE INDEX IF NOT EXISTS idx_library_catalog_actions_local_artist_id
    ON library_catalog_actions(local_artist_id);
CREATE INDEX IF NOT EXISTS idx_library_catalog_actions_actor_user_id
    ON library_catalog_actions(actor_user_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_callback_tokens_requested_by_user_id
    ON library_contribution_callback_tokens(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_callback_tokens_contribution_id
    ON library_contribution_callback_tokens(contribution_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_drafts_updated_by_user_id
    ON library_contribution_drafts(updated_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_drafts_created_by_user_id
    ON library_contribution_drafts(created_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_verification_jobs_requested_by_user_id
    ON library_contribution_verification_jobs(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_contribution_verification_jobs_contribution_id
    ON library_contribution_verification_jobs(contribution_id);
CREATE INDEX IF NOT EXISTS idx_library_custom_edition_manifests_sealed_by_user_id
    ON library_custom_edition_manifests(sealed_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_identification_attempts_requested_by_user_id
    ON library_identification_attempts(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_management_baselines_semantic_snapshot_blob_sha256
    ON library_management_baselines(semantic_snapshot_blob_sha256);
CREATE INDEX IF NOT EXISTS idx_library_management_exclusions_excluded_by_user_id
    ON library_management_exclusions(excluded_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_management_job_snapshots_linked_operation_job_id
    ON library_management_job_snapshots(linked_operation_job_id);
CREATE INDEX IF NOT EXISTS idx_library_management_overrides_actor_user_id
    ON library_management_overrides(actor_user_id);
CREATE INDEX IF NOT EXISTS idx_library_management_overrides_local_track_id
    ON library_management_overrides(local_track_id);
CREATE INDEX IF NOT EXISTS idx_library_management_overrides_local_album_id
    ON library_management_overrides(local_album_id);
CREATE INDEX IF NOT EXISTS idx_library_management_plan_items_local_track_id
    ON library_management_plan_items(local_track_id);
CREATE INDEX IF NOT EXISTS idx_library_management_plan_items_local_album_id
    ON library_management_plan_items(local_album_id);
CREATE INDEX IF NOT EXISTS idx_library_operation_control_idempotency_job_id
    ON library_operation_control_idempotency(job_id);
CREATE INDEX IF NOT EXISTS idx_library_operation_jobs_requested_by_user_id
    ON library_operation_jobs(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_operation_work_local_track_id
    ON library_operation_work(local_track_id);
CREATE INDEX IF NOT EXISTS idx_library_operation_work_local_album_id
    ON library_operation_work(local_album_id);
CREATE INDEX IF NOT EXISTS idx_library_reidentification_snapshots_local_album_id
    ON library_reidentification_snapshots(local_album_id);
CREATE INDEX IF NOT EXISTS idx_library_review_exclusions_local_track_id
    ON library_review_exclusions(local_track_id);
CREATE INDEX IF NOT EXISTS idx_library_scan_runs_requested_by_user_id
    ON library_scan_runs(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_library_track_management_state_last_operation_job_id
    ON library_track_management_state(last_operation_job_id);
CREATE INDEX IF NOT EXISTS idx_library_track_management_state_baseline_id
    ON library_track_management_state(baseline_id);
CREATE INDEX IF NOT EXISTS idx_library_work_control_requested_by_user_id
    ON library_work_control(requested_by_user_id);
CREATE INDEX IF NOT EXISTS idx_local_album_external_identities_selected_by_user_id
    ON local_album_external_identities(selected_by_user_id);
CREATE INDEX IF NOT EXISTS idx_local_album_external_identities_attempt_id
    ON local_album_external_identities(attempt_id);
CREATE INDEX IF NOT EXISTS idx_local_artist_external_identities_selected_by_user_id
    ON local_artist_external_identities(selected_by_user_id);
CREATE INDEX IF NOT EXISTS idx_local_artist_external_identities_attempt_id
    ON local_artist_external_identities(attempt_id);
CREATE INDEX IF NOT EXISTS idx_local_artist_merge_candidates_right_artist_id
    ON local_artist_merge_candidates(right_artist_id);
CREATE INDEX IF NOT EXISTS idx_local_entity_source_links_selected_by_user_id
    ON local_entity_source_links(selected_by_user_id);
CREATE INDEX IF NOT EXISTS idx_local_entity_source_links_local_track_id
    ON local_entity_source_links(local_track_id);
CREATE INDEX IF NOT EXISTS idx_local_entity_source_links_local_album_id
    ON local_entity_source_links(local_album_id);
CREATE INDEX IF NOT EXISTS idx_local_entity_source_links_local_artist_id
    ON local_entity_source_links(local_artist_id);
CREATE INDEX IF NOT EXISTS idx_local_track_external_identities_attempt_id
    ON local_track_external_identities(attempt_id);
CREATE INDEX IF NOT EXISTS idx_spotify_auth_requests_user_id
    ON spotify_auth_requests(user_id);

-- Migration high-water mark.
PRAGMA user_version = 67;
