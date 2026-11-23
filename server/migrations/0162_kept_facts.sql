-- The provider facts Discover keeps: each artist's similar artists, its
-- listener counts and its album list, shared by everyone. They are public
-- answers, nobody's listening, written only by the catalog's kept facts
-- and refreshed by one background job, so a page build reads local rows
-- for them and never waits on a provider. Clearing provider data empties
-- them.
--
-- `catalog_fact_fetches` holds one row per artist, kind and source: when it
-- last answered, how the last read went, when it is next due, and when a
-- reader last read it or found it missing. A failed read keeps the rows it
-- had. `catalog_background_read_days` counts the background reads made to
-- ListenBrainz and MusicBrainz each UTC day, the meter the daily ceiling
-- reads; it is not cleared with the facts.
--
-- The source lists name Last.fm, which only later work first writes.

CREATE TABLE catalog_fact_fetches (
    artist_mbid TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('neighbours', 'listeners', 'albums', 'album_counts')),
    source TEXT NOT NULL
        CHECK (source IN ('listenbrainz_labs', 'lastfm', 'listenbrainz', 'musicbrainz')),
    outcome TEXT CHECK (outcome IN ('found', 'none', 'failed')),
    fetched_at INTEGER,
    failing_since_at INTEGER,
    failures INTEGER NOT NULL DEFAULT 0 CHECK (failures >= 0),
    due_at INTEGER NOT NULL,
    wanted_at INTEGER,
    want_order INTEGER CHECK (want_order BETWEEN 0 AND 2),
    read_at INTEGER NOT NULL,
    PRIMARY KEY (artist_mbid, kind, source)
) STRICT;

CREATE INDEX catalog_fact_fetches_due ON catalog_fact_fetches (due_at);
CREATE INDEX catalog_fact_fetches_read ON catalog_fact_fetches (read_at);

CREATE TABLE catalog_artist_neighbours (
    artist_mbid TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('listenbrainz_labs', 'lastfm')),
    rank INTEGER NOT NULL CHECK (rank >= 0),
    neighbour_mbid TEXT NOT NULL,
    neighbour_name TEXT,
    score REAL NOT NULL,
    PRIMARY KEY (artist_mbid, source, rank)
) STRICT;

CREATE TABLE catalog_artist_listeners (
    artist_mbid TEXT PRIMARY KEY,
    listeners INTEGER,
    listens INTEGER
) STRICT;

CREATE TABLE catalog_artist_albums (
    artist_mbid TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    primary_type TEXT,
    secondary_types TEXT NOT NULL,
    first_release_date TEXT,
    credited_artist_mbids TEXT,
    listeners INTEGER,
    listens INTEGER,
    PRIMARY KEY (artist_mbid, release_group_mbid)
) STRICT;

CREATE INDEX catalog_artist_albums_group ON catalog_artist_albums (release_group_mbid);

CREATE TABLE catalog_background_read_days (
    day INTEGER NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('listenbrainz', 'listenbrainz_labs', 'musicbrainz')),
    reads INTEGER NOT NULL CHECK (reads >= 0),
    PRIMARY KEY (day, source)
) STRICT;

PRAGMA user_version = 162;
