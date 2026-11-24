-- Playlists kept in sync with Spotify, one row per playlist that has ever
-- had sync turned on (the row stays when it is turned off, with its grant).
-- `snapshot_id` is Spotify's version of the playlist the last full read
-- saw (NULL for Liked Songs, which has none). `next_check_at` is when the
-- due tick queues the next pass. `failure` is why the last pass did not
-- reach Spotify, `hold` why requests wait. The grant columns are a plain
-- user's standing approval for this playlist; admins and trusted users
-- hold it by role and never get a row value.

CREATE TABLE acquire_playlist_syncs (
    playlist_id            TEXT    PRIMARY KEY REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    enabled                INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    whole_albums           INTEGER NOT NULL DEFAULT 0 CHECK (whole_albums IN (0, 1)),
    enabled_at             INTEGER NOT NULL,
    snapshot_id            TEXT    CHECK (snapshot_id IS NULL OR length(snapshot_id) BETWEEN 1 AND 128),
    read_at                INTEGER,
    checked_at             INTEGER,
    next_check_at          INTEGER NOT NULL,
    failures               INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    failure                TEXT    CHECK (failure IN ('not_linked', 'needs_reconnect',
                               'playlist_gone', 'unavailable', 'rate_limited', 'save_failed')),
    hold                   TEXT    CHECK (hold IN ('quota', 'storage')),
    grant_state            TEXT    CHECK (grant_state IN ('pending', 'approved', 'rejected', 'revoked')),
    grant_requested_at     INTEGER,
    grant_reviewed_by_id   TEXT    REFERENCES auth_users(id) ON DELETE SET NULL,
    grant_reviewed_by_name TEXT,
    grant_reviewed_at      INTEGER
) STRICT;

CREATE INDEX idx_acquire_playlist_syncs_due
    ON acquire_playlist_syncs(next_check_at) WHERE enabled = 1;

CREATE INDEX idx_acquire_playlist_syncs_reviewer
    ON acquire_playlist_syncs(grant_reviewed_by_id) WHERE grant_reviewed_by_id IS NOT NULL;

CREATE INDEX idx_acquire_playlist_syncs_grants
    ON acquire_playlist_syncs(grant_state) WHERE grant_state IN ('pending', 'approved');

PRAGMA user_version = 170;
