-- 0041_drop_empty_tables.sql - drop tables no code ever fills.
--
-- These came with the baseline's copy of v2's schema. The server read,
-- updated or pruned a few of them but never inserted a row, and the v2
-- importer carries nothing into them, so every one is empty. They go
-- with their indexes. The four parents were kept by 0040 only because a
-- table in this list pointed at them.
--
-- Merged artists and albums keep resolving through local_artist_aliases
-- and local_album_aliases, which stay. No track alias was ever saved.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Tables that point at another table in this list go before it.

-- Older copies of tables that live on under another name: favourites
-- are library_user_favorites, playlists are playlists and
-- playlist_tracks.
DROP TABLE IF EXISTS user_favorites;
DROP TABLE IF EXISTS library_playlist_tracks;
DROP TABLE IF EXISTS library_playlists;
DROP TABLE IF EXISTS library_compat_id_map;

-- Discover.
DROP TABLE IF EXISTS discovery_optional_progress;

-- Identification and scan.
DROP TABLE IF EXISTS local_track_aliases;
DROP TABLE IF EXISTS library_edition_conversion_jobs;
DROP TABLE IF EXISTS library_scan_grouping_groups;
DROP TABLE IF EXISTS library_scan_grouping_contexts;

-- Library Management. File writes are journalled in the
-- library_publish_* tables.
DROP TABLE IF EXISTS library_management_blob_references;
DROP TABLE IF EXISTS library_file_mutation_journal;
DROP TABLE IF EXISTS library_management_operation_snapshots;

-- v2's own library upgrade.
DROP TABLE IF EXISTS library_migration_provenance;
DROP TABLE IF EXISTS library_migration_runs;

-- Migration high-water mark.
PRAGMA user_version = 41;
