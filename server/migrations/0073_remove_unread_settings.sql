-- 0073_remove_unread_settings.sql - settings nothing read are gone.
--
--   config_sections (rows deleted)
--       'home_settings' and 'scrobble_settings': the two sections were
--       removed whole, and nothing reads their rows
--
--   config_sections (one row rewritten)
--       'download_policy': each quality recipe step loses its
--       target_bitrate_kbps. A recipe step refuses fields it does not
--       know, so a stored step that kept the key would make the whole
--       download policy unreadable. The revision goes up by one; the row
--       is left alone when no step holds the key.
--
-- Removed fields in other sections stay in their rows: they are ignored
-- on read and dropped on the next save.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DELETE FROM config_sections WHERE key IN ('home_settings', 'scrobble_settings');

UPDATE config_sections
SET value_json = json_set(
        value_json,
        '$.quality_recipe',
        (
            SELECT json_group_array(
                json(json_remove(step.value, '$.target_bitrate_kbps')) ORDER BY step.key
            )
            FROM json_each(config_sections.value_json, '$.quality_recipe') AS step
        )
    ),
    revision = revision + 1
WHERE key = 'download_policy'
  AND EXISTS (
      SELECT 1
      FROM json_each(config_sections.value_json, '$.quality_recipe') AS step
      WHERE json_type(step.value, '$.target_bitrate_kbps') IS NOT NULL
  );

-- Migration high-water mark.
PRAGMA user_version = 73;
