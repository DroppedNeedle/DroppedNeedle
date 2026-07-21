"""DDL owned and executed only by NativeLibraryStore."""

SCHEMA_SQL = r"""
CREATE TABLE IF NOT EXISTS user_favorites (
    user_id TEXT NOT NULL,
    item_kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    created_at REAL NOT NULL,
    PRIMARY KEY(user_id, item_kind, item_id)
);

CREATE TABLE IF NOT EXISTS play_history (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT,
    recording_mbid TEXT,
    release_group_mbid TEXT,
    duration_ms INTEGER,
    source TEXT,
    played_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS playlists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    cover_image_path TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    source_ref TEXT,
    user_id TEXT,
    is_public INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS playlist_tracks (
    id TEXT PRIMARY KEY,
    playlist_id TEXT NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT NOT NULL,
    album_id TEXT,
    artist_id TEXT,
    track_source_id TEXT,
    cover_url TEXT,
    source_type TEXT NOT NULL,
    available_sources TEXT,
    format TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration INTEGER,
    created_at TEXT NOT NULL,
    plex_rating_key TEXT,
    library_file_id TEXT,
    UNIQUE(playlist_id, position)
);

CREATE TABLE IF NOT EXISTS album_release_pins (
    release_group_mbid TEXT PRIMARY KEY,
    release_mbid TEXT NOT NULL,
    set_by_user_id TEXT,
    set_at TEXT
);

CREATE TABLE IF NOT EXISTS artist_genres (
    artist_mbid_lower TEXT PRIMARY KEY,
    artist_mbid TEXT NOT NULL,
    genres_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS artist_genre_lookup (
    artist_mbid_lower TEXT NOT NULL,
    genre_lower TEXT NOT NULL,
    PRIMARY KEY (artist_mbid_lower, genre_lower)
);

CREATE INDEX IF NOT EXISTS idx_artist_genre_lookup_genre
ON artist_genre_lookup(genre_lower, artist_mbid_lower);

CREATE TABLE IF NOT EXISTS compat_bookmarks (
    user_id TEXT NOT NULL,
    file_id TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at REAL NOT NULL,
    changed_at REAL NOT NULL,
    PRIMARY KEY(user_id, file_id)
);

CREATE TABLE IF NOT EXISTS compat_play_queues (
    user_id TEXT PRIMARY KEY,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    changed_by_client TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS compat_play_queue_items (
    user_id TEXT NOT NULL REFERENCES compat_play_queues(user_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    PRIMARY KEY(user_id, item_index)
);

CREATE TABLE IF NOT EXISTS compat_id_map (
    jf_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    internal_id TEXT NOT NULL,
    UNIQUE(kind, internal_id)
);

CREATE TABLE IF NOT EXISTS library_user_favorites (
    user_id TEXT NOT NULL,
    item_kind TEXT NOT NULL CHECK(item_kind IN ('artist','album','track')),
    item_id TEXT NOT NULL,
    created_at REAL NOT NULL,
    PRIMARY KEY(user_id, item_kind, item_id)
);

CREATE TABLE IF NOT EXISTS library_play_history (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT,
    recording_mbid TEXT,
    release_group_mbid TEXT,
    duration_ms INTEGER,
    source TEXT,
    played_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS library_playlists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    cover_image_path TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    source_ref TEXT,
    user_id TEXT,
    is_public INTEGER NOT NULL DEFAULT 0 CHECK(is_public IN (0,1))
);

CREATE TABLE IF NOT EXISTS library_playlist_tracks (
    id TEXT PRIMARY KEY,
    playlist_id TEXT NOT NULL REFERENCES library_playlists(id) ON DELETE CASCADE,
    position INTEGER NOT NULL,
    track_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_name TEXT NOT NULL,
    album_id TEXT,
    artist_id TEXT,
    track_source_id TEXT,
    cover_url TEXT,
    source_type TEXT NOT NULL,
    available_sources TEXT,
    format TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    duration INTEGER,
    created_at TEXT NOT NULL,
    plex_rating_key TEXT,
    library_file_id TEXT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    reference_tombstone_id TEXT REFERENCES library_reference_tombstones(id) ON DELETE RESTRICT,
    UNIQUE(playlist_id, position)
);

CREATE TABLE IF NOT EXISTS library_album_release_pins (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT NOT NULL,
    set_by_user_id TEXT,
    set_at TEXT
);

CREATE TABLE IF NOT EXISTS library_compat_bookmarks (
    user_id TEXT NOT NULL,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at REAL NOT NULL,
    changed_at REAL NOT NULL,
    PRIMARY KEY(user_id, local_track_id)
);

CREATE TABLE IF NOT EXISTS library_compat_play_queues (
    user_id TEXT PRIMARY KEY,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    changed_by_client TEXT NOT NULL DEFAULT ''
);

CREATE TABLE IF NOT EXISTS library_compat_play_queue_items (
    user_id TEXT NOT NULL REFERENCES library_compat_play_queues(user_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    PRIMARY KEY(user_id, item_index)
);

CREATE TABLE IF NOT EXISTS library_compat_id_map (
    jf_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    internal_id TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS local_artists (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    sort_name TEXT,
    folded_name TEXT NOT NULL,
    normalized_name TEXT NOT NULL DEFAULT '',
    kind TEXT NOT NULL CHECK(kind IN ('person','group','various_artists','unknown')),
    retired_into_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS local_albums (
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
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS local_tracks (
    id TEXT PRIMARY KEY,
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
    tags_read_at REAL,
    metadata_incomplete INTEGER NOT NULL DEFAULT 0 CHECK(metadata_incomplete IN (0,1)),
    title TEXT NOT NULL,
    title_folded TEXT NOT NULL,
    artist_name TEXT,
    artist_name_folded TEXT,
    album_title TEXT NOT NULL,
    album_title_folded TEXT NOT NULL,
    album_artist_name TEXT,
    album_artist_name_folded TEXT,
    disc_number INTEGER NOT NULL DEFAULT 1,
    track_number INTEGER NOT NULL DEFAULT 0,
    year INTEGER,
    genre TEXT,
    genre_folded TEXT,
    title_sort TEXT,
    artist_sort TEXT,
    album_sort TEXT,
    album_artist_sort TEXT,
    disc_subtitle TEXT,
    is_compilation INTEGER NOT NULL DEFAULT 0 CHECK(is_compilation IN (0,1)),
    embedded_release_group_mbid TEXT,
    embedded_release_mbid TEXT,
    embedded_recording_mbid TEXT,
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
    missing_since REAL,
    excluded_at REAL,
    ingest_source TEXT NOT NULL,
    download_task_id TEXT,
    source_path TEXT,
    imported_at REAL NOT NULL,
    membership_source TEXT NOT NULL CHECK(membership_source IN ('automatic','legacy_import','manual')),
    membership_locked INTEGER NOT NULL DEFAULT 0 CHECK(membership_locked IN (0,1)),
    desired_policy_revision TEXT NOT NULL DEFAULT '',
    applied_policy_revision TEXT NOT NULL DEFAULT '',
    applied_policy TEXT NOT NULL DEFAULT 'automatic' CHECK(applied_policy IN ('local_metadata','automatic','excluded')),
    manual_excluded INTEGER NOT NULL DEFAULT 0 CHECK(manual_excluded IN (0,1)),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(root_id, relative_path)
);

CREATE TABLE IF NOT EXISTS local_album_artists (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_album_id, position)
);

CREATE TABLE IF NOT EXISTS local_track_artists (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_track_id, position)
);

CREATE TABLE IF NOT EXISTS library_identification_attempts (
    id TEXT PRIMARY KEY,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    trigger TEXT NOT NULL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    input_tag_revision TEXT NOT NULL,
    input_policy_revision TEXT NOT NULL,
    input_file_revision TEXT NOT NULL,
    matcher_version TEXT NOT NULL,
    state TEXT NOT NULL,
    terminal_reason_code TEXT NOT NULL,
    selected_candidate_key TEXT,
    candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
    degradation_flags_json TEXT NOT NULL DEFAULT '[]',
    started_at REAL NOT NULL,
    completed_at REAL NOT NULL,
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS library_identification_evidence (
    id TEXT PRIMARY KEY,
    attempt_id TEXT NOT NULL REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    candidate_key TEXT NOT NULL,
    evidence_json BLOB NOT NULL,
    evidence_size_bytes INTEGER NOT NULL CHECK(evidence_size_bytes >= 0),
    compacted INTEGER NOT NULL DEFAULT 0 CHECK(compacted IN (0,1)),
    created_at REAL NOT NULL,
    UNIQUE(attempt_id, candidate_key)
);

CREATE TRIGGER IF NOT EXISTS trg_library_identification_attempts_immutable
BEFORE UPDATE ON library_identification_attempts
BEGIN SELECT RAISE(ABORT, 'identification attempts are immutable'); END;

CREATE TRIGGER IF NOT EXISTS trg_library_identification_evidence_immutable
BEFORE UPDATE ON library_identification_evidence
BEGIN SELECT RAISE(ABORT, 'identification evidence is immutable'); END;

CREATE TABLE IF NOT EXISTS local_artist_external_identities (
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK(provider = 'musicbrainz'),
    provider_artist_id TEXT NOT NULL,
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_artist_id, provider),
    UNIQUE(provider, provider_artist_id)
);

CREATE TABLE IF NOT EXISTS local_album_external_identities (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL DEFAULT 'musicbrainz' CHECK(provider = 'musicbrainz'),
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT,
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    matcher_version TEXT,
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_album_id, provider)
);

CREATE TABLE IF NOT EXISTS local_track_external_identities (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL DEFAULT 'musicbrainz' CHECK(provider = 'musicbrainz'),
    recording_mbid TEXT NOT NULL,
    release_mbid TEXT,
    decision_source TEXT NOT NULL CHECK(decision_source IN ('embedded','automatic','manual','legacy_import')),
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    selected_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_track_id, provider)
);

CREATE TABLE IF NOT EXISTS local_artist_aliases (
    alias TEXT PRIMARY KEY,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_artist','merged_artist','compat_migration')),
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS local_album_aliases (
    alias TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_release_group','merged_album','compat_migration')),
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS local_artist_merge_candidates (
    id TEXT PRIMARY KEY,
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','resolved','dismissed')),
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(left_artist_id != right_artist_id),
    UNIQUE(left_artist_id, right_artist_id, reason_code)
);

CREATE TABLE IF NOT EXISTS local_album_artwork (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    cover_url TEXT,
    source TEXT NOT NULL CHECK(source IN ('embedded','cover_cache','manual','provider')),
    source_locator TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version BETWEEN 1 AND 9223372036854775807),
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_genre_artwork_revisions (
    genre_folded TEXT PRIMARY KEY,
    value INTEGER NOT NULL DEFAULT 1 CHECK(value BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS audio_fingerprint_outcomes (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    stat_revision TEXT NOT NULL,
    fingerprinter_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('matched','no_match','failed','disabled','skipped','deferred')),
    fingerprint TEXT,
    duration_seconds REAL,
    recording_mbid TEXT,
    release_group_ids_json TEXT NOT NULL DEFAULT '[]',
    score REAL,
    failure_code TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count >= 1),
    first_attempt_at REAL NOT NULL,
    last_attempt_at REAL NOT NULL,
    retry_after REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(local_track_id, stat_revision, fingerprinter_version)
);

CREATE TABLE IF NOT EXISTS library_identification_reviews (
    id TEXT PRIMARY KEY,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK(state IN ('needs_review','keep_tagged','excluded','resolved')),
    reason_code TEXT NOT NULL,
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    input_revision TEXT NOT NULL,
    decision_revision INTEGER NOT NULL DEFAULT 1 CHECK(decision_revision BETWEEN 1 AND 9223372036854775807),
    decided_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    decided_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_library_reviews_active_album
ON library_identification_reviews(local_album_id, input_revision)
WHERE local_album_id IS NOT NULL AND state != 'resolved';
CREATE UNIQUE INDEX IF NOT EXISTS idx_library_reviews_active_track
ON library_identification_reviews(local_track_id, input_revision)
WHERE local_track_id IS NOT NULL AND state != 'resolved';

CREATE TABLE IF NOT EXISTS library_enqueue_sequence (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 9223372036854775807)
);
INSERT OR IGNORE INTO library_enqueue_sequence(singleton, value) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS library_identification_jobs (
    id TEXT PRIMARY KEY,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('automatic','review_retry','post_processing')),
    state TEXT NOT NULL CHECK(state IN ('queued','running','succeeded','needs_review','failed','cancelled','paused')),
    priority INTEGER NOT NULL,
    enqueue_sequence INTEGER NOT NULL,
    input_revision TEXT NOT NULL,
    dedupe_key TEXT NOT NULL,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    not_before REAL NOT NULL DEFAULT 0,
    last_failure_code TEXT,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    terminal_result_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    checkpoint_json TEXT,
    lease_owner TEXT,
    lease_expires_at REAL,
    heartbeat_at REAL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    terminal_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_identification_jobs_active_dedupe
ON library_identification_jobs(dedupe_key)
WHERE state IN ('queued','running','paused');

CREATE TABLE IF NOT EXISTS library_operation_jobs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('bulk_review_apply','repair','explicit_reidentification')),
    state TEXT NOT NULL CHECK(state IN ('queued','running','paused','ready','succeeded','failed','cancelled','stopped')),
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    input_catalog_revision INTEGER CHECK(input_catalog_revision BETWEEN 0 AND 9223372036854775807),
    expected_work_count INTEGER NOT NULL DEFAULT 0 CHECK(expected_work_count >= 0),
    completed_count INTEGER NOT NULL DEFAULT 0 CHECK(completed_count >= 0),
    succeeded_count INTEGER NOT NULL DEFAULT 0 CHECK(succeeded_count >= 0),
    failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
    skipped_count INTEGER NOT NULL DEFAULT 0 CHECK(skipped_count >= 0),
    control_request TEXT NOT NULL DEFAULT 'none' CHECK(control_request IN ('none','pause','stop')),
    terminal_code TEXT,
    idempotency_key TEXT UNIQUE,
    lease_owner TEXT,
    lease_expires_at REAL,
    heartbeat_at REAL,
    created_at REAL NOT NULL,
    started_at REAL,
    phase_started_at REAL,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    updated_at REAL NOT NULL,
    terminal_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_operation_work (
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    expected_subject_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    action TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','running','succeeded','failed','skipped')),
    checkpoint_json TEXT,
    result_json TEXT,
    failure_code TEXT,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(job_id, ordinal),
    UNIQUE(job_id, idempotency_key),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS library_bulk_review_snapshots (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    selection_json TEXT NOT NULL,
    normalized_filter_json TEXT,
    preview_token TEXT NOT NULL,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_bulk_review_previews (
    preview_token TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    selection_json TEXT NOT NULL,
    normalized_filter_json TEXT,
    catalog_revision INTEGER,
    requires_local_metadata_confirmation INTEGER NOT NULL DEFAULT 0 CHECK(requires_local_metadata_confirmation IN (0,1)),
    created_at REAL NOT NULL,
    expires_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_bulk_review_preview_subjects (
    preview_token TEXT NOT NULL REFERENCES library_bulk_review_previews(preview_token) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    review_id TEXT NOT NULL,
    local_album_id TEXT,
    local_track_id TEXT,
    expected_subject_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    PRIMARY KEY(preview_token, ordinal),
    UNIQUE(preview_token, review_id),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
);

CREATE TABLE IF NOT EXISTS library_reidentification_snapshots (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    one_off_local_metadata INTEGER NOT NULL DEFAULT 0 CHECK(one_off_local_metadata IN (0,1)),
    selected_candidate_key TEXT,
    result_json TEXT,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_repair_snapshots (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    scope_json TEXT NOT NULL,
    source_matcher_version TEXT,
    target_matcher_version TEXT NOT NULL,
    phase TEXT NOT NULL DEFAULT 'dry_run' CHECK(phase IN ('dry_run','apply')),
    result_json TEXT,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_identity_repair_findings (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    evidence_id TEXT REFERENCES library_identification_evidence(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_identity_revision INTEGER,
    finding_code TEXT NOT NULL,
    confidence TEXT NOT NULL,
    reason_code TEXT NOT NULL DEFAULT '',
    apply_eligible INTEGER NOT NULL DEFAULT 0 CHECK(apply_eligible IN (0,1)),
    apply_result TEXT,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','applied','skipped','stale')),
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, local_album_id, finding_code)
);

CREATE TABLE IF NOT EXISTS library_catalog_actions (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    action_kind TEXT NOT NULL,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    reason_code TEXT,
    created_at REAL NOT NULL,
    CHECK(local_artist_id IS NOT NULL OR local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
);

CREATE TABLE IF NOT EXISTS library_policy_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    desired_policy_revision TEXT NOT NULL,
    pending_scope_ids_json TEXT NOT NULL DEFAULT '[]',
    pending_scopes_json TEXT NOT NULL DEFAULT '[]',
    changed_track_count INTEGER NOT NULL DEFAULT 0,
    cancelled_work_count INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_policy_transitions (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    previous_policy_revision TEXT NOT NULL,
    proposed_policy_revision TEXT NOT NULL,
    previous_settings_json TEXT NOT NULL,
    proposed_settings_json TEXT NOT NULL,
    scopes_json TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('prepared','completed','aborted')),
    prepared_at REAL NOT NULL,
    completed_at REAL
);

CREATE TABLE IF NOT EXISTS library_scan_runs (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('incremental','rescan_files','policy_reconcile')),
    trigger TEXT NOT NULL CHECK(trigger IN ('manual','automatic','subsonic','startup_resume','policy_apply')),
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    state TEXT NOT NULL CHECK(state IN ('queued','discovering','indexing','reconciling','pausing','paused','stopping','completed','cancelled','superseded_policy_changed','failed')),
    phase TEXT NOT NULL CHECK(phase IN ('queued','discovering','indexing','reconciling')),
    resume_phase TEXT CHECK(resume_phase IN ('queued','discovering','indexing','reconciling')),
    requested_control TEXT NOT NULL DEFAULT 'none' CHECK(requested_control IN ('none','pause','stop')),
    aggregate_scope TEXT NOT NULL,
    total_count INTEGER NOT NULL DEFAULT 0,
    discovered_count INTEGER NOT NULL DEFAULT 0,
    inspected_count INTEGER NOT NULL DEFAULT 0,
    new_count INTEGER NOT NULL DEFAULT 0,
    changed_count INTEGER NOT NULL DEFAULT 0,
    indexed_count INTEGER NOT NULL DEFAULT 0,
    unchanged_count INTEGER NOT NULL DEFAULT 0,
    excluded_count INTEGER NOT NULL DEFAULT 0,
    missing_count INTEGER NOT NULL DEFAULT 0,
    errored_count INTEGER NOT NULL DEFAULT 0,
    identification_enqueued_count INTEGER NOT NULL DEFAULT 0,
    coalesced_request_count INTEGER NOT NULL DEFAULT 0,
    queued_at REAL NOT NULL,
    started_at REAL,
    updated_at REAL NOT NULL,
    terminal_at REAL,
    heartbeat_at REAL,
    terminal_code TEXT,
    terminal_summary TEXT,
    stop_requested_at REAL,
    pause_requested_at REAL,
    control_latency_ms INTEGER,
    inventory_cleanup_pending INTEGER NOT NULL DEFAULT 0
        CHECK(inventory_cleanup_pending IN (0,1)),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_scan_run_scopes (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    scope_sequence INTEGER NOT NULL,
    root_id TEXT NOT NULL,
    scope_id TEXT,
    relative_path TEXT NOT NULL,
    root_path TEXT,
    effective_policy TEXT NOT NULL CHECK(effective_policy IN ('local_metadata','automatic','excluded')),
    policy_revision TEXT NOT NULL,
    estimated_count INTEGER,
    discovered_count INTEGER NOT NULL DEFAULT 0,
    discovery_state TEXT NOT NULL DEFAULT 'pending',
    discovery_generation INTEGER NOT NULL DEFAULT 1,
    reconciliation_state TEXT NOT NULL DEFAULT 'pending',
    reconciliation_cursor TEXT,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    error_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, scope_sequence),
    UNIQUE(run_id, root_id, relative_path)
);

CREATE TABLE IF NOT EXISTS library_scan_run_triggers (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    trigger_sequence INTEGER NOT NULL,
    trigger TEXT NOT NULL CHECK(trigger IN ('manual','automatic','subsonic','startup_resume','policy_apply')),
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reason TEXT NOT NULL,
    requested_at REAL NOT NULL,
    PRIMARY KEY(run_id, trigger_sequence)
);

CREATE TABLE IF NOT EXISTS library_scan_inventory (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    scope_relative_path TEXT NOT NULL DEFAULT '.',
    discovery_generation INTEGER NOT NULL DEFAULT 1,
    absolute_path TEXT NOT NULL,
    file_size_bytes INTEGER NOT NULL,
    file_mtime_ns INTEGER NOT NULL,
    stat_revision TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    effective_policy TEXT NOT NULL CHECK(effective_policy IN ('local_metadata','automatic','excluded')),
    comparison_result TEXT NOT NULL CHECK(comparison_result IN ('new','changed','unchanged','excluded','candidate_missing')),
    processing_state TEXT NOT NULL DEFAULT 'pending',
    checkpoint TEXT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, root_id, relative_path)
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_contexts (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','completed','failed')),
    staging_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(staging_state IN ('pending','tracks','tokens','groups','continuity','albums','memberships','retirement','queue','completed')),
    staging_cursor TEXT,
    application_cursor TEXT,
    queue_cursor TEXT,
    grouping_merge_target TEXT,
    grouping_merge_ready INTEGER NOT NULL DEFAULT 0
        CHECK(grouping_merge_ready IN (0,1)),
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, root_id, relative_directory)
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_evidence (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    preliminary_key TEXT NOT NULL,
    grouping_token TEXT,
    title TEXT NOT NULL,
    title_normalized TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    album_artist_normalized TEXT NOT NULL,
    track_number INTEGER NOT NULL,
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    album_created_at REAL NOT NULL,
    reason_code TEXT NOT NULL,
    PRIMARY KEY(run_id, root_id, relative_directory, local_track_id),
    FOREIGN KEY(run_id, root_id, relative_directory)
        REFERENCES library_scan_grouping_contexts(run_id, root_id, relative_directory)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_groups (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    grouping_token TEXT NOT NULL,
    grouping_key TEXT NOT NULL,
    title TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    retained_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    continuity_reason_code TEXT,
    local_album_id TEXT,
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    tag_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000',
    stat_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000',
    policy_revision_accumulator TEXT NOT NULL DEFAULT '0000000000000000000000000000000000000000000000000000000000000000',
    automatic_track_count INTEGER NOT NULL DEFAULT 0,
    local_metadata_track_count INTEGER NOT NULL DEFAULT 0,
    excluded_track_count INTEGER NOT NULL DEFAULT 0,
    embedded_identity_count INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(run_id, root_id, relative_directory, grouping_token),
    FOREIGN KEY(run_id, root_id, relative_directory)
        REFERENCES library_scan_grouping_contexts(run_id, root_id, relative_directory)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_values (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    grouping_token TEXT NOT NULL,
    value_kind TEXT NOT NULL CHECK(value_kind IN ('title','artist','reason')),
    normalized_value TEXT NOT NULL,
    display_value TEXT NOT NULL,
    occurrence_count INTEGER NOT NULL CHECK(occurrence_count > 0),
    PRIMARY KEY(
        run_id, root_id, relative_directory, grouping_token,
        value_kind, normalized_value
    ),
    FOREIGN KEY(run_id, root_id, relative_directory, grouping_token)
        REFERENCES library_scan_grouping_groups(
            run_id, root_id, relative_directory, grouping_token
        ) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_edges (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    grouping_token TEXT NOT NULL,
    overlap_count INTEGER NOT NULL CHECK(overlap_count > 0),
    processed INTEGER NOT NULL DEFAULT 0 CHECK(processed IN (0,1)),
    PRIMARY KEY(run_id, root_id, relative_directory, old_album_id, grouping_token),
    FOREIGN KEY(run_id, root_id, relative_directory, grouping_token)
        REFERENCES library_scan_grouping_groups(run_id, root_id, relative_directory, grouping_token)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_old_nodes (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    degree INTEGER NOT NULL CHECK(degree > 0),
    matched_grouping_token TEXT,
    PRIMARY KEY(run_id, root_id, relative_directory, old_album_id),
    FOREIGN KEY(run_id, root_id, relative_directory)
        REFERENCES library_scan_grouping_contexts(run_id, root_id, relative_directory)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_scan_grouping_new_nodes (
    run_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    relative_directory TEXT NOT NULL,
    grouping_token TEXT NOT NULL,
    degree INTEGER NOT NULL CHECK(degree > 0),
    matched_old_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    PRIMARY KEY(run_id, root_id, relative_directory, grouping_token),
    FOREIGN KEY(run_id, root_id, relative_directory, grouping_token)
        REFERENCES library_scan_grouping_groups(run_id, root_id, relative_directory, grouping_token)
        ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_work_control (
    queue_kind TEXT PRIMARY KEY CHECK(queue_kind = 'identification'),
    state TEXT NOT NULL CHECK(state IN ('running','paused')),
    requested_at REAL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    high_priority_claim_count INTEGER NOT NULL DEFAULT 0 CHECK(high_priority_claim_count >= 0),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);
INSERT OR IGNORE INTO library_work_control(queue_kind, state) VALUES ('identification', 'running');

CREATE TABLE IF NOT EXISTS library_catalog_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 9223372036854775807)
);
INSERT OR IGNORE INTO library_catalog_revision(singleton, value) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS library_event_stream_revisions (
    stream_kind TEXT PRIMARY KEY CHECK(stream_kind IN ('scan','identification','operation')),
    value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 9223372036854775807)
);
INSERT OR IGNORE INTO library_event_stream_revisions(stream_kind, value) VALUES ('scan', 0);
INSERT OR IGNORE INTO library_event_stream_revisions(stream_kind, value) VALUES ('identification', 0);
INSERT OR IGNORE INTO library_event_stream_revisions(stream_kind, value) VALUES ('operation', 0);

CREATE TABLE IF NOT EXISTS library_migration_runs (
    id TEXT PRIMARY KEY,
    source_revision TEXT NOT NULL,
    root_revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('dry_run','applying','completed','failed')),
    report_json TEXT NOT NULL,
    started_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    completed_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_migration_provenance (
    source_kind TEXT NOT NULL,
    source_key TEXT NOT NULL,
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    source_revision TEXT NOT NULL,
    imported_at REAL NOT NULL,
    migration_run_id TEXT REFERENCES library_migration_runs(id) ON DELETE RESTRICT,
    PRIMARY KEY(source_kind, source_key)
);

CREATE TABLE IF NOT EXISTS library_migration_markers (
    marker TEXT PRIMARY KEY,
    source_revision TEXT NOT NULL,
    target_catalog_revision INTEGER NOT NULL,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_reference_tombstones (
    id TEXT PRIMARY KEY,
    source_kind TEXT NOT NULL,
    source_key TEXT NOT NULL,
    legacy_file_id TEXT,
    title TEXT NOT NULL,
    artist_name TEXT,
    album_name TEXT,
    source_type TEXT,
    created_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(source_kind, source_key)
);

CREATE INDEX IF NOT EXISTS idx_local_artists_folded ON local_artists(folded_name, kind);
CREATE INDEX IF NOT EXISTS idx_local_artists_normalized ON local_artists(normalized_name, kind);
CREATE INDEX IF NOT EXISTS idx_local_artists_retired ON local_artists(retired_into_artist_id);
CREATE INDEX IF NOT EXISTS idx_local_albums_grouping ON local_albums(root_id, grouping_key);
CREATE INDEX IF NOT EXISTS idx_local_albums_search ON local_albums(title_folded, album_artist_name_folded);
CREATE INDEX IF NOT EXISTS idx_local_albums_ownership ON local_albums(title_folded, album_artist_name_folded, year);
CREATE INDEX IF NOT EXISTS idx_local_albums_retired ON local_albums(retired_into_album_id);
CREATE INDEX IF NOT EXISTS idx_local_tracks_album_order ON local_tracks(local_album_id, disc_number, track_number, id);
CREATE INDEX IF NOT EXISTS idx_local_tracks_album_availability ON local_tracks(local_album_id, availability);
CREATE INDEX IF NOT EXISTS idx_local_tracks_stat ON local_tracks(stat_revision);
CREATE INDEX IF NOT EXISTS idx_local_tracks_tag ON local_tracks(tag_revision);
CREATE INDEX IF NOT EXISTS idx_local_tracks_availability ON local_tracks(availability, missing_since);
CREATE INDEX IF NOT EXISTS idx_local_tracks_policy ON local_tracks(root_id, applied_policy, desired_policy_revision, relative_path);
CREATE INDEX IF NOT EXISTS idx_local_tracks_search ON local_tracks(title_folded, artist_name_folded, album_title_folded);
CREATE INDEX IF NOT EXISTS idx_local_tracks_path_hash ON local_tracks(path_hash);
CREATE INDEX IF NOT EXISTS idx_local_album_identity_rg ON local_album_external_identities(release_group_mbid);
CREATE INDEX IF NOT EXISTS idx_local_album_identity_rg_lower ON local_album_external_identities(lower(release_group_mbid));
CREATE INDEX IF NOT EXISTS idx_local_artist_identity_provider_lower ON local_artist_external_identities(lower(provider_artist_id));
CREATE INDEX IF NOT EXISTS idx_local_track_identity_recording ON local_track_external_identities(recording_mbid);
CREATE INDEX IF NOT EXISTS idx_album_alias_target ON local_album_aliases(local_album_id);
CREATE INDEX IF NOT EXISTS idx_artist_alias_target ON local_artist_aliases(local_artist_id);
CREATE INDEX IF NOT EXISTS idx_identification_attempt_subject_album ON library_identification_attempts(local_album_id, completed_at);
CREATE INDEX IF NOT EXISTS idx_identification_attempt_subject_track ON library_identification_attempts(local_track_id, completed_at);
CREATE INDEX IF NOT EXISTS idx_identification_evidence_attempt ON library_identification_evidence(attempt_id);
CREATE INDEX IF NOT EXISTS idx_identification_jobs_claim ON library_identification_jobs(state, not_before, priority, enqueue_sequence);
CREATE INDEX IF NOT EXISTS idx_identification_jobs_lease ON library_identification_jobs(state, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_identification_jobs_album_active ON library_identification_jobs(local_album_id, kind, state, enqueue_sequence) WHERE local_album_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_identification_jobs_track_active ON library_identification_jobs(local_track_id, kind, state, enqueue_sequence) WHERE local_track_id IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_library_reviews_cursor ON library_identification_reviews(updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_library_reviews_created_cursor ON library_identification_reviews(created_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_library_reviews_state_cursor ON library_identification_reviews(state, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_library_reviews_reason_cursor ON library_identification_reviews(reason_code, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_library_reviews_album ON library_identification_reviews(local_album_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_library_reviews_track_reason ON library_identification_reviews(local_track_id, reason_code);
CREATE INDEX IF NOT EXISTS idx_operation_jobs_claim ON library_operation_jobs(state, created_at);
CREATE INDEX IF NOT EXISTS idx_operation_jobs_lease ON library_operation_jobs(state, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_operation_work_claim ON library_operation_work(job_id, state, ordinal);
CREATE INDEX IF NOT EXISTS idx_bulk_review_preview_expiry ON library_bulk_review_previews(expires_at);
CREATE INDEX IF NOT EXISTS idx_repair_findings_cursor ON library_identity_repair_findings(job_id, finding_code, updated_at DESC, id DESC);
CREATE INDEX IF NOT EXISTS idx_scan_runs_state ON library_scan_runs(state, queued_at);
CREATE UNIQUE INDEX IF NOT EXISTS idx_scan_runs_single_active
ON library_scan_runs((1))
WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping');
CREATE UNIQUE INDEX IF NOT EXISTS idx_scan_runs_single_queued
ON library_scan_runs((1)) WHERE state = 'queued';
CREATE INDEX IF NOT EXISTS idx_scan_inventory_processing ON library_scan_inventory(run_id, processing_state, root_id, relative_path);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_pending ON library_scan_grouping_contexts(run_id, state, root_id, relative_directory);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_evidence_token ON library_scan_grouping_evidence(run_id, root_id, relative_directory, grouping_token, local_track_id);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_evidence_preliminary ON library_scan_grouping_evidence(run_id, root_id, relative_directory, preliminary_key, local_track_id);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_groups_key ON library_scan_grouping_groups(run_id, root_id, relative_directory, grouping_key);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_value_winner ON library_scan_grouping_values(run_id, root_id, relative_directory, grouping_token, value_kind, occurrence_count DESC, normalized_value);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_value_order ON library_scan_grouping_values(run_id, root_id, relative_directory, grouping_token, value_kind, normalized_value);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_edges_pending ON library_scan_grouping_edges(run_id, root_id, relative_directory, processed, old_album_id, grouping_token);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_old_degree ON library_scan_grouping_old_nodes(run_id, root_id, relative_directory, degree, old_album_id);
CREATE INDEX IF NOT EXISTS idx_scan_grouping_new_degree ON library_scan_grouping_new_nodes(run_id, root_id, relative_directory, degree, grouping_token);
CREATE INDEX IF NOT EXISTS idx_scan_inventory_track ON library_scan_inventory(local_track_id);
CREATE INDEX IF NOT EXISTS idx_migration_provenance_target ON library_migration_provenance(target_kind, target_id);
CREATE INDEX IF NOT EXISTS idx_reference_tombstone_legacy_file ON library_reference_tombstones(legacy_file_id);
CREATE INDEX IF NOT EXISTS idx_target_favorites_user_kind ON library_user_favorites(user_id, item_kind);
CREATE INDEX IF NOT EXISTS idx_target_history_user_played ON library_play_history(user_id, played_at DESC);
CREATE INDEX IF NOT EXISTS idx_target_history_track ON library_play_history(local_track_id);
CREATE INDEX IF NOT EXISTS idx_target_history_album ON library_play_history(local_album_id);
CREATE INDEX IF NOT EXISTS idx_target_history_artist ON library_play_history(local_artist_id);
CREATE INDEX IF NOT EXISTS idx_target_playlist_tracks_position ON library_playlist_tracks(playlist_id, position);
CREATE INDEX IF NOT EXISTS idx_target_playlist_tracks_track ON library_playlist_tracks(local_track_id);
CREATE INDEX IF NOT EXISTS idx_target_playlist_tracks_album ON library_playlist_tracks(local_album_id);
CREATE INDEX IF NOT EXISTS idx_target_playlist_tracks_artist ON library_playlist_tracks(local_artist_id);
CREATE UNIQUE INDEX IF NOT EXISTS idx_target_compat_id_internal
ON library_compat_id_map(kind, internal_id, jf_id);
"""
