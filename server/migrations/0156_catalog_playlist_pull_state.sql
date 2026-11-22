-- Whether a playlist follows its source, and what the source last said
-- about its contents. A playlist with a spotify:<id> source can pull new
-- entries; the snapshot id decides when that is worth a read.
CREATE TABLE catalog_playlist_pull_state (
    playlist_id TEXT PRIMARY KEY REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    snapshot_id TEXT,
    last_checked_at INTEGER,
    last_pulled_at INTEGER,
    last_error TEXT
);

PRAGMA user_version = 156;
