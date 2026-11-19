-- Whether a person chose their display name, so other users may see it
-- when sharing a playlist. A display name an account was made with from a
-- sign-in service, or one that is only the login name, is not shown.
-- Accounts with no sign-in but their password whose display name is not
-- their login name chose theirs when the account was made; everyone else
-- shows up once they save a display name under Profile.

ALTER TABLE auth_users ADD COLUMN display_name_chosen INTEGER NOT NULL DEFAULT 0
    CHECK (display_name_chosen IN (0, 1));

UPDATE auth_users SET display_name_chosen = 1
WHERE username IS NOT NULL AND trim(display_name) <> ''
    AND lower(trim(display_name)) IS NOT lower(username)
    AND lower(trim(display_name)) IS NOT lower(username_display)
    AND NOT EXISTS (SELECT 1 FROM auth_providers p
        WHERE p.user_id = auth_users.id AND p.provider <> 'local');

PRAGMA user_version = 151;
