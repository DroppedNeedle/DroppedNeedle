-- An album's listing revision moves whenever it is renamed, its album
-- artist changes, or it gains or loses a listed (indexed) track, whichever
-- write does it, so a removal sent from a page that showed another name or
-- other tracks is refused even when the count is the same. It is apart from
-- row_revision, which other readers compare and which these changes leave
-- alone. A change counter: nothing is recomputed, so there is no backfill.
-- local_tracks is never written with REPLACE.

ALTER TABLE local_albums ADD COLUMN listing_revision INTEGER NOT NULL DEFAULT 0
    CHECK(listing_revision >= 0);

CREATE TRIGGER trg_local_albums_listing_rename
AFTER UPDATE OF title, album_artist_name, album_artist_id ON local_albums
WHEN OLD.title IS NOT NEW.title
  OR OLD.album_artist_name IS NOT NEW.album_artist_name
  OR OLD.album_artist_id IS NOT NEW.album_artist_id
BEGIN
    UPDATE local_albums SET listing_revision = listing_revision + 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER trg_local_albums_listing_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    UPDATE local_albums SET listing_revision = listing_revision + 1
    WHERE id = NEW.local_album_id;
END;

CREATE TRIGGER trg_local_albums_listing_track_delete
AFTER DELETE ON local_tracks
WHEN OLD.availability = 'indexed'
BEGIN
    UPDATE local_albums SET listing_revision = listing_revision + 1
    WHERE id = OLD.local_album_id;
END;

CREATE TRIGGER trg_local_albums_listing_track_update
AFTER UPDATE OF local_album_id, availability ON local_tracks
WHEN (OLD.local_album_id IS NOT NEW.local_album_id
      OR (OLD.availability = 'indexed') IS NOT (NEW.availability = 'indexed'))
  AND (OLD.availability = 'indexed' OR NEW.availability = 'indexed')
BEGIN
    UPDATE local_albums SET listing_revision = listing_revision + 1
    WHERE (id = OLD.local_album_id AND OLD.availability = 'indexed')
       OR (id = NEW.local_album_id AND NEW.availability = 'indexed');
END;

PRAGMA user_version = 139;
