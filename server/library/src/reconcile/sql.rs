//! The SQL of the reconcile module.

use crate::identify::sql::CURRENT_PROOF;

droppedneedle_kernel::sql! {
    pub(super) INPUTS = "SELECT left_artist_id, right_artist_id, left_artist_revision, \
             right_artist_revision FROM library_artist_reconciliation_dismissals";
    pub(super) SHARED_GROUPS = "SELECT pair.left_artist_id, pair.right_artist_id \
             FROM local_artist_merge_candidates pair \
             JOIN local_artists left_artist ON left_artist.id = pair.left_artist_id \
              AND left_artist.retired_into_artist_id IS NULL \
             JOIN local_artists right_artist ON right_artist.id = pair.right_artist_id \
              AND right_artist.retired_into_artist_id IS NULL \
             WHERE pair.reason_code = ?1 AND pair.state = 'open' ORDER BY pair.id";
    pub(super) LIVE_ARTIST_REVISION = "SELECT row_revision FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL";
    pub(super) WRITE_DISMISSAL = "INSERT INTO library_artist_reconciliation_dismissals \
         (left_artist_id, right_artist_id, left_artist_revision, right_artist_revision, \
          dismissed_by_user_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6) \
         ON CONFLICT(left_artist_id, right_artist_id) DO UPDATE SET \
           left_artist_revision = excluded.left_artist_revision, \
           right_artist_revision = excluded.right_artist_revision, \
           dismissed_by_user_id = excluded.dismissed_by_user_id, \
           updated_at = excluded.updated_at, \
           row_revision = library_artist_reconciliation_dismissals.row_revision + 1";
    /// `album` still has an indexed file and was not merged away: a display
    /// filter, so credits on albums with no file left keep no group open.
    pub(super) fragment ACTIVE_ALBUM = "album.retired_into_album_id IS NULL AND EXISTS (SELECT 1 \
     FROM local_tracks live WHERE live.local_album_id = album.id AND live.availability = 'indexed')";
    /// The join that gives [`candidate_columns`] its `identity`.
    pub(super) fragment IDENTITY_JOIN = "LEFT JOIN local_artist_external_identities identity \
     ON identity.local_artist_id = artist.id AND identity.provider = 'musicbrainz'";
    pub(super) ARTISTS_OF_WAITING_ALBUMS = "SELECT DISTINCT album.album_artist_id FROM library_artist_reconciliation_state state \
             JOIN local_albums album ON album.id = state.local_album_id \
             WHERE state.state = 'ambiguous_credit_structure' AND " + ACTIVE_ALBUM;
    pub(super) PROVEN_MBIDS_OF_ARTIST = "SELECT DISTINCT p.artist_mbid FROM library_artist_credit_proofs p \
         JOIN local_albums album ON album.id = p.local_album_id \
         WHERE p.source_local_artist_id = ?1 AND " + ACTIVE_ALBUM + " AND (" + CURRENT_PROOF + ") \
         ORDER BY p.artist_mbid";
    pub(super) ARTIST_CREDIT_COUNTS = "SELECT \
           (SELECT COUNT(*) FROM local_album_artists credit \
              JOIN local_albums album ON album.id = credit.local_album_id \
              WHERE credit.local_artist_id = ?1 AND " + ACTIVE_ALBUM + " \
              AND EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
               WHERE p.subject_kind = 'album' AND p.subject_id = credit.local_album_id \
               AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
               AND (" + CURRENT_PROOF + "))) + \
           (SELECT COUNT(*) FROM local_track_artists credit \
              JOIN local_tracks track ON track.id = credit.local_track_id \
              JOIN local_albums album ON album.id = track.local_album_id \
              WHERE credit.local_artist_id = ?1 \
                AND album.retired_into_album_id IS NULL AND +track.availability = 'indexed' \
              AND EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
               WHERE p.subject_kind = 'track' AND p.subject_id = credit.local_track_id \
               AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
               AND (" + CURRENT_PROOF + ")))";
}
