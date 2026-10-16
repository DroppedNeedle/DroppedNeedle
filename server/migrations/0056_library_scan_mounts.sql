-- 0056_library_scan_mounts.sql - folders inside a library folder that are
-- drives or shares of their own.
--
-- A drive or network share mounted inside a library folder that drops out
-- leaves an empty folder behind, which looks like its files were deleted.
-- Scans record each folder they find on another filesystem than the folder
-- above it. When a later scan finds nothing at all under one that still has
-- indexed files, it marks none of them missing and records a failure naming
-- the folder, as it does for a library folder that goes empty. One row per
-- folder:
--
--   root_id        the library folder's root
--   relative_path  the folder, in the NFC form track paths are kept in;
--                  never `.`: the library folder itself is never recorded
--
-- A row is written when a scan finds the mount, however that scan ends, and
-- is never deleted: a folder that is no longer a mount matters only when it
-- still has indexed files and a scan finds it empty. Root ids live in
-- settings, so there is no foreign key.
--
-- The index serves the check, made for each file a scan reads, of whether
-- the scan held back a folder covering it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE library_scan_mounts (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL CHECK (relative_path NOT IN ('', '.')),
    PRIMARY KEY (root_id, relative_path)
) STRICT;

CREATE INDEX idx_scan_failures_guard ON library_scan_failures(run_id, root_id)
    WHERE failure_code = 'MASS_MISSING_GUARD';

-- Migration high-water mark.
PRAGMA user_version = 56;
