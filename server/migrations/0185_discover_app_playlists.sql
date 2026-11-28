-- Whether Discover keeps playlists for a person's apps.
--
-- `catalog_discover_prefs.app_playlists` is the person's switch for the
-- playlists Discover keeps for Subsonic and Jellyfin apps ("This week's
-- set", "From your shelves" and one for each Daily Mix): on unless they
-- turn it off, and everyone kept reads as on.

ALTER TABLE catalog_discover_prefs ADD COLUMN app_playlists INTEGER NOT NULL DEFAULT 1
    CHECK (app_playlists IN (0, 1));

PRAGMA user_version = 185;
