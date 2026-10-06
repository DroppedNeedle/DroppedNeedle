-- 0023_spotify_auth_requests.sql - Spotify authorize states that remember
-- their redirect URI.
--
-- Spotify requires the token exchange to repeat the exact redirect URI the
-- authorize step sent. An app registered against v2 lists the v2 callback
-- path and a new one lists the v3 path, so each state now records the URI
-- it was issued with and the callback exchanges with that one.
--
-- v2's spotify_oauth_states had no room for the URI. Its rows are
-- ten-minute states that v3 never read, so it is dropped rather than
-- altered, which also keeps this file safe to apply twice.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS spotify_auth_requests (
    state        TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL,
    expires_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_spotify_auth_requests_expires
    ON spotify_auth_requests(expires_at);

DROP TABLE IF EXISTS spotify_oauth_states;

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 23;
