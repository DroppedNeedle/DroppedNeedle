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
         COALESCE(CAST(strftime('%s', p.created_at) AS INTEGER), 0), \
         COALESCE(CAST(strftime('%s', p.updated_at) AS INTEGER), 0), \
         (c.playlist_id IS NOT NULL), \
         (SELECT COUNT(*) FROM playlist_tracks t WHERE t.playlist_id = p.id), \
         (SELECT SUM(CAST(t.duration AS REAL)) FROM playlist_tracks t \
          WHERE t.playlist_id = p.id), \
         (SELECT COUNT(*) FROM playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL), \
         (SELECT COALESCE(SUM(CAST(t.duration AS REAL)), 0.0) FROM playlist_tracks t \
          WHERE t.playlist_id = p.id AND t.library_file_id IS NOT NULL)";
    /// The tables a playlist row is read from.
    fragment PLAYLIST_FROM = " FROM playlists p \
         LEFT JOIN auth_users u ON u.id = p.user_id \
         LEFT JOIN playlist_covers c ON c.playlist_id = p.id";
    /// The list order.
    fragment BY_NAME = " ORDER BY p.name COLLATE NOCASE, p.id";
    /// One entry, in `PlaylistTrack`'s mapper order.
    fragment TRACK_COLUMNS = "SELECT id, position, track_name, artist_name, album_name, \
         album_id, artist_id, track_source_id, cover_url, source_type, available_sources, \
         format, track_number, disc_number, CAST(duration AS REAL), \
         COALESCE(CAST(strftime('%s', created_at) AS INTEGER), 0), \
         plex_rating_key, library_file_id FROM playlist_tracks";

    /// Every playlist, then whether the caller may read it.
    pub(super) LIST_ALL = PLAYLIST_COLUMNS + ", " + READABLE + PLAYLIST_FROM + BY_NAME;
    /// The playlists the caller may read.
    pub(super) LIST_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE " + READABLE + BY_NAME;
    /// The first `?2` entry covers of each playlist the caller may read.
    pub(super) LIST_COVERS = "SELECT r.playlist_id, r.cover_url FROM (SELECT playlist_id, \
         cover_url, ROW_NUMBER() OVER (PARTITION BY playlist_id ORDER BY position) AS rank \
         FROM playlist_tracks WHERE cover_url IS NOT NULL AND cover_url != '') r \
         JOIN playlists p ON p.id = r.playlist_id \
         WHERE r.rank <= ?2 AND " + READABLE + " ORDER BY r.playlist_id, r.rank";
    /// One playlist `?2` the caller may read.
    pub(super) GET_READABLE = PLAYLIST_COLUMNS + PLAYLIST_FROM
        + " WHERE p.id = ?2 AND " + READABLE;
    /// One playlist `?2` of the caller's.
    pub(super) GET_OWNED = PLAYLIST_COLUMNS + PLAYLIST_FROM + " WHERE p.id = ?2 AND " + OWNED;
    /// Whether the caller owns playlist `?2`, which they may read.
    pub(super) READABLE_OWNER = "SELECT COALESCE(p.user_id = ?1, 0) FROM playlists p \
         WHERE p.id = ?2 AND " + READABLE;
    /// The first `?2` entry covers of one playlist `?1`.
    pub(super) PLAYLIST_COVERS = "SELECT cover_url FROM playlist_tracks \
         WHERE playlist_id = ?1 AND cover_url IS NOT NULL AND cover_url != '' \
         ORDER BY position LIMIT ?2";
    /// The entries of playlist `?2`, which the caller may read.
    pub(super) TRACKS_READABLE = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM playlists p WHERE p.id = ?2 AND "
        + READABLE + ") ORDER BY position";
    /// The entries of playlist `?2`, the caller's.
    pub(super) TRACKS_OWNED = TRACK_COLUMNS
        + " WHERE playlist_id = (SELECT p.id FROM playlists p WHERE p.id = ?2 AND "
        + OWNED + ") ORDER BY position";
    /// The uploaded cover of playlist `?2`, which the caller may read.
    pub(super) COVER_READABLE = "SELECT c.content_type, c.image FROM playlist_covers c \
         JOIN playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + READABLE;
    /// The uploaded cover of playlist `?2`, the caller's.
    pub(super) COVER_OWNED = "SELECT c.content_type, c.image FROM playlist_covers c \
         JOIN playlists p ON p.id = c.playlist_id WHERE c.playlist_id = ?2 AND " + OWNED;
    /// The caller's playlist imported from source `?2`.
    pub(super) FIND_BY_SOURCE = "SELECT id FROM playlists WHERE user_id = ?1 AND source_ref = ?2";
    /// For each `[track, artist, album]` in the JSON array `?2`, by its
    /// index, the playlists the caller may read holding an entry with
    /// exactly those names.
    pub(super) MEMBERSHIP = "WITH q(idx, track_name, artist_name, album_name) AS \
         (SELECT key, json_extract(value, '$[0]'), json_extract(value, '$[1]'), \
         json_extract(value, '$[2]') FROM json_each(?2)) \
         SELECT DISTINCT q.idx, t.playlist_id FROM q \
         JOIN playlist_tracks t ON t.track_name = q.track_name \
         AND t.artist_name = q.artist_name AND t.album_name = q.album_name \
         JOIN playlists p ON p.id = t.playlist_id WHERE " + READABLE
        + " ORDER BY q.idx, t.playlist_id";

    /// Whether playlist `?1` is user `?2`'s.
    pub(super) OWNED_GUARD = "SELECT 1 FROM playlists WHERE id = ?1 AND user_id = ?2";
    /// Stamp user `?3`'s playlist `?2` as changed at `?1`, epoch seconds.
    pub(super) TOUCH = "UPDATE playlists \
         SET updated_at = strftime('%Y-%m-%dT%H:%M:%SZ', ?1, 'unixepoch') \
         WHERE id = ?2 AND user_id = ?3";
    /// One empty private playlist made at `?3`, epoch seconds.
    pub(super) INSERT_PLAYLIST = "INSERT INTO playlists (id, name, cover_image_path, \
         created_at, updated_at, source_ref, user_id, is_public) VALUES (?1, ?2, NULL, \
         strftime('%Y-%m-%dT%H:%M:%SZ', ?3, 'unixepoch'), \
         strftime('%Y-%m-%dT%H:%M:%SZ', ?3, 'unixepoch'), ?4, ?5, 0)";
    /// Rename user `?3`'s playlist `?2`.
    pub(super) RENAME = "UPDATE playlists SET name = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Set the public flag of user `?3`'s playlist `?2`.
    pub(super) SET_PUBLIC = "UPDATE playlists SET is_public = ?1 WHERE id = ?2 AND user_id = ?3";
    /// Delete user `?2`'s playlist `?1`; its entries and cover go with it.
    pub(super) DELETE_PLAYLIST = "DELETE FROM playlists WHERE id = ?1 AND user_id = ?2";
    /// One playlist as a write left it: name, flag and times in epoch
    /// seconds.
    pub(super) SAVED = "SELECT name, is_public, \
         COALESCE(CAST(strftime('%s', created_at) AS INTEGER), 0), \
         COALESCE(CAST(strftime('%s', updated_at) AS INTEGER), 0) \
         FROM playlists WHERE id = ?1";
    /// Entry ids in position order.
    pub(super) ENTRY_ORDER = "SELECT id FROM playlist_tracks WHERE playlist_id = ?1 \
         ORDER BY position";
    /// Place one entry.
    pub(super) SET_POSITION = "UPDATE playlist_tracks SET position = ?1 WHERE id = ?2";
    /// One entry, made at `?17`, epoch seconds.
    pub(super) INSERT_ENTRY = "INSERT INTO playlist_tracks (id, playlist_id, position, \
         track_name, artist_name, album_name, album_id, artist_id, track_source_id, \
         cover_url, source_type, available_sources, format, track_number, disc_number, \
         duration, created_at, plex_rating_key, library_file_id) VALUES \
         (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, \
         strftime('%Y-%m-%dT%H:%M:%SZ', ?17, 'unixepoch'), ?18, ?19)";
    /// Every entry of one playlist.
    pub(super) CLEAR_ENTRIES = "DELETE FROM playlist_tracks WHERE playlist_id = ?1";
    /// One entry of one playlist.
    pub(super) REMOVE_ENTRY = "DELETE FROM playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// Whether playlist `?1` holds entry `?2`.
    pub(super) ENTRY_EXISTS = "SELECT 1 FROM playlist_tracks WHERE playlist_id = ?1 AND id = ?2";
    /// An entry's preferred source.
    pub(super) SET_SOURCE_TYPE = "UPDATE playlist_tracks SET source_type = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// An entry's known sources, as a JSON list.
    pub(super) SET_AVAILABLE_SOURCES = "UPDATE playlist_tracks SET available_sources = ?1 \
         WHERE id = ?2 AND playlist_id = ?3";
    /// Store or replace one playlist's uploaded cover.
    pub(super) UPSERT_COVER = "INSERT INTO playlist_covers (playlist_id, content_type, image, \
         updated_at) VALUES (?1, ?2, ?3, ?4) ON CONFLICT (playlist_id) DO UPDATE SET \
         content_type = excluded.content_type, image = excluded.image, \
         updated_at = excluded.updated_at";
    /// Delete one playlist's uploaded cover.
    pub(super) DELETE_COVER = "DELETE FROM playlist_covers WHERE playlist_id = ?1";
}
