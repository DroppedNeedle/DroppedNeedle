-- Each person's theme and custom CSS, kept on the account so they follow
-- the person to every device, and the last saves of that CSS. No row
-- means the defaults: no theme chosen, no CSS, CSS switched on.

CREATE TABLE app_user_appearances (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    theme TEXT CHECK (theme IN ('system', 'light', 'dark', 'midnight')),
    custom_css TEXT NOT NULL DEFAULT '',
    custom_css_enabled INTEGER NOT NULL DEFAULT 1 CHECK (custom_css_enabled IN (0, 1)),
    css_saved_at INTEGER,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE app_custom_css_saves (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    css TEXT NOT NULL,
    saved_at INTEGER NOT NULL
) STRICT;

CREATE INDEX app_custom_css_saves_by_user
    ON app_custom_css_saves (user_id, saved_at DESC, id DESC);

PRAGMA user_version = 143;
