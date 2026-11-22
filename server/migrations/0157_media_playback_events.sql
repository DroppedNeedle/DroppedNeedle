-- How each playback ended, per person: what played, how long it was heard,
-- how it ended and what it was started from. A second record beside
-- library_play_history, never read for a play count or a statistic.
-- `local_track_id` names the library track it was, with no foreign key: an
-- event is history and must not hold back the library's merges and
-- removals. A person's events go with their account.

CREATE TABLE media_playback_events (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    local_track_id TEXT,
    recording_mbid TEXT,
    release_group_mbid TEXT,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    source TEXT NOT NULL
        CHECK (source IN ('local', 'youtube', 'jellyfin', 'navidrome', 'plex', 'plugin')),
    client TEXT NOT NULL CHECK (client IN ('web', 'subsonic', 'jellyfin')),
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL,
    listened_ms INTEGER NOT NULL CHECK (listened_ms >= 0),
    duration_ms INTEGER CHECK (duration_ms > 0),
    ended_by TEXT NOT NULL
        CHECK (ended_by IN ('finished', 'skipped', 'stopped', 'failed', 'unknown')),
    came_from TEXT NOT NULL
        CHECK (came_from IN ('album', 'artist', 'playlist', 'radio', 'mix', 'search',
            'library', 'unknown', 'weekly_set', 'dig_further', 'later_pile')),
    came_from_id TEXT,
    counted INTEGER NOT NULL CHECK (counted IN (0, 1))
) STRICT;

CREATE INDEX idx_media_playback_events_user_ended
    ON media_playback_events (user_id, ended_at);

PRAGMA user_version = 157;
