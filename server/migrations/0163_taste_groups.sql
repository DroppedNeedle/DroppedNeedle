-- A picture of each person's taste, worked out from their own history and
-- marks by one job, which rebuilds a person's rows whole. They are results,
-- not a read model: nothing keeps them by trigger, and forgetting a
-- person's history removes them. A person's rows go with their account.
--
-- `catalog_taste_builds` holds one row a person: when the rows were last
-- built, the plays and imported listens they were built from, how many
-- artists were played, how many are seeds, the span of the history read,
-- and whether something the build needed was not there to read, in which
-- case the groups before it were kept.
--
-- `catalog_taste_groups` holds the person's groups of seeds, largest share
-- first: each named after its anchor, its heaviest seed, with its share of
-- the seed weight, its share of the last 90 days of plays and the trend
-- the two make.
--
-- `catalog_taste_artists` holds every artist the person's own history and
-- marks name: the artists they know. `plays` counts their plays in the
-- app and `imported_listens` the listens imported from ListenBrainz, kept
-- apart since an imported listen is never a play. A seed may sit in one
-- group; `group_no` is null for every other artist.
-- `catalog_taste_artists_seeds` serves the reads of a person's seeds alone.
--
-- `catalog_known_release_groups` holds the release groups the person
-- played in the app; imported listens name none.

CREATE TABLE catalog_taste_builds (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    built_at INTEGER NOT NULL,
    plays_counted INTEGER NOT NULL CHECK (plays_counted >= 0),
    imported_listens_counted INTEGER NOT NULL CHECK (imported_listens_counted >= 0),
    artists_played INTEGER NOT NULL CHECK (artists_played >= 0),
    seeds INTEGER NOT NULL CHECK (seeds >= 0),
    history_from_at INTEGER,
    history_to_at INTEGER,
    incomplete INTEGER NOT NULL CHECK (incomplete IN (0, 1))
) STRICT;

CREATE TABLE catalog_taste_groups (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    group_no INTEGER NOT NULL CHECK (group_no >= 0),
    anchor_artist_mbid TEXT NOT NULL,
    anchor_name TEXT NOT NULL,
    share REAL NOT NULL,
    recent_share REAL NOT NULL,
    trend TEXT NOT NULL CHECK (trend IN ('rising', 'steady', 'fading')),
    PRIMARY KEY (user_id, group_no)
) STRICT;

CREATE TABLE catalog_taste_artists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid_lower TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    name TEXT NOT NULL,
    plays INTEGER NOT NULL CHECK (plays >= 0),
    imported_listens INTEGER NOT NULL CHECK (imported_listens >= 0),
    months_active INTEGER NOT NULL CHECK (months_active >= 0),
    last_played_at INTEGER,
    weight REAL NOT NULL,
    is_seed INTEGER NOT NULL CHECK (is_seed IN (0, 1)),
    group_no INTEGER CHECK (group_no >= 0),
    PRIMARY KEY (user_id, artist_mbid_lower)
) STRICT;

CREATE INDEX catalog_taste_artists_seeds
    ON catalog_taste_artists (user_id, artist_mbid_lower) WHERE is_seed = 1;

CREATE TABLE catalog_known_release_groups (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    plays INTEGER NOT NULL CHECK (plays > 0),
    last_played_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

PRAGMA user_version = 163;
