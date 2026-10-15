-- 0052_hands_off_replacement.sql - new files that take the place of a
-- hands-off album.
--
-- When an album that was left unmanaged, kept as a custom edition or
-- stepped back from loses its files in the same scan in which new files
-- appear beside it, the new album may be a person's retag of it. The app
-- leaves the new album alone and offers the two for a merge by hand. That
-- needs one new value in two CHECK lists:
--
--   library_album_takeovers.cause   'replaced_hands_off'
--   library_retag_pairs.reason      'replaced_hands_off'
--
-- SQLite cannot widen a CHECK in place, so both tables are rebuilt with the
-- same columns, types, defaults, foreign keys and STRICT, and every row is
-- copied. No other table, trigger or view references either.
-- idx_library_retag_pairs_new is made again.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_album_takeovers_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    cause TEXT NOT NULL
        CHECK(cause IN ('files_changed','retag_followed','run_undone','replaced_hands_off')),
    noticed_at INTEGER NOT NULL,
    handed_back_at INTEGER,
    hand_back_tries INTEGER NOT NULL DEFAULT 0 CHECK(hand_back_tries >= 0),
    hand_back_retry_at INTEGER
) STRICT;

INSERT INTO library_album_takeovers_new (local_album_id, cause, noticed_at, handed_back_at,
    hand_back_tries, hand_back_retry_at)
    SELECT local_album_id, cause, noticed_at, handed_back_at, hand_back_tries,
           hand_back_retry_at
    FROM library_album_takeovers;

DROP TABLE library_album_takeovers;
ALTER TABLE library_album_takeovers_new RENAME TO library_album_takeovers;

CREATE TABLE library_retag_pairs_new (
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    new_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason TEXT NOT NULL
        CHECK(reason IN ('mostly_missing','new_files_added','tracks_elsewhere','chosen_matches_differ','custom_edition','live_work','work_on_new_row','store_error','replaced_hands_off')),
    state TEXT NOT NULL
        CHECK(state IN ('open','joined','dismissed')),
    recorded_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (old_album_id, new_album_id)
) STRICT;

INSERT INTO library_retag_pairs_new (old_album_id, new_album_id, reason, state, recorded_at,
    updated_at)
    SELECT old_album_id, new_album_id, reason, state, recorded_at, updated_at
    FROM library_retag_pairs;

DROP TABLE library_retag_pairs;
ALTER TABLE library_retag_pairs_new RENAME TO library_retag_pairs;

CREATE INDEX idx_library_retag_pairs_new ON library_retag_pairs(new_album_id);

-- Migration high-water mark.
PRAGMA user_version = 52;
