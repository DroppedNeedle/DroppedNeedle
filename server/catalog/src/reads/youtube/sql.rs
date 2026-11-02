//! The YouTube link store's statements.
//!
//! Times are integer milliseconds, made by SQLite inside the write so a row's
//! time is the time it was stored, and read as ISO-8601 UTC text.

droppedneedle_kernel::sql! {
    /// Current time in integer milliseconds.
    fragment NOW = "CAST(round(unixepoch('now', 'subsec') * 1000) AS INTEGER)";
    /// An album link with its live track count.
    fragment LINK_SELECT = "SELECT l.album_id AS album_id, l.video_id AS video_id, \
         l.album_name AS album_name, l.artist_name AS artist_name, l.embed_url AS embed_url, \
         l.cover_url AS cover_url, \
         strftime('%Y-%m-%dT%H:%M:%f+00:00', l.created_at / 1000.0, 'unixepoch') AS created_at, \
         COALESCE(l.is_manual, 0) AS is_manual, \
         (SELECT COUNT(*) FROM catalog_youtube_track_links t WHERE t.album_id = l.album_id) AS track_count \
         FROM catalog_youtube_links l";
    /// A track link.
    fragment TRACK_SELECT = "SELECT album_id, album_name, disc_number, track_number, \
         track_name, video_id, artist_name, embed_url, \
         strftime('%Y-%m-%dT%H:%M:%f+00:00', created_at / 1000.0, 'unixepoch') AS created_at \
         FROM catalog_youtube_track_links";
    /// One album link.
    pub(super) LINK = LINK_SELECT + " WHERE l.album_id = ?1";
    /// Every album link, newest first.
    pub(super) LINKS = LINK_SELECT + " ORDER BY l.created_at DESC";
    pub(super) row LinkColumns for [LINK, LINKS] {
        album_id: String,
        video_id: Option<String>,
        album_name: String,
        artist_name: String,
        embed_url: Option<String>,
        cover_url: Option<String>,
        created_at: String,
        is_manual: bool,
        track_count: i64,
    }
    /// An album's track links, in disc then track order.
    pub(super) TRACK_LINKS = TRACK_SELECT + " WHERE album_id = ?1 ORDER BY disc_number, track_number";
    /// One track link.
    pub(super) TRACK_LINK = TRACK_SELECT
        + " WHERE album_id = ?1 AND disc_number = ?2 AND track_number = ?3";
    pub(super) row TrackColumns for [TRACK_LINKS, TRACK_LINK] {
        album_id: String,
        album_name: String,
        disc_number: i64,
        track_number: i64,
        track_name: String,
        video_id: String,
        artist_name: String,
        embed_url: String,
        created_at: String,
    }
    /// A saved album link up to its cover rule.
    fragment SAVE_LINK_START = "INSERT INTO catalog_youtube_links (album_id, video_id, album_name, \
         artist_name, embed_url, cover_url, created_at, is_manual, track_count) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, " + NOW + ", ?7, \
         (SELECT COUNT(*) FROM catalog_youtube_track_links WHERE album_id = ?1)) \
         ON CONFLICT(album_id) DO UPDATE SET video_id = excluded.video_id, \
         album_name = excluded.album_name, artist_name = excluded.artist_name, \
         embed_url = excluded.embed_url, cover_url = ";
    /// What follows the cover rule.
    fragment SAVE_LINK_END = ", created_at = excluded.created_at, is_manual = excluded.is_manual";
    /// Save a link YouTube search found: a new cover only replaces a saved one.
    pub(super) SAVE_FOUND_LINK = SAVE_LINK_START
        + "COALESCE(excluded.cover_url, catalog_youtube_links.cover_url)" + SAVE_LINK_END;
    /// Save a link a person pasted: the cover is taken as sent.
    pub(super) SAVE_PASTED_LINK = SAVE_LINK_START + "excluded.cover_url" + SAVE_LINK_END;
    /// Change a saved link; a NULL keeps the saved value, and `?6` keeps
    /// the saved cover.
    pub(super) EDIT_LINK = "UPDATE catalog_youtube_links SET video_id = COALESCE(?2, video_id), \
         embed_url = COALESCE(?3, embed_url), \
         album_name = COALESCE(?4, album_name), \
         artist_name = COALESCE(?5, artist_name), \
         cover_url = CASE WHEN ?6 THEN cover_url ELSE ?7 END \
         WHERE album_id = ?1";
    /// Delete an album's track links.
    pub(super) DELETE_TRACK_LINKS = "DELETE FROM catalog_youtube_track_links WHERE album_id = ?1";
    /// Delete an album link.
    pub(super) DELETE_LINK = "DELETE FROM catalog_youtube_links WHERE album_id = ?1";
    /// Save one track video.
    pub(super) SAVE_TRACK_LINK = "INSERT INTO catalog_youtube_track_links (album_id, track_number, \
         disc_number, album_name, track_name, video_id, artist_name, embed_url, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, " + NOW + ") \
         ON CONFLICT(album_id, disc_number, track_number) DO UPDATE SET \
         album_name = excluded.album_name, track_name = excluded.track_name, \
         video_id = excluded.video_id, artist_name = excluded.artist_name, \
         embed_url = excluded.embed_url, created_at = excluded.created_at";
    /// Make sure an album with track videos has an entry, with their count.
    pub(super) ENSURE_LINK = "INSERT INTO catalog_youtube_links (album_id, video_id, album_name, \
         artist_name, embed_url, cover_url, created_at, is_manual, track_count) \
         VALUES (?1, NULL, ?2, ?3, NULL, ?4, " + NOW + ", 0, \
         (SELECT COUNT(*) FROM catalog_youtube_track_links WHERE album_id = ?1)) \
         ON CONFLICT(album_id) DO UPDATE SET track_count = excluded.track_count, \
         cover_url = COALESCE(excluded.cover_url, catalog_youtube_links.cover_url)";
}
