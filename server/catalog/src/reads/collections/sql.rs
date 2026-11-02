//! The playlist store's statements. In each read that names a caller,
//! `?1` is that user's id.

droppedneedle_kernel::sql! {
    /// The caller may read the playlist: it is theirs, or it is public.
    fragment READABLE = "(COALESCE(p.user_id = ?1, 0) OR p.is_public != 0)";
    /// The playlist is the caller's.
    fragment OWNED = "p.user_id = ?1";
    /// One playlist with its aggregates, in `PlaylistRow`'s mapper order.
    fragment PLAYLIST_COLUMNS = "SELECT p.id, p.name, p.user_id, \
         COALESCE(u.display_name, u.username), p.is_public, p.source_ref, \
         p.created_at / 1000, p.updated_at / 1000, \
         (c.playlist_id IS NOT NULL), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t WHERE t.playlist_id = p.id), \
         (SELECT SUM(CAST(t.duration_ms AS REAL)) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id), \
         (SELECT COUNT(*) FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL), \
         (SELECT COALESCE(SUM(CAST(t.duration_ms AS REAL)), 0.0) / 1000.0 FROM catalog_playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL)";
    /// The tables a playlist row is read from.
    fragment PLAYLIST_FROM = " FROM catalog_playlists p \
         LEFT JOIN auth_users u ON u.id = p.user_id \
         LEFT JOIN catalog_playlist_covers c ON c.playlist_id = p.id";
    /// The list order.
    fragment BY_NAME = " ORDER BY p.name COLLATE NOCASE, p.id";
    /// One entry, in `PlaylistTrack`'s mapper order.
    fragment TRACK_COLUMNS = "SELECT id, position, track_name, artist_name, album_name, \
         album_id, artist_id, track_source_id, cover_url, source_type, available_sources, \
         format, track_number, disc_number, duration_ms / 1000.0, created_at / 1000, \
         plex_rating_key, library_file_id FROM catalog_playlist_tracks";

    /// Every playlist, then whether the caller may read it. A private
    /// playlist with no owner is left out unless `?2`, an admin asking.
    pub(super) LIST_ALL = PLAYLIST_COLUMNS + ", " + READABLE + PLAYLIST_FROM
        + " WHERE (p.user_id IS NOT NULL OR p.is_public != 0 OR ?2)" + BY_NAME;
    /// The playlists the caller may read.
    pub(super) LIST_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE " + READABLE + BY_NAME;
    /// The first `?2` entry covers of each playlist the caller may read.
    pub(super) LIST_COVERS = "SELECT r.playlist_id, r.cover_url FROM (SELECT playlist_id, \
         cover_url, ROW_NUMBER() OVER (PARTITION BY playlist_id ORDER BY position) AS rank \
         FROM catalog_playlist_tracks WHERE cover_url IS NOT NULL AND cover_url != '') r \
         JOIN catalog_playlists p ON p.id = r.playlist_id \
         WHERE r.rank <= ?2 AND " + READABLE + " ORDER BY r.playlist_id, r.rank";
    /// One playlist `?2` the caller may read.
    pub(super) GET_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM
        + " WHERE p.id = ?2 AND " + READABLE;
    /// One playlist `?2` of the caller's.
    pub(super) GET_OWNED = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE p.id = ?2 AND " + OWNED;
    /// Whether the caller owns playlist `?2`, which they may read.
    pub(super) READABLE_OWNER = "SELECT COALESCE(p.user_id = ?1, 0) FROM catalog_playlists p \
         WHERE p.id = ?2 AND " + READABLE;
    /// The first `?2` entry covers of one playlist `?1`.
    pub(super) PLAYLIST_COVERS = "SELECT cover_url FROM catalog_playlist_tracks \
         WHERE playlist_id = ?1 AND cover_url IS NOT NULL AND cover_url != '' \
         ORDER BY position LIMIT ?2";
    /// The entries of playlist `?2`, which the caller may read.
    pub(super) TRACKS_READABLE = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + READABLE + ") ORDER BY position";
    /// The entries of playlist `?2`, the caller's.
    pub(super) TRACKS_OWNED = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM catalog_playlists p WHERE p.id = ?2 AND "
        + OWNED + ") ORDER BY position";
    /// The uploaded cover of playlist `?2`, which the caller may read.
    pub(super) COVER_READABLE = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + READABLE;
    /// The uploaded cover of playlist `?2`, the caller's.
    pub(super) COVER_OWNED = "SELECT c.content_type, c.image FROM catalog_playlist_covers c \
         JOIN catalog_playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + OWNED;
    /// The caller's playlist imported from source `?2`.
    pub(super) FIND_BY_SOURCE = "SELECT id FROM catalog_playlists WHERE user_id = ?1 AND source_ref = ?2";
    /// For each `[track, artist, album]` in the JSON array `?2`, by its
    /// index, the playlists the caller may read holding an entry with
    /// exactly those names.
    pub(super) MEMBERSHIP = "WITH q(idx, track_name, artist_name, album_name) AS \
         (SELECT key, json_extract(value, '$[0]'), json_extract(value, '$[1]'), \
         json_extract(value, '$[2]') FROM json_each(?2)) \
         SELECT DISTINCT q.idx, t.playlist_id FROM q \
         JOIN catalog_playlist_tracks t ON t.track_name = q.track_name \
         AND t.artist_name = q.artist_name AND t.album_name = q.album_name \
         JOIN catalog_playlists p ON p.id = t.playlist_id WHERE " + READABLE
        + " ORDER BY q.idx, t.playlist_id";

    /// Whether playlist `?1` is user `?2`'s.
    pub(super) OWNED_GUARD = "SELECT 1 FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Stamp user `?3`'s playlist `?2` as changed at `?1`, milliseconds.
    pub(super) TOUCH = "UPDATE catalog_playlists SET updated_at = ?1 \
         WHERE id = ?2 AND user_id = ?3";
    /// One empty private playlist made at `?3`, milliseconds.
    pub(super) INSERT_PLAYLIST = "INSERT INTO catalog_playlists (id, name, created_at, \
         updated_at, source_ref, user_id, is_public) VALUES (?1, ?2, ?3, ?3, ?4, ?5, 0)";
    /// Rename user `?3`'s playlist `?2`.
    pub(super) RENAME = "UPDATE catalog_playlists SET name = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Set the public flag of user `?3`'s playlist `?2`.
    pub(super) SET_PUBLIC = "UPDATE catalog_playlists SET is_public = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Delete user `?2`'s playlist `?1`; its entries and cover go with it.
    pub(super) DELETE_PLAYLIST = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id = ?2";
    /// Delete playlist `?1` if it has no owner; its entries and cover go
    /// with it.
    pub(super) DELETE_STRAY = "DELETE FROM catalog_playlists WHERE id = ?1 AND user_id IS NULL";
    /// One playlist as a write left it: name, flag and made time in epoch
    /// seconds.
    pub(super) SAVED = "SELECT name, is_public, created_at / 1000 \
         FROM catalog_playlists WHERE id = ?1";
    /// Entry ids in position order.
    pub(super) ENTRY_ORDER = "SELECT id FROM catalog_playlist_tracks WHERE playlist_id = ?1 \
         ORDER BY position";
    /// Place one entry.
    pub(super) SET_POSITION = "UPDATE catalog_playlist_tracks SET position = ?1 WHERE id = ?2";
    /// One entry with its length `?16` and made time `?17`, milliseconds.
    pub(super) INSERT_ENTRY = "INSERT INTO catalog_playlist_tracks (id, playlist_id, position, \
         track_name, artist_name, album_name, album_id, artist_id, track_source_id, \
         cover_url, source_type, available_sources, format, track_number, disc_number, \
         duration_ms, created_at, plex_rating_key, library_file_id) VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19)";
    /// Every entry of one playlist.
    pub(super) CLEAR_ENTRIES = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1";
    /// One entry of one playlist.
    pub(super) REMOVE_ENTRY = "DELETE FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// Whether playlist `?1` holds entry `?2`.
    pub(super) ENTRY_EXISTS = "SELECT 1 FROM catalog_playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// An entry's preferred source.
    pub(super) SET_SOURCE_TYPE = "UPDATE catalog_playlist_tracks SET source_type = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// An entry's known sources, as a JSON list.
    pub(super) SET_AVAILABLE_SOURCES = "UPDATE catalog_playlist_tracks SET available_sources = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// Store or replace one playlist's uploaded cover.
    pub(super) UPSERT_COVER = "INSERT INTO catalog_playlist_covers (playlist_id, content_type, image, \
         updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (playlist_id) DO UPDATE SET \
         content_type = excluded.content_type, image = excluded.image, \
         updated_at = excluded.updated_at";
    /// Delete one playlist's uploaded cover.
    pub(super) DELETE_COVER = "DELETE FROM catalog_playlist_covers WHERE playlist_id = ?1";
}
