-- The page cache keeps three kinds of saved page: the Discover page, Home's
-- provider shelves and the Daily Mixes a Discover build reuses. Nothing
-- writes the queue deck's rows or the Top Picks memo any more, so their rows
-- go and the CHECK lists the three kinds left.
--
-- Discover pages that no longer decode go: one that does not say whether
-- the weekly set is on, or that names a section, zone or shape that has left
-- the page. A read would drop it anyway; its page is built again on its
-- person's next visit. Every other Discover page is copied.
-- Home's rows and the Daily Mixes are copied as they are.
--
-- Nothing references the table, so one transaction does.

CREATE TABLE catalog_cached_pages_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL CHECK (page IN ('discover', 'home', 'daily_mixes')),
    source TEXT NOT NULL,
    listenbrainz INTEGER NOT NULL CHECK (listenbrainz IN (0, 1)),
    lastfm INTEGER NOT NULL CHECK (lastfm IN (0, 1)),
    payload BLOB NOT NULL,
    built_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, page)
) STRICT;

INSERT INTO catalog_cached_pages_new
    (rowid, user_id, page, source, listenbrainz, lastfm, payload, built_at)
SELECT rowid, user_id, page, source, listenbrainz, lastfm, payload, built_at
FROM catalog_cached_pages AS saved
WHERE saved.page IN ('home', 'daily_mixes')
    OR (saved.page = 'discover' AND CASE
        WHEN json_valid(CAST(saved.payload AS TEXT)) THEN
            json_type(CAST(saved.payload AS TEXT), '$.page.weekly_set_enabled')
                IN ('true', 'false')
            AND NOT EXISTS (
                SELECT 1 FROM json_tree(CAST(saved.payload AS TEXT)) AS node
                WHERE node.type = 'text'
                    AND (node.key IN ('key', 'zone', 'shape') OR node.path = '$.skipped')
                    AND node.atom IN (
                        'discover_queue', 'top_picks', 'listeners_like_you',
                        'because_you_listen_to', 'artists_you_might_like',
                        'popular_in_your_genres', 'unexplored_genres', 'globally_trending',
                        'fresh_releases', 'new_from_followed', 'missing_essentials',
                        'rediscover', 'lastfm_recent_scrobbles', 'lastfm_weekly_artist_chart',
                        'lastfm_weekly_album_chart', 'because_you_listened', 'trending_now',
                        'because', 'picks'))
        ELSE 0
    END);

DROP TABLE catalog_cached_pages;

ALTER TABLE catalog_cached_pages_new RENAME TO catalog_cached_pages;

PRAGMA user_version = 178;
