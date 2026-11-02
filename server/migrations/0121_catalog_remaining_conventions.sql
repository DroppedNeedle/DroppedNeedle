-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0121_catalog_remaining_conventions.sql - people's favourites, follows, ratings
-- and marks, the new-release feed, YouTube links and two kept summaries follow
-- the schema conventions.
--
--   library_user_favorites           catalog_user_favorites
--   library_user_favorite_names      catalog_user_favorite_names
--   library_user_ratings             catalog_user_ratings
--   library_user_play_marks          catalog_user_play_marks
--   user_new_release_seen            catalog_new_release_seen_marks
--   youtube_links                    catalog_youtube_links
--   youtube_track_links              catalog_youtube_track_links
--   library_genre_artwork_revisions  catalog_genre_artwork_revisions
--   library_track_format_stats       catalog_track_format_stats
--   user_followed_artists            acquire_followed_artists
--   new_release_feed                 acquire_new_release_feed
--
-- The last two move to acquire, the only area that writes them. Ratings and
-- marks were already STRICT, in milliseconds and keyed, so only their names
-- change. The rest are rebuilt as STRICT tables, rowids and indexes carried (the
-- rebuild recipe in kernel/src/schema/boot.rs). What each row becomes:
--
--   * a time in REAL seconds becomes integer milliseconds, rounded. A YouTube
--     link's ISO text does too. One that is no time becomes 0, and the row stays;
--   * a favourite gains a key to its user, cascading, so a user's favourites go
--     with the user from now on;
--   * auto_download and is_manual become checked flags. Anything but 0 or NULL
--     reads as 1. A missing track_count reads as 0.
--
-- A favourite, follow or seen mark whose user has no account is not copied, nor
-- is a favourite's name with no favourite. Deleting a user used to leave their
-- favourites behind: no one could read them and a key cannot hold them. Rows with
-- a key of the wrong type cannot be named by any read and are not copied either.
-- No row of a user who still exists is lost, and no other bad value can refuse
-- the upgrade.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE catalog_rest_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_121 INTEGER CONSTRAINT guard_stamp_below_121 CHECK (stamp_below_121 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO catalog_rest_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO catalog_rest_guard (stamp_below_121)
    SELECT (SELECT user_version FROM pragma_user_version) < 121;

CREATE TEMP TABLE catalog_rest_orphans AS
    SELECT "table" AS child, rowid AS row, parent FROM pragma_foreign_key_check;
CREATE INDEX temp.catalog_rest_orphans_at ON catalog_rest_orphans(child, row, parent);

-- Every trigger that names these tables goes first and comes back at the end,
-- the same but for the name.
DROP TRIGGER trg_genre_artwork_album_update;
DROP TRIGGER trg_genre_artwork_artwork_delete;
DROP TRIGGER trg_genre_artwork_artwork_insert;
DROP TRIGGER trg_genre_artwork_artwork_update;
DROP TRIGGER trg_genre_artwork_normalized_delete;
DROP TRIGGER trg_genre_artwork_normalized_insert;
DROP TRIGGER trg_genre_artwork_normalized_update;
DROP TRIGGER trg_genre_artwork_track_delete;
DROP TRIGGER trg_genre_artwork_track_insert;
DROP TRIGGER trg_genre_artwork_track_update;
DROP TRIGGER trg_track_stats_add;
DROP TRIGGER trg_track_stats_delete;
DROP TRIGGER trg_track_stats_insert;
DROP TRIGGER trg_track_stats_remove;
DROP TRIGGER trg_user_album_plays_mark_delete;
DROP TRIGGER trg_user_album_plays_mark_insert;
DROP TRIGGER trg_user_album_plays_mark_update;
DROP TRIGGER trg_user_album_plays_refresh;

ALTER TABLE library_user_ratings RENAME TO catalog_user_ratings;
DROP INDEX idx_user_ratings_item;
CREATE INDEX idx_catalog_user_ratings_item ON catalog_user_ratings(item_kind, item_id);

ALTER TABLE library_user_play_marks RENAME TO catalog_user_play_marks;
DROP INDEX idx_user_play_marks_track;
CREATE INDEX idx_catalog_user_play_marks_track ON catalog_user_play_marks(local_track_id);

CREATE TABLE catalog_user_favorites (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('artist', 'album', 'track')),
    item_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id)
) STRICT;
INSERT INTO catalog_user_favorites (rowid, user_id, item_kind, item_id, created_at)
SELECT rowid, user_id, item_kind, item_id,
    CASE WHEN typeof(created_at) IN ('integer', 'real')
        THEN CAST(round(created_at * 1000) AS INTEGER) ELSE 0 END
FROM library_user_favorites
WHERE user_id IN (SELECT id FROM auth_users) AND typeof(item_id) = 'text';
CREATE INDEX idx_catalog_user_favorites_user_kind ON catalog_user_favorites(user_id, item_kind);

CREATE TABLE catalog_user_favorite_names (
    user_id TEXT NOT NULL,
    item_kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id),
    FOREIGN KEY (user_id, item_kind, item_id)
        REFERENCES catalog_user_favorites(user_id, item_kind, item_id) ON DELETE CASCADE
) STRICT;
INSERT INTO catalog_user_favorite_names (rowid, user_id, item_kind, item_id, display_name)
SELECT n.rowid, n.user_id, n.item_kind, n.item_id, CAST(n.display_name AS TEXT)
FROM library_user_favorite_names n
JOIN catalog_user_favorites f
    ON f.user_id = n.user_id AND f.item_kind = n.item_kind AND f.item_id = n.item_id;

CREATE TABLE acquire_followed_artists (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    auto_download INTEGER NOT NULL DEFAULT 0 CHECK (auto_download IN (0, 1)),
    followed_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, artist_mbid_lower)
) STRICT;
INSERT INTO acquire_followed_artists (rowid, user_id, artist_mbid, artist_mbid_lower, artist_name,
    auto_download, followed_at, updated_at)
SELECT rowid, user_id, CAST(artist_mbid AS TEXT), artist_mbid_lower, CAST(artist_name AS TEXT),
    CASE WHEN auto_download != 0 THEN 1 ELSE 0 END,
    CASE WHEN typeof(followed_at) IN ('integer', 'real')
        THEN CAST(round(followed_at * 1000) AS INTEGER) ELSE 0 END,
    CASE WHEN typeof(updated_at) IN ('integer', 'real')
        THEN CAST(round(updated_at * 1000) AS INTEGER) ELSE 0 END
FROM user_followed_artists
WHERE user_id IN (SELECT id FROM auth_users) AND typeof(artist_mbid_lower) = 'text';
CREATE INDEX idx_acquire_followed_artists_user ON acquire_followed_artists(user_id);
CREATE INDEX idx_acquire_followed_artists_mbid ON acquire_followed_artists(artist_mbid_lower);
CREATE INDEX idx_acquire_followed_artists_auto_download
    ON acquire_followed_artists(auto_download) WHERE auto_download = 1;

CREATE TABLE acquire_new_release_feed (
    release_group_mbid_lower TEXT PRIMARY KEY,
    release_group_mbid TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    title TEXT NOT NULL,
    primary_type TEXT,
    secondary_types TEXT,
    first_release_date TEXT,
    discovered_at INTEGER NOT NULL
) STRICT;
INSERT INTO acquire_new_release_feed (rowid, release_group_mbid_lower, release_group_mbid,
    artist_mbid_lower, artist_name, title, primary_type, secondary_types, first_release_date,
    discovered_at)
SELECT rowid, release_group_mbid_lower, CAST(release_group_mbid AS TEXT),
    CAST(artist_mbid_lower AS TEXT), CAST(artist_name AS TEXT), CAST(title AS TEXT),
    CAST(primary_type AS TEXT), CAST(secondary_types AS TEXT), CAST(first_release_date AS TEXT),
    CASE WHEN typeof(discovered_at) IN ('integer', 'real')
        THEN CAST(round(discovered_at * 1000) AS INTEGER) ELSE 0 END
FROM new_release_feed WHERE typeof(release_group_mbid_lower) = 'text';
CREATE INDEX idx_acquire_new_release_feed_artist ON acquire_new_release_feed(artist_mbid_lower);
CREATE INDEX idx_acquire_new_release_feed_date
    ON acquire_new_release_feed(first_release_date DESC);

CREATE TABLE catalog_new_release_seen_marks (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at INTEGER NOT NULL
) STRICT;
INSERT INTO catalog_new_release_seen_marks (rowid, user_id, seen_at)
SELECT rowid, user_id,
    CASE WHEN typeof(seen_at) IN ('integer', 'real')
        THEN CAST(round(seen_at * 1000) AS INTEGER) ELSE 0 END
FROM user_new_release_seen WHERE user_id IN (SELECT id FROM auth_users);

CREATE TABLE catalog_youtube_links (
    album_id TEXT PRIMARY KEY,
    video_id TEXT,
    album_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    embed_url TEXT,
    cover_url TEXT,
    created_at INTEGER NOT NULL,
    is_manual INTEGER NOT NULL DEFAULT 0 CHECK (is_manual IN (0, 1)),
    track_count INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO catalog_youtube_links (rowid, album_id, video_id, album_name, artist_name, embed_url,
    cover_url, created_at, is_manual, track_count)
SELECT rowid, album_id, CAST(video_id AS TEXT), CAST(album_name AS TEXT),
    CAST(artist_name AS TEXT), CAST(embed_url AS TEXT), CAST(cover_url AS TEXT),
    COALESCE(CAST(round(unixepoch(created_at, 'subsec') * 1000) AS INTEGER), 0),
    CASE WHEN COALESCE(is_manual, 0) != 0 THEN 1 ELSE 0 END,
    CASE WHEN typeof(track_count) = 'integer' THEN track_count ELSE 0 END
FROM youtube_links WHERE typeof(album_id) = 'text';

CREATE TABLE catalog_youtube_track_links (
    album_id TEXT NOT NULL,
    track_number INTEGER NOT NULL,
    disc_number INTEGER NOT NULL DEFAULT 1,
    album_name TEXT NOT NULL,
    track_name TEXT NOT NULL,
    video_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    embed_url TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (album_id, disc_number, track_number)
) STRICT;
INSERT INTO catalog_youtube_track_links (rowid, album_id, track_number, disc_number, album_name,
    track_name, video_id, artist_name, embed_url, created_at)
SELECT rowid, album_id, track_number, disc_number, CAST(album_name AS TEXT),
    CAST(track_name AS TEXT), CAST(video_id AS TEXT), CAST(artist_name AS TEXT),
    CAST(embed_url AS TEXT),
    COALESCE(CAST(round(unixepoch(created_at, 'subsec') * 1000) AS INTEGER), 0)
FROM youtube_track_links
WHERE typeof(album_id) = 'text' AND typeof(track_number) = 'integer'
    AND typeof(disc_number) = 'integer';

CREATE TABLE catalog_genre_artwork_revisions (
    genre_folded TEXT PRIMARY KEY,
    value INTEGER NOT NULL DEFAULT 1 CHECK (value BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO catalog_genre_artwork_revisions (rowid, genre_folded, value)
SELECT rowid, genre_folded, CAST(value AS INTEGER)
FROM library_genre_artwork_revisions WHERE typeof(genre_folded) = 'text';

CREATE TABLE catalog_track_format_stats (
    file_format TEXT PRIMARY KEY,
    indexed_tracks INTEGER NOT NULL DEFAULT 0 CHECK (indexed_tracks >= 0),
    indexed_bytes INTEGER NOT NULL DEFAULT 0 CHECK (indexed_bytes >= 0)
) STRICT;
INSERT INTO catalog_track_format_stats (rowid, file_format, indexed_tracks, indexed_bytes)
SELECT rowid, file_format, CAST(indexed_tracks AS INTEGER), CAST(indexed_bytes AS INTEGER)
FROM library_track_format_stats WHERE typeof(file_format) = 'text';

DROP TABLE library_user_favorite_names;
DROP TABLE library_user_favorites;
DROP TABLE user_followed_artists;
DROP TABLE new_release_feed;
DROP TABLE user_new_release_seen;
DROP TABLE youtube_track_links;
DROP TABLE youtube_links;
DROP TABLE library_genre_artwork_revisions;
DROP TABLE library_track_format_stats;

CREATE TRIGGER trg_genre_artwork_album_update
AFTER UPDATE OF title, album_artist_id, album_artist_name, retired_into_album_id
ON local_albums
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_artwork_delete
AFTER DELETE ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = OLD.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_artwork_insert
AFTER INSERT ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_artwork_update
AFTER UPDATE ON local_album_artwork
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_normalized_delete
AFTER DELETE ON local_track_genres
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_normalized_insert
AFTER INSERT ON local_track_genres
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    VALUES (NEW.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_normalized_update
AFTER UPDATE OF name, folded_name ON local_track_genres
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT NEW.folded_name, 1 WHERE NEW.folded_name != OLD.folded_name
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_track_delete
AFTER DELETE ON local_tracks
WHEN OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_genre_artwork_track_update
AFTER UPDATE OF genre, genre_folded, availability, local_album_id ON local_tracks
BEGIN
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1
    WHERE OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
    INSERT INTO catalog_genre_artwork_revisions(genre_folded, value)
    SELECT COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1
    WHERE NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
      AND COALESCE(NEW.genre_folded, lower(trim(NEW.genre)))
          != COALESCE(OLD.genre_folded, lower(trim(OLD.genre)), '')
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER trg_track_stats_add
AFTER UPDATE ON local_tracks
WHEN NEW.availability = 'indexed'
 AND (OLD.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    INSERT INTO catalog_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
END;

CREATE TRIGGER trg_track_stats_delete
AFTER DELETE ON local_tracks
WHEN OLD.availability = 'indexed'
BEGIN
    UPDATE catalog_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;

CREATE TRIGGER trg_track_stats_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    INSERT INTO catalog_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
END;

CREATE TRIGGER trg_track_stats_remove
AFTER UPDATE ON local_tracks
WHEN OLD.availability = 'indexed'
 AND (NEW.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    UPDATE catalog_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;

CREATE TRIGGER trg_user_album_plays_mark_delete
AFTER DELETE ON catalog_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id));
END;

CREATE TRIGGER trg_user_album_plays_mark_insert
AFTER INSERT ON catalog_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (NEW.user_id, (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id));
END;

CREATE TRIGGER trg_user_album_plays_mark_update
AFTER UPDATE ON catalog_user_play_marks
WHEN OLD.user_id IS NOT NEW.user_id
  OR OLD.local_track_id IS NOT NEW.local_track_id
  OR OLD.marked_played_at IS NOT NEW.marked_played_at
  OR OLD.marked_unplayed_at IS NOT NEW.marked_unplayed_at
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id)
    UNION
    SELECT NEW.user_id, (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id);
END;

CREATE TRIGGER trg_user_album_plays_refresh
INSTEAD OF INSERT ON library_user_album_plays_refresh
WHEN NEW.local_album_id IS NOT NULL
 AND EXISTS (SELECT 1 FROM local_albums WHERE id = NEW.local_album_id)
BEGIN
    DELETE FROM library_user_album_plays
    WHERE user_id = NEW.user_id AND local_album_id = NEW.local_album_id;
    INSERT INTO library_user_album_plays (user_id, local_album_id, plays, last_played_at, played)
    SELECT NEW.user_id, NEW.local_album_id, SUM(plays), MAX(last) * 1000,
           SUM(indexed) > 0 AND SUM(indexed AND plays = 0) = 0
    FROM (
        SELECT indexed,
               CASE WHEN counted > 0 THEN counted
                    WHEN marked IS NOT NULL THEN 1 ELSE 0 END AS plays,
               CASE WHEN counted > 0 THEN COALESCE(latest, marked) ELSE marked END AS last
        FROM (
            SELECT t.availability = 'indexed' AS indexed,
                   m.marked_played_at / 1000 AS marked,
                   (SELECT COUNT(*)
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR unixepoch(h.played_at) > m.marked_unplayed_at / 1000)) AS counted,
                   (SELECT MAX(unixepoch(h.played_at))
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR unixepoch(h.played_at) > m.marked_unplayed_at / 1000)) AS latest
            FROM local_tracks t
            LEFT JOIN catalog_user_play_marks m
                ON m.user_id = NEW.user_id AND m.local_track_id = t.id
            WHERE t.local_album_id = NEW.local_album_id
        )
        UNION ALL
        SELECT 0, COUNT(*), MAX(unixepoch(h.played_at))
        FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track
        WHERE h.user_id = NEW.user_id AND h.local_track_id IS NULL
          AND h.local_album_id = NEW.local_album_id AND h.local_album_id IS NOT NULL
    )
    HAVING SUM(plays) > 0;
END;

-- The planner's statistics on these tables went with them. With no record of
-- a run, the next check takes them again.
DELETE FROM db_planner_stats;

-- The two renamed tables keep any row that was already an orphan.
UPDATE catalog_rest_orphans SET child = 'catalog_user_ratings' WHERE child = 'library_user_ratings';
UPDATE catalog_rest_orphans SET child = 'catalog_user_play_marks' WHERE child = 'library_user_play_marks';

INSERT INTO catalog_rest_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM catalog_rest_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE catalog_rest_guard;
DROP TABLE catalog_rest_orphans;

-- Migration high-water mark.
PRAGMA user_version = 121;
COMMIT;
PRAGMA foreign_keys=ON;
