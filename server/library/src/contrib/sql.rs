//! The contribution store's newer statements.

use crate::identity::sql::PERSONS;

droppedneedle_kernel::sql! {
    /// Verifying contributions with no queued or running verification job.
    pub(super) VERIFYING_WITHOUT_JOB = "SELECT d.id FROM library_contribution_drafts d \
             WHERE d.state = 'verifying' AND NOT EXISTS (SELECT 1 FROM durable_jobs j \
             WHERE j.kind = 'library.contribution_verify' AND j.idempotency_key = d.id \
             AND j.state IN ('queued', 'running')) ORDER BY d.id";
    pub(super) ALBUM_INPUT = "SELECT row_revision, album_artist_id FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL";
    pub(super) ALBUM_INPUT_TRACKS = "SELECT id, tag_revision, stat_revision, applied_policy_revision, applied_policy \
             FROM local_tracks WHERE local_album_id = ?1 AND availability = 'indexed'";
    pub(super) ACTIVE_ID_FOR_ALBUM = "SELECT id FROM library_contribution_drafts WHERE local_album_id = ?1 \
         AND state NOT IN ('linked','cancelled','stale') ORDER BY created_at DESC LIMIT 1";
    pub(super) SAVE_ROW = "UPDATE library_contribution_drafts SET state = ?3, updated_by_user_id = ?4, \
             album_row_revision = ?5, input_revision = ?6, resolved_draft_json = ?7, \
             source_selection_json = ?8, provider_snapshot_expires_at = ?9, \
             duplicate_result_json = ?10, duplicate_checked_at = ?11, \
             duplicate_input_revision = ?12, result_release_mbid = ?13, result_source = ?14, \
             result_received_at = ?15, seed_snapshot_json = ?16, seed_hash = ?17, \
             seeded_at = ?18, terminal_at = ?19, updated_at = ?20, row_revision = ?21, \
             last_verification_failure = ?22 WHERE id = ?1 AND row_revision = ?2";
    pub(super) INSERT_ROW = "INSERT INTO library_contribution_drafts (id, local_album_id, created_by_user_id, \
         updated_by_user_id, state, album_row_revision, input_revision, local_snapshot_json, \
         resolved_draft_json, source_selection_json, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?3, 'draft', ?4, ?5, ?6, ?7, ?8, ?9, ?9)";
    pub(super) CONSUME_TOKENS = "UPDATE library_contribution_callback_tokens SET consumed_at = ?2 \
         WHERE contribution_id = ?1 AND consumed_at IS NULL";
    pub(super) ALBUM_IDENTITY = "SELECT release_mbid, release_group_mbid FROM local_album_external_identities \
         WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) ARTIST_IDENTITY = "SELECT provider_artist_id FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = ?2";
    pub(super) OFFER_MERGE = "INSERT OR IGNORE INTO local_artist_merge_candidates (id, left_artist_id, \
         right_artist_id, reason_code, created_at, updated_at) \
         VALUES (?1, ?2, ?3, 'SHARED_PROVIDER_IDENTITY', ?4, ?4)";
    pub(super) INSERT_ATTEMPT = "INSERT INTO library_identification_attempts (id, local_album_id, trigger, \
         requested_by_user_id, input_tag_revision, input_policy_revision, input_file_revision, \
         matcher_version, state, terminal_reason_code, selected_candidate_key, candidate_count, \
         started_at, completed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)";
    pub(super) BUMP_ALBUM_IF_REVISION = "UPDATE local_albums SET updated_at = ?3, row_revision = row_revision + 1 \
                 WHERE id = ?1 AND row_revision = ?2 RETURNING row_revision";
    pub(super) ARTIST_CAN_LINK = "SELECT kind <> 'various_artists' AND id NOT IN (?2, ?3) \
                     AND retired_into_artist_id IS NULL FROM local_artists WHERE id = ?1";
    pub(super) USER_EXISTS = "SELECT id FROM auth_users WHERE id = ?1";
    pub(super) BUMP_ARTIST_REVISION = "UPDATE local_artists SET updated_at = ?2, row_revision = row_revision + 1 \
                 WHERE id = ?1";
    pub(super) WRITE_ARTIST_LINK = "INSERT INTO local_artist_external_identities (local_artist_id, provider, \
                 provider_artist_id, decision_source, attempt_id, selected_by_user_id, \
                 selected_at) VALUES (?1, ?2, ?3, 'manual', ?4, ?5, ?6) \
                 ON CONFLICT (local_artist_id, provider) DO UPDATE SET \
                 provider_artist_id = excluded.provider_artist_id, \
                 decision_source = excluded.decision_source, \
                 attempt_id = excluded.attempt_id, \
                 selected_by_user_id = excluded.selected_by_user_id, \
                 selected_at = excluded.selected_at, \
                 row_revision = local_artist_external_identities.row_revision + 1";
    pub(super) APPROVE_PENDING_REVIEW = "UPDATE library_identify_reviews SET state = 'approved', resolved_by_user_id = (SELECT id FROM auth_users WHERE id = ?2), \
         selected_candidate_key = ?3, updated_at = ?4 \
         WHERE local_album_id = ?1 AND state = 'pending'";
    pub(super) APPLY = "INSERT INTO library_contribution_callback_tokens (token_hash, \
                     contribution_id, requested_by_user_id, expires_at, created_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5)";
    pub(super) CONSUME_CALLBACK_TOKEN = "SELECT contribution_id FROM library_contribution_callback_tokens \
                     WHERE token_hash = ?1 AND consumed_at IS NULL AND expires_at >= ?2";
    pub(super) LIST_FOR_PROVIDER_PURGE = "SELECT id FROM library_contribution_drafts \
                         WHERE provider_snapshot_expires_at IS NOT NULL \
                         AND (provider_snapshot_expires_at <= ?1 \
                         OR state IN ('linked','cancelled','stale')) \
                         ORDER BY provider_snapshot_expires_at, id LIMIT ?2";
    pub(super) PRUNE_CALLBACK_TOKENS = "DELETE FROM library_contribution_callback_tokens WHERE \
                         (consumed_at IS NOT NULL AND consumed_at < ?1) OR \
                         (consumed_at IS NULL AND expires_at < ?2)";
    pub(super) DRAFTS_WITH_OLD_SEEDS = "SELECT id FROM library_contribution_drafts \
                             WHERE seed_snapshot_json IS NOT NULL \
                             AND (state IN ('linked','cancelled','stale') OR (seeded_at IS NOT NULL \
                             AND NOT EXISTS (SELECT 1 FROM library_contribution_callback_tokens t \
                             WHERE t.contribution_id = library_contribution_drafts.id \
                             AND t.consumed_at IS NULL AND t.expires_at >= ?1))) ORDER BY id";
    pub(super) DROP_DRAFT_SEED = "UPDATE library_contribution_drafts SET seed_snapshot_json = NULL, \
                                     row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) PRUNE_ENDED_DRAFTS = "DELETE FROM library_contribution_drafts \
                         WHERE state IN ('cancelled','stale') AND result_release_mbid IS NULL \
                         AND terminal_at < ?1";
    pub(super) ALBUM_CONTEXT = "SELECT id, row_revision, title, album_artist_name, album_artist_id, \
             original_release_date, year, is_compilation FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL";
    pub(super) ARTIST_KIND = "SELECT kind FROM local_artists WHERE id = ?1";
    pub(super) ALBUM_CONTEXT_TRACKS = "SELECT t.id, t.disc_number, t.track_number, t.title, t.artist_name, \
             t.duration_seconds, t.availability, t.disc_subtitle, t.relative_path, \
             i.recording_mbid, t.embedded_recording_mbid, t.tag_revision, t.stat_revision, \
             t.applied_policy_revision, t.applied_policy \
             FROM local_tracks t LEFT JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = 'musicbrainz' \
             WHERE t.local_album_id = ?1 ORDER BY t.disc_number, t.track_number, t.id";
    pub(super) fragment ROW_COLUMNS = "id, local_album_id, created_by_user_id, updated_by_user_id, state, \
     album_row_revision, input_revision, local_snapshot_json, resolved_draft_json, \
     source_selection_json, provider_snapshot_expires_at, duplicate_result_json, \
     duplicate_checked_at, duplicate_input_revision, result_release_mbid, result_source, \
     result_received_at, seed_snapshot_json, seed_hash, seeded_at, terminal_at, created_at, \
     updated_at, row_revision, last_verification_failure";
    pub(super) LOAD_ROW = "SELECT " + ROW_COLUMNS + " FROM library_contribution_drafts WHERE id = ?1";
    pub(super) CHOSEN_ARTIST_IDENTITY = "SELECT provider_artist_id FROM local_artist_external_identities \
         WHERE local_artist_id = ?1 AND provider = ?2 AND decision_source IN " + PERSONS;
}
