//! The SQL of the repairs module.

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

use super::models::{ApplyResult, Confidence, FindingCode, FindingState};
use crate::identity::DecisionSource;
use crate::identity::sql::REVISABLE;
use crate::jobs::sql::live_job;

droppedneedle_kernel::sql_choice! {
    /// Which albums a repair job covers.
    pub(super) enum RepairScopeKind {
        /// Matches a given matcher version made.
        MatchesOfVersion,
        /// Matches the app may revise.
        RevisableMatches,
        ManagementReadiness,
        CatalogIdentityHygiene,
        /// No albums: play filing works through play rows.
        Nothing,
    }
}

droppedneedle_kernel::sql_choice! {
    /// The shape of a repair scope's statement. A list of no values adds no
    /// clause.
    pub(super) struct RepairScope {
        pub(super) kind: RepairScopeKind,
        pub(super) roots: ListLen,
        pub(super) albums: ListLen,
        pub(super) after_run: bool,
    }
}

droppedneedle_kernel::sql! {
    family QUEUE_REPAIR_WORK = queue_repair_work;
    family COUNT_REPAIR_SCOPE = count_repair_scope;
    family PREPARATION_ESTIMATE = preparation_estimate;
    family FINDINGS_PAGE = findings_page;
    family FINDING_COUNTS = finding_counts;
    pub(super) EXPORT_RUN = "SELECT * FROM library_scan_runs WHERE id = ?1";
    pub(super) EXPORT_RUN_SCOPES = "SELECT root_id, relative_path, effective_policy, policy_revision, estimated_count, \
         discovered_count, discovery_state, reconciliation_state, phase_timings_json, error_code \
         FROM library_scan_run_scopes WHERE run_id = ?1 ORDER BY scope_sequence";
    pub(super) EXPORT_RUN_INVENTORY = "SELECT root_id, relative_path, comparison_result, processing_state, failure_code, \
         file_size_bytes FROM library_scan_inventory WHERE run_id = ?1 \
         ORDER BY root_id, relative_path";
    pub(super) EXPORT_RUN_FAILURES = "SELECT root_id, relative_path, failure_code, phase, recorded_at \
         FROM library_scan_failures WHERE run_id = ?1 ORDER BY recorded_at, root_id, relative_path";
    pub(super) EXPORT_JOB_COUNTS = "SELECT json_group_object(state, n) FROM \
             (SELECT state, COUNT(*) AS n FROM library_identify_jobs GROUP BY state)";
    pub(super) ALBUM = "SELECT id, root_id, title_folded, COALESCE(album_artist_name, '') AS artist_name, \
         COALESCE(album_artist_name_folded, '') AS artist_folded, grouping_source, grouping_locked \
         FROM local_albums \
         WHERE id = ?1 AND retired_into_album_id IS NULL";
    pub(super) IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) ALBUM_HAS_TRACKS = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE local_album_id = ?1)";
    pub(super) SHELL_TARGET = "SELECT target.id FROM local_albums target WHERE target.id <> ?1 \
         AND target.retired_into_album_id IS NULL AND target.root_id = ?2 \
         AND target.title_folded = ?3 AND COALESCE(target.album_artist_name_folded, '') = ?4 \
         AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = target.id \
         AND t.availability = 'indexed') ORDER BY target.id LIMIT 2";
    pub(super) TRACKS = "SELECT t.id, t.relative_path, COALESCE(t.disc_number, 0) AS disc, \
         COALESCE(t.track_number, 0) AS number, t.tag_album_title AS tag_title, \
         t.tag_album_artist_name AS tag_artist, t.embedded_release_group_mbid AS embedded_group, \
         ti.release_track_mbid AS release_track, ti.release_mbid AS identity_release, \
         ti.decision_source AS identity_source, \
         t.availability = 'indexed' AS indexed FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         AND ti.provider = ?2 WHERE t.local_album_id = ?1 ORDER BY t.id";
    pub(super) LEGACY_SPLIT_TARGET = "SELECT target.id FROM local_albums target JOIN local_album_external_identities ti \
         ON ti.local_album_id = target.id AND ti.provider = 'musicbrainz' \
         WHERE target.id <> ?1 AND target.retired_into_album_id IS NULL \
         AND target.root_id = ?2 AND target.title_folded = ?3 \
         AND COALESCE(target.album_artist_name_folded, '') = ?4 \
         AND ti.release_group_mbid = ?5 AND EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = target.id AND t.availability = 'indexed') \
         ORDER BY target.id LIMIT 2";
    pub(super) BUMP_ALBUM_REVISION = "UPDATE local_albums SET updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) WRITE_ALBUM_CREDITS = "DELETE FROM local_album_artists WHERE local_album_id = ?1";
    pub(super) TRACK_ARTIST_TAGS = "SELECT id, COALESCE(album_artist_name, '') AS album_artist, \
             album_artist_provenance = 'tag' AS album_artist_from_tag, \
             album_artist_sort, COALESCE(artist_name, '') AS artist FROM local_tracks \
             WHERE local_album_id = ?1 AND availability = 'indexed' \
             ORDER BY disc_number, track_number, id";
    pub(super) ALBUM_HAS_PERSON_CHOICE = "SELECT EXISTS(SELECT 1 FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND actor_user_id IS NOT NULL) \
         OR EXISTS(SELECT 1 FROM library_identify_jobs \
         WHERE local_album_id = ?1 AND state = 'running')";
    pub(super) ALBUM_ARTIST_ID_AND_SORT = "SELECT album_artist_id, album_artist_sort_name FROM local_albums WHERE id = ?1";
    pub(super) WRITE_ALBUM_ARTIST = "UPDATE local_albums SET album_artist_name = ?2, album_artist_name_folded = ?3, \
         album_artist_id = ?4, album_artist_sort_name = ?5, updated_at = ?6, \
         row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) ARTIST_REPAIR_UNDONE = "SELECT EXISTS(SELECT 1 FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = ?2)";
    pub(super) ARTIST_REPAIR_ACTION = "SELECT local_album_id, before_json, after_json FROM library_catalog_actions \
             WHERE id = ?1 AND action_kind = 'album_artist' AND reason_code = ?2";
    pub(super) LIVE_ALBUM_ARTIST = "SELECT album_artist_id, album_artist_name, album_artist_name_folded, \
             album_artist_sort_name FROM local_albums \
             WHERE id = ?1 AND retired_into_album_id IS NULL";
    pub(super) ARTIST_IS_LIVE = "SELECT EXISTS(SELECT 1 FROM local_artists \
                 WHERE id = ?1 AND retired_into_artist_id IS NULL)";
    pub(super) DROP_REPAIR_IDENTIFY_JOB = "DELETE FROM library_identify_jobs WHERE local_album_id = ?1 \
                 AND state = 'queued' AND kind = 'historical' \
                 AND requested_by_user_id IS NULL AND created_at = ?2";
    pub(super) BY_RECORDING = "SELECT t.id, COALESCE(ti.recording_mbid, t.embedded_recording_mbid) = ?1 AS unchallenged \
         FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         WHERE t.id IN (SELECT local_track_id FROM local_track_external_identities \
                        WHERE recording_mbid = ?1 \
                        UNION SELECT id FROM local_tracks WHERE embedded_recording_mbid = ?1) \
         AND t.availability = 'indexed' ORDER BY t.id";
    pub(super) NAME_FITS = "SELECT t.id, t.duration_seconds AS seconds, ti.recording_mbid AS accepted, \
         t.embedded_recording_mbid AS tag \
         FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         WHERE t.title_folded = ?1 AND t.artist_name_folded = ?2 \
         AND (?3 IS NULL OR t.album_title_folded = ?3) AND t.availability = 'indexed' \
         ORDER BY t.id LIMIT 2";
    pub(super) REPAIR_ALREADY_ACTIVE = "SELECT EXISTS(SELECT 1 FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = ?2 AND j.state IN (?3, ?4, ?5) \
         AND s.purpose = ?1)";
    pub(super) CATALOG_REVISION = "SELECT value FROM library_catalog_revision WHERE singleton = 1";
    pub(super) QUEUE_REPAIR_JOB = "INSERT INTO library_operation_jobs (id, kind, state, input_catalog_revision, \
         expected_work_count, idempotency_key, created_at, updated_at) \
         VALUES (?1, ?6, ?7, ?2, ?3, ?4, ?5, ?5) \
         ON CONFLICT (idempotency_key) DO NOTHING";
    pub(super) QUEUE_REPAIR_SNAPSHOT = "INSERT INTO library_repair_snapshots (job_id, purpose, scope_json, \
         target_matcher_version, created_at) VALUES (?1, ?2, ?3, ?4, ?5)";
    pub(super) QUEUE_REPAIR_MATERIALIZATION = "INSERT INTO library_repair_materialization (job_id, pinned_catalog_revision, \
         eligibility_version, purpose, sealed, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, 1, ?5, ?5)";
    pub(super) FILE_PLAY_ON_TRACK = "UPDATE library_play_history SET local_track_id = t.id, \
         local_album_id = t.local_album_id, \
         local_artist_id = COALESCE((SELECT x.local_artist_id FROM local_track_artists x \
                                     WHERE x.local_track_id = t.id ORDER BY x.position LIMIT 1), \
                                    a.album_artist_id) \
         FROM local_tracks t JOIN local_albums a ON a.id = t.local_album_id \
         WHERE t.id = ?2 AND library_play_history.rowid = ?1 \
         AND library_play_history.local_track_id IS NULL \
         AND library_play_history.local_album_id IS NULL \
         AND library_play_history.local_artist_id IS NULL";
    pub(super) STAGING_CURSOR = "SELECT staging_cursor FROM library_repair_materialization WHERE job_id = ?1";
    pub(super) SNAPSHOT_RESULT = "SELECT result_json FROM library_repair_snapshots WHERE job_id = ?1";
    pub(super) ADVANCE_STAGING_CURSOR = "UPDATE library_repair_materialization SET staging_cursor = ?2, updated_at = ?3 \
             WHERE job_id = ?1";
    pub(super) WRITE_SNAPSHOT_RESULT = "UPDATE library_repair_snapshots SET result_json = ?2 WHERE job_id = ?1";
    pub(super) ADD_JOB_COUNTS = "UPDATE library_operation_jobs SET completed_count = completed_count + ?2, \
         succeeded_count = succeeded_count + ?3, skipped_count = skipped_count + ?4, \
         updated_at = ?5, row_revision = row_revision + 1, \
         event_revision = event_revision + 1 WHERE id = ?1";
    pub(super) SNAPSHOT = "SELECT purpose, phase, scope_json, target_matcher_version, result_json \
             FROM library_repair_snapshots WHERE job_id = ?1";
    pub(super) JOB_OF_IDEMPOTENCY_KEY = "SELECT id FROM library_operation_jobs WHERE idempotency_key = ?1";
    pub(super) USER_EXISTS = "SELECT id FROM auth_users WHERE id = ?1";
    pub(super) CREATE_REPAIR_SNAPSHOT = "INSERT INTO library_repair_snapshots (job_id, purpose, scope_json, \
         source_matcher_version, target_matcher_version, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    pub(super) CREATE_REPAIR_MATERIALIZATION = "INSERT INTO library_repair_materialization (job_id, pinned_catalog_revision, \
         eligibility_version, purpose, staging_cursor, staged_ordinal, staged_count, sealed, \
         created_at, updated_at) VALUES (?1, ?2, ?3, ?4, NULL, ?5, ?6, 1, ?7, ?7)";
    pub(super) SET_EXPECTED_WORK_COUNT = "UPDATE library_operation_jobs SET expected_work_count = ?2 WHERE id = ?1";
    pub(super) QUEUED = "SELECT EXISTS(SELECT 1 FROM library_operation_jobs WHERE idempotency_key = ?1)";
    pub(super) IDENTIFIED_WATERMARK = "SELECT identified_at AS at_ms, identified_job_id AS job_id FROM library_repair_watermarks \
             WHERE singleton = 1";
    pub(super) SET_IDENTIFIED_WATERMARK = "INSERT INTO library_repair_watermarks (singleton, identified_at, identified_job_id) \
         VALUES (1, ?1, ?2) ON CONFLICT (singleton) DO UPDATE SET \
         identified_at = excluded.identified_at, identified_job_id = excluded.identified_job_id";
    pub(super) LATEST_ENDED_SCAN = "SELECT id FROM library_scan_runs WHERE terminal_at IS NOT NULL AND state IN \
         ('completed','cancelled','failed','superseded_policy_changed') \
         ORDER BY terminal_at DESC, id DESC LIMIT 1";
    pub(super) CARRIED_FROM_V2 = "SELECT EXISTS(SELECT 1 FROM import_runs)";
    pub(super) OLDER_CONFIRMATION_ALBUMS = "SELECT embedded_release_group_mbid FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed'";
    pub(super) ALBUM_EDITION_TAGS = "SELECT h.barcode, h.catalog_number \
         FROM local_tracks t JOIN local_track_edition_tags h ON h.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND +t.availability = 'indexed'";
    pub(super) ACTIVE_COUNT = "SELECT COUNT(*) FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = ?2 AND j.state IN (?3, ?4, ?5, ?6) \
         AND s.purpose = ?1";
    pub(super) HISTORY = "SELECT j.id, s.purpose, s.phase, s.scope_json, s.target_matcher_version, \
         s.result_json FROM library_operation_jobs j \
         JOIN library_repair_snapshots s ON s.job_id = j.id \
         WHERE j.kind = ?5 AND s.purpose = ?1 \
         AND (?2 IS NULL OR j.created_at < ?2 OR (j.created_at = ?2 AND j.id < ?3)) \
         ORDER BY j.created_at DESC, j.id DESC LIMIT ?4";
    pub(super) SUBJECT_ALBUM_IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, row_revision \
             FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) SUBJECT_TRACK_IDENTITIES = "SELECT t.id AS local_track_id, ti.recording_mbid, ti.release_mbid, ti.release_track_mbid, \
         ti.medium_position, ti.release_track_position FROM local_tracks t \
         LEFT JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
         AND ti.provider = ?2 WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' \
         ORDER BY t.id";
    pub(super) SUBJECT_ALBUM_ROOT = "SELECT root_id FROM local_albums WHERE id = ?1";
    pub(super) CLAIM_ALBUM = "SELECT local_album_id FROM library_operation_work WHERE job_id = ?1 AND ordinal = ?2";
    pub(super) WORK_SUBJECT_REVISION = "SELECT expected_subject_revision FROM library_operation_work \
         WHERE job_id = ?1 AND ordinal = ?2";
    pub(super) WRITE_FINDING = "INSERT INTO library_identity_repair_findings (id, job_id, local_album_id, \
         expected_album_revision, expected_identity_revision, finding_code, confidence, \
         reason_code, apply_eligible, suggested_release_mbid, suggested_release_group_mbid, \
         suggested_edition_json, state, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?14, ?13, ?13) \
         ON CONFLICT (job_id, local_album_id, finding_code) DO UPDATE SET \
         expected_album_revision = excluded.expected_album_revision, \
         expected_identity_revision = excluded.expected_identity_revision, \
         confidence = excluded.confidence, reason_code = excluded.reason_code, \
         apply_eligible = excluded.apply_eligible, \
         suggested_release_mbid = excluded.suggested_release_mbid, \
         suggested_release_group_mbid = excluded.suggested_release_group_mbid, \
         suggested_edition_json = excluded.suggested_edition_json, state = excluded.state, \
         apply_result = NULL, updated_at = excluded.updated_at, row_revision = row_revision + 1";
    pub(super) AUTO_ACCEPT = "UPDATE library_identity_repair_findings SET finding_code = ?4, \
         reason_code = 'EXACT_EDITION_AUTO_ACCEPTED', apply_eligible = 0, state = ?5, \
         apply_result = ?6, updated_at = ?3, row_revision = row_revision + 1 \
         WHERE job_id = ?1 AND local_album_id = ?2 AND finding_code = ?7";
    pub(super) FINDING_COUNTS = "SELECT finding_code, reason_code, COUNT(*) AS finding_count FROM library_identity_repair_findings \
         WHERE job_id = ?1 GROUP BY finding_code, reason_code";
    pub(super) FINDING_ALBUMS_BY_ROOT = "SELECT a.root_id, COUNT(DISTINCT f.local_album_id) AS album_count FROM library_identity_repair_findings f \
         JOIN local_albums a ON a.id = f.local_album_id WHERE f.job_id = ?1 \
         GROUP BY a.root_id ORDER BY a.root_id";
    pub(super) FINDING_TRACK_COUNT = "SELECT COUNT(*) FROM local_tracks WHERE availability = 'indexed' AND local_album_id IN \
         (SELECT local_album_id FROM library_identity_repair_findings WHERE job_id = ?1)";
    pub(super) JOB_INPUT_REVISION = "SELECT COALESCE(input_catalog_revision, 0) FROM library_operation_jobs WHERE id = ?1";
    pub(super) CLEAR_JOB_WORK = "DELETE FROM library_operation_work WHERE job_id = ?1";
    pub(super) ADD_APPLY_WORK = "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
         expected_subject_revision, expected_input_revision, action, idempotency_key, \
         updated_at) SELECT ?1, ROW_NUMBER() OVER (ORDER BY f.id) - 1, f.local_album_id, \
         f.expected_album_revision, COALESCE(f.expected_identity_revision, '') || ':', \
         'repair_apply', ?1 || ':' || f.id || ':apply', ?2 \
         FROM library_identity_repair_findings f \
         WHERE f.job_id = ?1 AND f.apply_eligible = 1 AND f.state = ?3";
    pub(super) SET_SNAPSHOT_PHASE = "UPDATE library_repair_snapshots SET phase = ?2 WHERE job_id = ?1";
    pub(super) OPEN_FINDING = "SELECT id, finding_code, suggested_edition_json \
             FROM library_identity_repair_findings \
             WHERE job_id = ?1 AND local_album_id = ?2 AND apply_eligible = 1 AND state = ?3 \
             ORDER BY id LIMIT 1";
    pub(super) SETTLE_FINDING = "UPDATE library_identity_repair_findings SET state = ?2, apply_result = ?3, \
         updated_at = ?4, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) INSERT_ALBUM_CREDIT = "INSERT INTO local_album_artists (local_album_id, position, \
     local_artist_id, role, credited_name, join_phrase, artist_mbid) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    pub(super) TRACK_CREDITS = "SELECT local_artist_id AS artist_id, credited_name, join_phrase, artist_mbid, \
     'main' AS role FROM local_track_artists WHERE local_track_id = ?1 ORDER BY position";
    pub(super) ALBUM_CREDITS = "SELECT local_artist_id AS artist_id, credited_name, join_phrase, artist_mbid, \
     role FROM local_album_artists WHERE local_album_id = ?1 ORDER BY position";
    /// A row no repair has filed: it names no track, album or artist.
    pub(super) fragment UNFILED = "local_track_id IS NULL AND local_album_id IS NULL AND local_artist_id IS NULL";
    /// Albums the cleanup could apply to: imported groupings, automatic
    /// albums with no file left, and albums whose row names the "Unknown
    /// Artist" placeholder while a file's tags name an artist, unless a
    /// curator undid that repair on the album.
    pub(super) fragment HYGIENE_CANDIDATE = "(a.grouping_source = 'legacy_import' \
     OR (a.grouping_source = 'automatic' AND NOT EXISTS \
     (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id)) \
     OR (a.album_artist_name = 'Unknown Artist' \
     AND a.grouping_locked = 0 AND a.grouping_source <> 'manual' \
     AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
     AND t.availability = 'indexed' AND t.album_artist_provenance = 'tag' \
     AND t.album_artist_name <> 'Unknown Artist') \
     AND NOT EXISTS (SELECT 1 FROM library_catalog_actions u \
     WHERE u.local_album_id = a.id AND u.action_kind = 'album_artist_undone')))";
    /// Identification states that end a job.
    pub(super) fragment IDENTIFY_FINISHED = "('succeeded','failed','attention')";
    /// True when album `a` has an active custom edition sealed against its
    /// current revisions and files.
    pub(super) fragment CUSTOM_CURRENT = "COALESCE((SELECT m.source_album_revision = a.row_revision \
     AND COALESCE(m.source_identity_revision, 0) = COALESCE((SELECT ai.row_revision \
       FROM local_album_external_identities ai WHERE ai.local_album_id = a.id \
       AND ai.provider = 'musicbrainz'), 0) \
     AND (SELECT COUNT(*) FROM library_custom_edition_tracks ct JOIN local_tracks ctt \
       ON ctt.id = ct.local_track_id WHERE ct.manifest_id = m.id \
       AND ctt.local_album_id = a.id AND ctt.availability = 'indexed' \
       AND ct.stat_revision = ctt.stat_revision) = (SELECT COUNT(*) FROM local_tracks it \
       WHERE it.local_album_id = a.id AND it.availability = 'indexed') \
   FROM library_custom_edition_active ac JOIN library_custom_edition_manifests m \
   ON m.id = ac.manifest_id WHERE ac.local_album_id = a.id), 0)";
    pub(super) UNFILED_COUNT = "SELECT COUNT(*) FROM library_play_history WHERE " + UNFILED;
    pub(super) STEP = "SELECT rowid AS play_rowid, CAST(track_name AS TEXT) AS track_name, \
             CAST(artist_name AS TEXT) AS artist_name, CAST(album_name AS TEXT) AS album_name, \
             CAST(recording_mbid AS TEXT) AS recording_mbid, \
             CAST(release_group_mbid AS TEXT) AS release_group_mbid, \
             CAST(duration_ms AS INTEGER) AS duration_ms, CAST(source AS TEXT) AS source FROM library_play_history \
             WHERE " + UNFILED + " AND rowid > ?1 ORDER BY rowid LIMIT ?2";
    pub(super) HYGIENE_CANDIDATES_EXIST = "SELECT EXISTS(SELECT 1 FROM local_albums a \
             WHERE a.retired_into_album_id IS NULL AND " + HYGIENE_CANDIDATE + ")";
    pub(super) NEWEST_IDENTIFIED = "SELECT updated_at AS at_ms, id AS job_id FROM library_identify_jobs \
                 WHERE state IN " + IDENTIFY_FINISHED + " ORDER BY updated_at DESC, id DESC LIMIT 1";
    pub(super) IDENTIFIED_AFTER = "SELECT j.updated_at AS at_ms, j.id AS job_id, a.id AS album_id, \
         a.retired_into_album_id IS NULL AND " + HYGIENE_CANDIDATE + " AS candidate \
         FROM library_identify_jobs j JOIN local_albums a ON a.id = j.local_album_id \
         WHERE j.state IN " + IDENTIFY_FINISHED + " \
         AND (j.updated_at > ?1 OR (j.updated_at = ?1 AND j.id > ?2)) \
         ORDER BY j.updated_at, j.id LIMIT ?3";
    pub(super) CUSTOM_EDITION_IS_CURRENT = "SELECT " + CUSTOM_CURRENT + " FROM local_albums a WHERE a.id = ?1 AND EXISTS \
                 (SELECT 1 FROM library_custom_edition_active ac WHERE ac.local_album_id = a.id)";
    pub(super) TRACK_ID_BACKFILL_ALBUMS = "SELECT a.id FROM local_albums a \
         LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         LEFT JOIN library_album_match_state m ON m.local_album_id = a.id \
         WHERE a.retired_into_album_id IS NULL \
         AND (i.local_album_id IS NOT NULL OR m.local_album_id IS NOT NULL) \
         AND (i.decision_source IS NULL OR i.decision_source IN " + REVISABLE + ") \
         AND COALESCE(m.reason_code, '') != 'DETACHED_BY_PERSON' \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND ((i.local_album_id IS NOT NULL AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND COALESCE(t.embedded_release_mbid, '') != '' \
             AND COALESCE(t.embedded_release_track_mbid, '') != '' \
             AND COALESCE(t.embedded_recording_mbid, '') != '' \
             AND ((i.local_album_id IS NOT NULL \
                 AND lower(t.embedded_release_mbid) != lower(COALESCE(i.release_mbid, '')) \
                 AND NOT EXISTS (SELECT 1 FROM library_management_metadata_snapshots s, \
                     json_each(s.canonical_payload_json, '$.old_ids') old \
                     WHERE s.provider = ?1 AND s.entity_kind = 'release' \
                     AND s.entity_id = lower(i.release_mbid) \
                     AND lower(old.value) = lower(t.embedded_release_mbid))) \
             OR EXISTS (SELECT 1 FROM library_management_metadata_snapshots s \
                 WHERE s.provider = ?1 AND s.entity_kind = 'release' \
                 AND s.entity_id = lower(t.embedded_release_mbid) \
                 AND NOT EXISTS (SELECT 1 FROM json_each(s.canonical_payload_json, '$.tracks') rt \
                     WHERE lower(json_extract(rt.value, '$.id')) \
                         = lower(t.embedded_release_track_mbid)))))) \
         OR EXISTS (SELECT 1 FROM json_each(m.candidates_json) candidate, \
             json_each(candidate.value, '$.track_evidence') track, \
             json_each(track.value, '$.evidence_kinds') kind \
             WHERE kind.value = 'release_track_mbid_conflict')) \
         ORDER BY a.id";
    pub(super) GROUP_ONLY_BACKFILL_ALBUMS = "SELECT a.id FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NULL AND i.decision_source IN " + REVISABLE + " \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND t.applied_policy != 'automatic') \
         ORDER BY a.id";
    pub(super) CONFIRMED_ALBUMS_WITH_GROUP = "SELECT a.id, i.release_group_mbid FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NOT NULL AND i.decision_source IN " + REVISABLE + " \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state m \
             WHERE m.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND t.availability = 'indexed' \
             AND t.applied_policy != 'automatic') \
         ORDER BY a.id";
    pub(super) EDITION_ID_ALBUMS = "SELECT a.id, i.release_mbid, EXISTS (SELECT 1 FROM library_artist_reconciliation_state s \
             WHERE s.local_album_id = a.id AND s.state = 'waiting_for_identity' \
             AND s.reason_code = ?2) AS fetching, \
         EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND +t.availability = 'indexed' \
             AND NOT EXISTS (SELECT 1 FROM local_track_edition_tags h \
                 WHERE h.local_track_id = t.id)) AS unread, \
         EXISTS (SELECT 1 FROM local_tracks t \
             JOIN local_track_edition_tags h ON h.local_track_id = t.id \
             WHERE t.local_album_id = a.id AND +t.availability = 'indexed' \
             AND (trim(COALESCE(h.barcode, '')) <> '' \
                 OR trim(COALESCE(h.catalog_number, '')) <> '')) AS tagged \
         FROM local_albums a \
         JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = ?1 \
         WHERE a.retired_into_album_id IS NULL \
         AND i.release_mbid IS NOT NULL AND i.decision_source IN " + REVISABLE + " \
         AND NOT EXISTS (SELECT 1 FROM library_album_match_state m \
             WHERE m.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
             WHERE c.local_album_id = a.id) \
         AND NOT EXISTS (SELECT 1 FROM library_identify_reviews r \
             WHERE r.local_album_id = a.id AND r.state IN ('rejected','dismissed','excluded')) \
         AND EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND +t.availability = 'indexed') \
         AND NOT EXISTS (SELECT 1 FROM local_tracks t \
             WHERE t.local_album_id = a.id AND +t.availability = 'indexed' \
             AND (t.applied_policy != 'automatic' \
                 OR trim(COALESCE(t.embedded_release_mbid, '')) != '')) \
         ORDER BY a.id";
    /// Work a person or a job is doing on a list of albums (`?2` on); `?1` is now.
    family ALBUMS_HAVE_LIVE_WORK = albums_have_live_work;
    /// Move a list of tracks to the album bound first.
    family MOVE_TRACKS_TO_ALBUM = move_tracks_to_album;

    pub(super) row SnapshotRow for [SNAPSHOT] {
        purpose: Option<String>,
        phase: String,
        scope_json: String,
        target_matcher_version: String,
        result_json: Option<String>,
    }
    pub(super) row HistoryRow for [HISTORY] {
        id: String,
        purpose: Option<String>,
        phase: String,
        scope_json: String,
        target_matcher_version: String,
        result_json: Option<String>,
    }
    pub(super) row WatermarkRow for [NEWEST_IDENTIFIED, IDENTIFIED_WATERMARK] {
        at_ms: i64,
        job_id: String,
    }
    pub(super) row IdentifiedRow for [IDENTIFIED_AFTER] {
        at_ms: i64,
        job_id: String,
        album_id: String,
        candidate: bool,
    }
    pub(super) row ConfirmedAlbum for [CONFIRMED_ALBUMS_WITH_GROUP] {
        id: String,
        release_group_mbid: String,
    }
    pub(super) row EditionCandidate for [EDITION_ID_ALBUMS] {
        id: String,
        release_mbid: String,
        fetching: bool,
        unread: bool,
        tagged: bool,
    }
    pub(super) row EditionTags for [ALBUM_EDITION_TAGS] {
        barcode: Option<String>,
        catalog_number: Option<String>,
    }
    pub(super) row PreparationRow for family [preparation_estimate] {
        release_group_mbid: Option<String>,
        release_mbid: Option<String>,
        tracks: i64,
        mapped: i64,
        custom_current: bool,
    }
    pub(super) row SubjectIdentity for [SUBJECT_ALBUM_IDENTITY] {
        release_group_mbid: Option<String>,
        release_mbid: Option<String>,
        decision_source: DecisionSource,
        row_revision: i64,
    }
    pub(super) row SubjectTrack for [SUBJECT_TRACK_IDENTITIES] {
        local_track_id: String,
        recording_mbid: Option<String>,
        release_mbid: Option<String>,
        release_track_mbid: Option<String>,
        medium_position: Option<i64>,
        release_track_position: Option<i64>,
    }
    pub(super) row FindingCount for [FINDING_COUNTS] {
        finding_code: FindingCode,
        reason_code: String,
        finding_count: i64,
    }
    pub(super) row RootAlbumCount for [FINDING_ALBUMS_BY_ROOT] {
        root_id: String,
        album_count: i64,
    }
    pub(super) row OpenFindingRow for [OPEN_FINDING] {
        id: String,
        finding_code: FindingCode,
        suggested_edition_json: String,
    }
    pub(super) row FindingPageRow for family [findings_page] {
        id: String,
        local_album_id: String,
        album_title: String,
        album_artist_name: Option<String>,
        album_year: Option<i64>,
        cover_available: bool,
        review_id: Option<String>,
        finding_code: FindingCode,
        reason_code: String,
        confidence: Confidence,
        apply_eligible: bool,
        state: FindingState,
        apply_result: Option<ApplyResult>,
        suggested_release_mbid: Option<String>,
        suggested_release_group_mbid: Option<String>,
        suggested_edition_json: String,
        updated_at: Millis,
        row_revision: i64,
    }
    pub(super) row CurrentFindingCount for family [finding_counts] {
        finding_code: FindingCode,
        finding_count: i64,
    }
    #[derive(Debug, Clone)]
    pub(super) row HygieneAlbum for [ALBUM] {
        id: String,
        root_id: String,
        title_folded: String,
        artist_name: String,
        artist_folded: String,
        grouping_source: String,
        grouping_locked: bool,
    }
    pub(super) row HygieneIdentity for [IDENTITY] {
        release_group_mbid: String,
        release_mbid: Option<String>,
        decision_source: DecisionSource,
    }
    #[derive(Debug, Clone)]
    pub(super) row HygieneTrack for [TRACKS] {
        id: String,
        relative_path: String,
        disc: i64,
        number: i64,
        tag_title: Option<String>,
        tag_artist: Option<String>,
        embedded_group: Option<String>,
        release_track: Option<String>,
        identity_release: Option<String>,
        identity_source: Option<DecisionSource>,
        indexed: bool,
    }
    pub(super) row CreditColumns for [TRACK_CREDITS, ALBUM_CREDITS] {
        artist_id: String,
        credited_name: Option<String>,
        join_phrase: String,
        artist_mbid: Option<String>,
        role: String,
    }
    pub(super) row NamedTrack for [TRACK_ARTIST_TAGS] {
        id: String,
        album_artist: String,
        album_artist_from_tag: bool,
        album_artist_sort: Option<String>,
        artist: String,
    }
    pub(super) row AlbumArtistAndSort for [ALBUM_ARTIST_ID_AND_SORT] {
        album_artist_id: String,
        album_artist_sort_name: Option<String>,
    }
    pub(super) row ArtistRepairAction for [ARTIST_REPAIR_ACTION] {
        local_album_id: Option<String>,
        before_json: String,
        after_json: String,
    }
    pub(super) row LiveAlbumArtist for [LIVE_ALBUM_ARTIST] {
        album_artist_id: Option<String>,
        album_artist_name: Option<String>,
        album_artist_name_folded: Option<String>,
        album_artist_sort_name: Option<String>,
    }
    pub(super) row RecordingFit for [BY_RECORDING] {
        id: String,
        unchallenged: bool,
    }
    pub(super) row NameFit for [NAME_FITS] {
        id: String,
        seconds: Option<f64>,
        accepted: Option<String>,
        tag: Option<String>,
    }
    pub(super) row UnfiledPlay for [STEP] {
        play_rowid: i64,
        track_name: String,
        artist_name: String,
        album_name: Option<String>,
        recording_mbid: Option<String>,
        release_group_mbid: Option<String>,
        duration_ms: Option<i64>,
        source: Option<String>,
    }
}

pub(super) fn albums_have_live_work(albums: ListLen) -> String {
    let marks = (2..albums.get() + 2)
        .map(|number| format!("?{number}"))
        .collect::<Vec<_>>()
        .join(",");
    let live = live_job!("?1");
    format!(
        "SELECT EXISTS(SELECT 1 FROM library_contribution_drafts WHERE local_album_id IN \
             ({marks}) AND state IN ('draft','ready','seeded','verifying','needs_review')) \
             OR EXISTS(SELECT 1 FROM library_management_plan_items item \
             JOIN library_operation_jobs job ON job.id = item.job_id \
             WHERE item.local_album_id IN ({marks}) AND {live}) \
             OR EXISTS(SELECT 1 FROM library_management_overrides \
             WHERE local_album_id IN ({marks}))"
    )
}

pub(super) fn move_tracks_to_album(tracks: ListLen) -> String {
    let marks = crate::sql::marks(tracks);
    format!(
        "UPDATE local_tracks SET local_album_id = ?, row_revision = row_revision + 1 \
             WHERE id IN ({marks})"
    )
}

/// `FROM ... WHERE ...` for the albums of a repair scope.
fn repair_scope_from(scope: RepairScope) -> String {
    let roots = match scope.roots.get() {
        0 => String::new(),
        _ => format!("AND a.root_id IN ({})", crate::sql::marks(scope.roots)),
    };
    let indexed = "AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                   AND t.availability = 'indexed')";
    let matches = |identity: &str| {
        format!(
            "FROM local_albums a JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = 'musicbrainz' \
             WHERE a.retired_into_album_id IS NULL {indexed} {roots} {identity}"
        )
    };
    match scope.kind {
        RepairScopeKind::MatchesOfVersion => matches("AND i.matcher_version = ?"),
        RepairScopeKind::RevisableMatches => {
            matches(&format!("AND i.decision_source IN {REVISABLE}"))
        }
        RepairScopeKind::ManagementReadiness => format!(
            "FROM local_albums a WHERE a.retired_into_album_id IS NULL {indexed} \
             AND NOT EXISTS (SELECT 1 FROM library_management_exclusions x \
             WHERE x.local_album_id = a.id) {roots}"
        ),
        RepairScopeKind::CatalogIdentityHygiene => {
            let albums = match scope.albums.get() {
                0 => String::new(),
                _ => format!("AND a.id IN ({})", crate::sql::marks(scope.albums)),
            };
            // Albums the run touched: new or changed files, files that
            // moved in, and albums files moved out of (the shells the run
            // emptied).
            let run = if scope.after_run {
                "AND (EXISTS (SELECT 1 FROM library_scan_inventory inv \
                 JOIN local_tracks t ON t.id = inv.local_track_id WHERE inv.run_id = ? \
                 AND inv.comparison_result IN ('new','changed') \
                 AND t.local_album_id = a.id) \
                 OR EXISTS (SELECT 1 FROM library_scan_album_moves m \
                 JOIN local_tracks t ON t.id = m.local_track_id \
                 WHERE m.run_id = ? AND t.local_album_id = a.id) \
                 OR EXISTS (SELECT 1 FROM library_scan_album_moves m \
                 WHERE m.run_id = ? AND m.previous_album_id = a.id))"
            } else {
                ""
            };
            format!(
                "FROM local_albums a WHERE a.retired_into_album_id IS NULL \
                 AND {HYGIENE_CANDIDATE} {roots} {albums} {run}"
            )
        }
        RepairScopeKind::Nothing => "FROM local_albums a WHERE 0".to_owned(),
    }
}

pub(super) fn queue_repair_work(scope: RepairScope) -> String {
    let scope_sql = repair_scope_from(scope);
    format!(
        "INSERT INTO library_operation_work (job_id, ordinal, local_album_id, \
             expected_subject_revision, expected_input_revision, action, idempotency_key, \
             updated_at) SELECT ?1, ROW_NUMBER() OVER (ORDER BY a.id) - 1, a.id, \
             a.row_revision, '', ?2, ?1 || ':' || a.id || ':' || ?2, ?3 {scope_sql}"
    )
}

pub(super) fn count_repair_scope(scope: RepairScope) -> String {
    format!("SELECT COUNT(*) {}", repair_scope_from(scope))
}

pub(super) fn preparation_estimate(roots: ListLen) -> String {
    let scope = RepairScope {
        kind: RepairScopeKind::ManagementReadiness,
        roots,
        albums: ListLen::new(0),
        after_run: false,
    };
    format!(
        "SELECT i.release_group_mbid, i.release_mbid, \
         (SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = a.id \
          AND +t.availability = 'indexed') AS tracks, \
         (SELECT COUNT(DISTINCT ti.release_track_mbid) FROM local_tracks t \
          JOIN local_track_external_identities ti ON ti.local_track_id = t.id \
          AND ti.provider = 'musicbrainz' WHERE t.local_album_id = a.id \
          AND +t.availability = 'indexed' AND ti.release_track_mbid IS NOT NULL \
          AND ti.release_mbid = i.release_mbid AND ti.medium_position IS NOT NULL \
          AND ti.release_track_position IS NOT NULL) AS mapped, \
         {CUSTOM_CURRENT} AS custom_current \
         {from} ",
        from = repair_scope_from(scope).replacen(
            "FROM local_albums a",
            "FROM local_albums a LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = a.id AND i.provider = 'musicbrainz'",
            1
        )
    )
}

/// `FROM ... WHERE ...` of a job's findings, `?` being the job. With
/// `current`, only findings that still describe the album as it is.
fn findings_from(current: bool) -> String {
    let current = if current {
        format!(
            "AND ((f.state = '{open}' AND a.row_revision = f.expected_album_revision \
             AND ((f.expected_identity_revision IS NULL AND i.row_revision IS NULL) \
             OR i.row_revision = f.expected_identity_revision)) \
             OR (f.finding_code = '{accepted}' AND f.state = '{applied}' \
             AND EXISTS (SELECT 1 FROM library_automatic_edition_undo u \
             WHERE u.local_album_id = f.local_album_id AND u.consumed_at IS NULL)))",
            open = FindingState::Open.as_str(),
            accepted = FindingCode::ExactReleaseAutoAccepted.as_str(),
            applied = FindingState::Applied.as_str(),
        )
    } else {
        String::new()
    };
    format!(
        "FROM library_identity_repair_findings f JOIN local_albums a ON a.id = f.local_album_id \
         LEFT JOIN local_album_external_identities i ON i.local_album_id = a.id \
         AND i.provider = 'musicbrainz' WHERE f.job_id = ? {current}"
    )
}

pub(super) fn findings_page(
    current: bool,
    by_code: bool,
    codes: ListLen,
    after_cursor: bool,
) -> String {
    let from = findings_from(current);
    let mut filter = String::new();
    if by_code {
        filter.push_str(&format!(
            " AND f.finding_code IN ({})",
            crate::sql::marks(codes)
        ));
    }
    if after_cursor {
        filter.push_str(" AND (f.updated_at < ? OR (f.updated_at = ? AND f.id < ?))");
    }
    format!(
        "SELECT f.id, f.local_album_id, a.title AS album_title, a.album_artist_name, \
         a.year AS album_year, \
         (EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
          OR i.release_group_mbid IS NOT NULL) AS cover_available, \
         (SELECT r.id FROM library_identify_reviews r WHERE r.local_album_id = f.local_album_id \
          ORDER BY r.updated_at DESC, r.id DESC LIMIT 1) AS review_id, \
         f.finding_code, f.reason_code, f.confidence, f.apply_eligible, f.state, \
         f.apply_result, f.suggested_release_mbid, f.suggested_release_group_mbid, \
         f.suggested_edition_json, f.updated_at, f.row_revision \
         {from}{filter} ORDER BY f.updated_at DESC, f.id DESC LIMIT ?"
    )
}

pub(super) fn finding_counts(current: bool) -> String {
    format!(
        "SELECT f.finding_code, COUNT(*) AS finding_count {} GROUP BY f.finding_code",
        findings_from(current)
    )
}
