-- Manual sessions have no App Connect identity. A registered app can
-- replace only the session it issued for the same user and device.
ALTER TABLE auth_tokens ADD COLUMN app_connect_key TEXT
    CHECK (app_connect_key IS NULL OR session_kind = 'companion');

CREATE UNIQUE INDEX idx_auth_tokens_app_connect ON auth_tokens(user_id, app_connect_key)
    WHERE app_connect_key IS NOT NULL AND revoked = 0;

PRAGMA user_version = 171;
