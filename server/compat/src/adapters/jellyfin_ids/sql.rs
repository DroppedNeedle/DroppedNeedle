//! The statements of the stored Jellyfin ids (`compat_jellyfin_ids`), and
//! the reads of the library and playlist rows the fill looks for.

use droppedneedle_kernel::db::statement::ListLen;

droppedneedle_kernel::sql! {
    /// The catalog revision now; 0 before the first catalog change.
    pub(super) REVISION = "SELECT COALESCE((SELECT value FROM library_catalog_revision \
        WHERE singleton = 1), 0)";
    /// Live albums, owned or not, with no current id stored.
    pub(super) NEW_ALBUMS = "SELECT a.id FROM local_albums a \
        WHERE a.retired_into_album_id IS NULL AND NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids j \
        WHERE j.kind = 'album' AND j.internal_id = a.id AND j.current = 1)";
    /// Live artists with no current id stored.
    pub(super) NEW_ARTISTS = "SELECT r.id FROM local_artists r \
        WHERE r.retired_into_artist_id IS NULL AND NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids j \
        WHERE j.kind = 'artist' AND j.internal_id = r.id AND j.current = 1)";
    /// Tracks of any availability with no current id stored.
    pub(super) NEW_TRACKS = "SELECT t.id FROM local_tracks t \
        WHERE NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids j \
        WHERE j.kind = 'track' AND j.internal_id = t.id AND j.current = 1)";
    /// Files playlist entries name that have no track row and no current
    /// id stored: a playlist answer hands out a track id for them too.
    pub(super) NEW_PLAYLIST_FILES = "SELECT DISTINCT p.library_file_id FROM playlist_tracks p \
        WHERE p.library_file_id IS NOT NULL \
        AND NOT EXISTS (SELECT 1 FROM local_tracks t WHERE t.id = p.library_file_id) \
        AND NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids j \
        WHERE j.kind = 'track' AND j.internal_id = p.library_file_id AND j.current = 1)";
    /// Playlists with no current id stored.
    pub(super) NEW_PLAYLISTS = "SELECT p.id FROM playlists p \
        WHERE NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids j \
        WHERE j.kind = 'playlist' AND j.internal_id = p.id AND j.current = 1)";
    /// Every genre name an answer can carry: the genre rows, the tags and
    /// the albums' own genre, which an album keeps after its tracks lose it.
    pub(super) GENRE_NAMES = "SELECT name FROM local_track_genres \
        UNION SELECT genre FROM local_tracks WHERE genre IS NOT NULL \
        UNION SELECT primary_genre FROM local_albums WHERE primary_genre IS NOT NULL";
    /// Album ids a merge retired, as rows and as merge aliases.
    pub(super) RETIRED_ALBUMS = "SELECT id FROM local_albums \
        WHERE retired_into_album_id IS NOT NULL \
        UNION SELECT alias FROM local_album_aliases WHERE kind = 'merged_album'";
    /// Artist ids a merge retired or the upgrade from v2 replaced.
    pub(super) RETIRED_ARTISTS = "SELECT id FROM local_artists \
        WHERE retired_into_artist_id IS NOT NULL \
        UNION SELECT alias FROM local_artist_aliases \
        WHERE kind IN ('merged_artist', 'legacy_artist')";
    /// Which of some Jellyfin ids are stored.
    family STORED = stored;
    /// Store one id; an id or a current item already stored is kept.
    pub(super) INSERT = "INSERT INTO compat_jellyfin_ids (jellyfin_id, kind, internal_id, current) \
        VALUES (?1, ?2, ?3, ?4) ON CONFLICT DO NOTHING";
    /// Make a stored alias the current id of the item it names, when that
    /// item has none: an album or artist a merge retired is live again.
    pub(super) PROMOTE = "UPDATE compat_jellyfin_ids SET current = 1 \
        WHERE jellyfin_id = ?1 AND kind = ?2 AND internal_id = ?3 AND current = 0 \
        AND NOT EXISTS (SELECT 1 FROM compat_jellyfin_ids c \
        WHERE c.kind = ?2 AND c.internal_id = ?3 AND c.current = 1)";
    /// The current ids of albums and artists, which depend on the data key.
    pub(super) KEYED_CURRENT = "SELECT jellyfin_id, kind, internal_id FROM compat_jellyfin_ids \
        WHERE current = 1 AND kind IN ('album', 'artist')";
    /// What some Jellyfin ids name, by primary key.
    family LOOKUP = lookup;
}

/// `?, ?, ...` for a list of `len` values.
fn marks(len: ListLen) -> String {
    vec!["?"; len.get()].join(", ")
}

/// [`STORED`] for `len` ids.
pub(super) fn stored(len: ListLen) -> String {
    format!(
        "SELECT jellyfin_id FROM compat_jellyfin_ids WHERE jellyfin_id IN ({})",
        marks(len)
    )
}

/// [`LOOKUP`] for `len` ids.
pub(super) fn lookup(len: ListLen) -> String {
    format!(
        "SELECT jellyfin_id, kind, internal_id, current FROM compat_jellyfin_ids \
         WHERE jellyfin_id IN ({})",
        marks(len)
    )
}
