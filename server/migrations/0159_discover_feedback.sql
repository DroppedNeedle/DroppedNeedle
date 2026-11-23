-- What a person told Discover about an album or an artist, one row per
-- person and target: a second answer about the same album replaces the
-- first. `no` and `know_it` hide the album until undone, `not_now` until
-- `expires_at`, and `not_artist` (the one kind whose target is an artist)
-- every album and tile filed under that artist until undone. A row is in
-- effect while `expires_at` is null or still ahead. A person's rows go
-- with their account.
--
-- Every album hidden before this change carries over as a `no` that keeps
-- its old expiry, a year after it was hidden, and the old table goes.

CREATE TABLE catalog_discover_feedback (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    target TEXT NOT NULL CHECK (target IN ('album', 'artist')),
    target_mbid_lower TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('no', 'know_it', 'not_now', 'not_artist')),
    release_group_mbid TEXT NOT NULL DEFAULT '',
    artist_mbid TEXT NOT NULL DEFAULT '',
    release_name TEXT NOT NULL DEFAULT '',
    artist_name TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    PRIMARY KEY (user_id, target, target_mbid_lower),
    CHECK ((target = 'artist') = (kind = 'not_artist'))
) STRICT;

CREATE INDEX idx_catalog_discover_feedback_expires
    ON catalog_discover_feedback(expires_at);

INSERT INTO catalog_discover_feedback (user_id, target, target_mbid_lower, kind,
    release_group_mbid, artist_mbid, release_name, artist_name, created_at, expires_at)
SELECT user_id, 'album', release_group_mbid_lower, 'no', release_group_mbid, artist_mbid,
    release_name, artist_name, ignored_at, ignored_at + 31536000000
FROM catalog_ignored_releases;

DROP INDEX idx_catalog_ignored_releases_ignored;
DROP TABLE catalog_ignored_releases;

PRAGMA user_version = 159;
