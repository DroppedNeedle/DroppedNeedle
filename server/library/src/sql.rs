//! The SQL of the crate root files, and the one list of placeholders the
//! crate's family builders share.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;

droppedneedle_kernel::sql! {
    pub(super) RELEASE_GROUP_OF_ALBUM = "SELECT release_group_mbid FROM local_album_external_identities \
                 WHERE local_album_id = ?1 AND release_group_mbid IS NOT NULL LIMIT 1";
    pub(super) INDEXED_TRACK = "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed' \
                     AND (?2 IS NULL OR local_album_id = ?2)";
    pub(super) ALBUM_REVISIONS = "SELECT row_revision, listing_revision FROM local_albums \
                     WHERE id = ?1";
    pub(super) row AlbumRevisions for [ALBUM_REVISIONS] {
        row_revision: i64,
        listing_revision: i64,
    }
    pub(super) TRACK_ROW_REVISION = "SELECT row_revision FROM local_tracks WHERE id = ?1";
    pub(super) LIFT_EXCLUSION = "DELETE FROM library_management_exclusions \
                     WHERE local_album_id = ?1 AND row_revision = ?2 \
                     RETURNING reason, excluded_by_user_id, excluded_at, row_revision";
    pub(super) row LiftedExclusion for [LIFT_EXCLUSION] {
        reason: String,
        excluded_by_user_id: Option<String>,
        excluded_at: Millis,
        row_revision: i64,
    }
    pub(super) EXCLUSION_REVISION = "SELECT row_revision FROM library_management_exclusions \
                         WHERE local_album_id = ?1";
    pub(super) ALBUM_RETIRED_INTO = "SELECT retired_into_album_id FROM local_albums WHERE id = ?1";
    /// The album a release group (`?1`, any case) opens, as the pages do.
    pub(super) LIVE_ALBUM_OF_RELEASE_GROUP = (crate::copy_of_group!("= lower(?1)"));
    pub(super) ALBUM_TRACKS = "SELECT id, local_album_id, root_id, relative_path FROM local_tracks \
                 WHERE local_album_id = ?1 AND availability = 'indexed' \
                 ORDER BY disc_number, track_number, id";
    /// One indexed track's file, as `INDEXED_TRACK` and `ALBUM_TRACKS` read it.
    pub(super) row IndexedTrackFile for [INDEXED_TRACK, ALBUM_TRACKS] {
        id: String,
        local_album_id: String,
        root_id: String,
        relative_path: String,
    }
}

/// `?,?,?` for an `IN` list of `len` values.
pub(crate) fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(",")
}
