-- 0075_recycle_bin_expiry.sql - when a recycled file was expired.
--
--   library_publish_replacements.expired_at (new, INTEGER, nullable)
--       unix milliseconds when the library's recycle-bin expiry deleted
--       the row's copy in the bin, or found it already gone. NULL while
--       the copy is still there or has not reached the bin. Existing rows
--       start NULL, so the first pass looks at each one once.
--
--   idx_library_publish_replacements_due (new)
--       the rows a pass reads, by when their copy reached the bin; only
--       rows not yet expired are indexed. `recycled_at` stays REAL unix
--       seconds, so the pass compares it in seconds; `expired_at` is in
--       milliseconds like every newer `_at` column.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_replacements ADD COLUMN expired_at INTEGER;

CREATE INDEX IF NOT EXISTS idx_library_publish_replacements_due
    ON library_publish_replacements(recycled_at, journal_id) WHERE expired_at IS NULL;

-- Migration high-water mark.
PRAGMA user_version = 75;
