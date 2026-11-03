//! The statements of the kernel's own loops: plugin tick state and the
//! Navidrome playlist export.

crate::sql! {
    /// One plugin's value under one key.
    pub(super) TICK_STATE = "SELECT value FROM plugin_tick_state WHERE plugin = ?1 AND key = ?2";
    /// How many keys and bytes plugin `?1` holds under keys other than `?2`.
    pub(super) TICK_STATE_TOTALS = "SELECT COUNT(*) AS keys, \
        COALESCE(SUM(length(value)), 0) AS bytes \
        FROM plugin_tick_state WHERE plugin = ?1 AND key <> ?2";
    /// Store one plugin's value under one key, replacing the last one.
    pub(super) UPSERT_TICK_STATE = "INSERT INTO plugin_tick_state (plugin, key, value, updated_at) \
        VALUES (?1, ?2, ?3, ?4) \
        ON CONFLICT (plugin, key) DO UPDATE SET value = excluded.value, \
        updated_at = excluded.updated_at";

    /// Every playlist, the most recently changed first.
    pub(super) ALL_PLAYLISTS = "SELECT id, name FROM catalog_playlists \
        ORDER BY updated_at DESC, id";
    /// The public playlists, the most recently changed first.
    pub(super) PUBLIC_PLAYLISTS = "SELECT id, name FROM catalog_playlists WHERE is_public = 1 \
        ORDER BY updated_at DESC, id";
    /// One playlist's entries in order, each with its local file when it
    /// has one. Collections keeps the local track id in `library_file_id`.
    pub(super) PLAYLIST_ENTRIES = "SELECT pt.track_name, pt.artist_name, pt.duration_ms, \
        t.file_path FROM catalog_playlist_tracks pt \
        LEFT JOIN local_tracks t ON t.id = pt.library_file_id \
        WHERE pt.playlist_id = ?1 ORDER BY pt.position";

    /// What one plugin holds besides the key being written.
    pub(super) row TickStateTotals for [TICK_STATE_TOTALS] {
        keys: i64,
        bytes: i64,
    }
    /// One playlist to export.
    pub(super) row PlaylistRow for [ALL_PLAYLISTS, PUBLIC_PLAYLISTS] {
        id: String,
        name: String,
    }
    /// One playlist entry and its local file.
    pub(super) row PlaylistEntryRow for [PLAYLIST_ENTRIES] {
        track_name: String,
        artist_name: String,
        duration_ms: Option<i64>,
        file_path: Option<String>,
    }
}
