-- Playlist entries with no source yet, by the release group they name:
-- what the pass that links entries to the library reads after each library
-- change. Linked entries, most of them over time, stay out of it.

CREATE INDEX idx_catalog_playlist_tracks_unlinked
    ON catalog_playlist_tracks(album_id)
    WHERE source_type = '' AND album_id IS NOT NULL;

PRAGMA user_version = 152;
