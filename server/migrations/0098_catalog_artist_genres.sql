-- 0098_catalog_artist_genres.sql - the genres Discover has learned per
-- artist, rebuilt to the schema conventions.
--
--   catalog_artist_genres (from artist_genres)
--       one row per artist and genre, learned from provider answers a
--       Discover build already fetched (MusicBrainz and Last.fm tag
--       searches, Last.fm artist tags). artist_mbid is lowercased,
--       genre_folded is the genre trimmed and lowercased, genre the name
--       as the provider gave it. learned_at is the last build that saw
--       the pair; rows not seen for a while are pruned by the builds.
--       The library's own tags are read from the scan's tables, not
--       copied here.
--   artist_genres (dropped)
--       v2's per-artist JSON list. Nothing in v3 wrote it, so there is
--       nothing to carry.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TABLE artist_genres;

CREATE TABLE catalog_artist_genres (
    artist_mbid TEXT NOT NULL CHECK (length(artist_mbid) > 0),
    genre_folded TEXT NOT NULL CHECK (length(genre_folded) > 0),
    genre TEXT NOT NULL,
    learned_at INTEGER NOT NULL,
    PRIMARY KEY (artist_mbid, genre_folded)
) STRICT;

CREATE INDEX idx_catalog_artist_genres_learned
    ON catalog_artist_genres(learned_at);

-- Migration high-water mark.
PRAGMA user_version = 98;
