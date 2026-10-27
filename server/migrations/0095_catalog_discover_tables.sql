-- 0095_catalog_discover_tables.sql - discover's tables, rebuilt to the
-- schema conventions.
--
--   catalog_cached_pages (new)
--       one built page per user and kind: the discover page, home's
--       provider shelves, the queue deck, and the Daily Mixes and Top
--       Picks a discover build reuses. The row is the only copy. source,
--       listenbrainz and lastfm say what it was built for, and built_at
--       ages it; the library's catalog revision plays no part.
--   catalog_page_generations (new)
--       one row: a counter every clear of the page cache bumps. A build
--       keeps its page only under the generation it started in, so a
--       clear wins over a build already running.
--   discovery_snapshots (dropped)
--       the saved pages and decks it replaces. Not carried: each one is
--       built again on the next visit or warm pass.
--   catalog_discover_activity (from discovery_activity)
--       which features a user used in the last day, for the warm cycle.
--       Not carried: the next visit records it again.
--   catalog_ignored_releases (from ignored_releases)
--       the releases each user hid from the queue. Carried, with the time
--       in integer milliseconds. Rows of users who no longer exist go.
--   catalog_mbid_resolutions (from mbid_resolution_map)
--       remembered Last.fm release to release-group answers. Carried.
--
-- Every per-user row goes with its user.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE discovery_snapshots;
DROP TABLE discovery_activity;

CREATE TABLE catalog_cached_pages (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL
        CHECK (page IN ('discover', 'home', 'queue', 'daily_mixes', 'top_picks')),
    source TEXT NOT NULL,
    listenbrainz INTEGER NOT NULL CHECK (listenbrainz IN (0, 1)),
    lastfm INTEGER NOT NULL CHECK (lastfm IN (0, 1)),
    payload BLOB NOT NULL,
    built_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, page)
) STRICT;

CREATE TABLE catalog_page_generations (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation INTEGER NOT NULL
) STRICT;

INSERT INTO catalog_page_generations (singleton, generation) VALUES (1, 0);

CREATE TABLE catalog_discover_activity (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    feature TEXT NOT NULL CHECK (feature IN ('home', 'discover', 'queue', 'artist')),
    artist_mbid TEXT NOT NULL DEFAULT '',
    section TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    used_at INTEGER NOT NULL,
    retry_at INTEGER NOT NULL DEFAULT 0,
    serviced_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, feature, artist_mbid, section, provider)
) STRICT;

CREATE INDEX idx_catalog_discover_activity_due
    ON catalog_discover_activity(source, retry_at, serviced_at);
CREATE INDEX idx_catalog_discover_activity_used
    ON catalog_discover_activity(used_at);

CREATE TABLE catalog_ignored_releases (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    release_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    ignored_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

CREATE INDEX idx_catalog_ignored_releases_ignored
    ON catalog_ignored_releases(ignored_at);

INSERT INTO catalog_ignored_releases (user_id, release_group_mbid_lower,
    release_group_mbid, artist_mbid, release_name, artist_name, ignored_at)
SELECT user_id, release_group_mbid_lower, release_group_mbid, artist_mbid,
    release_name, artist_name, CAST(ignored_at * 1000 AS INTEGER)
FROM ignored_releases
WHERE user_id IN (SELECT id FROM auth_users);

DROP TABLE ignored_releases;

CREATE TABLE catalog_mbid_resolutions (
    source_mbid_lower TEXT PRIMARY KEY,
    source_mbid TEXT NOT NULL,
    release_group_mbid TEXT
) STRICT;

INSERT INTO catalog_mbid_resolutions (source_mbid_lower, source_mbid, release_group_mbid)
SELECT source_mbid_lower, source_mbid, release_group_mbid FROM mbid_resolution_map;

DROP TABLE mbid_resolution_map;

-- Migration high-water mark.
PRAGMA user_version = 95;
