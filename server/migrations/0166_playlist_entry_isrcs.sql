-- The ISRC a playlist file gave for an entry the library lacked, uppercased:
-- the strongest proof of the album the song is on. NULL when the file gave
-- none, and on every entry written before.

ALTER TABLE catalog_playlist_tracks ADD COLUMN isrc TEXT
    CHECK (isrc IS NULL OR length(isrc) = 12);

PRAGMA user_version = 166;
