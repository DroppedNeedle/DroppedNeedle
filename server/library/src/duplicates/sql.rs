//! The duplicate album list's statement.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use crate::identity::DecisionSource;

droppedneedle_kernel::sql! {
    /// Albums the library holds more than once, one row per copy: every
    /// release group two or more albums own (`all_owners!`, the one
    /// ownership rule), with those albums. A copy's release counts only
    /// under the group its row names. Only the asked groups' albums have
    /// their tracks read. Binds the last group id of the previous page
    /// (`?1`, none on the first) and the groups to read (`?2`).
    pub(super) DUPLICATE_ALBUMS = "WITH members(album_id, group_mbid) AS ("
        + (crate::all_owners!())
        + "), listed AS MATERIALIZED ( \
           SELECT DISTINCT m.album_id AS album_id, m.group_mbid AS group_mbid \
           FROM members m \
           WHERE m.group_mbid IN (SELECT group_mbid FROM members \
           GROUP BY group_mbid HAVING COUNT(DISTINCT album_id) > 1)), \
           asked AS ( \
           SELECT DISTINCT group_mbid FROM listed WHERE (?1 IS NULL OR +group_mbid > ?1) \
           ORDER BY group_mbid LIMIT ?2), \
           copies AS ( \
           SELECT l.album_id AS album_id, l.group_mbid AS group_mbid FROM listed l \
           WHERE l.group_mbid IN (SELECT group_mbid FROM asked)), \
           album_stats AS ( \
           SELECT t.local_album_id AS album_id, COUNT(*) AS track_count, \
           COALESCE(SUM(t.file_size_bytes), 0) AS total_size_bytes, \
           GROUP_CONCAT(DISTINCT t.file_format) AS formats \
           FROM local_tracks t \
           WHERE t.local_album_id IN (SELECT album_id FROM copies) \
           AND t.availability = 'indexed' \
           GROUP BY t.local_album_id), \
           stored AS ( \
           SELECT ae.local_album_id AS album_id, lower(ae.release_mbid) AS release_mbid, \
           lower(ae.release_group_mbid) AS release_group_mbid, \
           ae.decision_source AS decision_source \
           FROM local_album_external_identities ae \
           WHERE ae.provider = 'musicbrainz' \
           AND ae.local_album_id IN (SELECT album_id FROM copies)) \
           SELECT c.group_mbid AS group_mbid, \
           a.id AS album_id, a.title AS title, \
           COALESCE(an.display_name, a.album_artist_name, '') AS artist_name, \
           s.track_count AS track_count, s.total_size_bytes AS total_size_bytes, \
           s.formats AS formats, \
           EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id \
           AND w.source IN ('folder', 'embedded')) AS cover_available, \
           CASE WHEN r.release_group_mbid = c.group_mbid THEN r.release_mbid END \
           AS release_mbid, \
           CASE WHEN r.release_group_mbid = c.group_mbid THEN r.decision_source END \
           AS decision_source, \
           COALESCE(r.release_group_mbid = c.group_mbid, 0) AS stored_match, "
        + (crate::named_group_guess!("a.id"))
        + " AS doubted \
           FROM copies c \
           JOIN local_albums a ON a.id = c.album_id \
           LEFT JOIN local_artists an ON an.id = a.album_artist_id \
           JOIN album_stats s ON s.album_id = a.id \
           LEFT JOIN stored r ON r.album_id = a.id \
           ORDER BY c.group_mbid, a.created_at, a.id";
    /// One copy of one duplicate group.
    pub(super) row DuplicateAlbumRow for [DUPLICATE_ALBUMS] {
        group_mbid: String,
        album_id: String,
        title: String,
        artist_name: String,
        track_count: u64,
        total_size_bytes: u64,
        formats: Option<String>,
        cover_available: bool,
        release_mbid: Option<String>,
        decision_source: Option<DecisionSource>,
        stored_match: bool,
        doubted: bool,
    }
}
