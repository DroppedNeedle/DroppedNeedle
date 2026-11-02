//! The SQL of the reconcile module.

use droppedneedle_kernel::db::statement::ListLen;

use crate::corrections::artist_convergence::{MERGED_BY_NAME, MOVED_BY_NAME, MOVED_WITH_ID};
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
    /// The join that gives the candidate columns their `identity`.
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
    /// Catalog action reasons that record an automatic artist merge: v2's two, and the merge of a row a retag emptied by name.
    pub(super) fragment MERGE_REASONS = "('AUTOMATIC_PROVIDER_PROVEN_ARTIST_CONVERGENCE', \
         'AUTOMATIC_PROVIDER_ANCHORED_ARTIST_CONVERGENCE', '" + MERGED_BY_NAME + "')";
    /// The open moves of `artist.id`'s credits a scan recorded.
    pub(super) fragment OPEN_MOVES = "SELECT 1 FROM local_artist_merge_candidates moved WHERE moved.left_artist_id = " + "artist.id" + " \
         AND moved.reason_code IN ('" + MOVED_BY_NAME + "', '" + MOVED_WITH_ID + "') AND moved.state = 'open'";
    /// True when live artist `artist.id` has no credit at all and holds something a person made, with an open recorded move of its credits.
    pub(super) fragment EMPTIED_WITH_HISTORY = "(EXISTS (" + OPEN_MOVES + ") \
         AND NOT EXISTS (SELECT 1 FROM local_album_artists c WHERE c.local_artist_id = " + "artist.id" + ") \
         AND NOT EXISTS (SELECT 1 FROM local_track_artists c WHERE c.local_artist_id = " + "artist.id" + ") \
         AND NOT EXISTS (SELECT 1 FROM local_albums a WHERE a.album_artist_id = " + "artist.id" + ") \
         AND (EXISTS (SELECT 1 FROM library_user_favorites f \
               WHERE f.item_kind = 'artist' AND f.item_id = " + "artist.id" + ") \
          OR EXISTS (SELECT 1 FROM library_user_ratings r \
               WHERE r.item_kind = 'artist' AND r.item_id = " + "artist.id" + ") \
          OR EXISTS (SELECT 1 FROM library_play_history h WHERE h.local_artist_id = " + "artist.id" + ")))";
    /// True when an open move took a credit of `artist.id` to a row of another folded name.
    pub(super) fragment MOVED_TO_OTHER_NAME = "EXISTS (" + OPEN_MOVES + " AND EXISTS (SELECT 1 FROM local_artists dest \
         WHERE dest.id = moved.right_artist_id AND dest.folded_name IS NOT " + "artist.folded_name" + "))";
    /// The columns a candidate row reads for artist `artist`, joined to its MusicBrainz link as `identity`.
    pub(super) fragment CANDIDATE_COLUMNS = "artist.id, artist.display_name, artist.sort_name, artist.row_revision, \
           identity.provider_artist_id, artist.folded_name, artist.created_at, \
           (SELECT COUNT(*) FROM local_album_artists credit \
              JOIN local_albums album ON album.id = credit.local_album_id \
              WHERE credit.local_artist_id = artist.id AND " + ACTIVE_ALBUM + "), \
           (SELECT COUNT(*) FROM local_track_artists credit \
              JOIN local_tracks track ON track.id = credit.local_track_id \
              JOIN local_albums album ON album.id = track.local_album_id \
              WHERE credit.local_artist_id = artist.id \
                AND album.retired_into_album_id IS NULL AND +track.availability = 'indexed'), \
           (SELECT COUNT(*) FROM local_albums album \
              WHERE album.album_artist_id = artist.id AND " + ACTIVE_ALBUM + "), \
           (SELECT COUNT(*) FROM library_user_favorites \
              WHERE item_kind = 'artist' AND item_id = artist.id), \
           (SELECT COUNT(*) FROM library_play_history WHERE local_artist_id = artist.id), \
           (SELECT COUNT(*) FROM library_user_ratings \
              WHERE item_kind = 'artist' AND item_id = artist.id)";
    /// Live records that share a folded name with another active one, or that a scan emptied with every credit moved to a row of the same name.
    pub(super) SAME_NAME_CANDIDATES = "WITH active_artists AS ( \
           SELECT credit.local_artist_id id FROM local_album_artists credit \
           JOIN local_albums album ON album.id = credit.local_album_id WHERE " + ACTIVE_ALBUM + " \
           UNION SELECT credit.local_artist_id FROM local_track_artists credit \
           JOIN local_tracks track ON track.id = credit.local_track_id \
           JOIN local_albums album ON album.id = track.local_album_id \
           WHERE album.retired_into_album_id IS NULL AND +track.availability = 'indexed' \
           UNION SELECT artist.id FROM local_artists artist \
           WHERE " + EMPTIED_WITH_HISTORY + " AND NOT " + MOVED_TO_OTHER_NAME + "), \
         current_artists AS ( \
           SELECT artist.* FROM local_artists artist JOIN active_artists a ON a.id = artist.id \
           WHERE artist.retired_into_artist_id IS NULL), \
         duplicate_names AS ( \
           SELECT folded_name FROM current_artists GROUP BY folded_name HAVING COUNT(*) > 1) \
         SELECT " + CANDIDATE_COLUMNS + " FROM current_artists artist " + IDENTITY_JOIN + " \
         WHERE artist.folded_name IN (SELECT folded_name FROM duplicate_names) \
         ORDER BY artist.folded_name, artist.created_at, artist.id";
    /// Each live record a scan emptied whose credits went to a row of another folded name, with the rows they went to.
    pub(super) EMPTIED_MOVED_PAIRS = "SELECT DISTINCT pair.left_artist_id, pair.right_artist_id, dest.created_at \
             FROM local_artist_merge_candidates pair \
             JOIN local_artists artist ON artist.id = pair.left_artist_id \
              AND artist.retired_into_artist_id IS NULL \
             JOIN local_artists dest ON dest.id = pair.right_artist_id \
              AND dest.retired_into_artist_id IS NULL \
             WHERE pair.reason_code IN (?1, ?2) AND pair.state = 'open' \
             AND " + EMPTIED_WITH_HISTORY + " AND " + MOVED_TO_OTHER_NAME + " \
             ORDER BY pair.left_artist_id, dest.created_at, pair.right_artist_id";
    pub(super) CANDIDATE_BY_ID = "SELECT " + CANDIDATE_COLUMNS + " FROM local_artists artist " + IDENTITY_JOIN + " WHERE artist.id = ?1";
    /// Automatic merges from the catalog action log, newest first.
    pub(super) AUTOMATIC_MERGE_ACTIONS = "SELECT id, after_json, reason_code, created_at FROM library_catalog_actions \
             WHERE reason_code IN " + MERGE_REASONS + " ORDER BY created_at DESC, id DESC";
    family MERGED_ARTIST_ROWS = merged_artist_rows;
    /// How many records were merged automatically so far.
    pub(super) AUTOMATIC_MERGE_COUNT = "SELECT COALESCE(SUM(json_array_length(json_extract(after_json, \
             '$.retired_artist_ids'))), 0) FROM library_catalog_actions \
             WHERE reason_code IN " + MERGE_REASONS + " AND json_valid(after_json)";
    family GROUP_CREDIT_EVIDENCE = group_credit_evidence;
    family GROUP_OWNED_ALBUMS = group_owned_albums;
    family GROUP_OWNED_TRACKS = group_owned_tracks;
    family DISMISS_GROUP_CANDIDATES = dismiss_group_candidates;
}

pub(super) fn merged_artist_rows(ids: ListLen) -> String {
    let marks = crate::sql::marks(ids);
    format!(
        "SELECT artist.id, artist.display_name, artist.sort_name, artist.row_revision, \
           identity.provider_artist_id FROM local_artists artist \
         LEFT JOIN local_artist_external_identities identity \
           ON identity.local_artist_id = artist.id AND identity.provider = 'musicbrainz' \
         WHERE artist.id IN ({marks})"
    )
}

pub(super) fn group_credit_evidence(ids: ListLen) -> String {
    let marks = crate::sql::marks(ids);
    format!(
        "SELECT p.subject_kind, p.subject_id, \
               COALESCE(subject_album.title, subject_track.title, p.subject_id), \
               p.source_local_artist_id, p.local_artist_id, p.artist_mbid, \
               p.canonical_name, p.credited_name, p.join_phrase, p.release_mbid, \
               p.release_track_mbid, p.album_identity_revision, \
               p.track_identity_revision, p.evidence_hash \
             FROM library_artist_credit_proofs p \
             JOIN local_albums album ON album.id = p.local_album_id \
             LEFT JOIN local_albums subject_album ON p.subject_kind = 'album' \
              AND subject_album.id = p.subject_id \
             LEFT JOIN local_tracks subject_track ON p.subject_kind = 'track' \
              AND subject_track.id = p.subject_id \
             WHERE (p.source_local_artist_id IN ({marks}) OR p.local_artist_id IN ({marks})) \
               AND {ACTIVE_ALBUM} AND ({CURRENT_PROOF}) \
             ORDER BY p.local_album_id, p.subject_kind, p.subject_id, \
               p.credit_position LIMIT 500"
    )
}

pub(super) fn group_owned_albums(ids: ListLen) -> String {
    let marks = crate::sql::marks(ids);
    format!(
        "SELECT DISTINCT album.id, album.title, album.row_revision, \
               album_identity.release_mbid IS NOT NULL, \
               NOT EXISTS (SELECT 1 FROM local_tracks child \
                 LEFT JOIN local_track_external_identities track_identity \
                   ON track_identity.local_track_id = child.id \
                  AND track_identity.provider = 'musicbrainz' \
                 WHERE child.local_album_id = album.id AND +child.availability = 'indexed' \
                   AND (track_identity.release_track_mbid IS NULL \
                    OR track_identity.release_mbid IS NOT album_identity.release_mbid)), \
               album.title_folded \
             FROM local_albums album \
             LEFT JOIN local_album_external_identities album_identity \
               ON album_identity.local_album_id = album.id AND album_identity.provider = 'musicbrainz' \
             WHERE {ACTIVE_ALBUM} AND (album.album_artist_id IN ({marks}) OR EXISTS ( \
               SELECT 1 FROM local_album_artists credit WHERE credit.local_album_id = album.id \
                 AND credit.local_artist_id IN ({marks}))) \
             ORDER BY album.title_folded, album.id LIMIT 250"
    )
}

pub(super) fn group_owned_tracks(ids: ListLen) -> String {
    let marks = crate::sql::marks(ids);
    format!(
        "SELECT DISTINCT track.id, track.title, track.row_revision, \
               album_identity.release_mbid IS NOT NULL, \
               track_identity.release_track_mbid IS NOT NULL \
                 AND track_identity.release_mbid IS album_identity.release_mbid, \
               track.title_folded \
             FROM local_tracks track \
             JOIN local_albums album ON album.id = track.local_album_id \
             LEFT JOIN local_album_external_identities album_identity \
               ON album_identity.local_album_id = album.id AND album_identity.provider = 'musicbrainz' \
             LEFT JOIN local_track_external_identities track_identity \
               ON track_identity.local_track_id = track.id AND track_identity.provider = 'musicbrainz' \
             WHERE album.retired_into_album_id IS NULL AND +track.availability = 'indexed' \
               AND track.id IN (SELECT credit.local_track_id FROM local_track_artists credit \
                 WHERE credit.local_artist_id IN ({marks})) \
             ORDER BY track.title_folded, track.id LIMIT 500"
    )
}

pub(super) fn dismiss_group_candidates(ids: ListLen) -> String {
    let marks = crate::sql::marks(ids);
    format!(
        "UPDATE local_artist_merge_candidates SET state = 'dismissed', updated_at = ?, \
             row_revision = row_revision + 1 WHERE state = 'open' \
             AND left_artist_id IN ({marks}) AND right_artist_id IN ({marks})"
    )
}
