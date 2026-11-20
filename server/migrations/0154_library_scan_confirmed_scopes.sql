-- Scopes an admin explicitly confirmed as intentionally missing for a
-- specific scan run, plus exact missing and indexed counts recorded when
-- the mass-missing guard trips.

CREATE TABLE library_scan_confirmed_scopes (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    confirmed_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    confirmed_at INTEGER NOT NULL,
    PRIMARY KEY (run_id, root_id, relative_path)
) STRICT;

CREATE INDEX idx_scan_confirmed_scopes_user
    ON library_scan_confirmed_scopes(confirmed_by_user_id);

ALTER TABLE library_scan_failures ADD COLUMN missing_count INTEGER;
ALTER TABLE library_scan_failures ADD COLUMN indexed_count INTEGER;

ALTER TABLE library_scan_held_scopes ADD COLUMN missing_count INTEGER;
ALTER TABLE library_scan_held_scopes ADD COLUMN indexed_count INTEGER;

PRAGMA user_version = 154;
