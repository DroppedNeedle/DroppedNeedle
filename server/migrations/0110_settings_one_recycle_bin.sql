-- 0110_settings_one_recycle_bin.sql - the recycle bin path moves to
-- Library Management, the one place it is edited.
--
-- A Library Management path stored before this migration was never
-- honoured: it only enabled a choice in the collision dialog, and removals
-- used the download policy's. So the path in use decides.
--
--   config_sections (rows rewritten or added)
--       'library_management': a non-blank download policy path overwrites
--       its recycle_bin_path. With no Library Management row yet, one is
--       added holding only that path; every other field reads as its
--       default. With a blank or absent policy path, a stored
--       recycle_bin_path is removed, so removals keep each library
--       folder's .recycle.
--       'download_policy': loses recycle_bin_path.
--
-- Each rewritten row's revision goes up by one; a row with nothing to
-- change is left alone.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE config_sections
SET value_json = json_set(
        value_json,
        '$.recycle_bin_path',
        (
            SELECT json_extract(policy.value_json, '$.recycle_bin_path')
            FROM config_sections AS policy
            WHERE policy.key = 'download_policy'
        )
    ),
    revision = revision + 1
WHERE key = 'library_management'
  AND EXISTS (
      SELECT 1
      FROM config_sections AS policy
      WHERE policy.key = 'download_policy'
        AND json_type(policy.value_json, '$.recycle_bin_path') = 'text'
        AND trim(json_extract(policy.value_json, '$.recycle_bin_path')) <> ''
        AND json_extract(policy.value_json, '$.recycle_bin_path')
            IS NOT json_extract(config_sections.value_json, '$.recycle_bin_path')
  );

UPDATE config_sections
SET value_json = json_remove(value_json, '$.recycle_bin_path'),
    revision = revision + 1
WHERE key = 'library_management'
  AND json_type(value_json, '$.recycle_bin_path') IS NOT NULL
  AND NOT EXISTS (
      SELECT 1
      FROM config_sections AS policy
      WHERE policy.key = 'download_policy'
        AND json_type(policy.value_json, '$.recycle_bin_path') = 'text'
        AND trim(json_extract(policy.value_json, '$.recycle_bin_path')) <> ''
  );

INSERT INTO config_sections (key, value_json, revision, updated_at)
SELECT 'library_management',
       json_object('recycle_bin_path', json_extract(value_json, '$.recycle_bin_path')),
       1,
       CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
FROM config_sections
WHERE key = 'download_policy'
  AND json_type(value_json, '$.recycle_bin_path') = 'text'
  AND trim(json_extract(value_json, '$.recycle_bin_path')) <> ''
  AND NOT EXISTS (SELECT 1 FROM config_sections WHERE key = 'library_management');

UPDATE config_sections
SET value_json = json_remove(value_json, '$.recycle_bin_path'),
    revision = revision + 1
WHERE key = 'download_policy'
  AND json_type(value_json, '$.recycle_bin_path') IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 110;
