-- 0043_library_retag_pairs.sql - two album rows that look like one retagged album.
--
-- When every file of an album is retagged, a scan carries the album's row
-- over to the new name. Where it will not do that by itself (most of the
-- album's tracks are long gone, a new file arrived with the retag, two
-- people chose different editions, work was started on the new row), the
-- old row keeps the match, the favorites and the id, and the files sit on
-- a new row. This table records the two rows and why, so the pair can be
-- shown to a person and joined or dismissed.
--
--   old_album_id  the row the files left
--   new_album_id  the row they sit on now
--   reason        why the scan did not join them
--   state         open until a person joins or dismisses the pair
--
-- Both ids cascade: a pair goes with either of its albums, and never
-- keeps an album row from being deleted. Times are milliseconds.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_retag_pairs (
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    new_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason TEXT NOT NULL
        CHECK(reason IN ('mostly_missing','new_files_added','tracks_elsewhere','chosen_matches_differ','custom_edition','live_work','work_on_new_row','store_error')),
    state TEXT NOT NULL
        CHECK(state IN ('open','joined','dismissed')),
    recorded_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (old_album_id, new_album_id)
) STRICT;

CREATE INDEX idx_library_retag_pairs_new ON library_retag_pairs(new_album_id);

-- Migration high-water mark.
PRAGMA user_version = 43;
