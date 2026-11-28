-- Each person's Daily Mixes: one for each of their taste groups, built once
-- a day by one job from their own rows and replaced whole. The rows are a
-- result the job computes, not a read model: nothing keeps them by trigger,
-- forgetting a person's history removes them, and a person's rows go with
-- their account.
--
-- `catalog_mixes` holds one row a mix: its place on the page, the UTC day it
-- was built for, its name, the taste group it is for (by its anchor
-- artist's MusicBrainz id, lowercased), the artists its name names (a JSON
-- list), and how many of its tracks the library holds and how many are new.
--
-- `catalog_mix_tracks` holds each mix's tracks in their order: a library
-- track by one of the group's artists (`artist`), a library track found by
-- how it sounds (`sound`), or an album from the person's pool that the
-- library does not hold (`new`), named by its recording and release group,
-- with the reason it was offered as JSON. A library track is named by its
-- id with no foreign key: a track that leaves the library leaves the mix
-- when it is next read.
--
-- The page cache no longer keeps a Daily Mixes memo: its rows go, and its
-- CHECK lists the two kinds left. A saved Discover page that names the
-- Daily Mixes holds them in their old shape, so it goes too and is built
-- again on its person's next visit. Home's rows and every other Discover
-- page are copied. Nothing references the table, so one transaction does.

CREATE TABLE catalog_mixes (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    mix_no INTEGER NOT NULL CHECK (mix_no >= 0),
    day TEXT NOT NULL,
    built_at INTEGER NOT NULL,
    name TEXT NOT NULL,
    group_anchor_mbid TEXT NOT NULL,
    named_artists TEXT NOT NULL,
    yours_count INTEGER NOT NULL CHECK (yours_count >= 0),
    new_count INTEGER NOT NULL CHECK (new_count >= 0),
    UNIQUE (user_id, mix_no)
) STRICT;

CREATE TABLE catalog_mix_tracks (
    mix_id TEXT NOT NULL REFERENCES catalog_mixes(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('artist', 'sound', 'new')),
    local_track_id TEXT,
    recording_mbid TEXT,
    release_group_mbid TEXT,
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    artist_mbid TEXT,
    album_title TEXT,
    reason TEXT,
    CHECK ((kind = 'new') = (local_track_id IS NULL)),
    CHECK ((kind = 'new') = (reason IS NOT NULL)),
    CHECK (kind <> 'new' OR release_group_mbid IS NOT NULL),
    PRIMARY KEY (mix_id, position)
) STRICT;

CREATE TABLE catalog_cached_pages_new (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL CHECK (page IN ('discover', 'home')),
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
WHERE saved.page = 'home'
    OR (saved.page = 'discover' AND CASE
        WHEN json_valid(CAST(saved.payload AS TEXT)) THEN NOT EXISTS (
            SELECT 1 FROM json_tree(CAST(saved.payload AS TEXT)) AS node
            WHERE node.type = 'text' AND node.key = 'key' AND node.atom = 'daily_mixes')
        ELSE 0
    END);

DROP TABLE catalog_cached_pages;

ALTER TABLE catalog_cached_pages_new RENAME TO catalog_cached_pages;

PRAGMA user_version = 184;
