-- 0046_edition_remap_place_only.sql - a remap that only places files.
--
-- library_edition_remap_queue (0026) knew two origins, both a choice to
-- seal: 'album_pin' (a person chose this album's edition) and
-- 'group_pin' (the choice came from a release-group pin). A third,
-- 'place_only', is an album that already has its edition: the worker
-- puts the album's files on that release and changes nothing else (who
-- chose it, "leave unmanaged", the custom edition and the undo stay).
--
-- SQLite cannot widen a CHECK in place, so the table is rebuilt with the
-- same columns, types, defaults and foreign key, and every row is copied.
-- No other table references it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_edition_remap_queue_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    chosen_by_user_id TEXT,
    queued_at REAL NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before REAL NOT NULL DEFAULT 0,
    last_code TEXT,
    -- 'album_pin': a person chose this album's edition; 'group_pin': the
    -- album got the choice from a release-group pin covering every copy;
    -- 'place_only': the album has its edition, only its files are placed.
    origin TEXT NOT NULL DEFAULT 'album_pin'
        CHECK(origin IN ('album_pin', 'group_pin', 'place_only'))
);

INSERT INTO library_edition_remap_queue_new
    SELECT local_album_id, release_mbid, chosen_by_user_id, queued_at, attempts,
           not_before, last_code, origin
    FROM library_edition_remap_queue;

DROP TABLE library_edition_remap_queue;
ALTER TABLE library_edition_remap_queue_new RENAME TO library_edition_remap_queue;

-- Migration high-water mark.
PRAGMA user_version = 46;
