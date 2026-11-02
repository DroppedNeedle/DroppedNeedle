-- 0120_compat_user_settings.sql - one person's choices for their music apps.
--
--   compat_user_settings (new table)
--       include_public_playlists: apps list other people's public playlists
--       too. A person with no row has it off, so after this migration every
--       app lists its own user's playlists only until that user turns it on.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE compat_user_settings (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    include_public_playlists INTEGER NOT NULL CHECK (include_public_playlists IN (0, 1))
) STRICT;

PRAGMA user_version = 120;
