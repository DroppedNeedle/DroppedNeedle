//! The SQL of an album's edition.

use droppedneedle_kernel::db::time::Millis;

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
                 candidates_json, state, created_at, updated_at) \
                 VALUES (?1, ?2, 'AUTOMATIC_EDITION_CLEARED_TO_REVIEW', '[]', 'pending', ?3, ?3)";
    pub(crate) ALBUM_LIVE = "SELECT EXISTS(SELECT 1 FROM local_albums WHERE id = ?1 \
         AND retired_into_album_id IS NULL)";
    pub(super) CUSTOM_ACTIVE = "SELECT EXISTS(SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1)";
    pub(super) INDEXED_ON = "SELECT EXISTS(SELECT 1 FROM local_tracks WHERE id = ?1 AND local_album_id = ?2 \
         AND availability = 'indexed')";
    pub(super) STORED_PLACEMENTS = "SELECT t.id, i.release_mbid, i.release_track_mbid, \
         CASE WHEN i.decision_source = 'manual' \
         AND lower(h.release_mbid) = lower(i.release_mbid) \
         AND lower(h.release_track_mbid) = lower(i.release_track_mbid) \
         THEN h.matched_at END AS matched_at \
         FROM local_tracks t JOIN local_track_external_identities i \
         ON i.local_track_id = t.id AND i.provider = ?2 \
         LEFT JOIN library_track_hand_matches h ON h.local_track_id = t.id \
         WHERE t.local_album_id = ?1 AND +t.availability = 'indexed' \
         AND trim(COALESCE(i.release_track_mbid, '')) <> '' ORDER BY t.id";
    pub(super) WRITTEN_SINCE = "SELECT EXISTS(SELECT 1 FROM library_management_plan_items p \
         JOIN library_management_job_snapshots s ON s.job_id = p.job_id \
         JOIN library_operation_jobs j ON j.id = p.job_id \
         WHERE p.local_track_id = ?1 \
         AND s.mode IN (?3, ?4, ?5) \
         AND j.state NOT IN (?6, ?7, ?8) \
         AND p.eligibility IN (?9, ?10) \
         AND json_extract(p.desired_document_json, '$.plan') IS NOT NULL \
         AND p.created_at >= ?2 \
         AND NOT EXISTS (SELECT 1 FROM library_management_job_snapshots u \
         JOIN library_operation_work w ON w.job_id = u.job_id \
         WHERE u.linked_operation_job_id = p.job_id AND u.mode = ?11 \
         AND w.ordinal = p.bundle_ordinal AND w.state = ?12))";
    pub(super) UNMATCH_FILE = "SELECT release_mbid, release_track_mbid, prior_identity_json, matched_at \
             FROM library_track_hand_matches WHERE local_track_id = ?1";
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
    pub(super) WAITING_ALBUM_PAGE = "SELECT s.local_album_id, a.title, COALESCE(a.album_artist_name, '') AS artist_name, s.reason_code, \
         e.release_mbid, e.release_group_mbid, s.updated_at, \
         COALESCE(e.row_revision, 0) AS identity_revision \
         FROM library_album_match_state s \
         JOIN local_albums a ON a.id = s.local_album_id \
         LEFT JOIN local_album_external_identities e \
           ON e.local_album_id = s.local_album_id AND e.provider = ?2 \
         WHERE s.state = ?1 AND a.retired_into_album_id IS NULL \
         ORDER BY s.updated_at DESC, s.local_album_id LIMIT ?3 OFFSET ?4";
    /// The release of album `?1` a person chose, when it is `?3` and no custom edition is active.
    pub(super) RELEASE_A_PERSON_CHOSE = "SELECT release_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND provider = ?2 \
                 AND decision_source IN " + PERSONS + " \
                 AND lower(release_mbid) = lower(?3) \
                 AND NOT EXISTS (SELECT 1 FROM library_custom_edition_active c \
                 WHERE c.local_album_id = ?1)";
    pub(super) row PlacedFileRow for [PLACED_FILES] {
        root_id: String,
        relative_path: String,
    }
    pub(super) row StoredPlacementRow for [STORED_PLACEMENTS] {
        id: String,
        release_mbid: Option<String>,
        release_track_mbid: String,
        matched_at: Option<i64>,
    }
    pub(super) row HandMatchRow for [UNMATCH_FILE] {
        release_mbid: String,
        release_track_mbid: String,
        prior_identity_json: Option<String>,
        matched_at: i64,
    }
    pub(super) row EditionIdentity for [ALBUM_EDITION_IDENTITY] {
        release_group_mbid: String,
        release_mbid: Option<String>,
        decision_source: String,
        selected_by_user_id: Option<String>,
        selected_at: Millis,
        row_revision: i64,
    }
    pub(super) row MatchStateRow for [ALBUM_MATCH_STATE] {
        state: String,
        reason_code: String,
        candidates_json: String,
    }
    pub(super) row WaitingAlbumRow for [WAITING_ALBUM_PAGE] {
        local_album_id: String,
        title: String,
        artist_name: String,
        reason_code: String,
        release_mbid: Option<String>,
        release_group_mbid: Option<String>,
        updated_at: Millis,
        identity_revision: i64,
    }
}
