-- Each person's lists on rotation: playlists the app makes from their own
-- library and keeps fresh, five live at a time.
--
-- `catalog_rotation_lists` holds one row a person and idea once the app has
-- looked at that idea for them: how many songs matched at its last look
-- (`match_count`, at `checked_at`), the playlist it fills while it is live
-- (set null when the playlist goes, by the app or by the person), whether
-- the person keeps it on rotation (`pinned`), the decade the decade idea
-- shows, and when it was last live. The playlist is an ordinary
-- `catalog_playlists` row; this row is what makes it a list on rotation, so
-- no playlist can claim to be one by its `source_ref`.
--
-- `catalog_rotation_prefs` holds a person's switch. No row is on.
--
-- A person's rows go with their account. Forgetting a person's history
-- keeps them: they are worked out again from plays and marks Forget keeps.

CREATE TABLE catalog_rotation_lists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    idea TEXT NOT NULL CHECK (idea IN ('added_recently', 'on_repeat',
        'forgotten_favourites', 'deep_cuts', 'five_stars', 'never_played',
        'followed_artists', 'lossless', 'a_year_ago', 'decade', 'long_ones')),
    decade INTEGER CHECK (decade IS NULL OR (decade >= 0 AND decade % 10 = 0)),
    playlist_id TEXT REFERENCES catalog_playlists(id) ON DELETE SET NULL,
    pinned INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
    match_count INTEGER NOT NULL DEFAULT 0 CHECK (match_count >= 0),
    last_live_at INTEGER,
    checked_at INTEGER,
    PRIMARY KEY (user_id, idea)
) STRICT;

CREATE UNIQUE INDEX idx_catalog_rotation_lists_playlist
    ON catalog_rotation_lists(playlist_id);

CREATE TABLE catalog_rotation_prefs (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    updated_at INTEGER NOT NULL
) STRICT;

PRAGMA user_version = 176;
