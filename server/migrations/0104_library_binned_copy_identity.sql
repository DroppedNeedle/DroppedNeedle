-- 0104_library_binned_copy_identity.sql - what a recycled copy was when it
-- landed in the bin.
--
--   library_publish_replacements.binned_stat_json (new, TEXT, nullable)
--       the copy's identity on disk right after it landed in the bin:
--       {"dev", "ino", "size", "mtime_ns", "ctime_ns"}. Written in the same
--       write that sets `recycled_at`. Expiry deletes a copy whose identity
--       still matches it exactly, or whose bytes are the ones the removal
--       read; any write to the copy since moves its change time. A file
--       edited before it reached the bin is then still expired. Existing
--       rows start NULL and keep the bytes-only check.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_replacements ADD COLUMN binned_stat_json TEXT
    CHECK (binned_stat_json IS NULL OR json_valid(binned_stat_json));

-- Migration high-water mark.
PRAGMA user_version = 104;
