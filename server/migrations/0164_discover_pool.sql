-- Each person's candidate pool: the albums Discover could offer them, each
-- with the evidence that found it, worked out by one job from their taste
-- rows, the library and the kept facts, and rebuilt whole. The rows are
-- results, not a read model: nothing keeps them by trigger, forgetting a
-- person's history removes them, and a person's rows go with their account.
--
-- `catalog_discover_candidates` holds one row a person and release group:
-- what it is, the listener counts its floors read, how it was found
-- (`kind`), its support (the person's artists' weight times their links'
-- strength, summed), the taste group most of that support comes from (null
-- when its seeds are in no group) with that group's part, and its place in
-- the pool. The kinds are listed whole, those later work first writes
-- included, since a CHECK cannot change without rebuilding its table.
--
-- `catalog_discover_candidate_paths` holds each candidate's evidence, one
-- row a path: what kind of path, which service the fact came from and when
-- it was fetched, the person's artist it runs through (their plays, its
-- weight, how many of its albums the library holds), the link's strength,
-- its rank in that artist's list and the list's length, which services
-- named the link, and a known artist credited beside the candidate's own.
--
-- `catalog_discover_pool_builds` holds one row a person: when the pool was
-- built, whether facts it needed were still to come (`waiting_for` lists
-- the artists whose album facts it waits on), and the counts of the build.
-- `hubs_left_out` is null when the hub rule did not run.

CREATE TABLE catalog_discover_pool_builds (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    built_at INTEGER NOT NULL,
    incomplete INTEGER NOT NULL CHECK (incomplete IN (0, 1)),
    waiting_for TEXT NOT NULL,
    seeds_used INTEGER NOT NULL CHECK (seeds_used >= 0),
    neighbours_found INTEGER NOT NULL CHECK (neighbours_found >= 0),
    known_left_out INTEGER NOT NULL CHECK (known_left_out >= 0),
    hubs_left_out INTEGER CHECK (hubs_left_out >= 0),
    artists_ranked INTEGER NOT NULL CHECK (artists_ranked >= 0),
    artists_without_album INTEGER NOT NULL CHECK (artists_without_album >= 0),
    new_artist_candidates INTEGER NOT NULL CHECK (new_artist_candidates >= 0),
    source_pick_candidates INTEGER NOT NULL CHECK (source_pick_candidates >= 0),
    gap_candidates INTEGER NOT NULL CHECK (gap_candidates >= 0),
    shelf_candidates INTEGER NOT NULL CHECK (shelf_candidates >= 0),
    fresh_candidates INTEGER NOT NULL CHECK (fresh_candidates >= 0),
    thread_candidates INTEGER NOT NULL CHECK (thread_candidates >= 0)
) STRICT;

CREATE TABLE catalog_discover_candidates (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    rank INTEGER NOT NULL CHECK (rank >= 0),
    kind TEXT NOT NULL
        CHECK (kind IN ('new_artist', 'source_pick', 'gap', 'shelf', 'fresh', 'thread')),
    title TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    credited_artist_mbids TEXT NOT NULL,
    primary_type TEXT,
    first_release_date TEXT,
    album_listeners INTEGER CHECK (album_listeners >= 0),
    artist_listeners INTEGER CHECK (artist_listeners >= 0),
    support REAL NOT NULL,
    group_no INTEGER CHECK (group_no >= 0),
    group_support REAL NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

CREATE INDEX catalog_discover_candidates_rank ON catalog_discover_candidates (user_id, rank);

CREATE TABLE catalog_discover_candidate_paths (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    path_no INTEGER NOT NULL CHECK (path_no >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('seed', 'source', 'library', 'fresh', 'fact')),
    source TEXT NOT NULL
        CHECK (source IN ('listenbrainz_labs', 'lastfm', 'listenbrainz', 'musicbrainz',
            'library', 'data_file')),
    fetched_at INTEGER,
    artist_mbid TEXT,
    artist_name TEXT,
    artist_plays INTEGER CHECK (artist_plays >= 0),
    artist_weight REAL,
    artist_albums_owned INTEGER CHECK (artist_albums_owned >= 0),
    link_strength REAL,
    link_rank INTEGER CHECK (link_rank >= 1),
    link_of INTEGER CHECK (link_of >= 1),
    named_by_labs INTEGER NOT NULL CHECK (named_by_labs IN (0, 1)),
    named_by_lastfm INTEGER NOT NULL CHECK (named_by_lastfm IN (0, 1)),
    known_credit_mbid TEXT,
    PRIMARY KEY (user_id, release_group_mbid_lower, path_no),
    FOREIGN KEY (user_id, release_group_mbid_lower)
        REFERENCES catalog_discover_candidates (user_id, release_group_mbid_lower)
        ON DELETE CASCADE
) STRICT;

PRAGMA user_version = 164;
