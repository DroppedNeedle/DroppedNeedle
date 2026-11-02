//! The SQL of the operations module.

use droppedneedle_kernel::db::statement::ListLen;

use super::history::MAX_TITLES;
use super::reviews::ReviewOrder;
use crate::identity::sql::PERSONS;

droppedneedle_kernel::sql! {
    pub(super) PLACED_FILES = "SELECT root_id, relative_path FROM local_tracks WHERE id = ?1";
    pub(super) PLACE_IMPORTED = "SELECT EXISTS(SELECT 1 FROM local_tracks t \
             JOIN local_track_external_identities i \
             ON i.local_track_id = t.id AND i.provider = ?2 \
             WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' AND t.id <> ?3 \
             AND lower(i.release_track_mbid) = lower(?4) \
             AND (i.release_mbid IS NULL OR lower(i.release_mbid) = lower(?5)))";
    pub(super) UNDO_AUTOMATIC_EDITION = "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, \
                 candidates_json, state, created_ms, updated_ms) \
                 VALUES (?1, ?2, 'AUTOMATIC_EDITION_CLEARED_TO_REVIEW', '[]', 'pending', ?3, ?3)";
    pub(super) SETTLE_REVIEWS = "UPDATE library_identify_reviews SET state = ?2, resolved_by_user_id = ?3, \
         selected_candidate_key = ?4, updated_ms = ?5 \
         WHERE local_album_id = ?1 AND state = 'pending'";
    pub(super) RECORD_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         operation_job_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    pub(super) RECORD_ARTIST_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_artist_id, \
         before_json, after_json, reason_code, created_at) \
         VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8)";
    pub(super) ALBUM_LIVE = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
         AND retired_into_album_id IS NULL)";
    pub(super) CUSTOM_ACTIVE = "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)";
    pub(super) INDEXED_ON = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1 AND local_album_id = ?2 \
         AND availability = 'indexed')";
    pub(super) STORED_PLACEMENTS = "SELECT t.id, i.release_mbid, i.release_track_mbid, \
         CASE WHEN i.decision_source = 'manual' \
         AND lower(h.release_mbid) = lower(i.release_mbid) \
         AND lower(h.release_track_mbid) = lower(i.release_track_mbid) \
         THEN h.matched_at END \
         FROM local_tracks t JOIN local_track_external_identities i \
         ON i.local_track_id = t.id AND i.provider = ?2 \
         LEFT JOIN library_track_hand_matches h ON h.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' \
         AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id";
    pub(super) WRITTEN_SINCE = "SELECT EXISTS(SELECT 1 FROM library_management_plan_items p \
         JOIN library_management_job_snapshots s ON s.job_id = p.job_id \
         JOIN library_operation_jobs j ON j.id = p.job_id \
         WHERE p.local_track_id = ?1 \
         AND s.mode IN ('apply', 'automatic_apply', 'duplicate_resolution') \
         AND j.state NOT IN ('failed', 'cancelled', 'stopped') \
         AND p.eligibility IN ('eligible', 'warning') \
         AND json_extract(p.desired_document_json, '$.plan') IS NOT NULL \
         AND p.created_at * 1000 >= ?2 \
         AND NOT EXISTS (SELECT 1 FROM library_management_job_snapshots u \
         JOIN library_operation_work w ON w.job_id = u.job_id \
         WHERE u.linked_operation_job_id = p.job_id AND u.mode = 'undo' \
         AND w.ordinal = p.bundle_ordinal AND w.state = 'succeeded'))";
    pub(super) UNMATCH_FILE = "SELECT release_mbid, release_track_mbid, prior_identity_json, matched_at \
             FROM library_track_hand_matches WHERE local_track_id = ?1";
    pub(super) ALBUM_TITLE = "SELECT title FROM local_albums WHERE id = ?1";
    pub(super) FOLD_UNDONE = "SELECT actor_user_id, created_at FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = ?2 \
         AND json_extract(after_json, '$.fold_action_id') = ?3 AND created_at >= ?4 \
         ORDER BY created_at DESC LIMIT 1";
    pub(super) WITH_TRACKS = "SELECT id, title FROM local_tracks WHERE local_album_id = ?1 \
         ORDER BY disc_number, track_number, id";
    pub(super) REVIEW_GUARD_ROW = "SELECT local_album_id, state, row_revision FROM library_identify_reviews \
             WHERE id = ?1";
    pub(super) MARK = "UPDATE library_identify_reviews SET state = ?3, resolved_by_user_id = ?4, \
         updated_ms = ?5 WHERE id = ?1 AND state = ?2";
    pub(super) EXCLUDE_TRACK = "UPDATE local_tracks SET manual_excluded = 1, \
             availability = CASE WHEN availability = 'indexed' THEN 'excluded' \
                            ELSE availability END, \
             excluded_at = ?2, row_revision = row_revision + 1 \
             WHERE local_album_id = ?1 AND manual_excluded = 0 RETURNING id";
    pub(super) RECORD_REVIEW_EXCLUSION = "INSERT OR IGNORE INTO library_review_exclusions (review_id, local_track_id) \
             VALUES (?1, ?2)";
    pub(super) REVIEW_EXCLUSION_COUNT = "SELECT COUNT(*) FROM library_review_exclusions WHERE review_id = ?1";
    pub(super) CLEAR_REVIEW_EXCLUSIONS = "DELETE FROM library_review_exclusions WHERE review_id = ?1";
    pub(super) REQUESTER = "SELECT role, username_display FROM auth_users WHERE id = ?1";
    pub(super) ALBUM_EXISTS = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1)";
    pub(super) ALBUM_EDITION_IDENTITY = "SELECT release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at, row_revision FROM local_album_external_identities \
             WHERE local_album_id = ?1 AND provider = ?2";
    pub(super) ALBUM_MATCH_STATE = "SELECT state, reason_code, candidates_json FROM library_album_match_state \
             WHERE local_album_id = ?1";
    pub(super) PLACING_ATTEMPTS = "SELECT attempts FROM library_edition_remap_queue WHERE local_album_id = ?1";
    pub(super) IDENTIFYING = "SELECT EXISTS(SELECT 1 FROM library_identify_jobs WHERE local_album_id = ?1 \
         AND state IN ('queued', 'running', 'deferred'))";
    pub(super) WAITING_ALBUM_COUNT = "SELECT COUNT(*) FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         WHERE s.state = ?1 AND a.retired_into_album_id IS NULL";
    pub(super) WAITING_ALBUM_PAGE = "SELECT s.local_album_id, a.title, COALESCE(a.album_artist_name, ''), s.reason_code, \
         e.release_mbid, e.release_group_mbid, s.updated_at, COALESCE(e.row_revision, 0) \
         FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         LEFT JOIN local_album_external_identities e \
           ON e.local_album_id = s.local_album_id AND e.provider = ?2 \
         WHERE s.state = ?1 AND a.retired_into_album_id IS NULL \
         ORDER BY s.updated_at DESC, s.local_album_id LIMIT ?3 OFFSET ?4";
    pub(super) WORK_RESULTS = "SELECT ordinal, local_album_id, local_track_id, action, state, failure_code, \
         result_json FROM library_operation_work WHERE job_id = ?1 ORDER BY ordinal LIMIT ?2";
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
    pub(super) CONTROL_OF_IDEMPOTENCY_KEY = "SELECT job_id, control FROM library_operation_control_idempotency \
                 WHERE idempotency_key = ?1";
    pub(super) RECORD_CONTROL_REQUEST = "INSERT INTO library_operation_control_idempotency \
             (idempotency_key, job_id, control, requested_at) VALUES (?1, ?2, ?3, ?4)";
    pub(super) CLEAR_REIDENTIFICATION_RESULT = "UPDATE library_reidentification_snapshots SET result_json = NULL, \
                 selected_candidate_key = NULL WHERE job_id = ?1";
    pub(super) ALBUM_HISTORY_ROWS = "SELECT a.id, a.actor_user_id, a.action_kind, a.reason_code, \
    a.before_json, a.after_json, a.created_at FROM library_catalog_actions a \
    WHERE (a.local_album_id = ?1 OR a.local_album_id IN \
      (SELECT r.id FROM local_albums r WHERE r.retired_into_album_id = ?1)) \
    AND a.created_at >= ?2 \
    AND (a.action_kind IN ('continue_tracks', 'other_music_at_path', 'new_copy_at_path') \
      OR (a.action_kind = 'merge_album' AND a.reason_code IN ('RETAG_JOINED_ACROSS_SCANS', \
        'RETAG_JOINED_SAME_RELEASE', 'AUTOMATIC_EMPTY_ALBUM_SHELL_CONVERGENCE', \
        'AUTOMATIC_PROVIDER_CONTRADICTION_CATALOG_REPAIR')) \
      OR (a.action_kind = 'retag_first_half' \
        AND a.reason_code = 'RETAG_FIRST_HALF_LEFT_AS_IT_WAS') \
      OR a.action_kind = 'retag_dismissal_ended' \
      OR (a.action_kind = 'album_artist' AND a.local_album_id = ?1 \
        AND a.reason_code = 'PLACEHOLDER_ARTIST_FROM_TRACKS') \
      OR a.action_kind IN ('automatic_exact_edition', 'automatic_match_sealed', \
        'automatic_match_withdrawn', 'undo_automatic_edition', 'fold_undone', \
        'management_hand_back_given_up')) \
    ORDER BY a.created_at DESC, a.rowid DESC LIMIT 50";
    /// The release of album `?1` a person chose, when it is `?3` and no custom edition is active.
    pub(super) RELEASE_A_PERSON_CHOSE = "SELECT release_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2 \
                 AND decision_source IN " + PERSONS + " \
                 AND lower(release_mbid) = lower(?3) \
                 AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
                 WHERE c.local_album_id = ?1)";
    /// The first titles of a list of tracks, by disc and track number.
    family TITLES_OF_TRACKS = titles_of_tracks;
    /// How many reviews a listing's filter holds.
    pub(super) REVIEW_COUNT = "SELECT COUNT(*) " + REVIEW_LISTING;
    pub(super) REVIEW_COUNT_BY_REASON = "SELECT r.reason_code, COUNT(*) " + REVIEW_LISTING + " GROUP BY r.reason_code \
             ORDER BY COUNT(*) DESC";
    pub(super) REVIEW_COUNT_BY_STATE = "SELECT r.state, COUNT(*) " + REVIEW_LISTING + " GROUP BY r.state";
    pub(super) REVIEW_BY_ID = "SELECT " + REVIEW_COLUMNS + " FROM library_identify_reviews r \
             LEFT JOIN local_albums a ON a.id = r.local_album_id WHERE r.id = ?1";
    /// A review row's columns; the one before the revision says whether a person's choice, made now or carried over from v2, holds the album.
    pub(super) fragment REVIEW_COLUMNS = "r.id, r.local_album_id, r.reason_code, r.candidates_json, r.state, \
         r.resolved_by_user_id, r.selected_candidate_key, COALESCE(a.title, ''), \
         COALESCE(a.album_artist_name, ''), r.created_ms, r.updated_ms, \
         EXISTS (SELECT 1 FROM local_album_external_identities e \
                 WHERE e.local_album_id = r.local_album_id AND e.decision_source IN " + PERSONS + "), \
         r.row_revision";
    /// `FROM ... WHERE ...` of a review listing, every filter a named parameter that may be NULL.
    pub(super) fragment REVIEW_LISTING = "FROM library_identify_reviews r \
         JOIN local_albums a ON a.id = r.local_album_id \
         WHERE a.retired_into_album_id IS NULL \
         AND (:states IS NULL OR r.state IN (SELECT value FROM json_each(:states))) \
         AND (:album IS NULL OR r.local_album_id = :album) \
         AND (:reason IS NULL OR r.reason_code = :reason) \
         AND (:search IS NULL OR lower(a.title) LIKE :search \
              OR lower(COALESCE(a.album_artist_name, '')) LIKE :search) \
         AND (:ids IS NULL OR r.id IN (SELECT value FROM json_each(:ids))) \
         AND (:scopes IS NULL OR EXISTS (SELECT 1 FROM local_tracks t, \
              (SELECT json_extract(value, '$[0]') AS root, json_extract(value, '$[1]') AS path \
               FROM json_each(:scopes)) s \
              WHERE t.local_album_id = r.local_album_id AND t.root_id = s.root \
              AND (s.path = '.' OR t.relative_path = s.path \
                   OR substr(t.relative_path, 1, length(s.path) + 1) = s.path || '/'))) \
         AND (:policy IS NULL OR EXISTS (SELECT 1 FROM local_tracks t \
              WHERE t.local_album_id = r.local_album_id AND t.applied_policy = :policy)) \
         AND (:job IS NULL OR EXISTS (SELECT 1 FROM library_identify_jobs j \
              WHERE j.local_album_id = r.local_album_id AND j.state = :job)) \
         AND (:no_active = 0 OR NOT EXISTS (SELECT 1 FROM library_identify_jobs j \
              WHERE j.local_album_id = r.local_album_id \
              AND j.state IN ('queued', 'running', 'deferred'))) \
         AND (:created_from IS NULL OR r.created_ms >= :created_from) \
         AND (:created_to IS NULL OR r.created_ms <= :created_to) \
         AND (:updated_from IS NULL OR r.updated_ms >= :updated_from) \
         AND (:updated_to IS NULL OR r.updated_ms <= :updated_to) \
         AND (:with_candidates = 0 OR json_array_length(r.candidates_json) > 0)";
    pub(super) fragment LIFT_EXCLUSIONS = "UPDATE local_tracks SET manual_excluded = 0, \
             availability = CASE WHEN availability = 'excluded' AND applied_policy <> 'excluded' \
                            THEN 'indexed' ELSE availability END, \
             excluded_at = CASE WHEN applied_policy = 'excluded' THEN excluded_at ELSE NULL END, \
             row_revision = row_revision + 1 \
             WHERE manual_excluded = 1 AND ";
    /// Lift the exclusions review `?1` recorded. Both parameters are always bound; each scope uses the one it needs.
    pub(super) LIFT_RECORDED_EXCLUSIONS = LIFT_EXCLUSIONS + "id IN (SELECT local_track_id FROM library_review_exclusions WHERE review_id = ?1) \
         AND ?2 IS NOT NULL";
    /// Lift every exclusion on album `?2`, for a review with none recorded.
    pub(super) LIFT_ALBUM_EXCLUSIONS = LIFT_EXCLUSIONS + "local_album_id = ?2 AND ?1 IS NOT NULL";
    family REVIEW_PAGE = review_page;
}

pub(super) fn titles_of_tracks(tracks: ListLen) -> String {
    let marks = crate::sql::marks(tracks);
    format!(
        "SELECT title FROM local_tracks WHERE id IN ({marks}) \
         ORDER BY disc_number, track_number, title LIMIT {MAX_TITLES}"
    )
}

pub(super) fn review_page(order: ReviewOrder) -> String {
    let order = match order {
        ReviewOrder::Newest => "r.updated_ms DESC",
        ReviewOrder::Oldest => "r.updated_ms ASC",
        ReviewOrder::MostTracks => {
            "(SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = r.local_album_id) DESC, \
             r.updated_ms DESC"
        }
        ReviewOrder::Title => "a.title COLLATE NOCASE ASC, r.updated_ms DESC",
        ReviewOrder::Artist => {
            "COALESCE(a.album_artist_name, '') COLLATE NOCASE ASC, a.title COLLATE NOCASE ASC"
        }
        ReviewOrder::Root => {
            "(SELECT MIN(t.root_id) FROM local_tracks t WHERE t.local_album_id = r.local_album_id), \
             a.title COLLATE NOCASE ASC"
        }
        ReviewOrder::Reason => "r.reason_code ASC, r.updated_ms DESC",
    };
    format!(
        "SELECT {REVIEW_COLUMNS} {REVIEW_LISTING} ORDER BY {order}, r.id LIMIT :limit OFFSET :offset"
    )
}
