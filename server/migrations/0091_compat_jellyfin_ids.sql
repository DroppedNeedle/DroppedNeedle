-- 0091_compat_jellyfin_ids.sql - every Jellyfin id the server hands out is
-- stored, so an id an app cached before a restart resolves without reading
-- the catalog.
--
--   compat_jellyfin_ids (new)
--       jellyfin_id   the 32 lowercase hex characters an app holds.
--       kind          what it names: album, artist, track, genre, playlist
--                     or the library tile.
--       internal_id   the local id it names (a genre's slug). For an alias
--                     it is the alias text itself, never the item that
--                     holds it now, which is looked up at read time.
--       current       1 for the id handed out for the item today, 0 for an
--                     alias that still resolves (an album's or artist's old
--                     unkeyed id, the ids of a merged-away album or artist).
--
-- One column names rows of several tables and an alias outlives its row,
-- so there is no foreign key. Rows are only added, never deleted: a stored
-- id keeps naming its item, and a change of the data key never changes it.
-- The table starts empty; the server's fill loop writes it in chunks.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE compat_jellyfin_ids (
    jellyfin_id TEXT NOT NULL PRIMARY KEY
        CHECK (length(jellyfin_id) = 32 AND jellyfin_id NOT GLOB '*[^0-9a-f]*'),
    kind TEXT NOT NULL
        CHECK (kind IN ('album', 'artist', 'track', 'genre', 'playlist', 'library')),
    internal_id TEXT NOT NULL,
    current INTEGER NOT NULL CHECK (current IN (0, 1))
) STRICT, WITHOUT ROWID;

CREATE UNIQUE INDEX idx_compat_jellyfin_ids_current
    ON compat_jellyfin_ids (kind, internal_id) WHERE current = 1;

-- Migration high-water mark.
PRAGMA user_version = 91;
