-- Each person's weekly set: up to eight albums chosen for them once a week
-- from their pool, kept with what the set was made from, and their set day.
--
-- `catalog_weekly_sets` holds one row a person and week: when the week
-- starts (00:00 server time on their set day), when the set was built, why
-- it is short when it is (null for a full set), and the counts the set's
-- receipt prints, copied at build from the taste and pool builds and the
-- library and never worked out again: the plays and listens counted and
-- the span they cover, the seeds, the neighbours found, the known and the
-- hubs left out (null when that rule did not run), the artists and albums
-- on the shelves, whether a history import was current, and `tastes`, a
-- versioned JSON list of the taste groups that got a slot (name, anchor,
-- share, picks).
--
-- `catalog_weekly_set_picks` holds each set's picks in order: the album as
-- it was offered, the taste it is filed under (`taste_no`, an entry of the
-- set's `tastes`; null is "further out"), and its reason and evidence as
-- built that day, so a past week reads as it was shown. What became of a
-- pick is read from the pick record, the requests, the library and what
-- the person told Discover, never stored here.
--
-- A set is built once by one job and is a record from then on: nothing
-- rebuilds it. Forgetting a person's history removes their sets; a
-- person's rows go with their account.
--
-- `catalog_discover_prefs` holds a person's set day (no row is Friday),
-- and when their first set began waiting for their history import, which
-- it waits on for six hours at most. Forgetting clears that time and keeps
-- the day.

CREATE TABLE catalog_weekly_sets (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    week_start_at INTEGER NOT NULL,
    built_at INTEGER NOT NULL,
    short_reason TEXT
        CHECK (short_reason IN ('short_history', 'shelves_only', 'rest_known', 'not_checked')),
    plays_counted INTEGER NOT NULL CHECK (plays_counted >= 0),
    history_from_at INTEGER,
    history_to_at INTEGER,
    seeds INTEGER NOT NULL CHECK (seeds >= 0),
    neighbours_found INTEGER CHECK (neighbours_found >= 0),
    known_left_out INTEGER CHECK (known_left_out >= 0),
    hubs_left_out INTEGER CHECK (hubs_left_out >= 0),
    shelf_artists INTEGER NOT NULL CHECK (shelf_artists >= 0),
    shelf_albums INTEGER NOT NULL CHECK (shelf_albums >= 0),
    linked INTEGER NOT NULL CHECK (linked IN (0, 1)),
    tastes TEXT NOT NULL,
    PRIMARY KEY (user_id, week_start_at)
) STRICT;

CREATE TABLE catalog_weekly_set_picks (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    week_start_at INTEGER NOT NULL,
    pick_no INTEGER NOT NULL CHECK (pick_no BETWEEN 1 AND 8),
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    title TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    primary_type TEXT,
    first_release_date TEXT,
    taste_no INTEGER CHECK (taste_no >= 0),
    reason_line TEXT NOT NULL,
    evidence TEXT NOT NULL,
    PRIMARY KEY (user_id, week_start_at, pick_no),
    FOREIGN KEY (user_id, week_start_at)
        REFERENCES catalog_weekly_sets(user_id, week_start_at) ON DELETE CASCADE
) STRICT;

CREATE INDEX catalog_weekly_set_picks_album
    ON catalog_weekly_set_picks (user_id, release_group_mbid_lower);

CREATE TABLE catalog_discover_prefs (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    set_day TEXT NOT NULL DEFAULT 'friday'
        CHECK (set_day IN ('monday', 'tuesday', 'wednesday', 'thursday', 'friday', 'saturday',
            'sunday')),
    updated_at INTEGER NOT NULL,
    history_wait_from_at INTEGER
) STRICT;

PRAGMA user_version = 172;
