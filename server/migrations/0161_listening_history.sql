-- A copy of each person's ListenBrainz history, read from an export file
-- they upload, and where their import of it stands. An imported listen is
-- history only: it is never a play, and no play count, scrobble or
-- statistic reads it. A person's rows go with their account.
--
-- `media_imported_listens` holds each listen once: by its time, artist and
-- track, the names compared without case, so reading the same file again
-- adds nothing. Its unique index starts with the person and the time, and
-- serves the reads by time too. `artist_mbid` is the first of the listen's
-- artist ids. ListenBrainz names no release group, so
-- `release_group_mbid` stays empty for now.
--
-- `media_history_imports` holds one row a person. `import_id` names the
-- file being read and `entries_read` how many of its month files are
-- done. How many listens a person has, and the newest, are read from the
-- listens themselves, never kept here. Both lists are whole now, values
-- later work first writes included, since a CHECK cannot change without
-- rebuilding its table: the `pending` and `exporting` states and the
-- `export` source are first written by the export fetched from
-- ListenBrainz.

CREATE TABLE media_imported_listens (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    listened_at INTEGER NOT NULL,
    recording_mbid TEXT,
    artist_mbid TEXT,
    artist_name TEXT NOT NULL,
    track_name TEXT NOT NULL,
    release_name TEXT,
    release_mbid TEXT,
    release_group_mbid TEXT,
    imported_at INTEGER NOT NULL
) STRICT;

CREATE UNIQUE INDEX idx_media_imported_listens_once
    ON media_imported_listens (user_id, listened_at, lower(artist_name), lower(track_name));

CREATE INDEX idx_media_imported_listens_user_artist
    ON media_imported_listens (user_id, artist_mbid);

CREATE TABLE media_history_imports (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    state TEXT NOT NULL
        CHECK (state IN ('pending', 'exporting', 'importing', 'current', 'failed', 'forgotten')),
    source TEXT CHECK (source IN ('upload', 'export')),
    account TEXT,
    export_id TEXT,
    import_id TEXT,
    entries_read INTEGER NOT NULL DEFAULT 0 CHECK (entries_read >= 0),
    last_synced_at INTEGER,
    updated_at INTEGER NOT NULL
) STRICT;

PRAGMA user_version = 161;
