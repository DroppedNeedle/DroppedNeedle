-- 0123_recording_ids_on_links_and_entries.sql - the recording a YouTube track
-- link or an imported playlist entry stands for.
--
--   catalog_youtube_track_links.recording_mbid (new column)
--       Read from the album's tracklist when the link is made. A link from
--       before this column stays NULL and takes an id only when read, from
--       the library's tracks of the same album.
--   catalog_playlist_tracks.recording_mbid (new column)
--       The id the remote server gave for an imported entry. Entries from
--       before this column stay NULL.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE catalog_youtube_track_links ADD COLUMN recording_mbid TEXT;
ALTER TABLE catalog_playlist_tracks ADD COLUMN recording_mbid TEXT;

PRAGMA user_version = 123;
