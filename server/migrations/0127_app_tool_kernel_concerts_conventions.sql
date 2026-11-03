-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0127_app_tool_kernel_concerts_conventions.sql - plugin tick state, the
-- section preferences, the importer's records, the planner's record and the
-- concerts tables follow the schema conventions.
--
--   plugin_tick_state          same name
--   user_section_prefs         app_section_prefs
--   import_pending_links       same name
--   import_progress            same name
--   import_runs                same name
--   db_planner_stats           same name
--   artist_event_check         catalog_artist_event_checks
--   artist_skiddle_ids         catalog_artist_skiddle_ids
--   artist_skiddle_resolution  catalog_artist_skiddle_resolutions
--   artist_tm_attraction       catalog_artist_tm_attractions
--   live_event_feed            catalog_live_event_feed
--   user_event_cities          catalog_event_cities
--   user_event_seen            catalog_event_seen_marks
--
-- Each is rebuilt as a STRICT table, rowids and indexes carried (the rebuild
-- recipe in kernel/src/schema/boot.rs). No other table references these and
-- no trigger names them. What each row becomes:
--
--   * a time in REAL seconds or ISO text becomes integer milliseconds, rounded.
--     One that is no time becomes 0 (an artist's last check becomes NULL, so it
--     is checked first), and the row stays. A concert's starts_at is the
--     listing's own text, relayed as the source gave it, and stays text;
--   * a section preference gains a key to its user, cascading. One whose user
--     has no account is not copied: no one could read it. Its enabled flag
--     becomes a checked flag, anything but 0 reading as 1;
--   * an artist check's status outside ok and error becomes error, and an
--     import run's exit code outside the importer's codes becomes
--     FAILED_INTERNAL. The importer and the sweep only ever wrote those;
--   * a tick state value that is not bytes becomes its bytes;
--   * a number of the wrong type in a city or a feed row is cast. A city
--     whose coordinates or radius are not numbers is not copied: its read
--     fails, which hides every city of that user.
--
-- The planner's record goes: its statistics on these tables went with them,
-- and with no record of a run the next check takes them again.
--
-- A saved city or seen mark whose user is gone stays an orphan, as it was. Rows
-- with a key of the wrong type cannot be named by any read and are not copied.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE conventions_127_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_127 INTEGER CONSTRAINT guard_stamp_below_127 CHECK (stamp_below_127 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO conventions_127_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO conventions_127_guard (stamp_below_127)
    SELECT (SELECT user_version FROM pragma_user_version) < 127;

CREATE TEMP TABLE conventions_127_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.conventions_127_orphans_at ON conventions_127_orphans(child, row, parent);
-- The renamed tables' orphans report under their new names after the copy.
UPDATE conventions_127_orphans SET child = 'catalog_event_cities'
    WHERE child = 'user_event_cities';
UPDATE conventions_127_orphans SET child = 'catalog_event_seen_marks'
    WHERE child = 'user_event_seen';

-- Plugins -------------------------------------------------------------------

CREATE TABLE plugin_tick_state_new (
    plugin TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (plugin, key)
) STRICT;
INSERT INTO plugin_tick_state_new (rowid, plugin, key, value, updated_at)
SELECT rowid, plugin, key, CAST(value AS BLOB),
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM plugin_tick_state
WHERE typeof(plugin) = 'text' AND typeof(key) = 'text' AND value IS NOT NULL;

-- App -----------------------------------------------------------------------

CREATE TABLE app_section_prefs (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL,
    section_key TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, page, section_key)
) STRICT;
INSERT INTO app_section_prefs (rowid, user_id, page, section_key, enabled, updated_at)
SELECT rowid, user_id, page, section_key,
    CASE WHEN enabled != 0 THEN 1 ELSE 0 END,
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM user_section_prefs
WHERE user_id IN (SELECT id FROM auth_users)
    AND typeof(page) = 'text' AND typeof(section_key) = 'text';

-- Tool ----------------------------------------------------------------------

CREATE TABLE import_pending_links_new (
    target_table TEXT NOT NULL,
    target_key TEXT NOT NULL,
    ref_column TEXT NOT NULL,
    ref_kind TEXT NOT NULL
        CHECK (ref_kind IN ('track', 'album', 'artist', 'tombstone')),
    v2_id TEXT NOT NULL,
    mode TEXT NOT NULL
        CHECK (mode IN ('as_written', 'until_resolved', 'link_only')),
    PRIMARY KEY (target_table, target_key, ref_column)
) STRICT;
INSERT INTO import_pending_links_new (rowid, target_table, target_key, ref_column, ref_kind,
    v2_id, mode)
SELECT rowid, target_table, target_key, ref_column, ref_kind, CAST(v2_id AS TEXT), mode
FROM import_pending_links
WHERE typeof(target_table) = 'text' AND typeof(target_key) = 'text'
    AND typeof(ref_column) = 'text' AND v2_id IS NOT NULL;

CREATE TABLE import_progress_new (
    instance_id TEXT NOT NULL,
    section TEXT NOT NULL,
    export_digest TEXT NOT NULL,
    rows_written INTEGER NOT NULL,
    applied_at INTEGER NOT NULL,
    PRIMARY KEY (instance_id, section)
) STRICT;
INSERT INTO import_progress_new (rowid, instance_id, section, export_digest, rows_written,
    applied_at)
SELECT rowid, instance_id, section, CAST(export_digest AS TEXT),
    COALESCE(CAST(rows_written AS INTEGER), 0),
    COALESCE(CASE WHEN typeof(applied_at) IN ('integer', 'real')
        THEN CAST(round(applied_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(applied_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM import_progress
WHERE typeof(instance_id) = 'text' AND typeof(section) = 'text';

CREATE TABLE import_runs_new (
    id TEXT PRIMARY KEY NOT NULL,
    instance_id TEXT NOT NULL,
    exported_at INTEGER NOT NULL,
    exit_code TEXT NOT NULL CHECK (exit_code IN ('OK', 'OK_WITH_DROPS', 'FAILED_VALIDATION',
        'ENVELOPE_AUTH_FAILED', 'FAILED_INTERNAL')),
    entity_counts TEXT NOT NULL,
    applied_at INTEGER NOT NULL
) STRICT;
INSERT INTO import_runs_new (rowid, id, instance_id, exported_at, exit_code, entity_counts,
    applied_at)
SELECT rowid, id, CAST(instance_id AS TEXT),
    COALESCE(CASE WHEN typeof(exported_at) IN ('integer', 'real')
        THEN CAST(round(exported_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(exported_at, 'subsec') * 1000) AS INTEGER) END, 0),
    CASE WHEN exit_code IN ('OK', 'OK_WITH_DROPS', 'FAILED_VALIDATION',
        'ENVELOPE_AUTH_FAILED', 'FAILED_INTERNAL') THEN exit_code ELSE 'FAILED_INTERNAL' END,
    CAST(entity_counts AS TEXT),
    COALESCE(CASE WHEN typeof(applied_at) IN ('integer', 'real')
        THEN CAST(round(applied_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(applied_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM import_runs WHERE typeof(id) = 'text';

-- Kernel --------------------------------------------------------------------

CREATE TABLE db_planner_stats_new (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    analyzed_at INTEGER NOT NULL,
    indexed_tracks INTEGER NOT NULL
) STRICT;

-- Concerts ------------------------------------------------------------------

CREATE TABLE catalog_artist_event_checks (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    last_checked_at INTEGER,
    last_status TEXT CHECK (last_status IN ('ok', 'error')),
    last_error TEXT
) STRICT;
INSERT INTO catalog_artist_event_checks (rowid, artist_mbid_lower, last_checked_at,
    last_status, last_error)
SELECT rowid, artist_mbid_lower,
    CASE WHEN typeof(last_checked_at) IN ('integer', 'real')
        THEN CAST(round(last_checked_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(last_checked_at, 'subsec') * 1000) AS INTEGER) END,
    CASE WHEN last_status IS NULL OR last_status IN ('ok', 'error') THEN last_status
        ELSE 'error' END,
    CAST(last_error AS TEXT)
FROM artist_event_check WHERE typeof(artist_mbid_lower) = 'text';

CREATE TABLE catalog_artist_skiddle_ids (
    artist_mbid_lower TEXT NOT NULL,
    skiddle_artistid TEXT NOT NULL,
    PRIMARY KEY (artist_mbid_lower, skiddle_artistid)
) STRICT;
INSERT INTO catalog_artist_skiddle_ids (rowid, artist_mbid_lower, skiddle_artistid)
SELECT rowid, artist_mbid_lower, skiddle_artistid
FROM artist_skiddle_ids
WHERE typeof(artist_mbid_lower) = 'text' AND typeof(skiddle_artistid) = 'text';

CREATE TABLE catalog_artist_skiddle_resolutions (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    resolved_at INTEGER NOT NULL
) STRICT;
INSERT INTO catalog_artist_skiddle_resolutions (rowid, artist_mbid_lower, resolved_at)
SELECT rowid, artist_mbid_lower,
    COALESCE(CASE WHEN typeof(resolved_at) IN ('integer', 'real')
        THEN CAST(round(resolved_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(resolved_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM artist_skiddle_resolution WHERE typeof(artist_mbid_lower) = 'text';

CREATE TABLE catalog_artist_tm_attractions (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    attraction_id TEXT,
    match_basis TEXT NOT NULL CHECK (match_basis IN ('mbid', 'exact_name', 'none')),
    resolved_at INTEGER NOT NULL
) STRICT;
INSERT INTO catalog_artist_tm_attractions (rowid, artist_mbid_lower, attraction_id,
    match_basis, resolved_at)
SELECT rowid, artist_mbid_lower, CAST(attraction_id AS TEXT), match_basis,
    COALESCE(CASE WHEN typeof(resolved_at) IN ('integer', 'real')
        THEN CAST(round(resolved_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(resolved_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM artist_tm_attraction WHERE typeof(artist_mbid_lower) = 'text';

CREATE TABLE catalog_live_event_feed (
    source TEXT NOT NULL CHECK (source IN ('ticketmaster', 'skiddle')),
    source_event_id TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    event_name TEXT NOT NULL,
    venue_name TEXT,
    city TEXT,
    region TEXT,
    country_code TEXT,
    latitude REAL,
    longitude REAL,
    starts_at TEXT,
    local_date TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'cancelled', 'rescheduled')),
    ticket_url TEXT,
    match_confidence TEXT NOT NULL CHECK (match_confidence IN ('mbid', 'name')),
    discovered_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (source, source_event_id, artist_mbid_lower)
) STRICT;
INSERT INTO catalog_live_event_feed (rowid, source, source_event_id, artist_mbid_lower,
    artist_name, event_name, venue_name, city, region, country_code, latitude, longitude,
    starts_at, local_date, status, ticket_url, match_confidence, discovered_at, updated_at)
SELECT rowid, source, source_event_id, artist_mbid_lower,
    CAST(artist_name AS TEXT), CAST(event_name AS TEXT), CAST(venue_name AS TEXT),
    CAST(city AS TEXT), CAST(region AS TEXT), CAST(country_code AS TEXT),
    CASE WHEN typeof(latitude) IN ('integer', 'real') THEN CAST(latitude AS REAL) END,
    CASE WHEN typeof(longitude) IN ('integer', 'real') THEN CAST(longitude AS REAL) END,
    CAST(starts_at AS TEXT), CAST(local_date AS TEXT), status, CAST(ticket_url AS TEXT),
    match_confidence,
    COALESCE(CASE WHEN typeof(discovered_at) IN ('integer', 'real')
        THEN CAST(round(discovered_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(discovered_at, 'subsec') * 1000) AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM live_event_feed
WHERE typeof(source_event_id) = 'text' AND typeof(artist_mbid_lower) = 'text';

CREATE TABLE catalog_event_cities (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    city_name TEXT NOT NULL,
    country_code TEXT,
    latitude REAL NOT NULL,
    longitude REAL NOT NULL,
    radius_km REAL NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (user_id, latitude, longitude)
) STRICT;
INSERT INTO catalog_event_cities (rowid, user_id, city_name, country_code, latitude, longitude,
    radius_km, position)
SELECT rowid, user_id, CAST(city_name AS TEXT), CAST(country_code AS TEXT),
    CAST(latitude AS REAL), CAST(longitude AS REAL), CAST(radius_km AS REAL),
    COALESCE(CAST(position AS INTEGER), 0)
FROM user_event_cities
WHERE typeof(user_id) = 'text' AND typeof(latitude) IN ('integer', 'real')
    AND typeof(longitude) IN ('integer', 'real') AND typeof(radius_km) IN ('integer', 'real');

CREATE TABLE catalog_event_seen_marks (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at INTEGER NOT NULL
) STRICT;
INSERT INTO catalog_event_seen_marks (rowid, user_id, seen_at)
SELECT rowid, user_id,
    COALESCE(CASE WHEN typeof(seen_at) IN ('integer', 'real')
        THEN CAST(round(seen_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(seen_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM user_event_seen WHERE typeof(user_id) = 'text';

DROP TABLE plugin_tick_state;
DROP TABLE user_section_prefs;
DROP TABLE import_pending_links;
DROP TABLE import_progress;
DROP TABLE import_runs;
DROP TABLE db_planner_stats;
DROP TABLE artist_event_check;
DROP TABLE artist_skiddle_ids;
DROP TABLE artist_skiddle_resolution;
DROP TABLE artist_tm_attraction;
DROP TABLE live_event_feed;
DROP TABLE user_event_cities;
DROP TABLE user_event_seen;

ALTER TABLE plugin_tick_state_new RENAME TO plugin_tick_state;
ALTER TABLE import_pending_links_new RENAME TO import_pending_links;
ALTER TABLE import_progress_new RENAME TO import_progress;
ALTER TABLE import_runs_new RENAME TO import_runs;
ALTER TABLE db_planner_stats_new RENAME TO db_planner_stats;

CREATE INDEX idx_plugin_tick_state_plugin ON plugin_tick_state(plugin);
CREATE INDEX idx_import_pending_links_v2 ON import_pending_links(ref_kind, v2_id);
CREATE INDEX idx_import_runs_instance ON import_runs(instance_id);
CREATE INDEX idx_catalog_live_event_feed_artist ON catalog_live_event_feed(artist_mbid_lower);
CREATE INDEX idx_catalog_live_event_feed_date ON catalog_live_event_feed(local_date);

INSERT INTO conventions_127_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM conventions_127_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE conventions_127_guard;
DROP TABLE conventions_127_orphans;

-- Migration high-water mark.
PRAGMA user_version = 127;
COMMIT;
PRAGMA foreign_keys=ON;
