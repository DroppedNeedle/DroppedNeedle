-- 0007_acquire_durable_state.sql - acquisition state that has to survive a
-- restart.
--
-- Requests, co-requesters, dismissals, wanted watches, auto-download and
-- personal-mix approvals already have tables in 0001 and are written there
-- now. This migration adds the remaining acquisition state that had no
-- table: in-flight edition acquires, follow-poll cursors, the upgrade
-- worklist, drop-import quarantine entries and the flow operation records.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- One row per edition acquire while its download task runs. A repeat ask
-- answers the running task instead of starting a second fetch.
CREATE TABLE IF NOT EXISTS acquire_edition_acquires (
    release_group_mbid_lower TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    started_at INTEGER NOT NULL
);

-- Follow new-release poll cursor per artist. The known release groups live
-- in artist_known_releases (0001). `followers` is a JSON array of user ids,
-- `pending` a JSON array of {rg_mbid, title, date} releases held until
-- their date arrives.
CREATE TABLE IF NOT EXISTS acquire_follow_cursors (
    artist_mbid_lower TEXT PRIMARY KEY,
    baselined INTEGER NOT NULL DEFAULT 0 CHECK(baselined IN (0,1)),
    cursor_date TEXT,
    next_poll_at INTEGER NOT NULL,
    followers TEXT NOT NULL DEFAULT '[]',
    pending TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_acquire_follow_cursors_due
    ON acquire_follow_cursors(next_poll_at);

-- Cutoff-unmet albums waiting for a background upgrade, oldest first.
CREATE TABLE IF NOT EXISTS acquire_upgrade_worklist (
    release_group_mbid TEXT PRIMARY KEY,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    position INTEGER NOT NULL
);

-- Drop-import sources held back with their reason until resolved by hand.
CREATE TABLE IF NOT EXISTS acquire_flow_quarantine (
    key TEXT PRIMARY KEY,
    album_key TEXT,
    reason TEXT NOT NULL,
    quarantined_at INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_acquire_flow_quarantine_album
    ON acquire_flow_quarantine(album_key) WHERE album_key IS NOT NULL;

-- Free-music and drop-import operation records: one per idempotency key,
-- with attempts, timestamps and a terminal state.
CREATE TABLE IF NOT EXISTS acquire_operations (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    op_key TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('queued','running','succeeded','failed','cancelled')),
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    detail TEXT NOT NULL DEFAULT '',
    UNIQUE (kind, op_key)
);
CREATE INDEX IF NOT EXISTS idx_acquire_operations_unfinished
    ON acquire_operations(state) WHERE state IN ('queued','running');

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 7;
