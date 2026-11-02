//! Local album art lookups.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

/// The tracks that can place art row `w` in its library folder, as a JSON
/// array of `[file_path, relative_path]` pairs: the `embedded` alias, the
/// track holding the picture, for embedded art, else every indexed track
/// of the album.
macro_rules! art_tracks {
    ($embedded:literal) => {
        concat!(
            "CASE WHEN w.source = 'embedded' THEN json_array(json_array(",
            $embedded,
            ".file_path, ",
            $embedded,
            ".relative_path)) ELSE (SELECT json_group_array(\
             json_array(art_track.file_path, art_track.relative_path)) \
             FROM local_tracks art_track WHERE art_track.local_album_id = w.local_album_id \
             AND art_track.availability = 'indexed') END"
        )
    };
}

droppedneedle_kernel::sql! {
    /// One art row over a live album, with the file behind it.
    fragment ART_SELECT = "SELECT w.local_album_id AS album_id, w.source AS source, \
         CASE WHEN w.source = 'embedded' THEN t.file_path ELSE w.source_locator END AS path, "
        + (art_tracks!("t"))
        + " AS tracks, w.content_hash AS content_hash, w.version AS version \
         FROM local_album_artwork w \
         JOIN local_albums a ON a.id = w.local_album_id AND a.retired_into_album_id IS NULL \
         LEFT JOIN local_tracks t ON t.id = w.source_locator AND w.source = 'embedded' \
         AND t.availability = 'indexed'";
    /// The release group a live album is identified as.
    pub(super) ALBUM_GROUP = "SELECT ae.release_group_mbid FROM local_albums a \
         LEFT JOIN local_album_external_identities ae ON ae.local_album_id = a.id \
         WHERE a.id = ?1 AND a.retired_into_album_id IS NULL";
    /// One album's art.
    pub(super) ART_OF_ALBUM = ART_SELECT + " WHERE w.local_album_id = ?1";
    /// The art of the albums identified as one release group.
    pub(super) ART_OF_GROUP = ART_SELECT
        + " JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
         WHERE ae.release_group_mbid = ?1 ORDER BY w.local_album_id";
    /// The art of the albums identified as one exact release.
    pub(super) ART_OF_RELEASE = ART_SELECT
        + " JOIN local_album_external_identities ae ON ae.local_album_id = w.local_album_id \
         WHERE ae.release_mbid = ?1 ORDER BY w.local_album_id";
    /// The art of a library artist's albums: albums under the artist's name
    /// before ones that only credit them, studio albums before compilations,
    /// then the newest release, then the latest added.
    pub(super) ART_OF_ARTIST = ART_SELECT
        + " WHERE w.local_album_id IN (SELECT id FROM local_albums WHERE album_artist_id = ?1 \
         UNION SELECT local_album_id FROM local_album_artists WHERE local_artist_id = ?1) \
         AND a.has_indexed_track = 1 AND w.source IN ('folder', 'embedded') \
         ORDER BY a.album_artist_id = ?1 DESC, a.is_compilation, \
         a.year IS NULL, a.year DESC, a.created_at DESC, a.id LIMIT 40";
    pub(super) row ArtColumns for [ART_OF_ALBUM, ART_OF_GROUP, ART_OF_RELEASE, ART_OF_ARTIST] {
        album_id: String,
        source: String,
        path: Option<String>,
        tracks: Option<String>,
        content_hash: Option<String>,
        version: i64,
    }
}

/// The `art_tracks` column over another statement's alias for the track
/// holding the picture. Read with
/// [`library_folder`](super::local::library_folder).
pub(crate) fn art_tracks_column(embedded: &str) -> String {
    format!(art_tracks!("{embedded}"), embedded = embedded)
}
