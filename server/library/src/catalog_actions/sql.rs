//! The SQL of `library_catalog_actions`: every shape of its audit row and
//! the history read.

use droppedneedle_kernel::db::statement::ListLen;

use super::history::MAX_TITLES;

droppedneedle_kernel::sql! {
    pub(crate) RECORD_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         operation_job_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    pub(super) RECORD_ARTIST_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_artist_id, \
         before_json, after_json, reason_code, created_at) \
         VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8)";
    pub(super) ALBUM_TITLE = "SELECT title FROM local_albums WHERE id = ?1";
    pub(super) FOLD_UNDONE = "SELECT actor_user_id, created_at FROM library_catalog_actions \
         WHERE local_album_id = ?1 AND action_kind = ?2 \
         AND json_extract(after_json, '$.fold_action_id') = ?3 AND created_at >= ?4 \
         ORDER BY created_at DESC LIMIT 1";
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
    /// The first titles of a list of tracks, by disc and track number.
    family TITLES_OF_TRACKS = titles_of_tracks;
    pub(crate) RECORD_TRACK_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         local_track_id, before_json, after_json, reason_code, created_at) VALUES \
         (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
    pub(crate) RECORD_CORRECTION = "INSERT INTO library_catalog_actions (id, idempotency_key, actor_user_id, action_kind, \
         local_artist_id, local_album_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, ?2, (SELECT id FROM auth_users WHERE id = ?3), ?4, ?5, ?6, ?7, ?8, ?9, ?10)";
    /// One catalog action. The actor (`?2`) stays NULL when it is no
    /// account.
    pub(crate) RECORD_REMOVAL_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, \
             action_kind, local_album_id, local_track_id, before_json, after_json, reason_code, \
             created_at) VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, \
             ?7, ?8, ?9)";
    pub(super) row FoldUndo for [FOLD_UNDONE] {
        actor_user_id: Option<String>,
        created_at: i64,
    }
    pub(super) row HistoryRow for [ALBUM_HISTORY_ROWS] {
        id: String,
        actor_user_id: Option<String>,
        action_kind: String,
        reason_code: Option<String>,
        before_json: String,
        after_json: String,
        created_at: i64,
    }
}

pub(super) fn titles_of_tracks(tracks: ListLen) -> String {
    let marks = crate::sql::marks(tracks);
    format!(
        "SELECT title FROM local_tracks WHERE id IN ({marks}) \
         ORDER BY disc_number, track_number, title LIMIT {MAX_TITLES}"
    )
}
