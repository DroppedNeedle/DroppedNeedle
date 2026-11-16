//! The duplicate album list's statement.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use crate::identity::DecisionSource;

droppedneedle_kernel::sql! {
    /// Albums the library lists twice, one row per copy. A group is a
    /// release group every copy owns, by its match row or by the group its
    /// files' tags name; it is listed when two copies name one release, when
    /// it holds more than one release, or when a copy has fewer indexed
    /// tracks than another. Binds the last group id of the previous page
    /// (`?1`, none on the first) and the groups to read (`?2`).
    pub(super) DUPLICATE_ALBUMS = "WITH members(album_id, group_mbid) AS ("
        + (crate::all_owners!())
        + " UNION SELECT ae.local_album_id, lower(ae.release_group_mbid) \
           FROM local_album_external_identities ae \
           JOIN local_albums a ON a.id = ae.local_album_id \
           WHERE ae.provider = 'musicbrainz' AND "
        + (crate::owned_album!("a"))
        + "), album_stats AS ( \
           SELECT t.local_album_id AS album_id, COUNT(*) AS track_count, \
           COALESCE(SUM(t.file_size_bytes), 0) AS total_size_bytes, \
           GROUP_CONCAT(DISTINCT t.file_format) AS formats \
           FROM local_tracks t JOIN local_albums st ON st.id = t.local_album_id \
           WHERE +t.availability = 'indexed' AND "
        + (crate::owned_album!("st"))
        + " GROUP BY t.local_album_id), stored AS ( \
           SELECT ae.local_album_id AS album_id, lower(ae.release_mbid) AS release_mbid, \
           lower(ae.release_group_mbid) AS release_group_mbid, \
           ae.decision_source AS decision_source \
           FROM local_album_external_identities ae WHERE ae.provider = 'musicbrainz'), \
           release_copies AS ( \
           SELECT m.group_mbid AS group_mbid, r.release_mbid AS release_mbid, \
           COUNT(*) AS copies \
           FROM members m JOIN stored r ON r.album_id = m.album_id \
           WHERE r.release_mbid IS NOT NULL GROUP BY m.group_mbid, r.release_mbid), \
           group_flags AS ( \
           SELECT rc.group_mbid AS group_mbid, COUNT(*) AS release_count, \
           MAX(rc.copies) AS most_copies, \
           (SELECT rc2.release_mbid FROM release_copies rc2 \
            WHERE rc2.group_mbid = rc.group_mbid AND rc2.copies > 1 \
            ORDER BY rc2.copies DESC, rc2.release_mbid LIMIT 1) AS shared_release \
           FROM release_copies rc GROUP BY rc.group_mbid), \
           grouped AS ( \
           SELECT m.group_mbid AS group_mbid, \
           MIN(s.track_count) AS min_tracks, MAX(s.track_count) AS max_tracks, \
           COALESCE(MAX(gf.release_count), 0) AS release_count, \
           COALESCE(MAX(gf.most_copies), 0) AS most_copies, \
           MAX(gf.shared_release) AS shared_release \
           FROM members m JOIN album_stats s ON s.album_id = m.album_id \
           LEFT JOIN group_flags gf ON gf.group_mbid = m.group_mbid \
           GROUP BY m.group_mbid), \
           duplicate AS ( \
           SELECT group_mbid, shared_release FROM grouped \
           WHERE most_copies > 1 OR release_count > 1 OR min_tracks < max_tracks), \
           asked AS ( \
           SELECT group_mbid FROM duplicate WHERE (?1 IS NULL OR +group_mbid > ?1) \
           ORDER BY group_mbid LIMIT ?2) \
           SELECT d.group_mbid AS group_mbid, d.shared_release AS shared_release, \
           a.id AS album_id, a.title AS title, \
           COALESCE(an.display_name, a.album_artist_name, '') AS artist_name, \
           s.track_count AS track_count, s.total_size_bytes AS total_size_bytes, \
           s.formats AS formats, r.release_mbid AS release_mbid, \
           r.decision_source AS decision_source, \
           COALESCE(r.release_group_mbid = d.group_mbid, 0) AS stored_match, "
        + (crate::named_group_guess!("a.id"))
        + " AS doubted \
           FROM duplicate d JOIN asked k ON k.group_mbid = d.group_mbid \
           JOIN members m ON m.group_mbid = d.group_mbid \
           JOIN local_albums a ON a.id = m.album_id \
           LEFT JOIN local_artists an ON an.id = a.album_artist_id \
           JOIN album_stats s ON s.album_id = a.id \
           LEFT JOIN stored r ON r.album_id = a.id \
           ORDER BY d.group_mbid, a.created_at, a.id";
    /// One copy of one duplicate group, its group's columns repeated.
    pub(super) row DuplicateAlbumRow for [DUPLICATE_ALBUMS] {
        group_mbid: String,
        shared_release: Option<String>,
        album_id: String,
        title: String,
        artist_name: String,
        track_count: u64,
        total_size_bytes: u64,
        formats: Option<String>,
        release_mbid: Option<String>,
        decision_source: Option<DecisionSource>,
        stored_match: bool,
        doubted: bool,
    }
}
