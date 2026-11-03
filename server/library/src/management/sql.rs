//! Management's statements over its applied work: each album's outcome
//! and the publish bundles a job wrote, for its results and its undo.

use droppedneedle_kernel::db::time::Millis;

use super::models::{Eligibility, OverrideMode, TagScope};
use super::takeover::Cause;
use crate::jobs::{ManagementPhase, OperationState, WorkState};
use crate::publish::sql::HAS_BYTES_RECORD;
#[cfg(any(test, feature = "test-support"))]
use crate::publish::sql::RETRYING_BUNDLES;

droppedneedle_kernel::sql_choice! {
    /// What a kept tag value sits on.
    pub(super) enum OverrideSubject {
        Album,
        Track,
    }
}

impl OverrideSubject {
    /// The override row's key column and the subject's table.
    fn columns(self) -> (&'static str, &'static str) {
        match self {
            Self::Album => ("local_album_id", "local_albums"),
            Self::Track => ("local_track_id", "local_tracks"),
        }
    }
}

droppedneedle_kernel::sql! {
    /// One album's work outcome (`?1` job, `?2` ordinal): state, failure
    /// code, result and bundle.
    pub(super) WORK_RESULT = "SELECT state, failure_code, result_json, bundle_id \
             FROM library_operation_work WHERE job_id = ?1 AND ordinal = ?2";
    /// The next album automatic management looks at (`?1` now): (album,
    /// tries). With `?2` set (unix milliseconds), only an album queued at or
    /// before it whose identification has no job queued, running or
    /// deferred, nor one that landed after it.
    pub(super) AUTOMATIC_DUE = "SELECT q.local_album_id, q.attempts \
             FROM library_management_automatic_queue q WHERE q.not_before_at <= ?1 \
             AND (?2 IS NULL OR (q.queued_at <= ?2 AND NOT EXISTS (SELECT 1 \
                 FROM library_identify_jobs j WHERE j.local_album_id = q.local_album_id \
                 AND (j.state IN ('queued', 'running', 'deferred') \
                     OR j.updated_at > ?2)))) \
             ORDER BY q.not_before_at, q.local_album_id LIMIT 1";
    /// The bundles of job `?1` that count as applied: (ordinal, bundle id).
    /// A bundle counts when its album succeeded (`?2`) or its publish
    /// committed after the job stopped.
    pub(super) APPLIED_BUNDLES = "SELECT ordinal, bundle_id FROM library_operation_work \
             WHERE job_id = ?1 AND bundle_id IS NOT NULL \
             AND (state = ?2 OR bundle_id IN (SELECT bundle_id FROM library_publish_journal \
                 WHERE state IN ('committed', 'cleanup_pending', 'cleaned'))) \
             ORDER BY ordinal";
    /// Undo counts of job `?1` on the day starting at `?2`, its applied bundles chosen as in
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
                 ON b.bundle_id = s.bundle_id WHERE s.expires_at > ?2) AS available, \
               (SELECT COUNT(*) FROM written w WHERE NOT EXISTS (SELECT 1 \
                 FROM library_publish_snapshots s WHERE s.bundle_id = w.bundle_id \
                 AND s.track_id = w.track_id AND s.expires_at > ?2)) AS expired, \
               (SELECT MIN(s.expires_at) / 86400000 FROM library_publish_snapshots s JOIN bundles b \
                 ON b.bundle_id = s.bundle_id WHERE s.expires_at > ?2) AS first_expiry_day";
    pub(super) QUEUE = "INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before_at) \
         SELECT ?1, ?2, ?2 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
         ON CONFLICT (local_album_id) DO UPDATE SET queued_at = excluded.queued_at, \
         not_before_at = excluded.not_before_at, attempts = 0, last_reason_code = NULL";
    pub(super) STAMP_RETAG_FOLLOWED = "UPDATE library_management_automatic_queue SET retag_followed_at = ?2 \
         WHERE local_album_id = ?1";
    pub(super) HAND_BACK_STAMPED = "SELECT 1 FROM library_album_takeovers \
             WHERE local_album_id = ?1 AND handed_back_at IS NOT NULL";
    pub(super) RETAG_FOLLOWED = "SELECT 1 FROM library_management_automatic_queue \
             WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL";
    pub(super) WAKE = "UPDATE library_management_automatic_queue SET not_before_at = ?1, attempts = 0 \
         WHERE last_reason_code IS NOT NULL AND not_before_at > ?1 \
         AND (?2 IS NULL OR last_reason_code = ?2)";
    pub(super) ALBUM_FILES = "SELECT id, root_id, relative_path, ingest_source, stat_revision FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id";
    pub(super) WRITES_BY_ITSELF = "SELECT EXISTS(SELECT 1 FROM library_management_automatic_queue \
                 WHERE local_album_id = ?1 AND not_before_at < ?2 \
                 AND last_reason_code IS NOT 'PREVIEW_BLOCKED') \
                 OR EXISTS(SELECT 1 FROM library_operation_jobs j \
                 JOIN library_management_job_snapshots s ON s.job_id = j.id \
                 WHERE j.kind = ?3 AND j.state IN (?4, ?5, ?6) \
                 AND j.idempotency_key LIKE 'automatic:%' \
                 AND EXISTS(SELECT 1 FROM json_each(s.selection_json, '$.ids') \
                 WHERE value = ?1))";
    pub(super) DROP_AUTOMATIC_QUEUE_ROW = "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1";
    pub(super) PUT_OFF_AUTOMATIC_ALBUM = "UPDATE library_management_automatic_queue SET attempts = attempts + ?2, \
                     not_before_at = ?3, last_reason_code = ?4 WHERE local_album_id = ?1";
    pub(super) AUTOMATIC_SKIPPED = "SELECT q.local_album_id, a.title, a.album_artist_name, \
                 COALESCE(q.last_reason_code, ?2) AS reason_code, q.attempts, \
                 q.queued_at FROM library_management_automatic_queue q \
                 JOIN local_albums a ON a.id = q.local_album_id \
                 WHERE q.last_reason_code IS NOT NULL OR EXISTS (SELECT 1 \
                 FROM library_identify_jobs j WHERE j.local_album_id = q.local_album_id \
                 AND j.state IN ('queued', 'running', 'deferred')) \
                 ORDER BY q.queued_at DESC LIMIT ?1";
    pub(super) AUTOMATIC_GATE = "SELECT 1 FROM library_management_exclusions WHERE local_album_id = ?1";
    pub(super) ACTIVE_CUSTOM_EDITION = "SELECT m.id, m.release_group_mbid, m.album_title, m.album_artist_name, \
             m.artist_mbid, m.album_metadata_json FROM library_custom_edition_active a \
             JOIN library_custom_edition_manifests m ON m.id = a.manifest_id \
             WHERE a.local_album_id = ?1";
    pub(super) CUSTOM_EDITION_TRACKS = "SELECT local_track_id, title, artist_name, artist_mbid, disc_number, track_number, \
         duration_seconds, metadata_json, recording_mbid FROM library_custom_edition_tracks \
         WHERE manifest_id = ?1 ORDER BY ordinal";
    pub(super) ALBUM_INDEXED_TRACK_IDS = "SELECT id FROM local_tracks WHERE local_album_id = ?1 AND +availability = 'indexed' \
         ORDER BY id";
    pub(super) PLAN_DUPLICATE = "SELECT id FROM local_tracks WHERE root_id = ?1 \
                                 AND relative_path = ?2 AND availability = 'indexed'";
    pub(super) ALBUM_TRACK_PATHS = "SELECT root_id, relative_path FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY root_id";
    pub(super) ALBUM_HAS_CUSTOM_EDITION = "SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1";
    pub(super) EDITION_REMAP_ATTEMPTS = "SELECT attempts FROM library_edition_remap_queue WHERE local_album_id = ?1";
    pub(super) MATCH_GATE = "SELECT state, reason_code FROM library_album_match_state WHERE local_album_id = ?1";
    /// Whether anyone marked album `?1`'s match as looking wrong.
    pub(super) LOOKS_WRONG = "SELECT EXISTS (SELECT 1 FROM library_album_looks_wrong \
         WHERE local_album_id = ?1)";
    /// Whether a track of an album other than `?3` sits directly in folder
    /// `?2` (empty, or ending in `/`) of library folder `?1`.
    pub(super) FOLDER_SHARED = "SELECT EXISTS (SELECT 1 FROM local_tracks \
         WHERE root_id = ?1 AND relative_path >= ?2 AND relative_path < ?2 || char(1114111) \
         AND instr(substr(relative_path, length(?2) + 1), '/') = 0 \
         AND local_album_id IS NOT ?3)";
    pub(super) OVERRIDES = "SELECT id, row_revision, field_name, mode, value_json FROM library_management_overrides \
         WHERE local_album_id = ?1 OR local_track_id = ?2 \
         ORDER BY subject_kind, field_name, id";
    pub(super) DOWNLOAD_PUBLISH_SETTLED = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
                 WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
                 WHERE substr(source_rel, 1, length(?1)) = ?1) \
                 AND state NOT IN ('cleaned', 'compensated', 'cleanup_pending') \
                 AND NOT (state = 'needs_attention' AND bundle_id IN \
                 (SELECT bundle_id FROM library_publish_resolutions)))";
    pub(super) CLEANUP_PENDING_COUNT = "SELECT COUNT(*) FROM library_publish_journal WHERE state = 'cleanup_pending'";
    pub(super) JOURNAL_COUNTS_BY_STATE = "SELECT state, COUNT(*) AS total FROM library_publish_journal GROUP BY state ORDER BY state";
    pub(super) BUNDLE_RESOLUTION = "SELECT verified_files, as_before_files, empty_files \
                     FROM library_publish_resolutions WHERE bundle_id = ?1";
    pub(super) RECORD_BUNDLE_RESOLUTION = "INSERT OR IGNORE INTO library_publish_resolutions (bundle_id, resolved_by_user_id, \
                 verified_files, resolved_at, as_before_files, empty_files) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    pub(super) PURGE_OF_IDEMPOTENCY_KEY = "SELECT impact_token, purged_baseline_count, detached_reference_count \
                     FROM library_management_baseline_purges WHERE idempotency_key = ?1";
    pub(super) DROP_BASELINE_BLOB_REFS = "DELETE FROM library_publish_blob_refs WHERE owner_kind IN ('baseline', 'v2_baseline')";
    pub(super) DROP_PUBLISH_BASELINES = "DELETE FROM library_publish_baselines";
    pub(super) DROP_COVER_BASELINES = "DELETE FROM library_publish_cover_baselines";
    pub(super) DETACH_BASELINES = "UPDATE library_track_management_state SET baseline_id = NULL, \
                 row_revision = row_revision + 1 WHERE baseline_id IS NOT NULL";
    pub(super) DROP_MANAGEMENT_BASELINES = "DELETE FROM library_management_baselines";
    pub(super) RECORD_BASELINE_PURGE = "INSERT INTO library_management_baseline_purges (idempotency_key, impact_token, \
                 actor_user_id, purged_baseline_count, detached_reference_count, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    pub(super) BASELINE_BLOB_SIZES = "SELECT r.sha256, (SELECT LENGTH(b.bytes) FROM library_publish_blobs b \
         WHERE b.sha256 = r.sha256) AS size FROM (SELECT DISTINCT sha256 FROM library_publish_blob_refs \
         WHERE owner_kind IN ('baseline', 'v2_baseline')) r";
    pub(super) PUBLISH_BASELINE_COUNT = "SELECT COUNT(*) FROM library_publish_baselines";
    pub(super) OLD_BASELINE_COUNT = "SELECT COUNT(*) FROM library_management_baselines b WHERE NOT EXISTS \
         (SELECT 1 FROM library_publish_baselines p WHERE p.track_id = b.local_track_id)";
    pub(super) NEWEST_BASELINE_AT = "SELECT COALESCE(MAX(created_at), 0) FROM library_publish_baselines";
    pub(super) BASELINE_BLOB_COUNT = "SELECT COUNT(DISTINCT sha256) FROM library_publish_blob_refs \
         WHERE owner_kind IN ('baseline', 'v2_baseline')";
    pub(super) OPEN_JOURNAL_COUNT = "SELECT COUNT(*) FROM library_publish_journal \
         WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention')";
    pub(super) ACTIVE_BASELINE_RESTORE_COUNT = "SELECT COUNT(*) FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id \
         WHERE s.mode = ?1 AND j.state IN (?2, ?3, ?4, ?5) \
         AND (j.state <> ?5 OR COALESCE(s.preview_expires_at, 0) > CAST(strftime('%s', 'now') AS INTEGER) * 1000)";
    pub(super) TRACKS = "SELECT id, root_id, relative_path, title, artist_name, album_title, track_number, \
         disc_number, file_format, bit_rate, sample_rate, bit_depth FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' \
         ORDER BY disc_number, track_number, relative_path";
    pub(super) INSERT_JOB = "INSERT INTO library_management_job_snapshots (job_id, mode, origin, phase, \
         selection_json, profile_revision, settings_revision, naming_revision, policy_revision, \
         catalog_revision, profile_snapshot_json, preview_created_at, preview_expires_at, \
         target_root_id, linked_operation_job_id, intent_json, summary_json, created_at, \
         updated_at, proposed_settings_revision) \
         VALUES (?1, ?2, ?16, ?18, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, \
         ?14, ?15, ?10, ?10, ?17)";
    pub(super) PHASE = "SELECT phase FROM library_management_job_snapshots WHERE job_id = ?1";
    pub(super) SET_PHASE = "UPDATE library_management_job_snapshots SET phase = ?2, updated_at = ?3, \
         row_revision = row_revision + 1 WHERE job_id = ?1";
    pub(super) CLEAR_ITEMS = "DELETE FROM library_management_plan_items WHERE job_id = ?1";
    pub(super) INSERT_ITEMS = "INSERT INTO library_management_plan_items (job_id, ordinal, bundle_ordinal, \
         local_album_id, local_track_id, expected_album_revision, expected_identity_revision, \
         expected_album_identity_revision, expected_override_revision, expected_catalog_revision, \
         expected_policy_revision, expected_profile_revision, expected_root_id, \
         expected_relative_path, expected_stat_revision, expected_tag_revision, \
         expected_file_fingerprint, source_path_identity, destination_root_id, \
         destination_relative_path, desired_document_json, desired_document_hash, diff_json, \
         capability_json, eligibility, reason_code, estimated_temporary_bytes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, '', ?16, ?17, \
         ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)";
    pub(super) FINISH_PLANNING = "UPDATE library_management_job_snapshots SET phase = ?4, summary_json = ?2, \
         updated_at = ?3, row_revision = row_revision + 1 WHERE job_id = ?1";
    pub(super) WRITABLE_BUNDLES = "SELECT bundle_ordinal, MIN(local_album_id) AS local_album_id, \
         COALESCE(MIN(expected_album_revision), 1) AS album_revision \
         FROM library_management_plan_items WHERE job_id = ?1 \
         GROUP BY bundle_ordinal \
         HAVING SUM(eligibility NOT IN ('eligible', 'warning')) = 0 \
         AND SUM(json_extract(desired_document_json, '$.plan') IS NOT NULL) > 0 \
         ORDER BY bundle_ordinal";
    pub(super) BEGIN_APPLY = "UPDATE library_management_job_snapshots SET mode = ?2, phase = ?3, \
         apply_idempotency_key = ?4, updated_at = ?5, row_revision = row_revision + 1 \
         WHERE job_id = ?1";
    pub(super) APPLY_KEY = "SELECT apply_idempotency_key FROM library_management_job_snapshots WHERE job_id = ?1";
    pub(super) RECORD_APPLY_KEY = "UPDATE library_management_job_snapshots SET apply_idempotency_key = ?2 \
         WHERE job_id = ?1 AND apply_idempotency_key IS NULL";
    pub(super) SET_JELLYFIN_REFRESH_JOB = "UPDATE library_management_job_snapshots SET jellyfin_refresh_job_id = ?2 \
             WHERE job_id = ?1";
    pub(super) SET_PLEX_REFRESH_JOB = "UPDATE library_management_job_snapshots SET plex_refresh_job_id = ?2 \
             WHERE job_id = ?1";
    pub(super) SET_NAVIDROME_REFRESH_JOB = "UPDATE library_management_job_snapshots SET navidrome_refresh_job_id = ?2 \
             WHERE job_id = ?1";
    pub(super) REFRESH_JOBS = "SELECT jellyfin_refresh_job_id, plex_refresh_job_id, navidrome_refresh_job_id \
             FROM library_management_job_snapshots WHERE job_id = ?1";
    pub(super) SET_MODE = "UPDATE library_management_job_snapshots SET mode = ?2, row_revision = row_revision + 1 \
         WHERE job_id = ?1";
    pub(super) BUNDLE_ORDINALS = "SELECT DISTINCT bundle_ordinal FROM library_management_plan_items WHERE job_id = ?1 \
         ORDER BY bundle_ordinal";
    pub(super) ALBUM_REVISION = "SELECT row_revision FROM local_albums WHERE id = ?1";
    pub(super) ALBUM_INDEXED_TRACKS = "SELECT id FROM local_tracks WHERE local_album_id = ?1 AND +availability = 'indexed'";
    pub(super) LIVE_UNDOS = "SELECT j.state, s.phase FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id \
         WHERE s.linked_operation_job_id = ?1 AND s.mode = 'undo' \
         AND j.state NOT IN (?3, ?4, ?5) \
         AND NOT (s.phase IN (?6, ?7) AND COALESCE(s.preview_expires_at, 0) <= ?2)";
    pub(super) UNDONE_ORDINALS = "SELECT w.ordinal FROM library_operation_work w \
         JOIN library_management_job_snapshots s ON s.job_id = w.job_id \
         WHERE s.linked_operation_job_id = ?1 AND s.mode = 'undo' AND w.state = ?2";
    pub(super) BASELINE_AVAILABLE = "SELECT COUNT(DISTINCT p.local_track_id) FROM library_management_plan_items p \
         JOIN library_publish_baselines b ON b.track_id = p.local_track_id \
         WHERE p.job_id = ?1";
    pub(super) WRITE_MANAGED_STATE = "INSERT INTO library_track_management_state (local_track_id, applied_profile_id, \
         applied_profile_revision, applied_naming_script_revision, applied_override_revision, \
         last_operation_job_id, managed_root_id, last_managed_at, last_outcome, \
         managed_path_revision, written_sha256) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'succeeded', ?9, ?10) \
         ON CONFLICT (local_track_id) DO UPDATE SET \
         written_sha256 = COALESCE(excluded.written_sha256, \
         library_track_management_state.written_sha256), \
         applied_profile_id = excluded.applied_profile_id, \
         applied_profile_revision = excluded.applied_profile_revision, \
         applied_naming_script_revision = excluded.applied_naming_script_revision, \
         applied_override_revision = excluded.applied_override_revision, \
         last_operation_job_id = excluded.last_operation_job_id, \
         managed_root_id = excluded.managed_root_id, \
         last_managed_at = excluded.last_managed_at, last_outcome = 'succeeded', \
         managed_path_revision = excluded.managed_path_revision, \
         last_reason_code = NULL, \
         row_revision = library_track_management_state.row_revision + 1";
    pub(super) WRITE_MANAGED_IDENTITY = "INSERT INTO library_track_management_identity (local_track_id, \
         applied_album_identity_revision, applied_track_identity_revision) VALUES (?1, ?2, ?3) \
         ON CONFLICT (local_track_id) DO UPDATE SET \
         applied_album_identity_revision = excluded.applied_album_identity_revision, \
         applied_track_identity_revision = excluded.applied_track_identity_revision";
    pub(super) WRITE_RESTORED_STATE = "INSERT INTO library_track_management_state (local_track_id, last_operation_job_id, \
         last_managed_at, last_outcome) VALUES (?1, ?2, ?3, 'restored') \
         ON CONFLICT (local_track_id) DO UPDATE SET applied_profile_id = NULL, \
         applied_profile_revision = NULL, applied_naming_script_revision = NULL, \
         applied_override_revision = NULL, managed_path_revision = NULL, \
         last_operation_job_id = excluded.last_operation_job_id, \
         last_managed_at = excluded.last_managed_at, last_outcome = 'restored', \
         row_revision = library_track_management_state.row_revision + 1";
    pub(super) DROP_MANAGED_IDENTITY = "DELETE FROM library_track_management_identity WHERE local_track_id = ?1";
    pub(super) TRACK_STATE = "SELECT s.applied_profile_id, s.applied_profile_revision, \
         s.applied_naming_script_revision, s.applied_override_revision, s.managed_root_id, \
         s.last_outcome, s.last_managed_at, i.applied_album_identity_revision, \
         i.applied_track_identity_revision, s.managed_path_revision \
         FROM library_track_management_state s \
         LEFT JOIN library_track_management_identity i ON i.local_track_id = s.local_track_id \
         WHERE s.local_track_id = ?1";
    pub(super) EDIT_OVERRIDE_REVISIONS = "SELECT json_extract(result_json, '$.override_revisions') FROM library_operation_work \
             WHERE job_id = ?1 AND ordinal = ?2";
    pub(super) TRACK_TO_EDIT = "SELECT t.id AS track_id, t.local_album_id AS album_id, t.root_id, t.relative_path, \
                 t.row_revision AS track_revision, a.row_revision AS album_revision FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
                 WHERE t.id = ?1 AND t.availability = 'indexed'";
    pub(super) SAVED_OVERRIDES = "SELECT subject_kind, field_name, id, mode, row_revision \
                 FROM library_management_overrides \
                 WHERE (subject_kind = 'album' AND local_album_id = ?1) \
                 OR (subject_kind = 'track' AND local_track_id = ?2)";
    pub(super) CREATE_TAG_EDIT = "SELECT value_json FROM library_management_overrides WHERE id = ?1";
    pub(super) STATE_REVISIONS = "SELECT s.local_track_id, s.row_revision FROM library_track_management_state s \
         JOIN local_tracks t ON t.id = s.local_track_id WHERE t.local_album_id = ?1";
    pub(super) CHANGED_OUTSIDE = "SELECT row_revision FROM library_track_management_state \
                 WHERE local_track_id = ?1";
    pub(super) ACCEPT_WRITTEN_HASH = "UPDATE library_track_management_state SET written_sha256 = ?2, \
                 row_revision = row_revision + 1 WHERE local_track_id = ?1 AND row_revision = ?3";
    pub(super) ADD_WRITTEN_HASH = "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                 VALUES (?1, ?2) ON CONFLICT (local_track_id) DO NOTHING";
    pub(super) RECORDS_BEFORE = "SELECT json_extract(result_json, ?3) FROM library_operation_work \
             WHERE job_id = ?1 AND ordinal = ?2 AND json_valid(result_json)";
    pub(super) WRITTEN_HASH_OF_JOB = "SELECT written_sha256 FROM library_track_management_state \
                 WHERE local_track_id = ?1 AND last_operation_job_id = ?2 \
                 AND applied_projection_hash = ?3";
    pub(super) SET_WRITTEN_HASH = "UPDATE library_track_management_state SET written_sha256 = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1";
    pub(super) NOTE_WORK_RESULT = "UPDATE library_operation_work SET result_json = json_set(CASE \
             WHEN json_valid(result_json) AND json_type(result_json) = 'object' \
             THEN result_json ELSE '{}' END, ?3, json(?4)) \
             WHERE job_id = ?1 AND ordinal = ?2";
    pub(super) RESTORE_RECORDS = "UPDATE library_track_management_state SET written_sha256 = ?2, \
             row_revision = row_revision + 1 WHERE local_track_id = ?1 \
             AND written_sha256 IS applied_projection_hash";
    pub(super) MARK_FOLLOWED = "UPDATE library_management_automatic_queue SET retag_followed_at = NULL \
         WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL";
    pub(super) IS_MARKED = "SELECT 1 FROM library_album_takeovers WHERE local_album_id = ?1";
    pub(super) MARK_TAKEN_OVER = "INSERT INTO library_album_takeovers (local_album_id, cause, noticed_at) \
         VALUES (?1, ?2, ?3) ON CONFLICT (local_album_id) DO NOTHING";
    pub(super) QUEUE_AUTOMATIC_ALBUM = "INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before_at, \
         last_reason_code) VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT (local_album_id) DO UPDATE SET not_before_at = excluded.not_before_at, \
         attempts = 0, last_reason_code = excluded.last_reason_code";
    pub(super) COPY_MARK = "INSERT OR IGNORE INTO library_album_takeovers (local_album_id, cause, noticed_at, \
         handed_back_at) SELECT ?2, cause, noticed_at, handed_back_at \
         FROM library_album_takeovers WHERE local_album_id = ?1";
    pub(super) STAMP_ALL = "UPDATE library_album_takeovers SET handed_back_at = ?1, hand_back_tries = 0, \
             hand_back_retry_at = NULL WHERE handed_back_at IS NULL RETURNING local_album_id";
    pub(super) NEXT_STAMPED = "SELECT local_album_id FROM library_album_takeovers WHERE handed_back_at IS NOT NULL \
         AND (hand_back_retry_at IS NULL OR hand_back_retry_at <= ?1) \
         ORDER BY handed_back_at, local_album_id LIMIT 1";
    pub(super) COUNT_HAND_BACK_TRY = "UPDATE library_album_takeovers SET hand_back_tries = hand_back_tries + 1 \
         WHERE local_album_id = ?1 RETURNING hand_back_tries";
    pub(super) REOPEN_TAKEOVER = "UPDATE library_album_takeovers SET handed_back_at = NULL, hand_back_tries = 0, \
             hand_back_retry_at = NULL WHERE local_album_id = ?1";
    pub(super) SET_HAND_BACK_RETRY = "UPDATE library_album_takeovers SET hand_back_retry_at = ?2 WHERE local_album_id = ?1";
    pub(super) HAND_BACKS = "SELECT COUNT(*) FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = 'management_handed_back'";
    pub(super) ALBUM_INDEXED_TRACK_PLACES = "SELECT id, root_id, relative_path FROM local_tracks \
             WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id";
    pub(super) DROP_TAKEOVER = "DELETE FROM library_album_takeovers WHERE local_album_id = ?1";
    pub(super) FORGET_MANAGED_PATHS = "UPDATE library_track_management_state SET managed_path_revision = NULL, \
             row_revision = row_revision + 1 WHERE managed_path_revision IS NOT NULL \
             AND last_operation_job_id IS NULL AND local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) FORGET_WRITTEN_HASHES = "UPDATE library_track_management_state SET written_sha256 = NULL \
             WHERE written_sha256 IS NOT NULL AND local_track_id IN \
             (SELECT id FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) ADOPT_WRITTEN_HASH = "INSERT INTO library_track_management_state (local_track_id, written_sha256) \
                 SELECT id, ?4 FROM local_tracks WHERE id = ?1 AND local_album_id = ?5 \
                 AND root_id = ?2 AND relative_path = ?3 AND availability = 'indexed' \
                 ON CONFLICT (local_track_id) DO UPDATE SET \
                 written_sha256 = excluded.written_sha256";
    pub(super) CLEAR_RETAG_FOLLOWED = "UPDATE library_management_automatic_queue SET retag_followed_at = NULL \
             WHERE local_album_id = ?1";
    pub(super) TAKEOVER_COUNTS = "SELECT COALESCE(SUM(handed_back_at IS NULL), 0) AS total, \
         COALESCE(SUM(handed_back_at IS NOT NULL), 0) AS handing_back FROM library_album_takeovers";
    pub(super) WAITING_TAKEOVERS = "SELECT k.local_album_id, a.title, a.album_artist_name, k.cause, k.noticed_at, \
         EXISTS(SELECT 1 FROM library_catalog_actions c \
           WHERE c.local_album_id = k.local_album_id \
           AND c.action_kind = 'management_hand_back_given_up' \
           AND c.created_at >= k.noticed_at) AS hand_back_given_up \
         FROM library_album_takeovers k JOIN local_albums a ON a.id = k.local_album_id \
         WHERE k.handed_back_at IS NULL \
         ORDER BY k.noticed_at DESC, k.local_album_id LIMIT ?1";
    pub(super) fragment ALBUM_ORDER = "ORDER BY COALESCE(a.album_artist_name_folded, ''), a.title_folded, a.id";
    pub(super) fragment SNAPSHOT_COLUMNS = "s.mode, s.origin, s.phase, s.selection_json, s.profile_revision, \
     s.settings_revision, s.naming_revision, s.policy_revision, s.catalog_revision, \
     s.profile_snapshot_json, s.preview_created_at, s.preview_expires_at, \
     s.target_root_id, s.linked_operation_job_id, s.intent_json, s.summary_json, \
     j.idempotency_key, j.lease_expires_at, j.heartbeat_at, j.id, s.proposed_settings_revision";
    pub(super) fragment ITEM_COLUMNS = "ordinal, bundle_ordinal, local_album_id, local_track_id, \
     expected_album_revision, expected_root_id, expected_relative_path, destination_root_id, \
     destination_relative_path, eligibility, estimated_temporary_bytes, expected_file_fingerprint, \
     desired_document_json, diff_json, capability_json, reason_code";
    pub(super) TRACK_ALBUMS = "SELECT a.id AS album_id, a.row_revision AS album_revision, t.id AS track_id FROM local_tracks t \
         JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.id IN (SELECT value FROM json_each(?1)) AND t.availability = 'indexed' \
         " + ALBUM_ORDER + ", t.disc_number, t.track_number, t.relative_path";
    pub(super) SNAPSHOT = "SELECT " + SNAPSHOT_COLUMNS + " FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id WHERE s.job_id = ?1 AND j.kind = ?2";
    pub(super) ITEMS_PAGE = "SELECT " + ITEM_COLUMNS + " FROM library_management_plan_items WHERE job_id = ?1 \
         AND ordinal > ?2 AND (?3 IS NULL OR eligibility = ?3) \
         AND (?4 IS NULL OR reason_code = ?4) AND (?5 IS NULL OR expected_root_id = ?5) \
         AND (?6 IS NULL OR local_album_id = ?6) \
         AND (?7 IS NULL OR json_extract(capability_json, '$.audio_format') = ?7) \
         AND (?8 IS NULL \
           OR (?8 = 'tags' AND json_extract(diff_json, '$.tags_changed') = 1) \
           OR (?8 = 'path' AND json_extract(diff_json, '$.path_changed') = 1) \
           OR (?8 = 'sidecars' AND json_array_length(diff_json, '$.sidecars') > 0) \
           OR (?8 = 'no_change' AND COALESCE(json_extract(diff_json, '$.tags_changed'), 0) = 0 \
               AND COALESCE(json_extract(diff_json, '$.path_changed'), 0) = 0)) \
         ORDER BY ordinal LIMIT ?9";
    pub(super) BUNDLE_ITEMS = "SELECT " + ITEM_COLUMNS + " FROM library_management_plan_items \
         WHERE job_id = ?1 AND bundle_ordinal = ?2 ORDER BY ordinal";
    pub(super) HISTORY = "SELECT " + SNAPSHOT_COLUMNS + " FROM library_management_job_snapshots s \
         JOIN library_operation_jobs j ON j.id = s.job_id WHERE j.kind = ?1 \
         AND (?2 IS NULL OR s.origin = ?2) AND (?3 IS NULL OR s.target_root_id = ?3 \
           OR s.selection_json LIKE '%' || ?3 || '%') \
         AND (?4 IS NULL OR j.state = ?4) AND (?5 IS NULL OR s.mode = ?5) \
         AND (?6 IS NULL OR j.created_at >= ?6) AND (?7 IS NULL OR j.created_at <= ?7) \
         AND (?8 IS NULL OR json_extract(s.profile_snapshot_json, '$.profile.id') = ?8) \
         AND (?9 IS NULL OR j.created_at < ?9 OR (j.created_at = ?9 AND j.id < ?10)) \
         ORDER BY j.created_at DESC, j.id DESC LIMIT ?11";
    /// Albums with an indexed file in the library folders of the JSON list `?1`.
    pub(super) ALBUMS_OF_ROOTS = "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE a.root_id IN (SELECT value FROM json_each(?1)) AND " + ALBUM_HAS_INDEXED_TRACK + " " + ALBUM_ORDER;
    /// Albums with an indexed file among the ids of the JSON list `?1`.
    pub(super) ALBUMS_OF_IDS = "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE a.id IN (SELECT value FROM json_each(?1)) AND " + ALBUM_HAS_INDEXED_TRACK + " " + ALBUM_ORDER;
    /// Albums with an indexed file, by or credited to the artists of the JSON list `?1`.
    pub(super) ALBUMS_OF_ARTISTS = "SELECT a.id, a.row_revision FROM local_albums a \
                 WHERE (a.album_artist_id IN (SELECT value FROM json_each(?1)) \
                 OR EXISTS (SELECT 1 FROM local_album_artists x WHERE x.local_album_id = a.id \
                 AND x.local_artist_id IN (SELECT value FROM json_each(?1)))) \
                 AND " + ALBUM_HAS_INDEXED_TRACK + " " + ALBUM_ORDER;
    /// Albums a catalog filter covers: search `?1`, genre `?2`, years `?3` and `?4`, artists `?5`.
    family FILTERED_ALBUMS = filtered_albums;
    pub(super) fragment ALBUM_HAS_INDEXED_TRACK = "EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                   AND t.availability = 'indexed')";
    /// Whether the app has a record of track `?1`'s file.
    pub(super) TRACK_FILE_HAS_RECORD = "SELECT 1 FROM library_track_management_state s \
             WHERE s.local_track_id = ?1 AND " + HAS_BYTES_RECORD;
    pub(super) fragment JOURNAL_OPEN = "state NOT IN ('cleaned', 'compensated', 'needs_attention')";
    pub(super) fragment JOURNAL_NEEDS_ATTENTION = "state = 'needs_attention' AND bundle_id NOT IN \
                 (SELECT bundle_id FROM library_publish_resolutions)";
    pub(super) OPEN_BUNDLE_COUNT = "SELECT COUNT(DISTINCT bundle_id) FROM library_publish_journal WHERE " + JOURNAL_OPEN;
    pub(super) ATTENTION_JOURNAL_COUNT = "SELECT COUNT(*) FROM library_publish_journal WHERE " + JOURNAL_NEEDS_ATTENTION;
    /// The bundles that need an administrator, at most `?1`.
    pub(super) ATTENTION_BUNDLES = "SELECT bundle_id, COUNT(*) AS file_count, GROUP_CONCAT(dest_rel, char(10)) AS sample_paths, \
                 GROUP_CONCAT(source_rel, char(10)) AS source_paths \
                 FROM library_publish_journal WHERE " + JOURNAL_NEEDS_ATTENTION + " \
                 GROUP BY bundle_id ORDER BY bundle_id LIMIT ?1";
    /// Drop one field's kept value on a subject.
    family DROP_OVERRIDE = drop_override;
    /// Change one field's kept value when it differs, recording who.
    family CHANGE_OVERRIDE = change_override;
    /// Add one field's kept value, recording who.
    family ADD_OVERRIDE = add_override;
    /// Put one field's kept value back as a tag edit found it.
    family RESTORE_OVERRIDE = restore_override;
    /// Add back a kept value a tag edit dropped.
    family READD_OVERRIDE = readd_override;
    /// One album's work outcome.
    pub(super) row WorkOutcome for [WORK_RESULT] {
        state: WorkState,
        failure_code: Option<String>,
        result_json: Option<String>,
        bundle_id: Option<String>,
    }
    pub(super) row AutomaticDueRow for [AUTOMATIC_DUE] {
        local_album_id: String,
        attempts: i64,
    }
    pub(super) row AppliedBundle for [APPLIED_BUNDLES] {
        ordinal: i64,
        bundle_id: String,
    }
    pub(super) row UndoCountsRow for [UNDO_COUNTS] {
        available: i64,
        expired: i64,
        first_expiry_day: Option<i64>,
    }
    pub(super) row AlbumFile for [ALBUM_FILES] {
        id: String,
        root_id: String,
        relative_path: String,
        ingest_source: String,
        stat_revision: String,
    }
    pub(super) row SkippedRow for [AUTOMATIC_SKIPPED] {
        local_album_id: String,
        title: String,
        album_artist_name: Option<String>,
        reason_code: String,
        attempts: i64,
        queued_at: Millis,
    }
    pub(super) row CustomManifest for [ACTIVE_CUSTOM_EDITION] {
        id: String,
        release_group_mbid: String,
        album_title: String,
        album_artist_name: String,
        artist_mbid: Option<String>,
        album_metadata_json: String,
    }
    pub(super) row CustomTrack for [CUSTOM_EDITION_TRACKS] {
        local_track_id: String,
        title: String,
        artist_name: String,
        artist_mbid: Option<String>,
        disc_number: i64,
        track_number: i64,
        duration_seconds: Option<f64>,
        metadata_json: String,
        recording_mbid: Option<String>,
    }
    pub(super) row TrackPath for [ALBUM_TRACK_PATHS] {
        root_id: String,
        relative_path: String,
    }
    pub(super) row MatchGate for [MATCH_GATE] {
        state: String,
        reason_code: String,
    }
    pub(super) row OverrideRow for [OVERRIDES] {
        id: String,
        row_revision: i64,
        field_name: String,
        mode: String,
        value_json: String,
    }
    pub(super) row JournalStateCount for [JOURNAL_COUNTS_BY_STATE] {
        state: String,
        total: i64,
    }
    pub(super) row BundleResolution for [BUNDLE_RESOLUTION] {
        verified_files: i64,
        as_before_files: i64,
        empty_files: i64,
    }
    pub(super) row BaselinePurge for [PURGE_OF_IDEMPOTENCY_KEY] {
        impact_token: String,
        purged_baseline_count: i64,
        detached_reference_count: i64,
    }
    pub(super) row BlobSize for [BASELINE_BLOB_SIZES] {
        sha256: String,
        size: Option<i64>,
    }
    pub(super) row TrackRow for [TRACKS] {
        id: String,
        root_id: String,
        relative_path: String,
        title: String,
        artist_name: Option<String>,
        album_title: String,
        track_number: i64,
        disc_number: i64,
        file_format: String,
        bit_rate: Option<i64>,
        sample_rate: Option<i64>,
        bit_depth: Option<i64>,
    }
    pub(super) row WritableBundle for [WRITABLE_BUNDLES] {
        bundle_ordinal: i64,
        local_album_id: String,
        album_revision: i64,
    }
    pub(super) row RefreshJobs for [REFRESH_JOBS] {
        jellyfin_refresh_job_id: Option<i64>,
        plex_refresh_job_id: Option<i64>,
        navidrome_refresh_job_id: Option<i64>,
    }
    pub(super) row LiveUndo for [LIVE_UNDOS] {
        state: OperationState,
        phase: ManagementPhase,
    }
    pub(super) row TrackStateRow for [TRACK_STATE] {
        applied_profile_id: Option<String>,
        applied_profile_revision: Option<String>,
        applied_naming_script_revision: Option<String>,
        applied_override_revision: Option<String>,
        managed_root_id: Option<String>,
        last_outcome: Option<String>,
        last_managed_at: Option<Millis>,
        applied_album_identity_revision: Option<i64>,
        applied_track_identity_revision: Option<i64>,
        managed_path_revision: Option<String>,
    }
    pub(super) row EditSubject for [TRACK_TO_EDIT] {
        track_id: String,
        album_id: String,
        root_id: String,
        relative_path: String,
        track_revision: i64,
        album_revision: i64,
    }
    pub(super) row SavedOverride for [SAVED_OVERRIDES] {
        subject_kind: TagScope,
        field_name: String,
        id: String,
        mode: OverrideMode,
        row_revision: i64,
    }
    pub(super) row StateRevision for [STATE_REVISIONS] {
        local_track_id: String,
        row_revision: i64,
    }
    pub(super) row TrackPlace for [ALBUM_INDEXED_TRACK_PLACES] {
        id: String,
        root_id: String,
        relative_path: String,
    }
    pub(super) row TakeoverCounts for [TAKEOVER_COUNTS] {
        total: i64,
        handing_back: i64,
    }
    pub(super) row WaitingTakeover for [WAITING_TAKEOVERS] {
        local_album_id: String,
        title: String,
        album_artist_name: Option<String>,
        cause: Cause,
        noticed_at: i64,
        hand_back_given_up: bool,
    }
    pub(super) row TrackAlbum for [TRACK_ALBUMS] {
        album_id: String,
        album_revision: i64,
        track_id: String,
    }
    pub(super) row SnapshotRow for [SNAPSHOT, HISTORY] {
        mode: String,
        origin: String,
        phase: ManagementPhase,
        selection_json: String,
        profile_revision: String,
        settings_revision: String,
        naming_revision: String,
        policy_revision: String,
        catalog_revision: i64,
        profile_snapshot_json: String,
        preview_created_at: Option<Millis>,
        preview_expires_at: Option<Millis>,
        target_root_id: Option<String>,
        linked_operation_job_id: Option<String>,
        intent_json: String,
        summary_json: String,
        idempotency_key: Option<String>,
        lease_expires_at: Option<Millis>,
        heartbeat_at: Option<Millis>,
        id: String,
        proposed_settings_revision: Option<String>,
    }
    pub(super) row ItemRow for [ITEMS_PAGE, BUNDLE_ITEMS] {
        ordinal: i64,
        bundle_ordinal: i64,
        local_album_id: Option<String>,
        local_track_id: Option<String>,
        expected_album_revision: Option<i64>,
        expected_root_id: String,
        expected_relative_path: String,
        destination_root_id: Option<String>,
        destination_relative_path: Option<String>,
        eligibility: Eligibility,
        estimated_temporary_bytes: i64,
        expected_file_fingerprint: String,
        desired_document_json: String,
        diff_json: String,
        capability_json: String,
        reason_code: Option<String>,
    }
    pub(super) row AttentionBundleRow for [ATTENTION_BUNDLES, RETRYING_BUNDLES] {
        bundle_id: String,
        file_count: i64,
        sample_paths: Option<String>,
        source_paths: Option<String>,
    }
    pub(super) row AlbumScopeRow for [ALBUMS_OF_ROOTS, ALBUMS_OF_IDS, ALBUMS_OF_ARTISTS] {
        id: String,
        row_revision: i64,
    }
    pub(super) row FilteredAlbum for family [filtered_albums] {
        id: String,
        row_revision: i64,
    }
}

pub(super) fn filtered_albums(album_artist_only: bool) -> String {
    let artist_clause = if album_artist_only {
        "a.album_artist_id IN (SELECT value FROM json_each(?5))"
    } else {
        "(a.album_artist_id IN (SELECT value FROM json_each(?5)) \
         OR EXISTS (SELECT 1 FROM local_album_artists x WHERE x.local_album_id = a.id \
         AND x.local_artist_id IN (SELECT value FROM json_each(?5))))"
    };
    format!(
        "SELECT a.id, a.row_revision FROM local_albums a \
         WHERE EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
         AND t.availability = 'indexed') \
         AND (?1 IS NULL OR a.title_folded LIKE ?1 OR COALESCE(a.album_artist_name_folded, '') LIKE ?1) \
         AND (?2 IS NULL OR lower(COALESCE(a.primary_genre, '')) = ?2) \
         AND (?3 IS NULL OR a.year >= ?3) AND (?4 IS NULL OR a.year <= ?4) \
         AND (json_array_length(?5) = 0 OR {artist_clause}) {ALBUM_ORDER}"
    )
}

pub(super) fn drop_override(subject: OverrideSubject) -> String {
    let (column, _) = subject.columns();
    format!(
        "DELETE FROM library_management_overrides WHERE subject_kind = ?1 \
                     AND {column} = ?2 AND field_name = ?3"
    )
}

pub(super) fn change_override(subject: OverrideSubject) -> String {
    let (column, _) = subject.columns();
    format!(
        "UPDATE library_management_overrides SET value_json = ?4, mode = ?5, \
                 actor_user_id = ?6, updated_at = ?7, row_revision = row_revision + 1 \
                 WHERE subject_kind = ?1 AND {column} = ?2 AND field_name = ?3 \
                 AND (value_json <> ?4 OR mode <> ?5)"
    )
}

pub(super) fn add_override(subject: OverrideSubject) -> String {
    let (column, table) = subject.columns();
    format!(
        "INSERT OR IGNORE INTO library_management_overrides (id, subject_kind, {column}, \
                 field_name, value_json, mode, actor_user_id, reason, subject_revision, \
                 created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'tag_editor', \
                 COALESCE((SELECT row_revision FROM {table} WHERE id = ?3), 1), ?8, ?8)"
    )
}

pub(super) fn restore_override(subject: OverrideSubject) -> String {
    let (column, _) = subject.columns();
    format!(
        "UPDATE library_management_overrides SET value_json = ?4, mode = ?5, \
                         updated_at = ?6, row_revision = row_revision + 1 \
                         WHERE subject_kind = ?1 AND {column} = ?2 AND field_name = ?3"
    )
}

pub(super) fn readd_override(subject: OverrideSubject) -> String {
    let (column, table) = subject.columns();
    format!(
        "INSERT OR IGNORE INTO library_management_overrides (id, subject_kind, \
                             {column}, field_name, value_json, mode, reason, subject_revision, \
                             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'tag_editor', \
                             COALESCE((SELECT row_revision FROM {table} WHERE id = ?3), 1), ?7, ?7)"
    )
}
