//! The SQL of identification reviews.

use super::ReviewOrder;
use crate::identify::models::ReviewState;
use crate::identity::sql::PERSONS;

droppedneedle_kernel::sql! {
    pub(super) SETTLE_REVIEWS = "UPDATE library_identify_reviews SET state = ?2, resolved_by_user_id = (SELECT id FROM auth_users WHERE id = ?3), \
         selected_candidate_key = ?4, updated_at = ?5 \
         WHERE local_album_id = ?1 AND state = 'pending'";
    pub(super) WITH_TRACKS = "SELECT id, title FROM local_tracks WHERE local_album_id = ?1 \
         ORDER BY disc_number, track_number, id";
    pub(super) REVIEW_GUARD_ROW = "SELECT local_album_id, state, row_revision FROM library_identify_reviews \
             WHERE id = ?1";
    /// Review `?1` as a reopen reads it: who closed it, the revisions noted
    /// then, the album's match row revision now (0 without one), and
    /// whether the album has another open review.
    pub(super) REOPEN_GUARD = "SELECT r.local_album_id, r.state, r.row_revision, r.resolved_by_user_id, \
         r.reopen_review_revision, r.reopen_identity_revision, \
         (SELECT COALESCE(MAX(e.row_revision), 0) FROM local_album_external_identities e \
          WHERE e.local_album_id = r.local_album_id AND e.provider = 'musicbrainz') AS identity_revision, \
         EXISTS(SELECT 1 FROM library_identify_reviews o WHERE o.local_album_id = r.local_album_id \
          AND o.state = 'pending' AND o.id <> r.id) AS other_pending \
         FROM library_identify_reviews r WHERE r.id = ?1";
    /// A person closed review `?1`: note its revision and its album's match
    /// row revision, which a reopen must find unchanged.
    pub(super) NOTE_REOPENABLE = "UPDATE library_identify_reviews SET reopen_review_revision = row_revision, \
         reopen_identity_revision = (SELECT COALESCE(MAX(e.row_revision), 0) \
         FROM local_album_external_identities e \
         WHERE e.local_album_id = library_identify_reviews.local_album_id AND e.provider = 'musicbrainz') \
         WHERE id = ?1";
    /// Review `?1` is open again; it can no longer be reopened.
    pub(super) CLEAR_REOPENABLE = "UPDATE library_identify_reviews SET reopen_review_revision = NULL, \
         reopen_identity_revision = NULL WHERE id = ?1";
    pub(super) MARK = "UPDATE library_identify_reviews SET state = ?3, resolved_by_user_id = (SELECT id FROM auth_users WHERE id = ?4), \
         updated_at = ?5 WHERE id = ?1 AND state = ?2";
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
    /// How many reviews a listing's filter holds.
    pub(super) REVIEW_COUNT = "SELECT COUNT(*) " + REVIEW_LISTING;
    pub(super) REVIEW_COUNT_BY_REASON = "SELECT r.reason_code AS bucket, COUNT(*) AS total " + REVIEW_LISTING + " GROUP BY r.reason_code \
             ORDER BY COUNT(*) DESC";
    pub(super) REVIEW_COUNT_BY_STATE = "SELECT r.state AS bucket, COUNT(*) AS total " + REVIEW_LISTING + " GROUP BY r.state";
    pub(super) REVIEW_BY_ID = "SELECT " + REVIEW_COLUMNS + " FROM library_identify_reviews r \
             LEFT JOIN local_albums a ON a.id = r.local_album_id WHERE r.id = ?1";
    /// A review row's columns; the one before the revision says whether a person's choice, made now or carried over from v2, holds the album.
    pub(super) fragment REVIEW_COLUMNS = "r.id, r.local_album_id, r.reason_code, r.candidates_json, r.state, \
         r.resolved_by_user_id, r.selected_candidate_key, COALESCE(a.title, '') AS album_title, \
         COALESCE(a.album_artist_name, '') AS artist_name, r.created_at, r.updated_at, \
         EXISTS (SELECT 1 FROM local_album_external_identities e \
                 WHERE e.local_album_id = r.local_album_id AND e.decision_source IN " + PERSONS + ") AS manual_identity, \
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
         AND (:created_from IS NULL OR r.created_at >= :created_from) \
         AND (:created_to IS NULL OR r.created_at <= :created_to) \
         AND (:updated_from IS NULL OR r.updated_at >= :updated_from) \
         AND (:updated_to IS NULL OR r.updated_at <= :updated_to) \
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
    pub(super) row TrackTitle for [WITH_TRACKS] {
        id: String,
        title: String,
    }
    pub(super) row ReopenGuard for [REOPEN_GUARD] {
        local_album_id: String,
        state: ReviewState,
        row_revision: i64,
        resolved_by_user_id: Option<String>,
        reopen_review_revision: Option<i64>,
        reopen_identity_revision: Option<i64>,
        identity_revision: i64,
        other_pending: bool,
    }
    pub(super) row ReviewGuard for [REVIEW_GUARD_ROW] {
        local_album_id: String,
        state: ReviewState,
        row_revision: i64,
    }
    pub(super) row RequesterRow for [REQUESTER] {
        role: String,
        username_display: Option<String>,
    }
    pub(super) row ReviewCount for [REVIEW_COUNT_BY_REASON, REVIEW_COUNT_BY_STATE] {
        bucket: String,
        total: i64,
    }
    pub(super) row ReviewById for [REVIEW_BY_ID] {
        id: String,
        local_album_id: String,
        reason_code: String,
        candidates_json: String,
        state: ReviewState,
        resolved_by_user_id: Option<String>,
        selected_candidate_key: Option<String>,
        album_title: String,
        artist_name: String,
        created_at: i64,
        updated_at: i64,
        manual_identity: bool,
        row_revision: i64,
    }
    pub(super) row ReviewPageRow for family [review_page] {
        id: String,
        local_album_id: String,
        reason_code: String,
        candidates_json: String,
        state: ReviewState,
        resolved_by_user_id: Option<String>,
        selected_candidate_key: Option<String>,
        album_title: String,
        artist_name: String,
        created_at: i64,
        updated_at: i64,
        manual_identity: bool,
        row_revision: i64,
    }
}

pub(super) fn review_page(order: ReviewOrder) -> String {
    let order = match order {
        ReviewOrder::Newest => "r.updated_at DESC",
        ReviewOrder::Oldest => "r.updated_at ASC",
        ReviewOrder::MostTracks => {
            "(SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = r.local_album_id) DESC, \
             r.updated_at DESC"
        }
        ReviewOrder::Title => "a.title COLLATE NOCASE ASC, r.updated_at DESC",
        ReviewOrder::Artist => {
            "COALESCE(a.album_artist_name, '') COLLATE NOCASE ASC, a.title COLLATE NOCASE ASC"
        }
        ReviewOrder::Root => {
            "(SELECT MIN(t.root_id) FROM local_tracks t WHERE t.local_album_id = r.local_album_id), \
             a.title COLLATE NOCASE ASC"
        }
        ReviewOrder::Reason => "r.reason_code ASC, r.updated_at DESC",
    };
    format!(
        "SELECT {REVIEW_COLUMNS} {REVIEW_LISTING} ORDER BY {order}, r.id LIMIT :limit OFFSET :offset"
    )
}
