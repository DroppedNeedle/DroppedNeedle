-- 0116_settings_drop_primary_source.sql - the primary music source section
-- goes with its routes. Discovery answers from ListenBrainz unless a request
-- names a source, and a person's own choice stays in their scrobble
-- preferences.
--
--   config_sections (row removed)
--       'primary_music_source'
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DELETE FROM config_sections WHERE key = 'primary_music_source';

-- Migration high-water mark.
PRAGMA user_version = 116;
