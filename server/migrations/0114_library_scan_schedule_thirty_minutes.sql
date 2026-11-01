-- 0114_library_scan_schedule_thirty_minutes.sql - the 5 and 10 minute scan
-- schedules are gone.
--
--   config_sections (one row rewritten)
--       'library_scan_schedule': a stored scan_frequency of '5min' or
--       '10min' becomes '30min', its revision goes up by one, and every
--       other field stays, even one that does not decode. A row holding any
--       other value or shape is left alone: it reads as it did before.
--
-- value_json is always valid JSON (the table's CHECK), so reading it cannot
-- fail whatever the row holds.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE config_sections
SET value_json = json_set(value_json, '$.scan_frequency', '30min'),
    revision = revision + 1,
    updated_at = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
WHERE key = 'library_scan_schedule'
  AND json_extract(value_json, '$.scan_frequency') IN ('5min', '10min');

-- Migration high-water mark.
PRAGMA user_version = 114;
