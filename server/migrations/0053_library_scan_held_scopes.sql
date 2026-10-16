-- 0053_library_scan_held_scopes.sql - library folders a scan held back,
-- kept past the scan history.
--
-- When a scan finds most of a folder's files gone at once, it marks none of
-- them missing, in case a drive or share is not mounted, and records a
-- failure on the scan. Files that appeared in such a folder since may be a
-- person's own retag of the files that seem gone, so automatic management
-- leaves them alone until a scan finds the folder as expected again. The
-- history keeps only the last fifty scans, and a share can stay unmounted
-- for longer, so the hold has a table of its own. One row per held folder:
--
--   root_id        the library folder's root
--   relative_path  the folder the scan held back, `.` for the whole root
--   held_at        when the first scan of the current hold began
--
-- A row is written when a scan holds its folder back, and kept, with its
-- time, while later scans hold it back too. It goes when a completed scan
-- walks a folder covering it without holding it back. Root ids live in
-- settings, so there is no foreign key. Times are milliseconds.
--
-- The holds that stand at upgrade are carried over: a folder held back by
-- a scan with no later completed scan that walked a folder covering it and
-- did not hold that one back. Its time is the start of the earliest such
-- scan.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_scan_held_scopes (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    held_at INTEGER NOT NULL,
    PRIMARY KEY (root_id, relative_path)
) STRICT;

INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at)
SELECT g.root_id, g.relative_path, CAST(MIN(r.started_at) * 1000 AS INTEGER)
FROM library_scan_failures g
JOIN library_scan_runs r ON r.id = g.run_id
WHERE g.failure_code = 'MASS_MISSING_GUARD' AND r.started_at IS NOT NULL
AND NOT EXISTS (
    SELECT 1 FROM library_scan_runs c
    JOIN library_scan_run_scopes s ON s.run_id = c.id
    WHERE c.state = 'completed' AND c.started_at > r.started_at
    AND s.root_id = g.root_id AND s.discovery_state = 'completed'
    AND (s.relative_path = '.' OR g.relative_path = s.relative_path
        OR substr(g.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/')
    AND NOT EXISTS (
        SELECT 1 FROM library_scan_failures f
        WHERE f.run_id = c.id AND f.root_id = s.root_id
        AND f.relative_path = s.relative_path AND f.failure_code = 'MASS_MISSING_GUARD'))
GROUP BY g.root_id, g.relative_path;

-- Migration high-water mark.
PRAGMA user_version = 53;
