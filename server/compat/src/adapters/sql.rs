//! The statements of the compat adapters: saved play queues, bookmarks and
//! the read of which artists have album art to draw a picture from.

droppedneedle_kernel::sql! {
    /// A user's saved queue, without its items. Times are REAL unix seconds.
    pub(super) QUEUE_HEAD = "SELECT current_index, position_ms, updated_at, changed_by_client \
        FROM compat_play_queues WHERE user_id = ?1";
    pub(super) row QueueHead for [QUEUE_HEAD] {
        current_index: Option<i64>,
        position_ms: i64,
        updated_at: f64,
        changed_by_client: String,
    }
    /// A user's queued file ids in order.
    pub(super) QUEUE_ITEMS = "SELECT file_id FROM compat_play_queue_items \
        WHERE user_id = ?1 ORDER BY item_index";
    /// Save a user's queue, replacing the one they had.
    pub(super) PUT_QUEUE = "INSERT INTO compat_play_queues (user_id, current_index, position_ms, \
        updated_at, changed_by_client) VALUES (?1, ?2, ?3, ?4, ?5) \
        ON CONFLICT (user_id) DO UPDATE SET current_index = excluded.current_index, \
        position_ms = excluded.position_ms, updated_at = excluded.updated_at, \
        changed_by_client = excluded.changed_by_client";
    /// Forget a user's queued items.
    pub(super) CLEAR_QUEUE_ITEMS = "DELETE FROM compat_play_queue_items WHERE user_id = ?1";
    /// Add one item to a user's queue.
    pub(super) ADD_QUEUE_ITEM = "INSERT INTO compat_play_queue_items (user_id, item_index, file_id) \
        VALUES (?1, ?2, ?3)";
    /// A user's bookmarks, most recently changed first. Times are REAL unix
    /// seconds.
    pub(super) BOOKMARKS = "SELECT file_id, position_ms, comment, created_at, changed_at \
        FROM compat_bookmarks WHERE user_id = ?1 ORDER BY changed_at DESC, file_id";
    pub(super) row BookmarkColumns for [BOOKMARKS] {
        file_id: String,
        position_ms: i64,
        comment: String,
        created_at: f64,
        changed_at: f64,
    }
    /// Create a bookmark, or move one the user already has on that file.
    pub(super) PUT_BOOKMARK = "INSERT INTO compat_bookmarks (user_id, file_id, position_ms, comment, \
        created_at, changed_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
        ON CONFLICT (user_id, file_id) DO UPDATE SET \
        position_ms = excluded.position_ms, comment = excluded.comment, \
        changed_at = excluded.changed_at";
    /// Delete one bookmark.
    pub(super) DELETE_BOOKMARK = "DELETE FROM compat_bookmarks WHERE user_id = ?1 AND file_id = ?2";
    /// Which of some artists (?1, a JSON array) lead or are credited on a
    /// live album with its own art, folder or embedded in an indexed track.
    pub(super) ARTISTS_WITH_LOCAL_ART = "SELECT DISTINCT credit.artist_id FROM ( \
        SELECT album_artist_id AS artist_id, id AS album_id FROM local_albums \
        WHERE album_artist_id IN (SELECT value FROM json_each(?1)) \
        UNION SELECT local_artist_id, local_album_id FROM local_album_artists \
        WHERE local_artist_id IN (SELECT value FROM json_each(?1))) credit \
        JOIN local_albums a ON a.id = credit.album_id \
        AND a.retired_into_album_id IS NULL AND a.has_indexed_track = 1 \
        JOIN local_album_artwork w ON w.local_album_id = a.id \
        LEFT JOIN local_tracks t ON t.id = w.source_locator AND w.source = 'embedded' \
        AND t.availability = 'indexed' \
        WHERE (w.source = 'folder' AND w.source_locator IS NOT NULL) \
        OR (w.source = 'embedded' AND t.file_path IS NOT NULL)";
}
