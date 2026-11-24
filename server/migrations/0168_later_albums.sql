-- Each person's Later pile: albums they want to keep for another day, one
-- row a person and release group, with the album's title and artist for
-- the list and when it went on. An album the library comes to own leaves
-- the list, and its row goes at the person's next change to the pile.
-- Forgetting a person's history leaves the pile, as it leaves the rest of
-- what they told Discover; a person's rows go with their account.

CREATE TABLE catalog_later_albums (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    title TEXT NOT NULL,
    artist_mbid TEXT,
    artist_name TEXT NOT NULL,
    added_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

PRAGMA user_version = 168;
