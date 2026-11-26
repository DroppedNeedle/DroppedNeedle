-- The artists each person named as ones they love, one row a person and
-- artist, with the name as it was shown and when it was named. Each is one
-- more seed for Discover; naming one is not a follow and starts no watch
-- and no request. Forgetting a person's history leaves the list, as it
-- leaves the rest of what they told Discover; a person's rows go with
-- their account.

CREATE TABLE catalog_stated_artists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid_lower TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    stated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, artist_mbid_lower)
) STRICT;

PRAGMA user_version = 177;
