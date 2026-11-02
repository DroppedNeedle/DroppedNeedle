-- no-transaction
PRAGMA foreign_keys=OFF;
BEGIN IMMEDIATE;

-- 0119_library_catalog_conventions.sql - the catalog's parent tables and their
-- satellites follow the schema conventions.
--
-- Each table is rebuilt as a STRICT table, rowids, indexes and triggers carried
-- (the rebuild recipe in kernel/src/schema/boot.rs). What each row becomes:
--
--   * a time in REAL seconds becomes integer milliseconds, rounded. One too far
--     off to fit is held at the limit, and a required one that is no number
--     reads as 0, so it has passed. Two times inside one millisecond now read
--     as equal. Undo records, sealed plans and audit rows keep their times in
--     seconds: the code rounds them to the millisecond where it compares them
--     with a row or writes them back;
--   * local_tracks.missing_since becomes missing_at;
--   * local_tracks.search_key becomes the INTEGER PRIMARY KEY, with id kept
--     UNIQUE, so a search hit reaches its row with no extra index probe. Its
--     value is unchanged, so the search index (external content on this key)
--     is not touched, and the insert trigger no longer has a key to fill in.
--     idx_local_tracks_search_key goes: the key is the table's own order;
--   * local_albums.management_schedule_pending is a checked flag. Anything but
--     0 reads as 1.
--
-- duration_seconds and the replay gains stay REAL: they are measurements, not
-- points in time. The delete actions stay as they are.
--
-- Rows with a key of the wrong type cannot be named by any read and are not
-- copied. A whole number held as a fraction is cut down, and kept at 1 or more
-- where its CHECK asks for that. Rows no writer could make (text that casts to
-- a duplicate key, a non-integer search key, a row left pointing at a key of
-- the wrong type) may refuse the upgrade; a refusal is clean and changes
-- nothing.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Each guard refuses through a named CHECK, so a refusal says which one.
CREATE TEMP TABLE library_rebuild_guard (
    foreign_keys_off INTEGER CONSTRAINT guard_foreign_keys_off CHECK (foreign_keys_off = 1),
    stamp_below_119 INTEGER CONSTRAINT guard_stamp_below_119 CHECK (stamp_below_119 = 1),
    no_new_orphan INTEGER CONSTRAINT guard_no_new_orphan CHECK (no_new_orphan = 1)
) STRICT;
INSERT INTO library_rebuild_guard (foreign_keys_off)
    SELECT (SELECT foreign_keys FROM pragma_foreign_keys) = 0;
INSERT INTO library_rebuild_guard (stamp_below_119)
    SELECT (SELECT user_version FROM pragma_user_version) < 119;

-- A track's rowid becomes its search key, so an old orphan track is saved by key.
CREATE TEMP TABLE library_rebuild_orphans AS
    SELECT check_row."table" AS child,
        CASE WHEN check_row."table" = 'local_tracks'
            THEN (SELECT search_key FROM local_tracks WHERE rowid = check_row.rowid)
            ELSE check_row.rowid END AS row,
        check_row.parent AS parent
    FROM pragma_foreign_key_check AS check_row;
CREATE INDEX temp.library_rebuild_orphans_at
    ON library_rebuild_orphans(child, row, parent);

-- Everything that names these tables goes first and comes back at the end, word
-- for word, so nothing fires during the copy.
DROP TRIGGER trg_genre_artwork_normalized_insert;
DROP TRIGGER trg_genre_artwork_normalized_delete;
DROP TRIGGER trg_genre_artwork_normalized_update;
DROP TRIGGER trg_normalized_genres_legacy_scalar_update;
DROP TRIGGER trg_genre_artwork_track_insert;
DROP TRIGGER trg_genre_artwork_track_delete;
DROP TRIGGER trg_genre_artwork_track_update;
DROP TRIGGER trg_track_stats_insert;
DROP TRIGGER trg_track_stats_delete;
DROP TRIGGER trg_track_stats_remove;
DROP TRIGGER trg_track_stats_add;
DROP TRIGGER trg_user_album_plays_refresh;
DROP TRIGGER trg_user_album_plays_play_insert;
DROP TRIGGER trg_user_album_plays_play_delete;
DROP TRIGGER trg_user_album_plays_play_update;
DROP TRIGGER trg_user_album_plays_mark_insert;
DROP TRIGGER trg_user_album_plays_mark_delete;
DROP TRIGGER trg_user_album_plays_mark_update;
DROP TRIGGER trg_user_album_plays_track_insert;
DROP TRIGGER trg_user_album_plays_track_delete;
DROP TRIGGER trg_user_album_plays_track_update;
DROP TRIGGER trg_artist_album_credits_refresh;
DROP VIEW library_artist_counts_rule;
DROP TRIGGER trg_artist_counts_refresh;
DROP TRIGGER trg_artist_counts_credit_insert;
DROP TRIGGER trg_artist_counts_credit_delete;
DROP TRIGGER trg_artist_counts_credit_update;
DROP TRIGGER trg_artist_counts_track_update;
DROP TRIGGER trg_artist_counts_album_credit_insert;
DROP TRIGGER trg_artist_counts_album_credit_delete;
DROP TRIGGER trg_artist_counts_album_credit_update;
DROP TRIGGER trg_artist_counts_album_update;
DROP TRIGGER trg_artist_counts_artist_update;
DROP TRIGGER trg_genre_artwork_artwork_insert;
DROP TRIGGER trg_genre_artwork_artwork_update;
DROP TRIGGER trg_genre_artwork_artwork_delete;
DROP TRIGGER trg_genre_artwork_album_update;
DROP TRIGGER trg_tracks_fts_insert;
DROP TRIGGER trg_tracks_fts_delete;
DROP TRIGGER trg_tracks_fts_update;

-- A row with no key, which no trigger left behind, gets one in the copy and joins
-- the search index at the end.
CREATE TEMP TABLE library_rebuild_keyless AS
    SELECT id FROM local_tracks WHERE search_key IS NULL AND typeof(id) = 'text';

CREATE TABLE local_artists_new (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    sort_name TEXT,
    folded_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL CHECK(kind IN ('person','group','various_artists','unknown')),
    retired_into_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    converged_mbid TEXT
        CHECK (converged_mbid IS NULL OR (length(converged_mbid) = 36
        AND converged_mbid = lower(converged_mbid)))) STRICT;
INSERT INTO local_artists_new (rowid, id, display_name, sort_name, folded_name, normalized_name, kind,
    retired_into_artist_id, created_at, updated_at, row_revision,
    converged_mbid)
SELECT s.rowid, CAST(id AS TEXT), CAST(display_name AS TEXT), CAST(sort_name AS TEXT),
    CAST(folded_name AS TEXT), CAST(normalized_name AS TEXT),
    CAST(kind AS TEXT), CAST(retired_into_artist_id AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(converged_mbid AS TEXT)
FROM local_artists s WHERE typeof(id) = 'text';

CREATE TABLE local_albums_new (
    id TEXT PRIMARY KEY,
    root_id TEXT NOT NULL,
    grouping_key TEXT NOT NULL,
    title TEXT NOT NULL,
    title_folded TEXT NOT NULL,
    album_artist_name TEXT,
    album_artist_name_folded TEXT,
    tag_album_title TEXT,
    tag_album_artist_name TEXT,
    album_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    album_artist_sort_name TEXT,
    year INTEGER,
    original_release_date TEXT,
    primary_genre TEXT,
    is_compilation INTEGER NOT NULL DEFAULT 0 CHECK(is_compilation IN (0,1)),
    grouping_source TEXT NOT NULL CHECK(grouping_source IN ('automatic','legacy_import','manual')),
    grouping_locked INTEGER NOT NULL DEFAULT 0 CHECK(grouping_locked IN (0,1)),
    retired_into_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    management_schedule_pending INTEGER NOT NULL DEFAULT 0
        CHECK(management_schedule_pending IN (0,1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    has_indexed_track INTEGER NOT NULL DEFAULT 0
        CHECK (has_indexed_track IN (0, 1))) STRICT;
INSERT INTO local_albums_new (rowid, id, root_id, grouping_key, title, title_folded, album_artist_name,
    album_artist_name_folded, tag_album_title, tag_album_artist_name,
    album_artist_id, album_artist_sort_name, year, original_release_date,
    primary_genre, is_compilation, grouping_source, grouping_locked,
    retired_into_album_id, management_schedule_pending, created_at, updated_at,
    row_revision, has_indexed_track)
SELECT s.rowid, CAST(id AS TEXT), CAST(root_id AS TEXT), CAST(grouping_key AS TEXT),
    CAST(title AS TEXT), CAST(title_folded AS TEXT),
    CAST(album_artist_name AS TEXT), CAST(album_artist_name_folded AS TEXT),
    CAST(tag_album_title AS TEXT), CAST(tag_album_artist_name AS TEXT),
    CAST(album_artist_id AS TEXT), CAST(album_artist_sort_name AS TEXT),
    CAST(year AS INTEGER), CAST(original_release_date AS TEXT),
    CAST(primary_genre AS TEXT), CAST(is_compilation AS INTEGER),
    CAST(grouping_source AS TEXT), CAST(grouping_locked AS INTEGER),
    CAST(retired_into_album_id AS TEXT),
    CASE WHEN management_schedule_pending THEN 1 ELSE 0 END,
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(has_indexed_track AS INTEGER)
FROM local_albums s WHERE typeof(id) = 'text';

CREATE TABLE local_tracks_new (
    search_key INTEGER PRIMARY KEY,
    id TEXT NOT NULL UNIQUE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    root_id TEXT NOT NULL,
    file_path TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    path_hash TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL CHECK(file_size_bytes >= 0),
    file_mtime_ns INTEGER NOT NULL,
    stat_revision TEXT NOT NULL,
    stat_revision_kind TEXT NOT NULL DEFAULT 'unclassified'
        CHECK(stat_revision_kind IN ('exact','legacy_float','legacy_review','unclassified')),
    tag_revision TEXT,
    tags_read_at INTEGER,
    metadata_incomplete INTEGER NOT NULL DEFAULT 0 CHECK(metadata_incomplete IN (0,1)),
    title TEXT NOT NULL,
    title_folded TEXT NOT NULL,
    artist_name TEXT,
    artist_name_folded TEXT,
    album_title TEXT NOT NULL,
    album_title_folded TEXT NOT NULL,
    album_artist_name TEXT,
    album_artist_name_folded TEXT,
    tag_album_title TEXT,
    tag_album_artist_name TEXT,
    disc_number INTEGER NOT NULL DEFAULT 1,
    track_number INTEGER NOT NULL DEFAULT 0,
    year INTEGER,
    genre TEXT,
    genre_folded TEXT,
    release_type TEXT,
    title_sort TEXT,
    artist_sort TEXT,
    album_sort TEXT,
    album_artist_sort TEXT,
    disc_subtitle TEXT,
    is_compilation INTEGER NOT NULL DEFAULT 0 CHECK(is_compilation IN (0,1)),
    embedded_release_group_mbid TEXT,
    embedded_release_mbid TEXT,
    embedded_recording_mbid TEXT,
    embedded_release_track_mbid TEXT,
    embedded_artist_mbid TEXT,
    embedded_album_artist_mbid TEXT,
    duration_seconds REAL,
    file_format TEXT NOT NULL,
    bit_rate INTEGER,
    sample_rate INTEGER,
    bit_depth INTEGER,
    channels INTEGER,
    replaygain_track_gain REAL,
    replaygain_album_gain REAL,
    replaygain_track_peak REAL,
    replaygain_album_peak REAL,
    availability TEXT NOT NULL DEFAULT 'indexed' CHECK(availability IN ('indexed','excluded','missing')),
    missing_at INTEGER,
    excluded_at INTEGER,
    ingest_source TEXT NOT NULL,
    download_task_id TEXT,
    source_path TEXT,
    imported_at INTEGER NOT NULL,
    membership_source TEXT NOT NULL CHECK(membership_source IN ('automatic','legacy_import','manual')),
    membership_locked INTEGER NOT NULL DEFAULT 0 CHECK(membership_locked IN (0,1)),
    desired_policy_revision TEXT NOT NULL DEFAULT '',
    applied_policy_revision TEXT NOT NULL DEFAULT '',
    applied_policy TEXT NOT NULL DEFAULT 'automatic' CHECK(applied_policy IN ('local_metadata','automatic','excluded')),
    manual_excluded INTEGER NOT NULL DEFAULT 0 CHECK(manual_excluded IN (0,1)),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    title_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(title_provenance IN ('tag','parsed','placeholder','absent')),
    album_title_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(album_title_provenance IN ('tag','parsed','placeholder','absent')),
    album_artist_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(album_artist_provenance IN ('tag','parsed','placeholder','absent')),
    duration_measured INTEGER
        CHECK (duration_measured IN (0, 1)),
    UNIQUE(root_id, relative_path)
) STRICT;
INSERT INTO local_tracks_new (search_key, id, local_album_id, root_id, file_path, relative_path,
    path_hash, file_size_bytes, file_mtime_ns, stat_revision,
    stat_revision_kind, tag_revision, tags_read_at, metadata_incomplete, title,
    title_folded, artist_name, artist_name_folded, album_title,
    album_title_folded, album_artist_name, album_artist_name_folded,
    tag_album_title, tag_album_artist_name, disc_number, track_number, year,
    genre, genre_folded, release_type, title_sort, artist_sort, album_sort,
    album_artist_sort, disc_subtitle, is_compilation,
    embedded_release_group_mbid, embedded_release_mbid,
    embedded_recording_mbid, embedded_release_track_mbid, embedded_artist_mbid,
    embedded_album_artist_mbid, duration_seconds, file_format, bit_rate,
    sample_rate, bit_depth, channels, replaygain_track_gain,
    replaygain_album_gain, replaygain_track_peak, replaygain_album_peak,
    availability, missing_at, excluded_at, ingest_source, download_task_id,
    source_path, imported_at, membership_source, membership_locked,
    desired_policy_revision, applied_policy_revision, applied_policy,
    manual_excluded, row_revision, title_provenance, album_title_provenance,
    album_artist_provenance, duration_measured)
SELECT CAST(search_key AS INTEGER), CAST(id AS TEXT),
    CAST(local_album_id AS TEXT), CAST(root_id AS TEXT),
    CAST(file_path AS TEXT), CAST(relative_path AS TEXT),
    CAST(path_hash AS TEXT), CAST(file_size_bytes AS INTEGER),
    CAST(file_mtime_ns AS INTEGER), CAST(stat_revision AS TEXT),
    CAST(stat_revision_kind AS TEXT), CAST(tag_revision AS TEXT),
    CAST(round(tags_read_at * 1000) AS INTEGER),
    CAST(metadata_incomplete AS INTEGER), CAST(title AS TEXT),
    CAST(title_folded AS TEXT), CAST(artist_name AS TEXT),
    CAST(artist_name_folded AS TEXT), CAST(album_title AS TEXT),
    CAST(album_title_folded AS TEXT), CAST(album_artist_name AS TEXT),
    CAST(album_artist_name_folded AS TEXT), CAST(tag_album_title AS TEXT),
    CAST(tag_album_artist_name AS TEXT), CAST(disc_number AS INTEGER),
    CAST(track_number AS INTEGER), CAST(year AS INTEGER), CAST(genre AS TEXT),
    CAST(genre_folded AS TEXT), CAST(release_type AS TEXT),
    CAST(title_sort AS TEXT), CAST(artist_sort AS TEXT),
    CAST(album_sort AS TEXT), CAST(album_artist_sort AS TEXT),
    CAST(disc_subtitle AS TEXT), CAST(is_compilation AS INTEGER),
    CAST(embedded_release_group_mbid AS TEXT),
    CAST(embedded_release_mbid AS TEXT), CAST(embedded_recording_mbid AS TEXT),
    CAST(embedded_release_track_mbid AS TEXT),
    CAST(embedded_artist_mbid AS TEXT),
    CAST(embedded_album_artist_mbid AS TEXT), CAST(duration_seconds AS REAL),
    CAST(file_format AS TEXT), CAST(bit_rate AS INTEGER),
    CAST(sample_rate AS INTEGER), CAST(bit_depth AS INTEGER),
    CAST(channels AS INTEGER), CAST(replaygain_track_gain AS REAL),
    CAST(replaygain_album_gain AS REAL), CAST(replaygain_track_peak AS REAL),
    CAST(replaygain_album_peak AS REAL), CAST(availability AS TEXT),
    CAST(round(missing_since * 1000) AS INTEGER),
    CAST(round(excluded_at * 1000) AS INTEGER), CAST(ingest_source AS TEXT),
    CAST(download_task_id AS TEXT), CAST(source_path AS TEXT),
    COALESCE(CAST(round(imported_at * 1000) AS INTEGER), 0),
    CAST(membership_source AS TEXT), CAST(membership_locked AS INTEGER),
    CAST(desired_policy_revision AS TEXT),
    CAST(applied_policy_revision AS TEXT), CAST(applied_policy AS TEXT),
    CAST(manual_excluded AS INTEGER),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(title_provenance AS TEXT), CAST(album_title_provenance AS TEXT),
    CAST(album_artist_provenance AS TEXT), CAST(duration_measured AS INTEGER)
FROM local_tracks WHERE typeof(id) = 'text'
ORDER BY search_key IS NULL, search_key;

CREATE TABLE local_album_artists_new (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    join_phrase TEXT NOT NULL DEFAULT '',
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    artist_mbid TEXT
        CHECK (artist_mbid IS NULL OR (length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid))),
    PRIMARY KEY(local_album_id, position)
) STRICT;
INSERT INTO local_album_artists_new (rowid, local_album_id, position, local_artist_id, role, credited_name,
    join_phrase, row_revision, artist_mbid)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(position AS INTEGER),
    CAST(local_artist_id AS TEXT), CAST(role AS TEXT),
    CAST(credited_name AS TEXT), CAST(join_phrase AS TEXT),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(artist_mbid AS TEXT)
FROM local_album_artists s WHERE typeof(local_album_id) = 'text';

CREATE TABLE local_track_artists_new (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    join_phrase TEXT NOT NULL DEFAULT '',
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    artist_mbid TEXT
        CHECK (artist_mbid IS NULL OR (length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid))),
    PRIMARY KEY(local_track_id, position)
) STRICT;
INSERT INTO local_track_artists_new (rowid, local_track_id, position, local_artist_id, role, credited_name,
    join_phrase, row_revision, artist_mbid)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(position AS INTEGER),
    CAST(local_artist_id AS TEXT), CAST(role AS TEXT),
    CAST(credited_name AS TEXT), CAST(join_phrase AS TEXT),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(artist_mbid AS TEXT)
FROM local_track_artists s WHERE typeof(local_track_id) = 'text';

CREATE TABLE local_artist_external_identities_new (
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK(provider = 'musicbrainz'),
    provider_artist_id TEXT NOT NULL,
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
    PRIMARY KEY(local_artist_id, provider),
    UNIQUE(provider, provider_artist_id)
) STRICT;
INSERT INTO local_artist_external_identities_new (rowid, local_artist_id, provider, provider_artist_id, decision_source, attempt_id,
    selected_by_user_id, selected_at, row_revision, provider_source_mode,
    provider_source_id, provider_source_generation)
SELECT s.rowid, CAST(local_artist_id AS TEXT), CAST(provider AS TEXT),
    CAST(provider_artist_id AS TEXT), CAST(decision_source AS TEXT),
    CAST(attempt_id AS TEXT), CAST(selected_by_user_id AS TEXT),
    COALESCE(CAST(round(selected_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(provider_source_mode AS TEXT), CAST(provider_source_id AS TEXT),
    CAST(provider_source_generation AS INTEGER)
FROM local_artist_external_identities s WHERE typeof(local_artist_id) = 'text';

CREATE TABLE local_album_external_identities_new (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL DEFAULT 'musicbrainz' CHECK(provider = 'musicbrainz'),
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT,
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    matcher_version TEXT,
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
    provider_base_url TEXT,
    PRIMARY KEY(local_album_id, provider)
) STRICT;
INSERT INTO local_album_external_identities_new (rowid, local_album_id, provider, release_group_mbid, release_mbid,
    decision_source, matcher_version, attempt_id, selected_by_user_id,
    selected_at, row_revision, provider_source_mode, provider_source_id,
    provider_source_generation, provider_base_url)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(provider AS TEXT),
    CAST(release_group_mbid AS TEXT), CAST(release_mbid AS TEXT),
    CAST(decision_source AS TEXT), CAST(matcher_version AS TEXT),
    CAST(attempt_id AS TEXT), CAST(selected_by_user_id AS TEXT),
    COALESCE(CAST(round(selected_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(provider_source_mode AS TEXT), CAST(provider_source_id AS TEXT),
    CAST(provider_source_generation AS INTEGER),
    CAST(provider_base_url AS TEXT)
FROM local_album_external_identities s WHERE typeof(local_album_id) = 'text';

CREATE TABLE local_track_external_identities_new (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL DEFAULT 'musicbrainz' CHECK(provider = 'musicbrainz'),
    recording_mbid TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    medium_position INTEGER CHECK(medium_position IS NULL OR medium_position > 0),
    release_track_position INTEGER
        CHECK(release_track_position IS NULL OR release_track_position > 0),
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
    provider_base_url TEXT,
    PRIMARY KEY(local_track_id, provider)
) STRICT;
INSERT INTO local_track_external_identities_new (rowid, local_track_id, provider, recording_mbid, release_mbid, release_track_mbid,
    medium_position, release_track_position, decision_source, attempt_id,
    selected_at, row_revision, provider_source_mode, provider_source_id,
    provider_source_generation, provider_base_url)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(provider AS TEXT),
    CAST(recording_mbid AS TEXT), CAST(release_mbid AS TEXT),
    CAST(release_track_mbid AS TEXT),
    CASE WHEN medium_position IS NOT NULL THEN MAX(1, CAST(medium_position AS INTEGER)) END,
    CASE WHEN release_track_position IS NOT NULL THEN MAX(1, CAST(release_track_position AS INTEGER)) END,
    CAST(decision_source AS TEXT), CAST(attempt_id AS TEXT),
    COALESCE(CAST(round(selected_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1)),
    CAST(provider_source_mode AS TEXT), CAST(provider_source_id AS TEXT),
    CAST(provider_source_generation AS INTEGER),
    CAST(provider_base_url AS TEXT)
FROM local_track_external_identities s WHERE typeof(local_track_id) = 'text';

CREATE TABLE local_artist_aliases_new (
    alias TEXT PRIMARY KEY,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_artist','merged_artist','compat_migration')),
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO local_artist_aliases_new (rowid, alias, local_artist_id, kind, created_at)
SELECT s.rowid, CAST(alias AS TEXT), CAST(local_artist_id AS TEXT), CAST(kind AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0)
FROM local_artist_aliases s WHERE typeof(alias) = 'text' AND typeof(local_artist_id) = 'text';

CREATE TABLE local_album_aliases_new (
    alias TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_release_group','merged_album','compat_migration')),
    created_at INTEGER NOT NULL
) STRICT;
INSERT INTO local_album_aliases_new (rowid, alias, local_album_id, kind, created_at)
SELECT s.rowid, CAST(alias AS TEXT), CAST(local_album_id AS TEXT), CAST(kind AS TEXT),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0)
FROM local_album_aliases s WHERE typeof(alias) = 'text' AND typeof(local_album_id) = 'text';

CREATE TABLE local_track_genres_new (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK(position >= 0),
    name TEXT NOT NULL CHECK(length(trim(name)) > 0),
    folded_name TEXT NOT NULL CHECK(length(trim(folded_name)) > 0),
    source TEXT NOT NULL CHECK(source IN (
        'local','musicbrainz','listenbrainz','lastfm','override'
    )),
    genre_mbid TEXT,
    weight INTEGER,
    source_document_revision TEXT,
    PRIMARY KEY(local_track_id, position),
    UNIQUE(local_track_id, folded_name)
) STRICT;
INSERT INTO local_track_genres_new (rowid, local_track_id, position, name, folded_name, source, genre_mbid, weight,
    source_document_revision)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(position AS INTEGER),
    CAST(name AS TEXT), CAST(folded_name AS TEXT), CAST(source AS TEXT),
    CAST(genre_mbid AS TEXT), CAST(weight AS INTEGER),
    CAST(source_document_revision AS TEXT)
FROM local_track_genres s WHERE typeof(local_track_id) = 'text';

CREATE TABLE local_entity_source_links_new (
    id TEXT PRIMARY KEY,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE CASCADE,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE CASCADE,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK(length(provider) > 0),
    external_entity_type TEXT NOT NULL CHECK(length(external_entity_type) > 0),
    external_id TEXT NOT NULL CHECK(length(external_id) > 0),
    canonical_url TEXT NOT NULL CHECK(length(canonical_url) > 0),
    decision_source TEXT NOT NULL CHECK(length(decision_source) > 0),
    selected_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    verified_at INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(
        (local_artist_id IS NOT NULL) +
        (local_album_id IS NOT NULL) +
        (local_track_id IS NOT NULL) = 1
    )
) STRICT;
INSERT INTO local_entity_source_links_new (rowid, id, local_artist_id, local_album_id, local_track_id, provider,
    external_entity_type, external_id, canonical_url, decision_source,
    selected_by_user_id, verified_at, created_at, updated_at, row_revision)
SELECT s.rowid, CAST(id AS TEXT), CAST(local_artist_id AS TEXT),
    CAST(local_album_id AS TEXT), CAST(local_track_id AS TEXT),
    CAST(provider AS TEXT), CAST(external_entity_type AS TEXT),
    CAST(external_id AS TEXT), CAST(canonical_url AS TEXT),
    CAST(decision_source AS TEXT), CAST(selected_by_user_id AS TEXT),
    COALESCE(CAST(round(verified_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(created_at * 1000) AS INTEGER), 0),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1))
FROM local_entity_source_links s WHERE typeof(id) = 'text';

CREATE TABLE local_album_artwork_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    cover_url TEXT,
    source TEXT NOT NULL CHECK(source IN ('embedded','folder','cover_cache','manual','provider')),
    source_locator TEXT,
    content_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version BETWEEN 1 AND 9223372036854775807),
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;
INSERT INTO local_album_artwork_new (rowid, local_album_id, cover_url, source, source_locator, content_hash, version,
    updated_at, row_revision)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(cover_url AS TEXT),
    CAST(source AS TEXT), CAST(source_locator AS TEXT),
    CAST(content_hash AS TEXT), MAX(1, COALESCE(CAST(version AS INTEGER), 1)),
    COALESCE(CAST(round(updated_at * 1000) AS INTEGER), 0),
    MAX(1, COALESCE(CAST(row_revision AS INTEGER), 1))
FROM local_album_artwork s WHERE typeof(local_album_id) = 'text';

CREATE TABLE local_album_artwork_checks_new (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    signature TEXT NOT NULL,
    checked_at INTEGER NOT NULL,
    art_version INTEGER NOT NULL DEFAULT 0
) STRICT;
INSERT INTO local_album_artwork_checks_new (rowid, local_album_id, signature, checked_at, art_version)
SELECT s.rowid, CAST(local_album_id AS TEXT), CAST(signature AS TEXT),
    COALESCE(CAST(round(checked_at * 1000) AS INTEGER), 0),
    CAST(art_version AS INTEGER)
FROM local_album_artwork_checks s WHERE typeof(local_album_id) = 'text';

CREATE TABLE local_track_edition_tags_new (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    media TEXT,
    barcode TEXT,
    catalog_number TEXT,
    release_country TEXT,
    total_discs INTEGER,
    total_tracks INTEGER,
    release_date TEXT) STRICT;
INSERT INTO local_track_edition_tags_new (rowid, local_track_id, media, barcode, catalog_number, release_country,
    total_discs, total_tracks, release_date)
SELECT s.rowid, CAST(local_track_id AS TEXT), CAST(media AS TEXT), CAST(barcode AS TEXT),
    CAST(catalog_number AS TEXT), CAST(release_country AS TEXT),
    CAST(total_discs AS INTEGER), CAST(total_tracks AS INTEGER),
    CAST(release_date AS TEXT)
FROM local_track_edition_tags s WHERE typeof(local_track_id) = 'text';

-- Every copy is made before any old table goes: a parent and its children swap together.
DROP TABLE local_track_edition_tags;
DROP TABLE local_album_artwork_checks;
DROP TABLE local_album_artwork;
DROP TABLE local_entity_source_links;
DROP TABLE local_track_genres;
DROP TABLE local_album_aliases;
DROP TABLE local_artist_aliases;
DROP TABLE local_track_external_identities;
DROP TABLE local_album_external_identities;
DROP TABLE local_artist_external_identities;
DROP TABLE local_track_artists;
DROP TABLE local_album_artists;
DROP TABLE local_tracks;
DROP TABLE local_albums;
DROP TABLE local_artists;
ALTER TABLE local_artists_new RENAME TO local_artists;
ALTER TABLE local_albums_new RENAME TO local_albums;
ALTER TABLE local_tracks_new RENAME TO local_tracks;
ALTER TABLE local_album_artists_new RENAME TO local_album_artists;
ALTER TABLE local_track_artists_new RENAME TO local_track_artists;
ALTER TABLE local_artist_external_identities_new RENAME TO local_artist_external_identities;
ALTER TABLE local_album_external_identities_new RENAME TO local_album_external_identities;
ALTER TABLE local_track_external_identities_new RENAME TO local_track_external_identities;
ALTER TABLE local_artist_aliases_new RENAME TO local_artist_aliases;
ALTER TABLE local_album_aliases_new RENAME TO local_album_aliases;
ALTER TABLE local_track_genres_new RENAME TO local_track_genres;
ALTER TABLE local_entity_source_links_new RENAME TO local_entity_source_links;
ALTER TABLE local_album_artwork_new RENAME TO local_album_artwork;
ALTER TABLE local_album_artwork_checks_new RENAME TO local_album_artwork_checks;
ALTER TABLE local_track_edition_tags_new RENAME TO local_track_edition_tags;

CREATE INDEX idx_local_artists_folded ON local_artists(folded_name, kind);
CREATE INDEX idx_local_artists_normalized ON local_artists(normalized_name, kind);
CREATE INDEX idx_local_artists_retired ON local_artists(retired_into_artist_id);
CREATE INDEX idx_local_albums_artist_id
    ON local_albums(album_artist_id);
CREATE INDEX idx_local_albums_created ON local_albums(created_at, id);
CREATE INDEX idx_local_albums_grouping ON local_albums(root_id, grouping_key);
CREATE INDEX idx_local_albums_grouping_key
    ON local_albums(grouping_key);
CREATE INDEX idx_local_albums_owned_leader
    ON local_albums(album_artist_id, retired_into_album_id, has_indexed_track)
    WHERE retired_into_album_id IS NULL AND has_indexed_track = 1;
CREATE INDEX idx_local_albums_ownership ON local_albums(title_folded, album_artist_name_folded, year);
CREATE INDEX idx_local_albums_retired ON local_albums(retired_into_album_id);
CREATE INDEX idx_local_albums_search ON local_albums(title_folded, album_artist_name_folded);
CREATE INDEX idx_local_tracks_album_availability ON local_tracks(local_album_id, availability);
CREATE INDEX idx_local_tracks_album_order ON local_tracks(local_album_id, disc_number, track_number, id);
CREATE INDEX idx_local_tracks_album_order_folded
    ON local_tracks(availability, album_title_folded, local_album_id, disc_number,
                    track_number, id);
CREATE INDEX idx_local_tracks_artist_order
    ON local_tracks(availability, COALESCE(artist_name_folded, ''), album_title_folded,
                    local_album_id, disc_number, track_number, id);
CREATE INDEX idx_local_tracks_availability
    ON local_tracks(availability, missing_at, search_key);
CREATE INDEX idx_local_tracks_availability_title
    ON local_tracks(availability, title_folded, id);
CREATE INDEX idx_local_tracks_download_task
    ON local_tracks(download_task_id) WHERE download_task_id IS NOT NULL;
CREATE INDEX idx_local_tracks_genre_artwork
    ON local_tracks(genre_folded, availability, local_album_id);
CREATE INDEX idx_local_tracks_id_availability_album
    ON local_tracks(id, availability, local_album_id);
CREATE INDEX idx_local_tracks_path_hash ON local_tracks(path_hash);
CREATE INDEX idx_local_tracks_policy ON local_tracks(root_id, applied_policy, desired_policy_revision, relative_path);
CREATE INDEX idx_local_tracks_recent ON local_tracks(availability, imported_at DESC, id DESC);
CREATE INDEX idx_local_tracks_recording
    ON local_tracks(embedded_recording_mbid)
    WHERE embedded_recording_mbid IS NOT NULL;
CREATE INDEX idx_local_tracks_recording_lower
    ON local_tracks(lower(COALESCE(embedded_recording_mbid, '')));
CREATE INDEX idx_local_tracks_release_group_lower
    ON local_tracks(lower(embedded_release_group_mbid))
    WHERE embedded_release_group_mbid IS NOT NULL;
CREATE INDEX idx_local_tracks_search ON local_tracks(title_folded, artist_name_folded, album_title_folded);
CREATE INDEX idx_local_tracks_stat ON local_tracks(stat_revision);
CREATE INDEX idx_local_tracks_tag ON local_tracks(tag_revision);
CREATE INDEX idx_local_tracks_year_id
    ON local_tracks(year, id);
CREATE INDEX idx_local_album_artists_artist_mbid
    ON local_album_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_album_artists_mbid
    ON local_album_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_album_artists_reverse ON local_album_artists(local_artist_id, local_album_id);
CREATE INDEX idx_local_track_artists_artist_mbid
    ON local_track_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_track_artists_mbid
    ON local_track_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;
CREATE INDEX idx_local_track_artists_reverse ON local_track_artists(local_artist_id, local_track_id);
CREATE INDEX idx_local_artist_external_identities_attempt_id
    ON local_artist_external_identities(attempt_id);
CREATE INDEX idx_local_artist_external_identities_selected_by_user_id
    ON local_artist_external_identities(selected_by_user_id);
CREATE INDEX idx_local_artist_identity_provider_lower ON local_artist_external_identities(lower(provider_artist_id));
CREATE INDEX idx_local_album_external_identities_attempt_id
    ON local_album_external_identities(attempt_id);
CREATE INDEX idx_local_album_external_identities_selected_by_user_id
    ON local_album_external_identities(selected_by_user_id);
CREATE INDEX idx_local_album_identity_release
    ON local_album_external_identities(release_mbid);
CREATE INDEX idx_local_album_identity_release_lower ON local_album_external_identities(lower(release_mbid));
CREATE INDEX idx_local_album_identity_rg ON local_album_external_identities(release_group_mbid);
CREATE INDEX idx_local_album_identity_rg_lower ON local_album_external_identities(lower(release_group_mbid));
CREATE INDEX idx_local_track_external_identities_attempt_id
    ON local_track_external_identities(attempt_id);
CREATE INDEX idx_local_track_identity_recording ON local_track_external_identities(recording_mbid);
CREATE INDEX idx_local_track_identity_recording_lower
    ON local_track_external_identities(lower(recording_mbid));
CREATE INDEX idx_local_track_identity_release_track
    ON local_track_external_identities(release_track_mbid);
CREATE INDEX idx_artist_alias_target ON local_artist_aliases(local_artist_id);
CREATE INDEX idx_album_alias_target ON local_album_aliases(local_album_id);
CREATE INDEX idx_local_track_genres_folded ON local_track_genres(folded_name, local_track_id);
CREATE INDEX idx_local_track_genres_source ON local_track_genres(source, local_track_id);
CREATE UNIQUE INDEX idx_entity_source_album_unique
ON local_entity_source_links(local_album_id, provider, external_entity_type, external_id)
WHERE local_album_id IS NOT NULL;
CREATE UNIQUE INDEX idx_entity_source_artist_unique
ON local_entity_source_links(local_artist_id, provider, external_entity_type, external_id)
WHERE local_artist_id IS NOT NULL;
CREATE INDEX idx_entity_source_provider
ON local_entity_source_links(provider, external_entity_type, external_id);
CREATE UNIQUE INDEX idx_entity_source_track_unique
ON local_entity_source_links(local_track_id, provider, external_entity_type, external_id)
WHERE local_track_id IS NOT NULL;
CREATE INDEX idx_local_entity_source_links_local_album_id
    ON local_entity_source_links(local_album_id);
CREATE INDEX idx_local_entity_source_links_local_artist_id
    ON local_entity_source_links(local_artist_id);
CREATE INDEX idx_local_entity_source_links_local_track_id
    ON local_entity_source_links(local_track_id);
CREATE INDEX idx_local_entity_source_links_selected_by_user_id
    ON local_entity_source_links(selected_by_user_id);

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

CREATE TRIGGER trg_genre_artwork_normalized_insert
AFTER INSERT ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (NEW.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_normalized_delete
AFTER DELETE ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_normalized_update
AFTER UPDATE OF name, folded_name ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT NEW.folded_name, 1 WHERE NEW.folded_name != OLD.folded_name
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_normalized_genres_legacy_scalar_update
AFTER UPDATE OF genre, genre_folded ON local_tracks
WHEN NEW.genre IS NOT OLD.genre OR NEW.genre_folded IS NOT OLD.genre_folded
BEGIN
    DELETE FROM local_track_genres
    WHERE local_track_id = NEW.id
      AND NOT EXISTS (
          SELECT 1 FROM local_track_genres protected
          WHERE protected.local_track_id = NEW.id
            AND (protected.position != 0 OR protected.source != 'local')
      );
    INSERT INTO local_track_genres(
        local_track_id, position, name, folded_name, source
    )
    SELECT NEW.id, 0, trim(NEW.genre),
           COALESCE(NULLIF(NEW.genre_folded, ''), lower(trim(NEW.genre))), 'local'
    WHERE NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
      AND NOT EXISTS (
          SELECT 1 FROM local_track_genres genre
          WHERE genre.local_track_id = NEW.id
      );
END;
CREATE TRIGGER trg_genre_artwork_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_track_delete
AFTER DELETE ON local_tracks
WHEN OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_track_update
AFTER UPDATE OF genre, genre_folded, availability, local_album_id ON local_tracks
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1
    WHERE OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1
    WHERE NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
      AND COALESCE(NEW.genre_folded, lower(trim(NEW.genre)))
          != COALESCE(OLD.genre_folded, lower(trim(OLD.genre)), '')
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_track_stats_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    INSERT INTO library_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
END;
CREATE TRIGGER trg_track_stats_delete
AFTER DELETE ON local_tracks
WHEN OLD.availability = 'indexed'
BEGIN
    UPDATE library_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;
CREATE TRIGGER trg_track_stats_remove
AFTER UPDATE ON local_tracks
WHEN OLD.availability = 'indexed'
 AND (NEW.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    UPDATE library_track_format_stats
    SET indexed_tracks = indexed_tracks - 1,
        indexed_bytes = indexed_bytes - OLD.file_size_bytes
    WHERE file_format = OLD.file_format;
END;
CREATE TRIGGER trg_track_stats_add
AFTER UPDATE ON local_tracks
WHEN NEW.availability = 'indexed'
 AND (OLD.availability IS NOT 'indexed'
      OR NEW.file_format IS NOT OLD.file_format
      OR NEW.file_size_bytes IS NOT OLD.file_size_bytes)
BEGIN
    INSERT INTO library_track_format_stats(file_format, indexed_tracks, indexed_bytes)
    VALUES (NEW.file_format, 1, NEW.file_size_bytes)
    ON CONFLICT(file_format) DO UPDATE SET
        indexed_tracks = indexed_tracks + 1,
        indexed_bytes = indexed_bytes + excluded.indexed_bytes;
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
            LEFT JOIN library_user_play_marks m
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
CREATE TRIGGER trg_user_album_plays_play_insert
AFTER INSERT ON library_play_history
WHEN NEW.local_track_id IS NOT NULL OR NEW.local_album_id IS NOT NULL
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (NEW.user_id, CASE WHEN NEW.local_track_id IS NULL THEN NEW.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id) END);
END;
CREATE TRIGGER trg_user_album_plays_play_delete
AFTER DELETE ON library_play_history
WHEN OLD.local_track_id IS NOT NULL OR OLD.local_album_id IS NOT NULL
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, CASE WHEN OLD.local_track_id IS NULL THEN OLD.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id) END);
END;
CREATE TRIGGER trg_user_album_plays_play_update
AFTER UPDATE OF user_id, local_track_id, local_album_id, played_at ON library_play_history
WHEN OLD.user_id IS NOT NEW.user_id
  OR OLD.local_track_id IS NOT NEW.local_track_id
  OR OLD.played_at IS NOT NEW.played_at
  OR ((OLD.local_track_id IS NULL OR NEW.local_track_id IS NULL)
      AND OLD.local_album_id IS NOT NEW.local_album_id)
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT OLD.user_id, CASE WHEN OLD.local_track_id IS NULL THEN OLD.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id) END
    UNION
    SELECT NEW.user_id, CASE WHEN NEW.local_track_id IS NULL THEN NEW.local_album_id
        ELSE (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id) END;
END;
CREATE TRIGGER trg_user_album_plays_mark_insert
AFTER INSERT ON library_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (NEW.user_id, (SELECT local_album_id FROM local_tracks WHERE id = NEW.local_track_id));
END;
CREATE TRIGGER trg_user_album_plays_mark_delete
AFTER DELETE ON library_user_play_marks
BEGIN
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    VALUES (OLD.user_id, (SELECT local_album_id FROM local_tracks WHERE id = OLD.local_track_id));
END;
CREATE TRIGGER trg_user_album_plays_mark_update
AFTER UPDATE ON library_user_play_marks
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
CREATE TRIGGER trg_user_album_plays_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.availability = 'indexed'
BEGIN
    UPDATE local_albums SET has_indexed_track = 1
    WHERE id = NEW.local_album_id AND has_indexed_track = 0;
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT user_id, local_album_id FROM library_user_album_plays
    WHERE local_album_id = NEW.local_album_id;
END;
CREATE TRIGGER trg_user_album_plays_track_delete
AFTER DELETE ON local_tracks
BEGIN
    UPDATE local_albums SET has_indexed_track = 0
    WHERE id = OLD.local_album_id AND has_indexed_track = 1
      AND NOT EXISTS (SELECT 1 FROM local_tracks t
                      WHERE t.local_album_id = OLD.local_album_id AND t.availability = 'indexed');
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT user_id, local_album_id FROM library_user_album_plays
    WHERE local_album_id = OLD.local_album_id;
END;
CREATE TRIGGER trg_user_album_plays_track_update
AFTER UPDATE OF availability, local_album_id ON local_tracks
WHEN OLD.local_album_id IS NOT NEW.local_album_id
  OR (OLD.availability = 'indexed') IS NOT (NEW.availability = 'indexed')
BEGIN
    UPDATE local_albums SET has_indexed_track = NOT has_indexed_track
    WHERE id IN (OLD.local_album_id, NEW.local_album_id)
      AND has_indexed_track IS NOT EXISTS (SELECT 1 FROM local_tracks t
          WHERE t.local_album_id = local_albums.id AND t.availability = 'indexed');
    INSERT INTO library_user_album_plays_refresh (user_id, local_album_id)
    SELECT u.user_id, x.id
    FROM (SELECT DISTINCT user_id FROM library_user_album_plays
          WHERE local_album_id IN (OLD.local_album_id, NEW.local_album_id)) u
    CROSS JOIN (SELECT OLD.local_album_id AS id UNION SELECT NEW.local_album_id) x;
END;
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
CREATE TRIGGER trg_artist_counts_artist_update
AFTER UPDATE OF folded_name, retired_into_artist_id ON local_artists
WHEN OLD.folded_name IS NOT NEW.folded_name
  OR (OLD.retired_into_artist_id IS NULL) IS NOT (NEW.retired_into_artist_id IS NULL)
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.id);
END;
CREATE TRIGGER trg_genre_artwork_artwork_insert
AFTER INSERT ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
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
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_artwork_delete
AFTER DELETE ON local_album_artwork
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = OLD.local_album_id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_genre_artwork_album_update
AFTER UPDATE OF title, album_artist_id, album_artist_name, retired_into_album_id
ON local_albums
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t INDEXED BY idx_local_tracks_album_availability
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;
CREATE TRIGGER trg_tracks_fts_insert
AFTER INSERT ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES (NEW.search_key, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''), NEW.album_title_folded);
END;
CREATE TRIGGER trg_tracks_fts_delete
AFTER DELETE ON local_tracks
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.search_key, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
END;
CREATE TRIGGER trg_tracks_fts_update
AFTER UPDATE OF title_folded, artist_name_folded, album_title_folded ON local_tracks
WHEN OLD.title_folded IS NOT NEW.title_folded
  OR OLD.artist_name_folded IS NOT NEW.artist_name_folded
  OR OLD.album_title_folded IS NOT NEW.album_title_folded
BEGIN
    INSERT INTO local_tracks_fts(local_tracks_fts, rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES ('delete', OLD.search_key, OLD.title_folded, COALESCE(OLD.artist_name_folded, ''), OLD.album_title_folded);
    INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    VALUES (NEW.search_key, NEW.title_folded, COALESCE(NEW.artist_name_folded, ''), NEW.album_title_folded);
END;

INSERT INTO local_tracks_fts(rowid, title_folded, artist_name_folded, album_title_folded)
    SELECT t.search_key, t.title_folded, COALESCE(t.artist_name_folded, ''), t.album_title_folded
    FROM library_rebuild_keyless k JOIN local_tracks t ON t.id = k.id;

-- The planner's statistics on these tables went with them. With no record of
-- a run, the next check takes them again.
DELETE FROM db_planner_stats;

INSERT INTO library_rebuild_guard (no_new_orphan) SELECT NOT EXISTS (
    SELECT 1 FROM (
        SELECT "table" AS child, rowid AS row, parent, COUNT(*) AS n
        FROM pragma_foreign_key_check GROUP BY 1, 2, 3) AS now
    WHERE now.n > (SELECT COUNT(*) FROM library_rebuild_orphans AS before
        WHERE before.child = now.child AND before.row IS now.row
            AND before.parent = now.parent));

DROP TABLE library_rebuild_guard;
DROP TABLE library_rebuild_orphans;
DROP TABLE library_rebuild_keyless;

-- Migration high-water mark.
PRAGMA user_version = 119;
COMMIT;
PRAGMA foreign_keys=ON;
