-- Save the rule only. Matching files and ratings stay in their source tables.
CREATE TABLE catalog_smart_playlists (
    id TEXT PRIMARY KEY NOT NULL,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    min_rating INTEGER NOT NULL CHECK (min_rating BETWEEN 1 AND 5)
) STRICT;
CREATE INDEX idx_catalog_smart_playlists_owner
    ON catalog_smart_playlists(user_id, name COLLATE NOCASE, id);

PRAGMA user_version = 173;
