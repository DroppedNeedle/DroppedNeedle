-- 0044_library_album_takeovers.sql - albums whose files were changed outside
-- the app, and what the app last wrote to each file.
--
-- When a file the app wrote is retagged by something else, the app stops
-- managing that album by itself and says so, until a person hands the album
-- back. Three things carry that:
--
--   library_album_takeovers                  one row per album the app
--                                            stepped back from
--     cause           why: its files were changed outside, it was followed
--                     across a retag, or a person undid the app's run
--     noticed_at      when the app stepped back
--     handed_back_at  NULL, or when a person handed back every album at
--                     once; the row goes when the album's turn has come
--     hand_back_tries how often that turn failed (its files could not be
--                     read); after a few the album waits for a person again
--     hand_back_retry_at  NULL, or the earliest time of the next try
--   library_track_management_state
--     .written_sha256 the file's SHA-256 as the app last wrote it, last
--                     found it the way it wanted it, or was last handed it
--   library_management_automatic_queue
--     .retag_followed_at  set by a scan that followed the album across a
--                     retag. Nothing writes it yet: it is here so the change
--                     that does needs no migration of its own.
--
-- Automatic management reads an album's hand-backs from the audit log on
-- every look, so that log gains an index by album and kind.
--
-- The row cascades from its album: it never keeps an album row from being
-- deleted. Times are milliseconds. Both new columns start NULL.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_album_takeovers (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    cause TEXT NOT NULL
        CHECK(cause IN ('files_changed','retag_followed','run_undone')),
    noticed_at INTEGER NOT NULL,
    handed_back_at INTEGER,
    hand_back_tries INTEGER NOT NULL DEFAULT 0 CHECK(hand_back_tries >= 0),
    hand_back_retry_at INTEGER
) STRICT;

CREATE INDEX idx_library_catalog_actions_album_kind
    ON library_catalog_actions(local_album_id, action_kind);

ALTER TABLE library_track_management_state ADD COLUMN written_sha256 TEXT;
ALTER TABLE library_management_automatic_queue ADD COLUMN retag_followed_at INTEGER;

-- Migration high-water mark.
PRAGMA user_version = 44;
