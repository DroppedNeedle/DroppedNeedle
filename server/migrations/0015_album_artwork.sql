-- 0015_album_artwork.sql - local album art found by the library scan.
--
-- After each scan the library records the cover it finds for every album:
-- an image in the album folder (cover.jpg, folder.png and the like) or a
-- picture embedded in one of its tracks.
--
-- - local_album_artwork learns the 'folder' source and keeps the image's
--   content hash. The hash is the art's identity: when it changes, the row's
--   version goes up, and every cover URL and compat cover id that carries
--   the version changes with it, so clients fetch the new art.
-- - local_album_artwork_checks remembers what the scan last looked at for
--   each album (a signature of its track files and folders), so a scan only
--   re-reads albums whose files changed, including albums with no art. It
--   also keeps the highest art version issued, so art that disappears and
--   comes back never reuses an old version.
-- - An index on release MBIDs lets the release cover route find the local
--   album for a release without a table scan.
--
-- SQLite cannot widen a CHECK constraint in place, so local_album_artwork is
-- rebuilt with its rows kept. Dropping the old table drops its three genre
-- artwork triggers; they are recreated unchanged below.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE local_album_artwork_next (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    cover_url TEXT,
    source TEXT NOT NULL CHECK(source IN ('embedded','folder','cover_cache','manual','provider')),
    source_locator TEXT,
    content_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version BETWEEN 1 AND 9223372036854775807),
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

INSERT INTO local_album_artwork_next
    (local_album_id, cover_url, source, source_locator, version, updated_at, row_revision)
SELECT local_album_id, cover_url, source, source_locator, version, updated_at, row_revision
FROM local_album_artwork;

DROP TABLE local_album_artwork;

ALTER TABLE local_album_artwork_next RENAME TO local_album_artwork;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_insert
AFTER INSERT ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_update
AFTER UPDATE ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_artwork_delete
AFTER DELETE ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = OLD.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TABLE IF NOT EXISTS local_album_artwork_checks (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    signature TEXT NOT NULL,
    checked_at REAL NOT NULL,
    art_version INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_local_album_identity_release
    ON local_album_external_identities(release_mbid);

-- Migration high-water mark.
PRAGMA user_version = 15;
