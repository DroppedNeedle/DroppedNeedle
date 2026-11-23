-- The record of what Discover offered each person and what came of it, and
-- where a request, a batch or an answer to Discover was made.
--
-- `catalog_discover_picks` holds one row per person and release group ever
-- offered as a pick. A build stamps when it was offered and why; the web
-- UI stamps when it was shown, sampled and opened; one daily job works out
-- the rest from the request, library, play and feedback rows. A person's
-- rows go with their account.
--
-- `made_from` says which surface a request, a discovery batch or an answer
-- was made on. Rows made before this change have none. Both place columns
-- list every surface now, those later work first writes included, since a
-- CHECK cannot change without rebuilding its table.

CREATE TABLE catalog_discover_picks (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL DEFAULT '',
    album_name TEXT NOT NULL DEFAULT '',
    artist_name TEXT NOT NULL DEFAULT '',
    first_surface TEXT NOT NULL
        CHECK (first_surface IN ('deck', 'top_picks', 'discover_shelf', 'home_shelf',
            'playlist_suggestions', 'album_page', 'artist_page', 'weekly_mix', 'search',
            'new_releases', 'weekly_set', 'dig_further', 'later', 'thread_page', 'radio',
            'mix', 'listen_first', 'passed_on')),
    first_offered_at INTEGER NOT NULL,
    last_offered_at INTEGER NOT NULL,
    evidence TEXT NOT NULL,
    first_shown_at INTEGER,
    last_shown_at INTEGER,
    shown_days INTEGER NOT NULL DEFAULT 0 CHECK (shown_days >= 0),
    sampled_at INTEGER,
    sampled_seconds INTEGER CHECK (sampled_seconds >= 0),
    opened_at INTEGER,
    requested_at INTEGER,
    requested_from TEXT
        CHECK (requested_from IN ('deck', 'top_picks', 'discover_shelf', 'home_shelf',
            'playlist_suggestions', 'album_page', 'artist_page', 'weekly_mix', 'search',
            'new_releases', 'weekly_set', 'dig_further', 'later', 'thread_page', 'radio',
            'mix', 'listen_first', 'passed_on')),
    arrived_at INTEGER,
    first_played_at INTEGER,
    plays_30d INTEGER NOT NULL DEFAULT 0 CHECK (plays_30d >= 0),
    plays_90d INTEGER NOT NULL DEFAULT 0 CHECK (plays_90d >= 0),
    feedback_kind TEXT CHECK (feedback_kind IN ('no', 'know_it', 'not_now', 'not_artist')),
    feedback_at INTEGER,
    let_go_at INTEGER,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

ALTER TABLE request_history_requesters ADD COLUMN made_from TEXT
    CHECK (made_from IN ('deck', 'top_picks', 'discover_shelf', 'home_shelf',
        'playlist_suggestions', 'album_page', 'artist_page', 'weekly_mix', 'search',
        'new_releases', 'weekly_set', 'dig_further', 'later', 'thread_page', 'radio',
        'mix', 'listen_first', 'passed_on'));

ALTER TABLE acquire_discovery_batches ADD COLUMN made_from TEXT
    CHECK (made_from IN ('deck', 'top_picks', 'discover_shelf', 'home_shelf',
        'playlist_suggestions', 'album_page', 'artist_page', 'weekly_mix', 'search',
        'new_releases', 'weekly_set', 'dig_further', 'later', 'thread_page', 'radio',
        'mix', 'listen_first', 'passed_on'));

ALTER TABLE catalog_discover_feedback ADD COLUMN made_from TEXT
    CHECK (made_from IN ('deck', 'top_picks', 'discover_shelf', 'home_shelf',
        'playlist_suggestions', 'album_page', 'artist_page', 'weekly_mix', 'search',
        'new_releases', 'weekly_set', 'dig_further', 'later', 'thread_page', 'radio',
        'mix', 'listen_first', 'passed_on'));

PRAGMA user_version = 160;
