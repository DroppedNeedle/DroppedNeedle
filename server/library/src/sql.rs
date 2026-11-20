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
    /// Whether any scan is active, queued, paused, or still owing end passes.
    pub(super) SCAN_ACTIVE_OR_FINISHING = "SELECT EXISTS(SELECT 1 FROM library_scan_runs \
                     WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
                     OR next_end_pass IS NOT NULL)";
    /// Up to `?2` indexed track ids belonging to removed root `?1`, in id order.
    /// Unary `+` prevents SQLite from walking `idx_local_tracks_recent` on
    /// `availability` instead of seeking `(root_id, relative_path)`.
    pub(super) REMOVED_ROOT_INDEXED_TRACK_IDS = "SELECT id FROM local_tracks \
                     WHERE root_id = ?1 AND +availability = 'indexed' ORDER BY id LIMIT ?2";
    /// Mark one track (`?1`) missing at `?2`, keeping plays, ratings, favorites,
    /// and playlists intact while firing every read-model trigger.
    pub(super) MARK_REMOVED_ROOT_TRACK_MISSING = "UPDATE local_tracks \
                     SET availability = 'missing', missing_at = ?2, row_revision = row_revision + 1 \
                     WHERE id = ?1 AND availability = 'indexed'";
    /// Drop residual mass-missing holds for removed root `?1`.
    pub(super) DROP_ROOT_HELD_SCOPES = "DELETE FROM library_scan_held_scopes WHERE root_id = ?1";
    /// Drop unjudged-track marks for non-indexed tracks of removed root `?1`.
    pub(super) DROP_ROOT_UNJUDGED_NON_INDEXED = "DELETE FROM library_scan_unjudged_tracks \
                     WHERE local_track_id IN (SELECT id FROM local_tracks \
                       WHERE root_id = ?1 AND availability <> 'indexed')";
    /// Drop remembered sub-mounts for removed root `?1`.
    pub(super) DROP_ROOT_MOUNTS = "DELETE FROM library_scan_mounts WHERE root_id = ?1";
    /// Drop cached folder digests for removed root `?1`.
    pub(super) DROP_ROOT_FOLDER_DIGESTS = "DELETE FROM library_folder_digests WHERE root_id = ?1";
}

/// `?,?,?` for an `IN` list of `len` values.
pub(crate) fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(",")
}
