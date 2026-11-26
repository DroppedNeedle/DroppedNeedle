-- Each person's Discover section switches, carried over the sections this
-- release changed. Only Discover rows of the keys below change; every other
-- row stays as it is.
--
-- - The queue deck is now the weekly set: a deck switched off is a week
--   switched off.
-- - Missing Essentials is now Gaps in artists you collect, and Rediscover is
--   From your shelves.
-- - New from your artists replaces Fresh Releases and New From Artists You
--   Follow, and is off only for a person who had both off.
-- - Milestone Anniversaries moved to Home, its switch with it.
-- - The switches of the sections that left Discover go: Top Picks, the
--   Listening Lounge, Because You Listened, Artists You Might Like, Popular
--   in Your Genres, Genres to Explore, Globally Trending, and the three
--   Last.fm lists, which moved to the person's own charts page.
--
-- A rename that meets a row already saved under the new key keeps that row.

UPDATE OR IGNORE app_section_prefs SET section_key = 'weekly_set'
    WHERE page = 'discover' AND section_key = 'discover_queue';

UPDATE OR IGNORE app_section_prefs SET section_key = 'gaps_in_your_artists'
    WHERE page = 'discover' AND section_key = 'missing_essentials';

UPDATE OR IGNORE app_section_prefs SET section_key = 'from_your_shelves'
    WHERE page = 'discover' AND section_key = 'rediscover';

INSERT OR IGNORE INTO app_section_prefs (user_id, page, section_key, enabled, updated_at)
SELECT fresh.user_id, 'discover', 'new_from_your_artists', 0,
    MAX(fresh.updated_at, followed.updated_at)
FROM app_section_prefs AS fresh
JOIN app_section_prefs AS followed
    ON followed.user_id = fresh.user_id
    AND followed.page = 'discover'
    AND followed.section_key = 'new_from_followed'
    AND followed.enabled = 0
WHERE fresh.page = 'discover' AND fresh.section_key = 'fresh_releases' AND fresh.enabled = 0;

UPDATE OR IGNORE app_section_prefs SET page = 'home'
    WHERE page = 'discover' AND section_key = 'anniversaries';

DELETE FROM app_section_prefs
    WHERE page = 'discover' AND section_key IN (
        'discover_queue', 'missing_essentials', 'rediscover', 'fresh_releases',
        'new_from_followed', 'anniversaries', 'top_picks', 'listeners_like_you',
        'because_you_listen_to', 'artists_you_might_like', 'popular_in_your_genres',
        'unexplored_genres', 'globally_trending', 'lastfm_recent_scrobbles',
        'lastfm_weekly_artist_chart', 'lastfm_weekly_album_chart');

PRAGMA user_version = 179;
