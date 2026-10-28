//! The local file catalog's statements.

use droppedneedle_library::corrections::continuation::CURRENT_TRACK;

droppedneedle_kernel::sql! {
    /// The track columns a `CatalogFile` is read from, in its mapper's order.
    fragment TRACK_FILES = "SELECT id, root_id, relative_path, file_path, title, disc_number, \
         track_number, file_format, bit_rate, duration_seconds FROM local_tracks";
    /// Albums by their MusicBrainz identity.
    fragment ALBUM_IDENTITIES = "SELECT b.id FROM local_album_external_identities e \
         JOIN local_albums b ON b.id = e.local_album_id";
    /// Only a live album with a streamable track, the oldest first.
    fragment LIVE_OLDEST = " AND b.retired_into_album_id IS NULL \
         AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
         AND t.availability = 'indexed') \
         ORDER BY b.created_at, b.id LIMIT 1";
    /// One streamable track's file; an id that continued into another
    /// track answers with that track.
    pub(super) TRACK_FILE = TRACK_FILES
        + " WHERE id = (" + CURRENT_TRACK + ") AND availability = 'indexed'";
    /// One album, or the album it merged into, with its artist's name.
    pub(super) ALBUM = "SELECT a.id, a.title, \
         COALESCE(an.display_name, a.album_artist_name, '') \
         FROM local_albums a LEFT JOIN local_artists an ON an.id = a.album_artist_id \
         WHERE a.id = (SELECT COALESCE(retired_into_album_id, id) FROM local_albums \
         WHERE id = ?1)";
    /// One album's streamable files in disc and track order.
    pub(super) ALBUM_FILES = TRACK_FILES
        + " WHERE local_album_id = ?1 AND availability = 'indexed' \
         ORDER BY disc_number, track_number, id";
    /// The album a lowered release group id names.
    pub(super) ALBUM_BY_RELEASE_GROUP = ALBUM_IDENTITIES
        + " WHERE lower(e.release_group_mbid) = ?1" + LIVE_OLDEST;
    /// The album a lowered release id names.
    pub(super) ALBUM_BY_RELEASE = ALBUM_IDENTITIES
        + " WHERE lower(e.release_mbid) = ?1" + LIVE_OLDEST;
}
