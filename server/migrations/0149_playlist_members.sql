-- The people a playlist is shared with. A member may read the playlist;
-- `can_edit` marks one who may also change its songs. Rows go with the
-- playlist and with the member.

CREATE TABLE catalog_playlist_members (
    playlist_id TEXT    NOT NULL REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    user_id     TEXT    NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    can_edit    INTEGER NOT NULL DEFAULT 0 CHECK (can_edit IN (0, 1)),
    added_at    INTEGER NOT NULL,
    PRIMARY KEY (playlist_id, user_id)
) STRICT;

CREATE INDEX idx_catalog_playlist_members_user ON catalog_playlist_members(user_id);

PRAGMA user_version = 149;
