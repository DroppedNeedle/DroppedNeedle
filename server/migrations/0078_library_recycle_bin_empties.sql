-- 0078_library_recycle_bin_empties.sql - what each "Empty recycle bin" did.
--
--   library_recycle_bin_empties (new, STRICT)
--       one row per run of "Empty recycle bin", added to by each attempt
--       of its job; the attempt that finishes writes finished_at in the
--       same transaction that completes the job.
--       id             the run's job id.
--       actor_user_id  the admin who asked; NULL once the account is gone.
--       before_at      unix milliseconds: only copies that reached the bin
--                      before this were deleted.
--       finished_at    unix milliseconds when it finished; NULL while it
--                      has not.
--       deleted        copies deleted, or found already gone, over every
--                      attempt.
--       freed_bytes    the bytes the deleted copies held.
--       older_folders  old bin folders no row named, emptied.
--       kept           copies the latest attempt kept (changed in the bin,
--                      or not where a bin that may be emptied holds them).
--       waited         copies the latest attempt left for later (held by a
--                      put-back, or a bin that looked empty).
--       kept_json      up to 20 kept copies: [{"name", "why"}].
--
--   idx_library_recycle_bin_empties_actor_user_id (new)
--       the foreign key's index.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_recycle_bin_empties (
    id            TEXT    PRIMARY KEY,
    actor_user_id TEXT    REFERENCES auth_users(id) ON DELETE SET NULL,
    before_at     INTEGER NOT NULL,
    finished_at   INTEGER,
    deleted       INTEGER NOT NULL CHECK (deleted >= 0),
    freed_bytes   INTEGER NOT NULL CHECK (freed_bytes >= 0),
    older_folders INTEGER NOT NULL CHECK (older_folders >= 0),
    kept          INTEGER NOT NULL CHECK (kept >= 0),
    waited        INTEGER NOT NULL CHECK (waited >= 0),
    kept_json     TEXT    NOT NULL
) STRICT;

CREATE INDEX IF NOT EXISTS idx_library_recycle_bin_empties_actor_user_id
    ON library_recycle_bin_empties(actor_user_id);

-- Migration high-water mark.
PRAGMA user_version = 78;
