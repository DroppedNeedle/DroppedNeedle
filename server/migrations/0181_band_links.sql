-- Band links: a kept fact holding an artist's MusicBrainz relationships to
-- other artists (its members, the bands a person is in, its founders,
-- subgroups, collaborations and other names), and the pool paths that
-- name a pick found through one.
--
-- `catalog_fact_fetches` is rebuilt so its kinds include `band_links`.
-- Nothing references it, so one transaction does; every row carries
-- across, rowid and all, and both indexes are made again.
--
-- `catalog_band_links` holds one row an artist, kind, direction and linked
-- artist, in the order MusicBrainz answered (`position`): what the link
-- is, which way it runs from the artist (`forward`: the artist is the one
-- in the band, the founder, the collaborator or the person behind the
-- name), the linked artist's id and name, and whether the link has ended.
-- Like the other kept facts it is a public answer shared by everyone,
-- written only by the catalog's kept facts, emptied with provider data.
--
-- `catalog_discover_candidate_paths` is rebuilt with what a band-link
-- path holds: the pick's role beside the person's artist (`fact_role`),
-- the member it runs through, when one (`via_mbid`, `via_name`), and the
-- linked artist's own name (`fact_artist_name`), since the album's credit
-- may name another.
-- The data file source goes from its sources: nothing ever wrote it.
-- Nothing references the table, so one transaction does, and every other
-- row carries across, rowid and all.

CREATE TABLE catalog_fact_fetches_new (
    artist_mbid TEXT NOT NULL,
    kind TEXT NOT NULL
        CHECK (kind IN ('neighbours', 'listeners', 'albums', 'album_counts', 'band_links')),
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

INSERT INTO catalog_fact_fetches_new
    (rowid, artist_mbid, kind, source, outcome, fetched_at, failing_since_at, failures,
     due_at, wanted_at, want_order, read_at)
SELECT rowid, artist_mbid, kind, source, outcome, fetched_at, failing_since_at, failures,
    due_at, wanted_at, want_order, read_at
FROM catalog_fact_fetches;

DROP TABLE catalog_fact_fetches;

ALTER TABLE catalog_fact_fetches_new RENAME TO catalog_fact_fetches;

CREATE INDEX catalog_fact_fetches_due ON catalog_fact_fetches (due_at);
CREATE INDEX catalog_fact_fetches_read ON catalog_fact_fetches (read_at);

CREATE TABLE catalog_band_links (
    artist_mbid TEXT NOT NULL,
    position INTEGER NOT NULL CHECK (position >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('member_of_band', 'subgroup', 'founder', 'is_person',
        'collaboration')),
    direction TEXT NOT NULL CHECK (direction IN ('forward', 'backward')),
    linked_mbid TEXT NOT NULL,
    linked_name TEXT NOT NULL,
    ended INTEGER NOT NULL CHECK (ended IN (0, 1)),
    PRIMARY KEY (artist_mbid, kind, direction, linked_mbid)
) STRICT;

CREATE TABLE catalog_discover_candidate_paths_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    path_no INTEGER NOT NULL CHECK (path_no >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('seed', 'source', 'library', 'fresh', 'fact')),
    source TEXT NOT NULL
        CHECK (source IN ('listenbrainz_labs', 'lastfm', 'listenbrainz', 'musicbrainz',
            'library')),
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
    link_closest_new INTEGER NOT NULL DEFAULT 0 CHECK (link_closest_new IN (0, 1)),
    fact_role TEXT CHECK (fact_role IN ('member', 'band', 'other_name', 'shared_member')),
    via_mbid TEXT,
    via_name TEXT,
    fact_artist_name TEXT,
    CHECK ((via_mbid IS NULL) = (via_name IS NULL)),
    PRIMARY KEY (user_id, release_group_mbid_lower, path_no),
    FOREIGN KEY (user_id, release_group_mbid_lower)
        REFERENCES catalog_discover_candidates (user_id, release_group_mbid_lower)
        ON DELETE CASCADE
) STRICT;

INSERT INTO catalog_discover_candidate_paths_new
    (rowid, user_id, release_group_mbid_lower, path_no, kind, source, fetched_at,
     artist_mbid, artist_name, artist_plays, artist_weight, artist_albums_owned,
     link_strength, link_rank, link_of, named_by_labs, named_by_lastfm, known_credit_mbid,
     link_closest_new)
SELECT rowid, user_id, release_group_mbid_lower, path_no, kind, source, fetched_at,
    artist_mbid, artist_name, artist_plays, artist_weight, artist_albums_owned,
    link_strength, link_rank, link_of, named_by_labs, named_by_lastfm, known_credit_mbid,
    link_closest_new
FROM catalog_discover_candidate_paths
WHERE source != 'data_file';

DROP TABLE catalog_discover_candidate_paths;

ALTER TABLE catalog_discover_candidate_paths_new RENAME TO catalog_discover_candidate_paths;

PRAGMA user_version = 181;
