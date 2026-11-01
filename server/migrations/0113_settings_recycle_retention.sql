-- 0113_settings_recycle_retention.sql - the recycle bin retention moves to
-- Library Management, beside the bin's path.
--
-- The value moves as it was stored, not read and rewritten. One that does
-- not read in the download policy (text, null, a fraction) does not read in
-- Library Management either, so expiry waits for a save instead of deleting
-- on the default. A download policy row that is not an object may have held
-- a value nobody can recover, so Library Management gets a null, with the
-- same result.
--
--   config_sections (rows rewritten or added)
--       'library_management': gains recycle_retention_days. With no row
--       yet, one is added holding only that; every other field reads as
--       its default. A row that is not an object is left alone: it does not
--       read, and a save replaces it.
--       'download_policy': loses recycle_retention_days.
--
-- With no stored retention nothing moves, and the field reads as its
-- default. Each rewritten row's revision goes up by one; a row with nothing
-- to change is left alone.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE config_sections
SET value_json = json_set(
        value_json,
        '$.recycle_retention_days',
        (
            SELECT json(coalesce(policy.value_json -> '$.recycle_retention_days', 'null'))
            FROM config_sections AS policy
            WHERE policy.key = 'download_policy'
        )
    ),
    revision = revision + 1
WHERE key = 'library_management'
  AND json_type(value_json) = 'object'
  AND EXISTS (
      SELECT 1
      FROM config_sections AS policy
      WHERE policy.key = 'download_policy'
        AND (
            json_type(policy.value_json) <> 'object'
            OR json_type(policy.value_json, '$.recycle_retention_days') IS NOT NULL
        )
  );

INSERT INTO config_sections (key, value_json, revision, updated_at)
SELECT 'library_management',
       json_set(
           '{}',
           '$.recycle_retention_days',
           json(coalesce(value_json -> '$.recycle_retention_days', 'null'))
       ),
       1,
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM config_sections
WHERE key = 'download_policy'
  AND (
      json_type(value_json) <> 'object'
      OR json_type(value_json, '$.recycle_retention_days') IS NOT NULL
  )
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE key = 'library_management');

UPDATE config_sections
SET value_json = json_remove(value_json, '$.recycle_retention_days'),
    revision = revision + 1
WHERE key = 'download_policy'
  AND json_type(value_json, '$.recycle_retention_days') IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 113;
