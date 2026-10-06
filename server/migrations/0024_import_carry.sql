-- 0024_import_carry.sql - v2 import section markers and pending library links.
--
-- The v2 import carries user data one section at a time, each section in
-- its own transaction. import_progress marks every section that has been
-- carried from a v2 instance, in the same transaction as its rows. A
-- section is carried once per instance: an interrupted import resumes at
-- the first unmarked section, and a later export of the same instance
-- brings only sections never carried before, so rows a user deleted in v3
-- do not come back. export_digest names the export file that carried it.
--
-- import_pending_links keeps every v2 library reference (track, album,
-- artist or tombstone id) that a carried row holds while the v3 catalog
-- does not have that id yet. The library carry resolves them once the
-- catalog lands. mode says how the row holds the reference:
--   as_written      ref_column already holds the v2 id (no foreign key);
--   until_resolved  ref_column waits empty (it has a foreign key);
--   link_only       v3 has no column for it; it lives only here.
-- target_key is a JSON array of the carried row's key values.
--
-- Safe to apply twice. No down migration: rollback is restoring a
-- pre-upgrade backup.

CREATE TABLE IF NOT EXISTS import_progress (
    instance_id   TEXT NOT NULL,
    section       TEXT NOT NULL,
    export_digest TEXT NOT NULL,
    rows_written  INTEGER NOT NULL,
    applied_at    TEXT NOT NULL,
    PRIMARY KEY (instance_id, section)
);

CREATE TABLE IF NOT EXISTS import_pending_links (
    target_table TEXT NOT NULL,
    target_key   TEXT NOT NULL,
    ref_column   TEXT NOT NULL,
    ref_kind     TEXT NOT NULL
        CHECK (ref_kind IN ('track', 'album', 'artist', 'tombstone')),
    v2_id        TEXT NOT NULL,
    mode         TEXT NOT NULL
        CHECK (mode IN ('as_written', 'until_resolved', 'link_only')),
    PRIMARY KEY (target_table, target_key, ref_column)
);
CREATE INDEX IF NOT EXISTS idx_import_pending_links_v2
    ON import_pending_links(ref_kind, v2_id);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 24;
