-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0118_catalog_playlists_conventions.sql - playlists follow the schema
-- conventions and belong to their owner.
--
-- playlists, playlist_tracks and playlist_covers become catalog_playlists,
-- catalog_playlist_tracks and catalog_playlist_covers, STRICT, rowids carried
-- (the rebuild recipe in kernel/src/schema/boot.rs). No trigger or view names
-- them. A playlist cascades from its owner from now on, so deleting a user
-- deletes their playlists, entries and covers.
--
-- What each row becomes:
--
--   * an owner with no account row becomes NULL: an ownerless stray, which an
--     admin may delete. This migration deletes no playlist for that;
--   * a time in ISO text, whole seconds or v2's fractional and offset forms,
--     becomes integer milliseconds. One no form can read becomes 0, which is
--     what readers already showed for it. A cover's REAL seconds are rounded
--     to milliseconds;
--   * duration in seconds becomes duration_ms, rounded. One that is no number
--     becomes NULL;
--   * is_public becomes 0 or 1, by the != 0 test readers already apply;
--   * a track or disc number that is no integer becomes NULL, since no read
--     could return the playlist holding it;
--   * a playlist with an entry whose position is no integer is renumbered from
--     0: its other entries in their order, then that entry, for the same reason;
--   * cover_image_path goes: v3 keeps cover bytes in the covers table and only
--     ever wrote NULL there.
--
-- Rows with a key of the wrong type (a NULL or BLOB id) cannot be named by any
-- read and are not copied, nor are entries and covers with no playlist. A BLOB
-- source_ref becomes NULL, so that casting it cannot make two rows clash. No
-- other bad value can refuse the upgrade.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE catalog_playlists_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_118 INTEGER CONSTRAINT guard_stamp_below_118 CHECK (stamp_below_118 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO catalog_playlists_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO catalog_playlists_guard (stamp_below_118)
    SELECT (SELECT user_version FROM pragma_user_version) < 118;

CREATE TEMP TABLE catalog_playlists_orphans AS
    SELECT "table" AS child, rowid AS row, parent, fkid FROM pragma_foreign_key_check;

CREATE TABLE catalog_playlists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source_ref TEXT,
    user_id TEXT REFERENCES auth_users(id) ON DELETE CASCADE,
    is_public INTEGER NOT NULL DEFAULT 0 CHECK (is_public IN (0, 1))
) STRICT;
INSERT INTO catalog_playlists (rowid, id, name, created_at, updated_at, source_ref, user_id,
    is_public)
SELECT rowid, id, CAST(name AS TEXT),
    COALESCE(CAST(round(unixepoch(created_at, 'subsec') * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(unixepoch(updated_at, 'subsec') * 1000) AS INTEGER), 0),
    CASE WHEN typeof(source_ref) != 'blob' THEN source_ref END,
    CASE WHEN user_id IN (SELECT id FROM auth_users) THEN user_id END,
    CASE WHEN is_public != 0 THEN 1 ELSE 0 END
FROM playlists WHERE typeof(id) = 'text';
CREATE UNIQUE INDEX idx_catalog_playlists_user_source_ref
    ON catalog_playlists(user_id, source_ref) WHERE source_ref IS NOT NULL;
CREATE INDEX idx_catalog_playlists_user ON catalog_playlists(user_id);

CREATE TABLE catalog_playlist_tracks (
    id TEXT PRIMARY KEY,
    playlist_id TEXT NOT NULL REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT NOT NULL,
    album_id TEXT,
    artist_id TEXT,
    track_source_id TEXT,
    cover_url TEXT,
    source_type TEXT NOT NULL,
    available_sources TEXT,
    format TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration_ms INTEGER,
    created_at INTEGER NOT NULL,
    plex_rating_key TEXT,
    library_file_id TEXT,
    UNIQUE(playlist_id, position)
) STRICT;
INSERT INTO catalog_playlist_tracks (rowid, id, playlist_id, position, track_name, artist_name,
    album_name, album_id, artist_id, track_source_id, cover_url, source_type, available_sources,
    format, track_number, disc_number, duration_ms, created_at, plex_rating_key, library_file_id)
SELECT t.rowid, t.id, t.playlist_id,
    CASE WHEN EXISTS (SELECT 1 FROM playlist_tracks m
            WHERE m.playlist_id = t.playlist_id AND typeof(m.position) != 'integer')
        THEN ROW_NUMBER() OVER (PARTITION BY t.playlist_id
            ORDER BY typeof(t.position) != 'integer',
                CASE WHEN typeof(t.position) = 'integer' THEN t.position END, t.rowid) - 1
        ELSE t.position
    END,
    CAST(t.track_name AS TEXT), CAST(t.artist_name AS TEXT), CAST(t.album_name AS TEXT),
    CAST(t.album_id AS TEXT), CAST(t.artist_id AS TEXT), CAST(t.track_source_id AS TEXT),
    CAST(t.cover_url AS TEXT), CAST(t.source_type AS TEXT), CAST(t.available_sources AS TEXT),
    CAST(t.format AS TEXT),
    CASE WHEN typeof(t.track_number) = 'integer' THEN t.track_number END,
    CASE WHEN typeof(t.disc_number) = 'integer' THEN t.disc_number END,
    CASE WHEN typeof(t.duration) IN ('integer', 'real')
        THEN CAST(round(t.duration * 1000) AS INTEGER) END,
    COALESCE(CAST(round(unixepoch(t.created_at, 'subsec') * 1000) AS INTEGER), 0),
    CAST(t.plex_rating_key AS TEXT), CAST(t.library_file_id AS TEXT)
FROM playlist_tracks t
WHERE typeof(t.id) = 'text' AND t.playlist_id IN (SELECT id FROM catalog_playlists);
CREATE INDEX idx_catalog_playlist_tracks_library_file
    ON catalog_playlist_tracks(library_file_id) WHERE library_file_id IS NOT NULL;

CREATE TABLE catalog_playlist_covers (
    playlist_id TEXT PRIMARY KEY REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    image BLOB NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
INSERT INTO catalog_playlist_covers (rowid, playlist_id, content_type, image, updated_at)
SELECT rowid, playlist_id, CAST(content_type AS TEXT), CAST(image AS BLOB),
    CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER) ELSE 0 END
FROM playlist_covers WHERE playlist_id IN (SELECT id FROM catalog_playlists);

DROP TABLE playlist_covers;
DROP TABLE playlist_tracks;
DROP TABLE playlists;

INSERT INTO catalog_playlists_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, fkid, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3, 4) AS now
    WHERE now.n > (SELECT COUNT(*) FROM catalog_playlists_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent AND before.fkid = now.fkid));

DROP TABLE catalog_playlists_guard;
DROP TABLE catalog_playlists_orphans;

-- Migration high-water mark.
PRAGMA user_version = 118;
COMMIT;
PRAGMA foreign_keys=ON;
