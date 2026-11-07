-- Covers carried from v2 kept v2's paths, which v3 does not serve. A
-- root-relative path's /api/ segment is found behind any base path v2's
-- web UI put in front, which is dropped (v3 adds its own). Cover paths
-- move to /api/v3/covers/ and media server images to /api/v3/remotes/,
-- for the same ids. Any other v2 /api/ path has no v3 route and becomes
-- no cover. Full addresses and v3 paths stay. The v2 export makes the
-- same change.

UPDATE catalog_playlist_tracks SET cover_url = CASE
    WHEN substr(cover_url, instr(cover_url, '/api/'), 15) = '/api/v1/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 15)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 12) = '/api/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 12)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 23) = '/api/v1/jellyfin/image/'
        THEN '/api/v3/remotes/jellyfin/images/' || substr(cover_url, instr(cover_url, '/api/') + 23)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 24) = '/api/v1/navidrome/cover/'
        THEN '/api/v3/remotes/navidrome/images/' || substr(cover_url, instr(cover_url, '/api/') + 24)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 19) = '/api/v1/plex/thumb/'
        THEN '/api/v3/remotes/plex/images/' || substr(cover_url, instr(cover_url, '/api/') + 19)
    END
WHERE substr(cover_url, 1, 1) = '/' AND substr(cover_url, 1, 2) != '//' AND instr(cover_url, '/api/') > 0
    AND (instr(cover_url, '?') = 0 OR instr(cover_url, '/api/') < instr(cover_url, '?'))
    AND substr(cover_url, instr(cover_url, '/api/'), 8) != '/api/v3/';

UPDATE request_history SET cover_url = CASE
    WHEN substr(cover_url, instr(cover_url, '/api/'), 15) = '/api/v1/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 15)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 12) = '/api/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 12)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 23) = '/api/v1/jellyfin/image/'
        THEN '/api/v3/remotes/jellyfin/images/' || substr(cover_url, instr(cover_url, '/api/') + 23)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 24) = '/api/v1/navidrome/cover/'
        THEN '/api/v3/remotes/navidrome/images/' || substr(cover_url, instr(cover_url, '/api/') + 24)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 19) = '/api/v1/plex/thumb/'
        THEN '/api/v3/remotes/plex/images/' || substr(cover_url, instr(cover_url, '/api/') + 19)
    END
WHERE substr(cover_url, 1, 1) = '/' AND substr(cover_url, 1, 2) != '//' AND instr(cover_url, '/api/') > 0
    AND (instr(cover_url, '?') = 0 OR instr(cover_url, '/api/') < instr(cover_url, '?'))
    AND substr(cover_url, instr(cover_url, '/api/'), 8) != '/api/v3/';

UPDATE wanted_watches SET cover_url = CASE
    WHEN substr(cover_url, instr(cover_url, '/api/'), 15) = '/api/v1/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 15)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 12) = '/api/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 12)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 23) = '/api/v1/jellyfin/image/'
        THEN '/api/v3/remotes/jellyfin/images/' || substr(cover_url, instr(cover_url, '/api/') + 23)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 24) = '/api/v1/navidrome/cover/'
        THEN '/api/v3/remotes/navidrome/images/' || substr(cover_url, instr(cover_url, '/api/') + 24)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 19) = '/api/v1/plex/thumb/'
        THEN '/api/v3/remotes/plex/images/' || substr(cover_url, instr(cover_url, '/api/') + 19)
    END
WHERE substr(cover_url, 1, 1) = '/' AND substr(cover_url, 1, 2) != '//' AND instr(cover_url, '/api/') > 0
    AND (instr(cover_url, '?') = 0 OR instr(cover_url, '/api/') < instr(cover_url, '?'))
    AND substr(cover_url, instr(cover_url, '/api/'), 8) != '/api/v3/';

UPDATE catalog_youtube_links SET cover_url = CASE
    WHEN substr(cover_url, instr(cover_url, '/api/'), 15) = '/api/v1/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 15)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 12) = '/api/covers/'
        THEN '/api/v3/covers/' || substr(cover_url, instr(cover_url, '/api/') + 12)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 23) = '/api/v1/jellyfin/image/'
        THEN '/api/v3/remotes/jellyfin/images/' || substr(cover_url, instr(cover_url, '/api/') + 23)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 24) = '/api/v1/navidrome/cover/'
        THEN '/api/v3/remotes/navidrome/images/' || substr(cover_url, instr(cover_url, '/api/') + 24)
    WHEN substr(cover_url, instr(cover_url, '/api/'), 19) = '/api/v1/plex/thumb/'
        THEN '/api/v3/remotes/plex/images/' || substr(cover_url, instr(cover_url, '/api/') + 19)
    END
WHERE substr(cover_url, 1, 1) = '/' AND substr(cover_url, 1, 2) != '//' AND instr(cover_url, '/api/') > 0
    AND (instr(cover_url, '?') = 0 OR instr(cover_url, '/api/') < instr(cover_url, '?'))
    AND substr(cover_url, instr(cover_url, '/api/'), 8) != '/api/v3/';

PRAGMA user_version = 144;
