-- 0010_collections_storage.sql - playlist covers and favorite names.
--
-- Playlists, favorites, follows, pins, compat play queues and bookmarks all
-- live in tables the 0001 baseline already holds. Two pieces had no home:
--
-- * Uploaded playlist covers. v2 kept them as files beside the database;
--   v3 keeps the bytes in the database so they travel with backups and
--   vanish with their playlist.
-- * The display name a native favorite was saved with, so the favorites
--   page can label an item the catalog no longer holds. It sits in its own
--   table so this file stays safe to apply twice, like every migration.
--
-- It also indexes album creation time, so the player protocols' "recently
-- added" pages walk an index instead of sorting every album.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS playlist_covers (
    playlist_id  TEXT PRIMARY KEY REFERENCES playlists(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    image        BLOB NOT NULL,
    updated_at   REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_user_favorite_names (
    user_id      TEXT NOT NULL,
    item_kind    TEXT NOT NULL,
    item_id      TEXT NOT NULL,
    display_name TEXT NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id),
    FOREIGN KEY (user_id, item_kind, item_id)
        REFERENCES library_user_favorites(user_id, item_kind, item_id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_playlists_user ON playlists(user_id);
CREATE INDEX IF NOT EXISTS idx_playlist_tracks_library_file
    ON playlist_tracks(library_file_id) WHERE library_file_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_local_albums_created ON local_albums(created_at, id);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 10;
