-- Whether a playlist's pull also asks for the songs the library lacks.
ALTER TABLE catalog_playlist_pull_state ADD COLUMN acquire INTEGER NOT NULL DEFAULT 0
    CHECK (acquire IN (0, 1));

PRAGMA user_version = 157;
