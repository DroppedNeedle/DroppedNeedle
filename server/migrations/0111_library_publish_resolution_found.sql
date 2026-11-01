-- 0111_library_publish_resolution_found.sql - what closing a write found.
--
-- A write that needs attention can now be closed when a destination holds
-- the bytes from before the write or holds nothing, as well as when it
-- holds what the write placed. A closed write records how many of each.
--
--   library_publish_resolutions.as_before_files
--   library_publish_resolutions.empty_files
--       new columns, 0 for writes closed earlier (those were all checked
--       as written).
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_resolutions
    ADD COLUMN as_before_files INTEGER NOT NULL DEFAULT 0 CHECK (as_before_files >= 0);
ALTER TABLE library_publish_resolutions
    ADD COLUMN empty_files INTEGER NOT NULL DEFAULT 0 CHECK (empty_files >= 0);

-- Migration high-water mark.
PRAGMA user_version = 111;
