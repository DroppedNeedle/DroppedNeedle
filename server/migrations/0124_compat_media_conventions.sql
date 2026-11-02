-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0124_compat_media_conventions.sql - saved play queues, bookmarks and the
-- listening and Navidrome folder preferences follow the schema conventions.
--
--   compat_bookmarks                   same name
--   compat_play_queues                 same name
--   compat_play_queue_items            same name
--   user_listening_prefs               same name
--   user_navidrome_folder_preferences  media_navidrome_folder_preferences
--
-- Each is rebuilt as a STRICT table, rowids carried (the rebuild recipe in
-- kernel/src/schema/boot.rs). What each row becomes:
--
--   * a time in REAL seconds or ISO text becomes integer milliseconds, rounded.
--     One that is no time becomes 0, and the row stays;
--   * a listening preference's flags become checked flags: anything but 0 reads
--     as 1, as the reads already took it;
--   * a primary source or now-playing visibility outside the allowed values
--     becomes the default (listenbrainz, full), which is how the live feed
--     already showed unknown visibility text. A visibility that is not text
--     failed its read, which hides the song, so it becomes track_hidden;
--   * a queue position that is not a whole number becomes one by CAST. It only
--     resumes a track.
--
-- file_id stays without a foreign key to local_tracks. No code deletes a track
-- row while its file is offline, or at all: a missing track keeps its row. The
-- key still cannot hold: the migration tool writes v2 track ids here before the
-- library has those tracks (they wait as pending links), so a key would refuse
-- them. And deleting a queue item by cascade would leave gaps in item_index and
-- a current_index pointing at the wrong track; the scan's sweep rewrites both.
--
-- Every row is carried, an orphan included: the guard below leaves one as it was.
-- The one exception is a key of the wrong type, which no statement can name and
-- no column can now hold.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE compat_media_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_124 INTEGER CONSTRAINT guard_stamp_below_124 CHECK (stamp_below_124 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO compat_media_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO compat_media_guard (stamp_below_124)
    SELECT (SELECT user_version FROM pragma_user_version) < 124;

CREATE TEMP TABLE compat_media_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.compat_media_orphans_at ON compat_media_orphans(child, row, parent);
-- The renamed table's orphans report under its new name after the copy.
UPDATE compat_media_orphans SET child = 'media_navidrome_folder_preferences'
    WHERE child = 'user_navidrome_folder_preferences';

CREATE TABLE compat_bookmarks_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, file_id)
) STRICT;
INSERT INTO compat_bookmarks_new (rowid, user_id, file_id, position_ms, comment, created_at,
    changed_at)
SELECT rowid, user_id, file_id, CAST(position_ms AS INTEGER), CAST(comment AS TEXT),
    COALESCE(CASE WHEN typeof(created_at) IN ('integer', 'real')
        THEN CAST(round(created_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(created_at, 'subsec') * 1000) AS INTEGER) END, 0),
    COALESCE(CASE WHEN typeof(changed_at) IN ('integer', 'real')
        THEN CAST(round(changed_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(changed_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM compat_bookmarks WHERE typeof(user_id) = 'text' AND typeof(file_id) = 'text';

CREATE TABLE compat_play_queues_new (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    changed_by_client TEXT NOT NULL DEFAULT ''
) STRICT;
INSERT INTO compat_play_queues_new (rowid, user_id, current_index, position_ms, updated_at,
    changed_by_client)
SELECT rowid, user_id, CAST(current_index AS INTEGER), CAST(position_ms AS INTEGER),
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0),
    CAST(changed_by_client AS TEXT)
FROM compat_play_queues WHERE typeof(user_id) = 'text';

-- Its REFERENCES names the final table, which the rename below brings back.
CREATE TABLE compat_play_queue_items_new (
    user_id TEXT NOT NULL REFERENCES compat_play_queues(user_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    PRIMARY KEY (user_id, item_index)
) STRICT;
INSERT INTO compat_play_queue_items_new (rowid, user_id, item_index, file_id)
SELECT rowid, user_id, item_index, file_id
FROM compat_play_queue_items
WHERE typeof(user_id) = 'text' AND typeof(item_index) = 'integer' AND typeof(file_id) = 'text';

CREATE TABLE user_listening_prefs_new (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0 CHECK (scrobble_to_lastfm IN (0, 1)),
    scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0
        CHECK (scrobble_to_listenbrainz IN (0, 1)),
    navidrome_handles_external_scrobbles INTEGER NOT NULL DEFAULT 1
        CHECK (navidrome_handles_external_scrobbles IN (0, 1)),
    primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz'
        CHECK (primary_music_source IN ('listenbrainz', 'lastfm')),
    now_playing_visibility TEXT NOT NULL DEFAULT 'full'
        CHECK (now_playing_visibility IN ('full', 'track_hidden', 'offline')),
    updated_at INTEGER NOT NULL,
    auto_request_personal_mix INTEGER NOT NULL DEFAULT 0
        CHECK (auto_request_personal_mix IN (0, 1))
) STRICT;
INSERT INTO user_listening_prefs_new (rowid, user_id, scrobble_to_lastfm,
    scrobble_to_listenbrainz, navidrome_handles_external_scrobbles, primary_music_source,
    now_playing_visibility, updated_at, auto_request_personal_mix)
SELECT rowid, user_id,
    CASE WHEN scrobble_to_lastfm != 0 THEN 1 ELSE 0 END,
    CASE WHEN scrobble_to_listenbrainz != 0 THEN 1 ELSE 0 END,
    CASE WHEN navidrome_handles_external_scrobbles != 0 THEN 1 ELSE 0 END,
    CASE WHEN primary_music_source IN ('listenbrainz', 'lastfm')
        THEN primary_music_source ELSE 'listenbrainz' END,
    CASE WHEN typeof(now_playing_visibility) <> 'text' THEN 'track_hidden'
        WHEN now_playing_visibility IN ('full', 'track_hidden', 'offline')
        THEN now_playing_visibility ELSE 'full' END,
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0),
    CASE WHEN auto_request_personal_mix != 0 THEN 1 ELSE 0 END
FROM user_listening_prefs WHERE typeof(user_id) = 'text';

CREATE TABLE media_navidrome_folder_preferences (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('all', 'selected')),
    selected_ids_json TEXT NOT NULL DEFAULT '[]',
    server_identity TEXT,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO media_navidrome_folder_preferences (rowid, user_id, mode, selected_ids_json,
    server_identity, updated_at)
SELECT rowid, user_id, mode, CAST(selected_ids_json AS TEXT), CAST(server_identity AS TEXT),
    COALESCE(CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER)
        ELSE CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER) END, 0)
FROM user_navidrome_folder_preferences WHERE typeof(user_id) = 'text';

DROP TABLE compat_bookmarks;
DROP TABLE compat_play_queue_items;
DROP TABLE compat_play_queues;
DROP TABLE user_listening_prefs;
DROP TABLE user_navidrome_folder_preferences;

ALTER TABLE compat_bookmarks_new RENAME TO compat_bookmarks;
ALTER TABLE compat_play_queues_new RENAME TO compat_play_queues;
ALTER TABLE compat_play_queue_items_new RENAME TO compat_play_queue_items;
ALTER TABLE user_listening_prefs_new RENAME TO user_listening_prefs;

-- The planner's statistics on these tables went with them. With no record of
-- a run, the next check takes them again.
DELETE FROM db_planner_stats;

INSERT INTO compat_media_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM compat_media_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE compat_media_guard;
DROP TABLE compat_media_orphans;

-- Migration high-water mark.
PRAGMA user_version = 124;
COMMIT;
PRAGMA foreign_keys=ON;
