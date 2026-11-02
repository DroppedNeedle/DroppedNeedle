//! The SQL of the crate root files, and the one list of placeholders the
//! crate's family builders share.

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    pub(super) RELEASE_GROUP_OF_ALBUM = "SELECT release_group_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND release_group_mbid IS NOT NULL LIMIT 1";
    pub(super) INDEXED_TRACK = "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed' \
                     AND (?2 IS NULL OR local_album_id = ?2)";
    pub(super) ALBUM_ROW_REVISION = "SELECT row_revision FROM local_albums WHERE id = ?1";
    pub(super) ALBUM_INDEXED_TRACKS = "SELECT COUNT(*) FROM local_tracks \
                     WHERE local_album_id = ?1 AND availability = 'indexed'";
    pub(super) TRACK_ROW_REVISION = "SELECT row_revision FROM local_tracks WHERE id = ?1";
    pub(super) LIFT_EXCLUSION = "DELETE FROM library_management_exclusions \
                     WHERE local_album_id = ?1 AND row_revision = ?2 \
                     RETURNING reason, excluded_by_user_id, excluded_at, row_revision";
    pub(super) EXCLUSION_REVISION = "SELECT row_revision FROM library_management_exclusions \
                         WHERE local_album_id = ?1";
    pub(super) ALBUM_RETIRED_INTO = "SELECT retired_into_album_id FROM local_albums WHERE id = ?1";
    pub(super) LIVE_ALBUM_OF_RELEASE_GROUP = "SELECT b.id FROM local_album_external_identities e \
                 JOIN local_albums b ON b.id = e.local_album_id \
                 WHERE lower(e.release_group_mbid) = lower(?1) \
                 AND b.retired_into_album_id IS NULL ORDER BY b.created_at LIMIT 1";
    pub(super) ALBUM_TRACKS = "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed' \
                 ORDER BY disc_number, track_number, id";
    pub(super) RECORD_ACTION = "INSERT INTO library_catalog_actions (id, actor_user_id, action_kind, local_album_id, \
         local_track_id, before_json, after_json, reason_code, created_at) VALUES \
         (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, ?7, ?8, ?9)";
}

/// `?,?,?` for an `IN` list of `len` values.
pub(crate) fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(",")
}
