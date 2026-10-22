-- 0080_library_artist_counts.sql - each artist's album and appearance
-- counts, kept so the artist list sorted by either count reads a page by
-- key instead of counting every artist's albums and credits.
--
--   library_artist_album_credits
--       one row per artist and album where the artist is credited on at
--       least one indexed track of the album, whatever the album's state:
--         indexed_tracks  the album's indexed tracks credited to the artist
--         live            1 when the album is not retired
--         appearance      1 when the album is live and led by someone else
--
--   library_artist_counts
--       one row per live artist credited on an owned album, on an indexed
--       track of a live album, or leading an owned album (an owned album is
--       live and has an indexed track):
--         folded_name       the artist's folded name, for ties and the name
--                           filter
--         album_count       owned albums the artist leads
--         appearance_count  live albums led by someone else holding an
--                           indexed track credited to the artist
--       and the two counts negated, so a list sorted by a count, most
--       first, with ties by name A to Z, walks one index in one direction.
--
-- No code writes either table. The triggers below keep them, inside the
-- transaction of every write to local_track_artists, local_tracks,
-- local_albums, local_album_artists and local_artists, whoever makes it.
-- Each rule lives once: inserting (artist, album) into
-- library_artist_album_credits_refresh works that pair out again from the
-- source rows, and inserting an artist into library_artist_counts_refresh
-- works its row out again through the view library_artist_counts_rule.
-- The backfill and every trigger go through them.
-- idx_local_albums_owned_leader serves that rule's album count.
--
-- Writes that need no trigger: a track, album or artist is inserted before
-- any credit names it and deleted only after its credits are gone (the
-- credit keys restrict), and a new album has no indexed track.
--
-- The followed tables are never written with REPLACE, which removes rows
-- without firing their delete triggers. A later migration that rebuilds
-- one of them drops these triggers with it and must create them again
-- (the schema test counts the triggers). Changing a rule needs a migration
-- that recreates its refresh and runs the backfill again.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE INDEX idx_local_albums_owned_leader
    ON local_albums(album_artist_id, retired_into_album_id, has_indexed_track)
    WHERE retired_into_album_id IS NULL AND has_indexed_track = 1;

CREATE TABLE library_artist_album_credits (
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    indexed_tracks INTEGER NOT NULL CHECK (indexed_tracks > 0),
    live INTEGER NOT NULL CHECK (live IN (0, 1)),
    appearance INTEGER NOT NULL CHECK (appearance IN (0, 1)),
    PRIMARY KEY (local_artist_id, local_album_id)
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_artist_album_credits_album ON library_artist_album_credits(local_album_id);
CREATE INDEX idx_artist_album_credits_appearances ON library_artist_album_credits(local_artist_id)
    WHERE appearance = 1;

CREATE TABLE library_artist_counts (
    local_artist_id TEXT PRIMARY KEY REFERENCES local_artists(id) ON DELETE CASCADE,
    folded_name TEXT NOT NULL,
    album_count INTEGER NOT NULL CHECK (album_count >= 0),
    appearance_count INTEGER NOT NULL CHECK (appearance_count >= 0),
    album_count_neg INTEGER GENERATED ALWAYS AS (-album_count) STORED,
    appearance_count_neg INTEGER GENERATED ALWAYS AS (-appearance_count) STORED
) STRICT, WITHOUT ROWID;

CREATE INDEX idx_artist_counts_albums
    ON library_artist_counts(album_count, folded_name, local_artist_id);
CREATE INDEX idx_artist_counts_albums_desc
    ON library_artist_counts(album_count_neg, folded_name, local_artist_id);
CREATE INDEX idx_artist_counts_appearances
    ON library_artist_counts(appearance_count, folded_name, local_artist_id, album_count);
CREATE INDEX idx_artist_counts_appearances_desc
    ON library_artist_counts(appearance_count_neg, folded_name, local_artist_id, album_count);

-- The pair refresh. The count is driven from the album's tracks, never the
-- artist's credits, so it costs the album's size. An unchanged pair is not
-- rewritten.
CREATE VIEW library_artist_album_credits_refresh (local_artist_id, local_album_id) AS
    SELECT NULL, NULL WHERE 0;

CREATE TRIGGER trg_artist_album_credits_refresh
INSTEAD OF INSERT ON library_artist_album_credits_refresh
WHEN NEW.local_artist_id IS NOT NULL AND NEW.local_album_id IS NOT NULL
BEGIN
    DELETE FROM library_artist_album_credits
    WHERE local_artist_id = NEW.local_artist_id AND local_album_id = NEW.local_album_id
      AND NOT EXISTS (
          SELECT 1 FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
          WHERE t.local_album_id = NEW.local_album_id AND t.availability = 'indexed'
            AND EXISTS (SELECT 1 FROM local_track_artists c
                        WHERE c.local_track_id = t.id
                          AND c.local_artist_id = NEW.local_artist_id));
    INSERT INTO library_artist_album_credits
        (local_artist_id, local_album_id, indexed_tracks, live, appearance)
    SELECT NEW.local_artist_id, b.id, n.tracks, b.retired_into_album_id IS NULL,
           b.retired_into_album_id IS NULL AND b.album_artist_id != NEW.local_artist_id
    FROM local_albums b,
         (SELECT COUNT(*) AS tracks
          FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
          WHERE t.local_album_id = NEW.local_album_id AND t.availability = 'indexed'
            AND EXISTS (SELECT 1 FROM local_track_artists c
                        WHERE c.local_track_id = t.id
                          AND c.local_artist_id = NEW.local_artist_id)) n
    WHERE b.id = NEW.local_album_id AND n.tracks > 0
    ON CONFLICT (local_artist_id, local_album_id) DO UPDATE SET
        indexed_tracks = excluded.indexed_tracks,
        live = excluded.live,
        appearance = excluded.appearance
    WHERE indexed_tracks IS NOT excluded.indexed_tracks
       OR live IS NOT excluded.live
       OR appearance IS NOT excluded.appearance;
END;

-- The artist rule: every artist that belongs in library_artist_counts,
-- with its values. The refresh reads it for one artist.
CREATE VIEW library_artist_counts_rule
    (local_artist_id, folded_name, album_count, appearance_count) AS
SELECT r.id, r.folded_name,
       (SELECT COUNT(*) FROM local_albums lo INDEXED BY idx_local_albums_owned_leader
        WHERE lo.album_artist_id = r.id AND lo.retired_into_album_id IS NULL
          AND lo.has_indexed_track = 1),
       (SELECT COUNT(*) FROM library_artist_album_credits p
        WHERE p.local_artist_id = r.id AND p.appearance = 1)
FROM local_artists r
WHERE r.retired_into_artist_id IS NULL
  AND (EXISTS (SELECT 1 FROM local_albums la INDEXED BY idx_local_albums_owned_leader
               WHERE la.album_artist_id = r.id AND la.retired_into_album_id IS NULL
                 AND la.has_indexed_track = 1)
       OR EXISTS (SELECT 1 FROM library_artist_album_credits p
                  WHERE p.local_artist_id = r.id AND p.live = 1)
       OR EXISTS (SELECT 1 FROM local_album_artists laa
                  JOIN local_albums lb ON lb.id = laa.local_album_id
                  WHERE laa.local_artist_id = r.id AND lb.retired_into_album_id IS NULL
                    AND lb.has_indexed_track = 1));

CREATE VIEW library_artist_counts_refresh (local_artist_id) AS SELECT NULL WHERE 0;

-- The artist refresh. An unchanged row is not rewritten.
CREATE TRIGGER trg_artist_counts_refresh
INSTEAD OF INSERT ON library_artist_counts_refresh
WHEN NEW.local_artist_id IS NOT NULL
BEGIN
    DELETE FROM library_artist_counts
    WHERE local_artist_id = NEW.local_artist_id
      AND NOT EXISTS (SELECT 1 FROM library_artist_counts_rule
                      WHERE local_artist_id = NEW.local_artist_id);
    INSERT INTO library_artist_counts
        (local_artist_id, folded_name, album_count, appearance_count)
    SELECT local_artist_id, folded_name, album_count, appearance_count
    FROM library_artist_counts_rule WHERE local_artist_id = NEW.local_artist_id
    ON CONFLICT (local_artist_id) DO UPDATE SET
        folded_name = excluded.folded_name,
        album_count = excluded.album_count,
        appearance_count = excluded.appearance_count
    WHERE folded_name IS NOT excluded.folded_name
       OR album_count IS NOT excluded.album_count
       OR appearance_count IS NOT excluded.appearance_count;
END;

-- Backfill, through the same refreshes, before the triggers that follow
-- the pairs exist: the pairs first, then every live artist once.
INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
SELECT DISTINCT c.local_artist_id, t.local_album_id
FROM local_track_artists c JOIN local_tracks t ON t.id = c.local_track_id
WHERE t.availability = 'indexed';

INSERT INTO library_artist_counts_refresh (local_artist_id)
SELECT id FROM local_artists WHERE retired_into_artist_id IS NULL;

-- Pairs. Their track count alone changes no artist row. A new pair that
-- is no appearance only adds to an artist already listed when one is; a
-- pair on a retired album is read by neither count nor the listing.
CREATE TRIGGER trg_artist_counts_pair_insert
AFTER INSERT ON library_artist_album_credits
WHEN NEW.appearance = 1
  OR NOT EXISTS (SELECT 1 FROM library_artist_counts
                 WHERE local_artist_id = NEW.local_artist_id)
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_pair_delete
AFTER DELETE ON library_artist_album_credits
WHEN OLD.live = 1
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (OLD.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_pair_update
AFTER UPDATE OF live, appearance ON library_artist_album_credits
WHEN OLD.live IS NOT NEW.live OR OLD.appearance IS NOT NEW.appearance
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.local_artist_id);
END;

-- Track credits, for the credit's artist on its track's album.
CREATE TRIGGER trg_artist_counts_credit_insert
AFTER INSERT ON local_track_artists
BEGIN
    INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
    SELECT NEW.local_artist_id, local_album_id FROM local_tracks WHERE id = NEW.local_track_id;
END;

CREATE TRIGGER trg_artist_counts_credit_delete
AFTER DELETE ON local_track_artists
BEGIN
    INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
    SELECT OLD.local_artist_id, local_album_id FROM local_tracks WHERE id = OLD.local_track_id;
END;

CREATE TRIGGER trg_artist_counts_credit_update
AFTER UPDATE OF local_artist_id, local_track_id ON local_track_artists
WHEN OLD.local_artist_id IS NOT NEW.local_artist_id
  OR OLD.local_track_id IS NOT NEW.local_track_id
BEGIN
    INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
    SELECT OLD.local_artist_id, local_album_id FROM local_tracks WHERE id = OLD.local_track_id
    UNION
    SELECT NEW.local_artist_id, local_album_id FROM local_tracks WHERE id = NEW.local_track_id;
END;

-- A track moved, or crossing 'indexed' either way: every artist credited
-- on it, on the album it left and the album it is on.
CREATE TRIGGER trg_artist_counts_track_update
AFTER UPDATE OF availability, local_album_id ON local_tracks
WHEN OLD.local_album_id IS NOT NEW.local_album_id
  OR (OLD.availability = 'indexed') IS NOT (NEW.availability = 'indexed')
BEGIN
    INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
    SELECT DISTINCT c.local_artist_id, x.id
    FROM local_track_artists c
    CROSS JOIN (SELECT OLD.local_album_id AS id UNION SELECT NEW.local_album_id) x
    WHERE c.local_track_id = NEW.id;
END;

-- Album credits count toward whether an artist is listed, so a new one
-- changes nothing for an artist already listed.
CREATE TRIGGER trg_artist_counts_album_credit_insert
AFTER INSERT ON local_album_artists
WHEN NOT EXISTS (SELECT 1 FROM library_artist_counts
                 WHERE local_artist_id = NEW.local_artist_id)
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_album_credit_delete
AFTER DELETE ON local_album_artists
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (OLD.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_album_credit_update
AFTER UPDATE OF local_artist_id, local_album_id ON local_album_artists
WHEN OLD.local_artist_id IS NOT NEW.local_artist_id
  OR OLD.local_album_id IS NOT NEW.local_album_id
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id)
    SELECT OLD.local_artist_id UNION SELECT NEW.local_artist_id;
END;

-- An album's leader, retirement or ownership. A new leader or a retirement
-- either way changes the album's pairs; any of the three changes the
-- leaders' album counts and whether its album-credited artists are listed.
-- Repointing an album already retired changes nothing the rules read.
CREATE TRIGGER trg_artist_counts_album_update
AFTER UPDATE OF album_artist_id, retired_into_album_id, has_indexed_track ON local_albums
WHEN OLD.album_artist_id IS NOT NEW.album_artist_id
  OR (OLD.retired_into_album_id IS NULL) IS NOT (NEW.retired_into_album_id IS NULL)
  OR OLD.has_indexed_track IS NOT NEW.has_indexed_track
BEGIN
    INSERT INTO library_artist_album_credits_refresh (local_artist_id, local_album_id)
    SELECT local_artist_id, local_album_id FROM library_artist_album_credits
    WHERE local_album_id = NEW.id
      AND (OLD.album_artist_id IS NOT NEW.album_artist_id
           OR (OLD.retired_into_album_id IS NULL) IS NOT (NEW.retired_into_album_id IS NULL));
    INSERT INTO library_artist_counts_refresh (local_artist_id)
    SELECT OLD.album_artist_id
    UNION SELECT NEW.album_artist_id
    UNION SELECT local_artist_id FROM local_album_artists WHERE local_album_id = NEW.id;
END;

-- An artist renamed, retired or brought back.
CREATE TRIGGER trg_artist_counts_artist_update
AFTER UPDATE OF folded_name, retired_into_artist_id ON local_artists
WHEN OLD.folded_name IS NOT NEW.folded_name
  OR (OLD.retired_into_artist_id IS NULL) IS NOT (NEW.retired_into_artist_id IS NULL)
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.id);
END;

-- Migration high-water mark.
PRAGMA user_version = 80;
