-- 0135_baseline.sql - the whole v3 schema: the tables, their seed rows,
-- then the indexes, views and triggers, so no trigger fires on a seed row.
-- A new database starts here. The runner wraps the file in one
-- transaction with its stamp. A later schema change is a new numbered
-- file; this one is never edited.

-- Tables: kernel

CREATE TABLE config_ingests (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    outcome TEXT NOT NULL CHECK (outcome IN ('ingested', 'skipped_new_key')),
    file_sha256 TEXT NOT NULL,
    ingested_at INTEGER NOT NULL,
    keys_copied TEXT NOT NULL CHECK (json_valid(keys_copied)),
    keys_left TEXT NOT NULL CHECK (json_valid(keys_left))
) STRICT;

CREATE TABLE config_sections (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    revision INTEGER NOT NULL CHECK (revision >= 1),
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "db_planner_stats" (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    analyzed_at INTEGER NOT NULL,
    indexed_tracks INTEGER NOT NULL
) STRICT;

CREATE TABLE durable_jobs (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (length(kind) BETWEEN 1 AND 64),
    idempotency_key TEXT NOT NULL CHECK (length(idempotency_key) BETWEEN 1 AND 200),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'done', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    not_before_at INTEGER NOT NULL,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json) AND length(payload_json) <= 16384),
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) <= 64),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    finished_at INTEGER,
    CHECK (state <> 'running' OR (lease_owner IS NOT NULL AND lease_expires_at IS NOT NULL)),
    CHECK ((state IN ('done', 'failed')) = (finished_at IS NOT NULL))
) STRICT;

CREATE TABLE durable_loops (
    name TEXT PRIMARY KEY NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'stopped', 'failed')),
    last_heartbeat_at INTEGER,
    last_error TEXT CHECK (last_error IS NULL OR length(last_error) <= 64),
    updated_at INTEGER NOT NULL
) STRICT;

-- Tables: auth

CREATE TABLE auth_app_passwords (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    secret_sha256 TEXT NOT NULL UNIQUE,
    secret_encrypted TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    last_client TEXT,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1))
) STRICT;

CREATE TABLE "auth_oidc_states" (
    state TEXT NOT NULL PRIMARY KEY,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    code_verifier TEXT
) STRICT;

CREATE TABLE "auth_password_recovery_codes" (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "auth_providers" (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_uid TEXT NOT NULL,
    provider_data TEXT,
    password_hash TEXT,
    hash_scheme TEXT CHECK(hash_scheme IN ('bcrypt', 'argon2id')),
    created_at INTEGER NOT NULL,
    UNIQUE (provider, provider_uid),
    CHECK ((password_hash IS NULL) = (hash_scheme IS NULL)),
    CHECK (provider = 'local' OR password_hash IS NULL),
    CHECK (provider <> 'local' OR provider_data IS NULL)
) STRICT;

CREATE TABLE "auth_tokens" (
    id TEXT NOT NULL PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    issued_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0 CHECK (revoked IN (0, 1)),
    user_agent TEXT,
    session_kind TEXT NOT NULL DEFAULT 'standard'
        CHECK(session_kind IN ('standard', 'companion')),
    label TEXT,
    CHECK (session_kind = 'companion' OR label IS NULL)
) STRICT;

CREATE TABLE "auth_users" (
    id TEXT NOT NULL PRIMARY KEY,
    display_name TEXT NOT NULL,
    email TEXT UNIQUE,
    avatar_url TEXT,
    role TEXT NOT NULL DEFAULT 'user' CHECK(role IN ('user', 'trusted', 'admin')),
    created_at INTEGER NOT NULL,
    last_login_at INTEGER,
    username TEXT,
    username_display TEXT
) STRICT;

CREATE TABLE "user_connections" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    connection_data TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, service)
) STRICT;

-- Tables: library

CREATE TABLE library_album_departures (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    previous_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    departed_at INTEGER NOT NULL,
    arrived_at INTEGER NOT NULL,
    PRIMARY KEY (local_track_id, previous_album_id)
) STRICT;

CREATE TABLE library_album_looks_wrong (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (local_album_id, user_id)
) STRICT;

CREATE TABLE "library_album_match_state" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN ('unconfirmed', 'unmatched')),
    reason_code TEXT NOT NULL,
    release_mbid TEXT,
    candidates_json TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_album_takeovers" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    cause TEXT NOT NULL
        CHECK(cause IN ('files_changed','retag_followed','run_undone','replaced_hands_off')),
    noticed_at INTEGER NOT NULL,
    handed_back_at INTEGER,
    hand_back_tries INTEGER NOT NULL DEFAULT 0 CHECK(hand_back_tries >= 0),
    hand_back_retry_at INTEGER
) STRICT;

CREATE TABLE library_artist_credit_proofs (
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('album','track')),
    subject_id TEXT NOT NULL,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE CASCADE,
    credit_position INTEGER NOT NULL CHECK(credit_position >= 0),
    source_local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    artist_mbid TEXT NOT NULL
        CHECK(length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)),
    canonical_name TEXT NOT NULL,
    credited_name TEXT NOT NULL,
    sort_name TEXT NOT NULL DEFAULT '',
    join_phrase TEXT NOT NULL DEFAULT '',
    release_mbid TEXT NOT NULL,
    release_track_mbid TEXT,
    album_identity_revision INTEGER NOT NULL
        CHECK(album_identity_revision BETWEEN 1 AND 9223372036854775807),
    track_identity_revision INTEGER
        CHECK(track_identity_revision IS NULL OR track_identity_revision
              BETWEEN 1 AND 9223372036854775807),
    evidence_hash TEXT NOT NULL CHECK(
        length(evidence_hash) = 64 AND evidence_hash = lower(evidence_hash)
        AND evidence_hash NOT GLOB '*[^0-9a-f]*'
    ),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(subject_kind, subject_id, credit_position),
    CHECK(
        (subject_kind = 'album' AND subject_id = local_album_id AND local_track_id IS NULL
         AND release_track_mbid IS NULL AND track_identity_revision IS NULL)
        OR
        (subject_kind = 'track' AND subject_id = local_track_id AND local_track_id IS NOT NULL
         AND release_track_mbid IS NOT NULL AND track_identity_revision IS NOT NULL)
    )
) STRICT;

CREATE TABLE "library_artist_reconciliation_dismissals" (
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    left_artist_revision INTEGER NOT NULL
        CHECK(left_artist_revision BETWEEN 1 AND 9223372036854775807),
    right_artist_revision INTEGER NOT NULL
        CHECK(right_artist_revision BETWEEN 1 AND 9223372036854775807),
    dismissed_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL DEFAULT 'MARKED_DISTINCT',
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(left_artist_id, right_artist_id),
    CHECK(left_artist_id < right_artist_id)
) STRICT;

CREATE TABLE library_artist_reconciliation_state (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    input_revision TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN (
        'waiting_for_identity','ambiguous_credit_structure','projected'
    )),
    reason_code TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_audio_fingerprint_outcomes" (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    stat_revision TEXT NOT NULL,
    fingerprinter_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('matched','no_match','deferred')),
    fingerprint TEXT,
    duration_seconds REAL,
    recording_mbid TEXT,
    release_group_ids_json TEXT NOT NULL DEFAULT '[]',
    partial_decode INTEGER NOT NULL DEFAULT 0 CHECK(partial_decode IN (0,1)),
    score REAL,
    failure_code TEXT,
    attempt_count INTEGER NOT NULL DEFAULT 1 CHECK(attempt_count >= 1),
    first_attempt_at INTEGER NOT NULL,
    last_attempt_at INTEGER NOT NULL,
    retry_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(local_track_id, stat_revision, fingerprinter_version)
) STRICT;

CREATE TABLE "library_automatic_edition_undo" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL UNIQUE REFERENCES local_albums(id) ON DELETE CASCADE,
    job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE SET NULL,
    evidence_id TEXT,
    prior_identity_json TEXT,
    prior_track_identities_json TEXT NOT NULL DEFAULT '[]',
    expected_post_album_revision INTEGER NOT NULL
        CHECK(expected_post_album_revision BETWEEN 1 AND 9223372036854775807),
    expected_post_identity_revision INTEGER NOT NULL
        CHECK(expected_post_identity_revision BETWEEN 1 AND 9223372036854775807),
    reason_code TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    consumed_at INTEGER,
    consumed_action_id TEXT
) STRICT;

CREATE TABLE "library_catalog_actions" (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT UNIQUE,
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    action_kind TEXT NOT NULL CHECK(action_kind IN ('accept_management_track_mappings',
        'album_artist', 'album_artist_undone', 'artist_link_moved', 'artist_link_removed',
        'artist_link_set', 'automatic_exact_edition', 'automatic_match_sealed',
        'automatic_match_withdrawn', 'continue_tracks', 'custom_edition', 'edition_choice',
        'edition_choice_cleared', 'edition_choice_undone', 'edition_confirmed',
        'files_put_back', 'files_recycled', 'fold_undone', 'follow_merged_artist_id',
        'leave_unmanaged', 'management_hand_back_given_up', 'management_handed_back',
        'management_reenabled', 'management_stepped_back', 'merge', 'merge_album',
        'merge_artist', 'move', 'new_copy_at_path', 'other_music_at_path', 'remove_track',
        'repair_detach', 'reset', 'retag_dismissal_ended', 'retag_first_half',
        'review_approve', 'review_detach', 'review_exclude', 'review_reopened',
        'review_restore', 'split', 'track_match_undone', 'track_matched',
        'undo_automatic_edition')),
    local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    before_json TEXT NOT NULL,
    after_json TEXT NOT NULL,
    reason_code TEXT,
    created_at INTEGER NOT NULL,
    CHECK(local_artist_id IS NOT NULL OR local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
) STRICT;

CREATE TABLE "library_catalog_revision" (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    value INTEGER NOT NULL DEFAULT 0
        CHECK(value BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE "library_contribution_callback_tokens" (
    token_hash TEXT PRIMARY KEY,
    contribution_id TEXT NOT NULL
        REFERENCES library_contribution_drafts(id) ON DELETE CASCADE,
    requested_by_user_id TEXT NOT NULL
        REFERENCES auth_users(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_contribution_drafts" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    created_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    updated_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    state TEXT NOT NULL CHECK(state IN (
        'draft', 'ready', 'seeded', 'verifying', 'linked',
        'needs_review', 'stale', 'cancelled'
    )),
    album_row_revision INTEGER NOT NULL,
    input_revision TEXT NOT NULL,
    local_snapshot_json TEXT NOT NULL,
    resolved_draft_json TEXT NOT NULL,
    source_selection_json TEXT NOT NULL,
    provider_snapshot_expires_at INTEGER,
    duplicate_result_json TEXT,
    duplicate_checked_at INTEGER,
    duplicate_input_revision TEXT,
    result_release_mbid TEXT,
    result_source TEXT CHECK(result_source IN ('callback', 'manual') OR result_source IS NULL),
    result_received_at INTEGER,
    seed_snapshot_json TEXT,
    seed_hash TEXT,
    seeded_at INTEGER,
    terminal_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
, last_verification_failure TEXT) STRICT;

CREATE TABLE "library_custom_edition_active" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    manifest_id TEXT NOT NULL UNIQUE REFERENCES library_custom_edition_manifests(id) ON DELETE RESTRICT,
    activated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
) STRICT;

CREATE TABLE "library_custom_edition_manifests" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    version INTEGER NOT NULL CHECK(version > 0),
    release_group_mbid TEXT NOT NULL,
    album_title TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    artist_mbid TEXT,
    album_metadata_json TEXT NOT NULL DEFAULT '{}',
    source_album_revision INTEGER NOT NULL CHECK(source_album_revision > 0),
    source_identity_revision INTEGER CHECK(source_identity_revision IS NULL OR source_identity_revision > 0),
    input_revision TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    selected_candidate_key TEXT,
    sealed_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    sealed_at INTEGER NOT NULL,
    UNIQUE(local_album_id, version),
    UNIQUE(local_album_id, content_hash)
) STRICT;

CREATE TABLE "library_custom_edition_tracks" (
    manifest_id TEXT NOT NULL REFERENCES library_custom_edition_manifests(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    source_track_revision INTEGER NOT NULL CHECK(source_track_revision > 0),
    source_identity_revision INTEGER CHECK(source_identity_revision IS NULL OR source_identity_revision > 0),
    stat_revision TEXT NOT NULL,
    tag_revision TEXT NOT NULL,
    title TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    album_artist_name TEXT NOT NULL,
    disc_number INTEGER NOT NULL CHECK(disc_number > 0),
    track_number INTEGER NOT NULL CHECK(track_number > 0),
    recording_mbid TEXT,
    artist_mbid TEXT,
    album_artist_mbid TEXT,
    metadata_json TEXT NOT NULL DEFAULT '{}',
    file_format TEXT NOT NULL DEFAULT '',
    duration_seconds REAL,
    PRIMARY KEY(manifest_id, ordinal),
    UNIQUE(manifest_id, local_track_id),
    UNIQUE(manifest_id, disc_number, track_number)
) STRICT;

CREATE TABLE "library_edition_choice_undo" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL UNIQUE REFERENCES local_albums(id) ON DELETE CASCADE,
    action_id TEXT NOT NULL,
    prior_identity_json TEXT,
    prior_tracks_json TEXT NOT NULL DEFAULT '[]',
    prior_match_state_json TEXT,
    expected_identity_revision INTEGER NOT NULL,
    created_at INTEGER NOT NULL
, prior_hold_json TEXT) STRICT;

CREATE TABLE "library_edition_remap_queue" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    chosen_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    queued_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    not_before_at INTEGER NOT NULL DEFAULT 0,
    last_code TEXT,
    -- 'album_pin': a person chose this album's edition; 'group_pin': the
    -- album got the choice from a release-group pin covering every copy;
    -- 'place_only': the album has its edition, only its files are placed.
    origin TEXT NOT NULL DEFAULT 'album_pin'
        CHECK(origin IN ('album_pin', 'group_pin', 'place_only'))
) STRICT;

CREATE TABLE "library_event_stream_revisions" (
    stream_kind TEXT PRIMARY KEY NOT NULL
        CHECK(stream_kind IN ('scan','identification','operation')),
    value INTEGER NOT NULL CHECK(value BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE library_folder_digests (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL CHECK (relative_path != ''),
    digest INTEGER NOT NULL,
    audio_files INTEGER NOT NULL CHECK (audio_files >= 0),
    PRIMARY KEY (root_id, relative_path)
) STRICT, WITHOUT ROWID;

CREATE TABLE "library_identification_attempts" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    trigger TEXT NOT NULL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    input_tag_revision TEXT NOT NULL,
    input_policy_revision TEXT NOT NULL,
    input_file_revision TEXT NOT NULL,
    input_identity_revision TEXT NOT NULL DEFAULT '',
    matcher_version TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('identified','needs_review')),
    terminal_reason_code TEXT NOT NULL,
    selected_candidate_key TEXT,
    candidate_count INTEGER NOT NULL DEFAULT 0 CHECK(candidate_count >= 0),
    degradation_flags_json TEXT NOT NULL DEFAULT '[]',
    started_at INTEGER NOT NULL,
    completed_at INTEGER NOT NULL,
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
) STRICT;

CREATE TABLE library_identify_artist_id_checks (
    artist_mbid TEXT PRIMARY KEY
        CHECK(length(artist_mbid) = 36 AND artist_mbid = lower(artist_mbid)),
    state TEXT NOT NULL CHECK(state IN ('pending','kept','merged')),
    canonical_mbid TEXT
        CHECK(canonical_mbid IS NULL
              OR (length(canonical_mbid) = 36 AND canonical_mbid = lower(canonical_mbid))),
    nominated_at INTEGER NOT NULL,
    checked_at INTEGER,
    CHECK((state = 'merged') = (canonical_mbid IS NOT NULL)),
    CHECK(canonical_mbid IS NULL OR canonical_mbid <> artist_mbid),
    CHECK((state = 'pending') = (checked_at IS NULL))
) STRICT;

CREATE TABLE "library_identify_jobs" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('automatic','manual','historical')),
    priority INTEGER NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('queued','running','deferred','succeeded','failed','attention')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    not_before_at INTEGER NOT NULL DEFAULT 0,
    lease_expires_at INTEGER,
    input_revision TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    failure_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_identify_reviews" (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason_code TEXT NOT NULL,
    candidates_json TEXT NOT NULL,
    state TEXT NOT NULL
        CHECK(state IN ('pending','approved','rejected','dismissed','excluded')),
    resolved_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    selected_candidate_key TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
, row_revision INTEGER NOT NULL DEFAULT 1, reopen_review_revision INTEGER, reopen_identity_revision INTEGER) STRICT;

CREATE TABLE "library_identity_repair_findings" (
    id TEXT NOT NULL PRIMARY KEY,
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_identity_revision INTEGER,
    finding_code TEXT NOT NULL,
    confidence TEXT NOT NULL,
    reason_code TEXT NOT NULL DEFAULT '',
    apply_eligible INTEGER NOT NULL DEFAULT 0 CHECK(apply_eligible IN (0,1)),
    apply_result TEXT,
    suggested_release_mbid TEXT,
    suggested_release_group_mbid TEXT,
    suggested_edition_json TEXT NOT NULL DEFAULT '{}',
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','applied','skipped','stale')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, local_album_id, finding_code)
) STRICT;

CREATE TABLE library_leftover_checks (
    root_id TEXT PRIMARY KEY,
    checked_at INTEGER NOT NULL,
    file_count INTEGER NOT NULL CHECK (file_count >= 0),
    files_json TEXT NOT NULL CHECK (json_valid(files_json) AND json_type(files_json) = 'array')
) STRICT, WITHOUT ROWID;

CREATE TABLE "library_management_automatic_queue" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    queued_at INTEGER NOT NULL,
    not_before_at INTEGER NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    last_reason_code TEXT,
    retag_followed_at INTEGER
) STRICT;

CREATE TABLE "library_management_baseline_purges" (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    impact_token TEXT NOT NULL CHECK(length(trim(impact_token)) > 0),
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    purged_baseline_count INTEGER NOT NULL CHECK(purged_baseline_count >= 0),
    detached_reference_count INTEGER NOT NULL CHECK(detached_reference_count >= 0),
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_management_baselines" (
    id TEXT PRIMARY KEY,
    local_track_id TEXT NOT NULL UNIQUE REFERENCES local_tracks(id) ON DELETE RESTRICT,
    original_root_id TEXT NOT NULL CHECK(length(trim(original_root_id)) > 0),
    original_relative_path TEXT NOT NULL CHECK(length(trim(original_relative_path)) > 0),
    format TEXT NOT NULL CHECK(length(trim(format)) > 0),
    adapter_version TEXT NOT NULL CHECK(length(trim(adapter_version)) > 0),
    semantic_snapshot_blob_sha256 TEXT NOT NULL
        REFERENCES library_management_blobs(sha256) ON DELETE RESTRICT,
    image_snapshot_json TEXT NOT NULL DEFAULT '[]',
    ancillary_snapshot_json TEXT NOT NULL DEFAULT '[]',
    file_mtime_ns INTEGER,
    file_mode INTEGER,
    stat_revision TEXT NOT NULL,
    tag_revision TEXT NOT NULL,
    identity_revision INTEGER
        CHECK(identity_revision IS NULL OR identity_revision BETWEEN 1 AND 9223372036854775807),
    created_at INTEGER NOT NULL,
    restore_status TEXT NOT NULL DEFAULT 'available' CHECK(restore_status IN (
        'available','restoring','restored','stale','purged'
    )),
    last_verified_at INTEGER,
    catalog_document_json TEXT,
    catalog_document_hash TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE "library_management_blobs" (
    sha256 TEXT PRIMARY KEY
        CHECK(length(sha256) = 64 AND sha256 = lower(sha256)
              AND sha256 NOT GLOB '*[^0-9a-f]*'),
    kind TEXT NOT NULL CHECK(kind IN (
        'tag_snapshot','image','sidecar_manifest','metadata_document'
    )),
    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
    relative_path TEXT NOT NULL CHECK(length(trim(relative_path)) > 0),
    media_metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE "library_management_exclusions" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    excluded_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    excluded_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
) STRICT;

CREATE TABLE "library_management_job_snapshots" (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK(mode IN (
        'preview','apply','automatic_apply','undo','baseline_restore','duplicate_resolution'
    )),
    origin TEXT NOT NULL CHECK(origin IN (
        'manual','acquisition','drop_import','scan_discovered'
    )),
    phase TEXT NOT NULL CHECK(phase IN (
        'planning','ready','applying','undoing','restoring','complete'
    )),
    selection_json TEXT NOT NULL,
    profile_revision TEXT NOT NULL,
    settings_revision TEXT NOT NULL,
    proposed_settings_revision TEXT,
    naming_revision TEXT NOT NULL,
    policy_revision TEXT NOT NULL,
    catalog_revision INTEGER NOT NULL
        CHECK(catalog_revision BETWEEN 0 AND 9223372036854775807),
    profile_snapshot_json TEXT NOT NULL,
    preview_token_hash TEXT,
    preview_created_at INTEGER,
    preview_expires_at INTEGER,
    apply_idempotency_key TEXT,
    target_root_id TEXT,
    linked_operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE SET NULL,
    intent_json TEXT NOT NULL DEFAULT '{}',
    summary_json TEXT NOT NULL DEFAULT '{}',
    warnings_json TEXT NOT NULL DEFAULT '[]',
    staging_cursor TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    jellyfin_refresh_job_id INTEGER,
    plex_refresh_job_id INTEGER,
    navidrome_refresh_job_id INTEGER
) STRICT;

CREATE TABLE "library_management_metadata_snapshots" (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL CHECK(length(trim(provider)) > 0),
    entity_kind TEXT NOT NULL CHECK(length(trim(entity_kind)) > 0),
    entity_id TEXT NOT NULL CHECK(length(trim(entity_id)) > 0),
    input_hash TEXT NOT NULL
        CHECK(length(input_hash) = 64 AND input_hash = lower(input_hash)
              AND input_hash NOT GLOB '*[^0-9a-f]*'),
    canonical_payload_json TEXT NOT NULL,
    payload_sha256 TEXT NOT NULL
        CHECK(length(payload_sha256) = 64 AND payload_sha256 = lower(payload_sha256)
              AND payload_sha256 NOT GLOB '*[^0-9a-f]*'),
    fetched_at INTEGER NOT NULL,
    expires_at INTEGER,
    provider_version_notes TEXT,
    UNIQUE(provider, entity_kind, entity_id, input_hash, payload_sha256)
) STRICT;

CREATE TABLE "library_management_overrides" (
    id TEXT PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('album','track')),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    field_name TEXT NOT NULL CHECK(length(trim(field_name)) > 0),
    value_json TEXT NOT NULL,
    mode TEXT NOT NULL CHECK(mode IN ('replace','preserve','clear')),
    actor_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reason TEXT,
    subject_revision INTEGER NOT NULL
        CHECK(subject_revision BETWEEN 1 AND 9223372036854775807),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(
        (subject_kind = 'album' AND local_album_id IS NOT NULL AND local_track_id IS NULL)
        OR
        (subject_kind = 'track' AND local_track_id IS NOT NULL AND local_album_id IS NULL)
    )
) STRICT;

CREATE TABLE "library_management_plan_items" (
    job_id TEXT NOT NULL
        REFERENCES library_management_job_snapshots(job_id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    bundle_ordinal INTEGER NOT NULL CHECK(bundle_ordinal >= 0),
    local_album_id TEXT REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER CHECK(
        expected_album_revision IS NULL
        OR expected_album_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_track_revision INTEGER CHECK(
        expected_track_revision IS NULL
        OR expected_track_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_identity_revision INTEGER CHECK(
        expected_identity_revision IS NULL
        OR expected_identity_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_album_identity_revision INTEGER CHECK(
        expected_album_identity_revision IS NULL
        OR expected_album_identity_revision BETWEEN 1 AND 9223372036854775807
    ),
    expected_override_revision TEXT,
    expected_catalog_revision INTEGER NOT NULL
        CHECK(expected_catalog_revision BETWEEN 0 AND 9223372036854775807),
    expected_policy_revision TEXT NOT NULL,
    expected_profile_revision TEXT NOT NULL,
    expected_root_id TEXT NOT NULL,
    expected_relative_path TEXT NOT NULL,
    expected_stat_revision TEXT NOT NULL,
    expected_tag_revision TEXT NOT NULL,
    expected_file_fingerprint TEXT NOT NULL,
    source_path_identity TEXT NOT NULL,
    destination_root_id TEXT,
    destination_relative_path TEXT,
    destination_collision_key TEXT,
    desired_document_json TEXT NOT NULL,
    desired_document_hash TEXT NOT NULL
        CHECK(length(desired_document_hash) = 64
              AND desired_document_hash = lower(desired_document_hash)
              AND desired_document_hash NOT GLOB '*[^0-9a-f]*'),
    catalog_document_json TEXT,
    catalog_document_hash TEXT,
    artwork_choices_json TEXT NOT NULL DEFAULT '[]',
    diff_json TEXT NOT NULL DEFAULT '{}',
    capability_json TEXT NOT NULL DEFAULT '{}',
    collision_json TEXT NOT NULL DEFAULT '[]',
    eligibility TEXT NOT NULL CHECK(eligibility IN ('eligible','warning','blocked','stale')),
    reason_code TEXT,
    estimated_temporary_bytes INTEGER NOT NULL DEFAULT 0
        CHECK(estimated_temporary_bytes >= 0),
    created_at INTEGER NOT NULL,
    PRIMARY KEY(job_id, ordinal),
    CHECK(local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
) STRICT;

CREATE TABLE "library_operation_control_idempotency" (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    control TEXT NOT NULL CHECK(control IN ('pause','resume','stop')),
    requested_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_operation_jobs" (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('bulk_review_apply','repair','explicit_reidentification','library_management')),
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
    lease_expires_at INTEGER,
    heartbeat_at INTEGER,
    next_attempt_at INTEGER,
    reidentification_attempt_count INTEGER NOT NULL DEFAULT 0
        CHECK(reidentification_attempt_count >= 0),
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    phase_started_at INTEGER,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE "library_operation_work" (
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
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    bundle_id TEXT,
    PRIMARY KEY(job_id, ordinal),
    UNIQUE(job_id, idempotency_key),
    CHECK((local_album_id IS NOT NULL) != (local_track_id IS NOT NULL))
) STRICT;

CREATE TABLE "library_publish_baselines" (
    track_id TEXT PRIMARY KEY,
    blob_sha256 TEXT NOT NULL,
    original_root TEXT NOT NULL,
    original_rel TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_publish_blob_refs" (
    sha256 TEXT NOT NULL,
    owner_kind TEXT NOT NULL
        CHECK(owner_kind IN ('operation','preview','baseline','v2_baseline')),
    owner_id TEXT NOT NULL,
    PRIMARY KEY (sha256, owner_kind, owner_id)
) STRICT;

CREATE TABLE "library_publish_blobs" (
    sha256 TEXT PRIMARY KEY,
    bytes BLOB NOT NULL
) STRICT;

CREATE TABLE library_publish_cover_baselines (
    track_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    rel TEXT NOT NULL,
    blob_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (track_id, root_id, rel)
) STRICT;

CREATE TABLE library_publish_created_folders (
    bundle_id  TEXT NOT NULL
        REFERENCES library_publish_operations(bundle_id) ON DELETE CASCADE,
    root_id    TEXT NOT NULL,
    folder_rel TEXT NOT NULL CHECK (folder_rel <> ''),
    PRIMARY KEY (bundle_id, root_id, folder_rel)
) STRICT;

CREATE TABLE "library_publish_journal" (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN (
        'audio','sidecar','artwork','output','delete','remove','put_back','add'
    )),
    source_root TEXT,
    source_rel TEXT,
    dest_root TEXT NOT NULL,
    dest_rel TEXT NOT NULL,
    staged TEXT NOT NULL,
    backup TEXT,
    source_sha256 TEXT,
    staged_sha256 TEXT NOT NULL,
    track_id TEXT,
    state TEXT NOT NULL CHECK(state IN (
        'prepared','staged','published','committed','cleaned','compensated','needs_attention',
        'cleanup_pending'
    )),
    seq INTEGER NOT NULL DEFAULT 0,
    fence_revision INTEGER,
    commit_as TEXT CHECK (commit_as IN ('managed', 'imported'))
) STRICT;

CREATE TABLE "library_publish_operations" (
    bundle_id TEXT PRIMARY KEY,
    bundle_json TEXT NOT NULL,
    applied_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE library_publish_outputs (
    journal_id TEXT PRIMARY KEY
        REFERENCES library_publish_journal(id) ON DELETE CASCADE,
    bundle_id TEXT NOT NULL
        REFERENCES library_publish_operations(bundle_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL CHECK (item_index >= 0),
    output_index INTEGER NOT NULL CHECK (output_index >= 0),
    track_id TEXT NOT NULL,
    dest_root TEXT NOT NULL,
    dest_rel TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('artwork', 'lyrics')),
    UNIQUE (bundle_id, item_index, output_index)
) STRICT;

CREATE TABLE "library_publish_replacements" (
    journal_id TEXT PRIMARY KEY REFERENCES library_publish_journal(id) ON DELETE CASCADE,
    replaced_rel TEXT NOT NULL,
    replaced_sha256 TEXT NOT NULL,
    replaced_track_id TEXT,
    recycle_to TEXT NOT NULL,
    recycled_at INTEGER,
    expired_at INTEGER,
    binned_stat_json TEXT
        CHECK (binned_stat_json IS NULL OR json_valid(binned_stat_json))
) STRICT;

CREATE TABLE "library_publish_resolutions" (
    bundle_id TEXT PRIMARY KEY,
    resolved_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    verified_files INTEGER NOT NULL CHECK(verified_files >= 0),
    resolved_at INTEGER NOT NULL,
    as_before_files INTEGER NOT NULL DEFAULT 0 CHECK (as_before_files >= 0),
    empty_files INTEGER NOT NULL DEFAULT 0 CHECK (empty_files >= 0)
) STRICT;

CREATE TABLE "library_publish_snapshots" (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    blob_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL
) STRICT;

CREATE TABLE library_recycle_bin_empties (
    id            TEXT    PRIMARY KEY,
    actor_user_id TEXT    REFERENCES auth_users(id) ON DELETE SET NULL,
    before_at     INTEGER NOT NULL,
    finished_at   INTEGER,
    deleted       INTEGER NOT NULL CHECK (deleted >= 0),
    freed_bytes   INTEGER NOT NULL CHECK (freed_bytes >= 0),
    older_folders INTEGER NOT NULL CHECK (older_folders >= 0),
    kept          INTEGER NOT NULL CHECK (kept >= 0),
    waited        INTEGER NOT NULL CHECK (waited >= 0),
    kept_json     TEXT    NOT NULL
) STRICT;

CREATE TABLE "library_reidentification_snapshots" (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    expected_album_revision INTEGER NOT NULL,
    expected_input_revision TEXT NOT NULL,
    expected_identity_revision TEXT NOT NULL DEFAULT '',
    one_off_local_metadata INTEGER NOT NULL DEFAULT 0 CHECK(one_off_local_metadata IN (0,1)),
    requested_release_mbid TEXT,
    selected_candidate_key TEXT,
    result_json TEXT,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_repair_materialization" (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    pinned_catalog_revision INTEGER NOT NULL
        CHECK(pinned_catalog_revision BETWEEN 0 AND 9223372036854775807),
    eligibility_version TEXT NOT NULL CHECK(length(trim(eligibility_version)) > 0),
    purpose TEXT NOT NULL CHECK(length(trim(purpose)) > 0),
    staging_cursor TEXT,
    staged_ordinal INTEGER NOT NULL DEFAULT -1 CHECK(staged_ordinal >= -1),
    staged_count INTEGER NOT NULL DEFAULT 0 CHECK(staged_count >= 0),
    sealed INTEGER NOT NULL DEFAULT 0 CHECK(sealed IN (0,1)),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_repair_snapshots" (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    scope_json TEXT NOT NULL,
    source_matcher_version TEXT,
    target_matcher_version TEXT NOT NULL,
    phase TEXT NOT NULL DEFAULT 'dry_run' CHECK(phase IN ('dry_run','apply')),
    result_json TEXT,
    created_at INTEGER NOT NULL
, purpose TEXT
    CHECK (purpose IN ('existing_matches', 'management_readiness',
                       'catalog_identity_hygiene', 'play_history_filing'))) STRICT;

CREATE TABLE library_repair_watermarks (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    identified_at INTEGER NOT NULL,
    identified_job_id TEXT NOT NULL
) STRICT;

CREATE TABLE "library_retag_pairs" (
    old_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    new_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    reason TEXT NOT NULL
        CHECK(reason IN ('mostly_missing','new_files_added','tracks_elsewhere','chosen_matches_differ','custom_edition','live_work','work_on_new_row','store_error','replaced_hands_off')),
    state TEXT NOT NULL
        CHECK(state IN ('open','joined','dismissed')),
    recorded_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (old_album_id, new_album_id)
) STRICT;

CREATE TABLE "library_review_exclusions" (
    review_id TEXT NOT NULL REFERENCES library_identify_reviews(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    PRIMARY KEY (review_id, local_track_id)
) STRICT;

CREATE TABLE "library_scan_album_moves" (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    local_track_id TEXT NOT NULL,
    previous_album_id TEXT NOT NULL,
    PRIMARY KEY (run_id, root_id, relative_path)
) STRICT;

CREATE TABLE "library_scan_failures" (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    failure_code TEXT NOT NULL,
    failure_detail TEXT NOT NULL DEFAULT '',
    phase TEXT NOT NULL CHECK(phase IN ('discovering','indexing','reconciling')),
    recorded_at INTEGER NOT NULL,
    PRIMARY KEY(run_id, root_id, relative_path, phase, failure_code)
) STRICT;

CREATE TABLE library_scan_fences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision  INTEGER NOT NULL
) STRICT;

CREATE TABLE library_scan_held_scopes (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    held_at INTEGER NOT NULL,
    PRIMARY KEY (root_id, relative_path)
) STRICT;

CREATE TABLE "library_scan_inventory" (
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
    processing_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(processing_state IN ('pending','indexed','skipped','deferred','failed')),
    checkpoint TEXT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(run_id, root_id, relative_path)
) STRICT;

CREATE TABLE library_scan_mounts (
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL CHECK (relative_path NOT IN ('', '.')),
    PRIMARY KEY (root_id, relative_path)
) STRICT;

CREATE TABLE "library_scan_run_scopes" (
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
    discovery_state TEXT NOT NULL DEFAULT 'pending'
        CHECK(discovery_state IN ('pending','completed','partially_read','unavailable')),
    discovery_generation INTEGER NOT NULL DEFAULT 1,
    reconciliation_cursor TEXT,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    error_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807), walk_revision INTEGER,
    PRIMARY KEY(run_id, scope_sequence),
    UNIQUE(run_id, root_id, relative_path)
) STRICT;

CREATE TABLE "library_scan_runs" (
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
    queued_at INTEGER NOT NULL,
    started_at INTEGER,
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER,
    heartbeat_at INTEGER,
    terminal_code TEXT,
    terminal_summary TEXT,
    phase_started_at INTEGER,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    stop_requested_at INTEGER,
    pause_requested_at INTEGER,
    control_latency_ms INTEGER,
    next_end_pass TEXT CHECK(next_end_pass IN ('scan_end_join_across_scans','scan_end_carry_to_first_half','scan_end_join_replaced','scan_end_keep_departures','scan_end_place_scanned','scan_end_continue_tracks','scan_end_continue_moved','scan_end_converge_artists','scan_end_hold_replacements','scan_end_offer_held_back','scan_end_held_scopes')),
    end_pass_waits_used INTEGER NOT NULL DEFAULT 0 CHECK(end_pass_waits_used >= 0),
    end_pass_retry_at INTEGER,
    inventory_cleanup_pending INTEGER NOT NULL DEFAULT 0
        CHECK(inventory_cleanup_pending IN (0,1)),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0 CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
) STRICT;

CREATE TABLE library_scan_unjudged_tracks (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    held_at INTEGER NOT NULL
) STRICT;

CREATE TABLE library_track_hand_matches (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    release_mbid TEXT NOT NULL,
    release_track_mbid TEXT NOT NULL,
    prior_identity_json TEXT,
    matched_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    matched_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "library_track_management_identity" (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    applied_album_identity_revision INTEGER,
    applied_track_identity_revision INTEGER
) STRICT;

CREATE TABLE "library_track_management_state" (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE RESTRICT,
    baseline_id TEXT REFERENCES library_management_baselines(id) ON DELETE RESTRICT,
    applied_profile_id TEXT,
    applied_profile_revision TEXT,
    applied_projection_hash TEXT,
    applied_naming_script_revision TEXT,
    applied_override_revision TEXT,
    last_operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    managed_root_id TEXT,
    managed_path_revision TEXT,
    last_managed_at INTEGER,
    last_outcome TEXT,
    last_reason_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
, written_sha256 TEXT) STRICT;

CREATE TABLE "library_work_control" (
    queue_kind TEXT PRIMARY KEY CHECK(queue_kind = 'identification'),
    state TEXT NOT NULL CHECK(state IN ('running','paused')),
    requested_at INTEGER,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    high_priority_claim_count INTEGER NOT NULL DEFAULT 0 CHECK(high_priority_claim_count >= 0),
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE "local_album_aliases" (
    alias TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_release_group','merged_album','compat_migration')),
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "local_album_artists" (
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

CREATE TABLE "local_album_artwork" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    cover_url TEXT,
    source TEXT NOT NULL CHECK(source IN ('embedded','folder','manual')),
    source_locator TEXT,
    content_hash TEXT,
    version INTEGER NOT NULL DEFAULT 1 CHECK(version BETWEEN 1 AND 9223372036854775807),
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE "local_album_artwork_checks" (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    signature TEXT NOT NULL,
    checked_at INTEGER NOT NULL,
    art_version INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE "local_album_external_identities" (
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

CREATE TABLE local_album_lyrics_checks (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE CASCADE,
    signature TEXT NOT NULL,
    checked_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "local_albums" (
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

CREATE TABLE "local_artist_aliases" (
    alias TEXT PRIMARY KEY,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    kind TEXT NOT NULL CHECK(kind IN ('legacy_artist','merged_artist','compat_migration')),
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "local_artist_external_identities" (
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

CREATE TABLE "local_artist_merge_candidates" (
    id TEXT PRIMARY KEY,
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'open' CHECK(state IN ('open','resolved','dismissed')),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(left_artist_id != right_artist_id),
    UNIQUE(left_artist_id, right_artist_id, reason_code)
) STRICT;

CREATE TABLE "local_artists" (
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

CREATE TABLE "local_entity_source_links" (
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

CREATE TABLE local_track_aliases (
    alias TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL, moves_overrides INTEGER NOT NULL DEFAULT 1 CHECK (moves_overrides IN (0, 1)),
    CHECK (alias <> local_track_id)
) STRICT;

CREATE TABLE "local_track_artists" (
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

CREATE TABLE "local_track_edition_tags" (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    media TEXT,
    barcode TEXT,
    catalog_number TEXT,
    release_country TEXT,
    total_discs INTEGER,
    total_tracks INTEGER,
    release_date TEXT) STRICT;

CREATE TABLE "local_track_external_identities" (
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

CREATE TABLE "local_track_genres" (
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

CREATE TABLE local_track_lyrics (
    local_track_id TEXT PRIMARY KEY REFERENCES local_tracks(id) ON DELETE CASCADE,
    source TEXT NOT NULL CHECK (source IN ('sidecar', 'tag'))
) STRICT;

-- search_key keys the full-text index (local_tracks_fts). It is an ordinary
-- column, so a rebuild copies it with the row and never renumbers it: a
-- renumbered key makes a search name the wrong tracks.
CREATE TABLE "local_tracks" (
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

CREATE VIRTUAL TABLE local_tracks_fts USING fts5(
    title_folded,
    artist_name_folded,
    album_title_folded,
    content='local_tracks',
    content_rowid='search_key',
    tokenize='trigram'
);

-- Tables: plugins

CREATE TABLE plugin_library_writes (
    bundle_id  TEXT    PRIMARY KEY,
    plugin     TEXT    NOT NULL,
    bytes      INTEGER NOT NULL CHECK (bytes >= 0),
    written_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "plugin_tick_state" (
    plugin TEXT NOT NULL,
    key TEXT NOT NULL,
    value BLOB NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (plugin, key)
) STRICT;

-- Tables: media

CREATE TABLE "library_play_history" (
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
    played_at INTEGER NOT NULL
) STRICT;

CREATE TABLE media_navidrome_folder_preferences (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('all', 'selected')),
    selected_ids_json TEXT NOT NULL DEFAULT '[]',
    server_identity TEXT,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "user_listening_prefs" (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0 CHECK (scrobble_to_lastfm IN (0, 1)),
    scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0
        CHECK (scrobble_to_listenbrainz IN (0, 1)),
    navidrome_handles_external_scrobbles INTEGER NOT NULL DEFAULT 1
        CHECK (navidrome_handles_external_scrobbles IN (0, 1)),
    primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz'
        CHECK (primary_music_source IN ('listenbrainz', 'lastfm')),
    now_playing_visibility TEXT NOT NULL DEFAULT 'full'
        CHECK (now_playing_visibility IN ('full', 'track_hidden', 'offline')),
    updated_at INTEGER NOT NULL,
    auto_request_personal_mix INTEGER NOT NULL DEFAULT 0
        CHECK (auto_request_personal_mix IN (0, 1))
) STRICT;

-- Tables: catalog

CREATE TABLE catalog_artist_event_checks (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    last_checked_at INTEGER,
    last_status TEXT CHECK (last_status IN ('ok', 'error')),
    last_error TEXT
) STRICT;

CREATE TABLE catalog_artist_genres (
    artist_mbid TEXT NOT NULL CHECK (length(artist_mbid) > 0),
    genre_folded TEXT NOT NULL CHECK (length(genre_folded) > 0),
    genre TEXT NOT NULL,
    learned_at INTEGER NOT NULL,
    PRIMARY KEY (artist_mbid, genre_folded)
) STRICT;

CREATE TABLE catalog_artist_skiddle_ids (
    artist_mbid_lower TEXT NOT NULL,
    skiddle_artistid TEXT NOT NULL,
    PRIMARY KEY (artist_mbid_lower, skiddle_artistid)
) STRICT;

CREATE TABLE catalog_artist_skiddle_resolutions (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    resolved_at INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_artist_tm_attractions (
    artist_mbid_lower TEXT PRIMARY KEY NOT NULL,
    attraction_id TEXT,
    match_basis TEXT NOT NULL CHECK (match_basis IN ('mbid', 'exact_name', 'none')),
    resolved_at INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_cached_pages (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL
        CHECK (page IN ('discover', 'home', 'queue', 'daily_mixes', 'top_picks')),
    source TEXT NOT NULL,
    listenbrainz INTEGER NOT NULL CHECK (listenbrainz IN (0, 1)),
    lastfm INTEGER NOT NULL CHECK (lastfm IN (0, 1)),
    payload BLOB NOT NULL,
    built_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, page)
) STRICT;

CREATE TABLE catalog_discover_activity (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    feature TEXT NOT NULL CHECK (feature IN ('home', 'discover', 'queue', 'artist')),
    artist_mbid TEXT NOT NULL DEFAULT '',
    section TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    used_at INTEGER NOT NULL,
    retry_at INTEGER NOT NULL DEFAULT 0,
    serviced_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (user_id, feature, artist_mbid, section, provider)
) STRICT;

CREATE TABLE catalog_event_cities (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    city_name TEXT NOT NULL,
    country_code TEXT,
    latitude REAL NOT NULL,
    longitude REAL NOT NULL,
    radius_km REAL NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (user_id, latitude, longitude)
) STRICT;

CREATE TABLE catalog_event_seen_marks (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_genre_artwork_revisions (
    genre_folded TEXT PRIMARY KEY,
    value INTEGER NOT NULL DEFAULT 1 CHECK (value BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE catalog_ignored_releases (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    release_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    ignored_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
) STRICT;

CREATE TABLE catalog_live_event_feed (
    source TEXT NOT NULL CHECK (source IN ('ticketmaster', 'skiddle')),
    source_event_id TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    event_name TEXT NOT NULL,
    venue_name TEXT,
    city TEXT,
    region TEXT,
    country_code TEXT,
    latitude REAL,
    longitude REAL,
    start_text TEXT,
    local_date TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'scheduled'
        CHECK (status IN ('scheduled', 'cancelled', 'rescheduled')),
    ticket_url TEXT,
    match_confidence TEXT NOT NULL CHECK (match_confidence IN ('mbid', 'name')),
    discovered_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (source, source_event_id, artist_mbid_lower)
) STRICT;

CREATE TABLE catalog_mbid_resolutions (
    source_mbid_lower TEXT PRIMARY KEY,
    source_mbid TEXT NOT NULL,
    release_group_mbid TEXT
) STRICT;

CREATE TABLE catalog_new_release_seen_marks (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_page_generations (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    generation INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_playlist_covers (
    playlist_id TEXT PRIMARY KEY REFERENCES catalog_playlists(id) ON DELETE CASCADE,
    content_type TEXT NOT NULL,
    image BLOB NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE catalog_playlist_tracks (
    id TEXT PRIMARY KEY,
    playlist_id TEXT NOT NULL REFERENCES catalog_playlists(id) ON DELETE CASCADE,
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
    duration_ms INTEGER,
    created_at INTEGER NOT NULL,
    plex_rating_key TEXT,
    library_file_id TEXT, recording_mbid TEXT,
    UNIQUE(playlist_id, position)
) STRICT;

CREATE TABLE catalog_playlists (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    source_ref TEXT,
    user_id TEXT REFERENCES auth_users(id) ON DELETE CASCADE,
    is_public INTEGER NOT NULL DEFAULT 0 CHECK (is_public IN (0, 1))
) STRICT;

CREATE TABLE catalog_track_format_stats (
    file_format TEXT PRIMARY KEY,
    indexed_tracks INTEGER NOT NULL DEFAULT 0 CHECK (indexed_tracks >= 0),
    indexed_bytes INTEGER NOT NULL DEFAULT 0 CHECK (indexed_bytes >= 0)
) STRICT;

CREATE TABLE catalog_user_favorite_names (
    user_id TEXT NOT NULL,
    item_kind TEXT NOT NULL,
    item_id TEXT NOT NULL,
    display_name TEXT NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id),
    FOREIGN KEY (user_id, item_kind, item_id)
        REFERENCES catalog_user_favorites(user_id, item_kind, item_id) ON DELETE CASCADE
) STRICT;

CREATE TABLE catalog_user_favorites (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('artist', 'album', 'track')),
    item_id TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id)
) STRICT;

CREATE TABLE "catalog_user_play_marks" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE CASCADE,
    marked_played_at INTEGER,
    marked_unplayed_at INTEGER,
    PRIMARY KEY (user_id, local_track_id),
    CHECK (marked_played_at IS NOT NULL OR marked_unplayed_at IS NOT NULL)
) STRICT;

CREATE TABLE "catalog_user_ratings" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    item_kind TEXT NOT NULL CHECK (item_kind IN ('artist', 'album', 'track')),
    item_id TEXT NOT NULL,
    rating INTEGER NOT NULL CHECK (rating BETWEEN 1 AND 5),
    rated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id)
) STRICT;

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

CREATE TABLE catalog_youtube_track_links (
    album_id TEXT NOT NULL,
    track_number INTEGER NOT NULL,
    disc_number INTEGER NOT NULL DEFAULT 1,
    album_name TEXT NOT NULL,
    track_name TEXT NOT NULL,
    video_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    embed_url TEXT NOT NULL,
    created_at INTEGER NOT NULL, recording_mbid TEXT,
    PRIMARY KEY (album_id, disc_number, track_number)
) STRICT;

CREATE TABLE library_artist_album_credits (
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    indexed_tracks INTEGER NOT NULL CHECK (indexed_tracks > 0),
    live INTEGER NOT NULL CHECK (live IN (0, 1)),
    appearance INTEGER NOT NULL CHECK (appearance IN (0, 1)),
    PRIMARY KEY (local_artist_id, local_album_id)
) STRICT, WITHOUT ROWID;

CREATE TABLE library_artist_counts (
    local_artist_id TEXT PRIMARY KEY REFERENCES local_artists(id) ON DELETE CASCADE,
    folded_name TEXT NOT NULL,
    album_count INTEGER NOT NULL CHECK (album_count >= 0),
    appearance_count INTEGER NOT NULL CHECK (appearance_count >= 0),
    album_count_neg INTEGER GENERATED ALWAYS AS (-album_count) STORED,
    appearance_count_neg INTEGER GENERATED ALWAYS AS (-appearance_count) STORED
) STRICT, WITHOUT ROWID;

CREATE TABLE library_user_album_plays (
    user_id TEXT NOT NULL,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    plays INTEGER NOT NULL CHECK (plays > 0),
    last_played_at INTEGER,
    played INTEGER NOT NULL CHECK (played IN (0, 1)),
    PRIMARY KEY (user_id, local_album_id)
) STRICT, WITHOUT ROWID;

-- Tables: acquire

CREATE TABLE acquire_discovery_batch_items (
    batch_id TEXT NOT NULL REFERENCES acquire_discovery_batches(id) ON DELETE CASCADE,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL DEFAULT '',
    album_name TEXT NOT NULL DEFAULT '',
    artist_name TEXT NOT NULL DEFAULT '',
    outcome TEXT NOT NULL CHECK (outcome IN
        ('requested', 'skipped_in_library', 'skipped_duplicate', 'not_requested')),
    added_at INTEGER NOT NULL,
    PRIMARY KEY (batch_id, release_group_mbid)
) STRICT;

CREATE TABLE acquire_discovery_batches (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    source_section TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "acquire_edition_acquires" (
    release_group_mbid_lower TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE,
    started_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "acquire_follow_cursors" (
    artist_mbid_lower TEXT PRIMARY KEY,
    baselined INTEGER NOT NULL DEFAULT 0 CHECK(baselined IN (0,1)),
    cursor_date TEXT,
    next_poll_at INTEGER NOT NULL,
    pending TEXT NOT NULL DEFAULT '[]'
) STRICT;

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

CREATE TABLE acquire_held_entries (
    id INTEGER PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK (origin IN ('download', 'upload')),
    group_key TEXT NOT NULL,
    release_group_mbid TEXT,
    recording_mbid TEXT,
    drop_item_id INTEGER REFERENCES drop_import_items(id) ON DELETE CASCADE,
    artist_name TEXT,
    album_title TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'placing', 'resolved', 'expired')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    resolved_at INTEGER
, retry_tries INTEGER NOT NULL DEFAULT 0, retry_at INTEGER) STRICT;

CREATE TABLE "acquire_held_files" (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES acquire_held_entries(id) ON DELETE CASCADE,
    root TEXT NOT NULL CHECK (root IN ('held', 'drop')),
    path TEXT NOT NULL,
    original_name TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    disc_number INTEGER,
    track_number INTEGER,
    track_title TEXT,
    file_format TEXT,
    duration_seconds REAL,
    expected_duration_seconds REAL,
    evidence_title TEXT,
    evidence_artist TEXT,
    evidence_score REAL,
    source TEXT NOT NULL,
    source_task_id TEXT,
    reason TEXT NOT NULL CHECK (reason IN (
        'weak_match', 'no_artist_evidence', 'mostly_unmatched', 'tag_mismatch',
        'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only',
        'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours',
        'owned_elsewhere', 'owned_on_album', 'album_needs_check',
        'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition',
        'samples', 'too_many_files', 'interrupted', 'import_failed',
        'held_by_earlier_version', 'format_not_listed', 'target_occupied', 'upgrade_blocked',
        'library_changed', 'library_unavailable', 'naming_script_failed',
        'library_folder_missing', 'library_off', 'local_fault', 'files_missing', 'files_unlisted',
        'outside_downloads_folder', 'size_mismatch', 'corrupt', 'no_audio', 'sample',
        'wrong_edition', 'not_pinned_edition', 'wrong_album', 'quality_rejected',
        'not_an_upgrade', 'wrong_track', 'stalled', 'queue_timeout', 'queue_gave_way',
        'never_started',
        'deadline', 'source_failed', 'no_source', 'download_failed',
        'album_lookup_unavailable', 'recording_unknown', 'no_album_for_recording',
        'no_album_folder', 'no_album_release', 'album_releases_exhausted',
        'track_not_found', 'nothing_allowed', 'no_download_source', 'settings_unreadable',
        'journal_unreadable', 'musicbrainz_unavailable', 'acoustid_unavailable',
        'held_for_review',
        'tracks_missing', 'already_owned', 'cancelled', 'moved_on')),
    reason_detail TEXT,
    state TEXT NOT NULL CHECK (state IN ('held', 'imported', 'already_owned', 'discarded')),
    created_at INTEGER NOT NULL,
    resolved_at INTEGER
) STRICT;

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

CREATE TABLE "artist_known_releases" (
    artist_mbid_lower TEXT NOT NULL,
    rg_mbid_lower TEXT NOT NULL,
    auto_policy_revision INTEGER,
    PRIMARY KEY (artist_mbid_lower, rg_mbid_lower)
) STRICT;

CREATE TABLE "auto_download_approvals" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','approved','rejected','revoked')),
    requested_at INTEGER NOT NULL,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER,
    batch_id TEXT,
    source TEXT,
    PRIMARY KEY (user_id, artist_mbid_lower)
) STRICT;

CREATE TABLE "download_activity_global_revision" (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE "download_activity_user_revisions" (
    user_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0
) STRICT;

CREATE TABLE "download_attempts" (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source IN ('soulseek','usenet') OR source LIKE 'plugin:%'),
    candidate_index INTEGER NOT NULL CHECK(candidate_index >= 0),
    job_name TEXT NOT NULL DEFAULT '',
    handle_json TEXT NOT NULL,
    remote_storage TEXT,
    mount_root TEXT,
    workspace_path TEXT,
    materialized_paths_json TEXT NOT NULL DEFAULT '[]',
    materialized_fingerprints_json TEXT NOT NULL DEFAULT '{}',
    publisher_bundle_ids_json TEXT NOT NULL DEFAULT '[]',
    legacy_reconciled INTEGER NOT NULL DEFAULT 0 CHECK(legacy_reconciled IN (0,1)),
    state TEXT NOT NULL CHECK(state IN (
        'acquiring','in_use','cleanup_pending','workspace_removed','complete',
        'preserved','needs_attention'
    )),
    disposition TEXT NOT NULL DEFAULT 'undecided'
        CHECK(disposition IN ('undecided','discard','preserve')),
    cleanup_failures INTEGER NOT NULL DEFAULT 0 CHECK(cleanup_failures >= 0),
    next_retry_at INTEGER NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at INTEGER,
    error_code TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    completed_at INTEGER,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
) STRICT;

CREATE TABLE "download_idempotency_keys" (
    key TEXT PRIMARY KEY,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE
        DEFERRABLE INITIALLY DEFERRED,
    operation TEXT NOT NULL,
    created_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "download_import_decisions" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES download_tasks(id) ON DELETE CASCADE,
    attempt_id TEXT REFERENCES download_attempts(id) ON DELETE SET NULL,
    outcome TEXT NOT NULL
        CHECK(outcome IN ('imported','partial','held','rejected','deferred')),
    reason_code TEXT,
    detail TEXT,
    release_mbid TEXT,
    distance REAL,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_imported INTEGER NOT NULL DEFAULT 0,
    files_held INTEGER NOT NULL DEFAULT 0,
    checks_json TEXT NOT NULL DEFAULT '[]',
    missing_positions TEXT NOT NULL DEFAULT '[]',
    decided_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "download_quarantine" (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL DEFAULT 'soulseek',
    identity TEXT NOT NULL,
    release_group_mbid TEXT,
    reason TEXT NOT NULL
        CHECK(reason IN ('verify_failed','corrupt','fingerprint_mismatch',
                         'duration_mismatch','download_failed','manual')),
    quarantined_at INTEGER NOT NULL,
    UNIQUE (source, identity, release_group_mbid)
) STRICT;

CREATE TABLE "download_tasks" (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    request_history_mbid TEXT,
    download_type TEXT NOT NULL DEFAULT 'album',
    release_group_mbid TEXT NOT NULL,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    artist_mbid TEXT,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    track_title TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    year INTEGER,
    track_count INTEGER,
    track_duration_seconds REAL,
    download_client TEXT NOT NULL DEFAULT 'slskd',
    source TEXT NOT NULL DEFAULT 'soulseek',
    origin TEXT NOT NULL DEFAULT 'user'
        CHECK(origin IN ('user','approval','retry','wanted','upgrade','edition','follow')),
    source_username TEXT,
    source_directory TEXT,
    search_query TEXT,
    search_job_id TEXT,
    candidate_index INTEGER,
    status TEXT NOT NULL DEFAULT 'queued'
        CHECK(status IN ('queued','downloading','processing',
                         'completed','partial','failed','cancelled')),
    preflight_score REAL,
    progress_percent INTEGER NOT NULL DEFAULT 0,
    total_size_bytes INTEGER,
    downloaded_bytes INTEGER NOT NULL DEFAULT 0,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_completed INTEGER NOT NULL DEFAULT 0,
    files_failed INTEGER NOT NULL DEFAULT 0,
    quality_format TEXT,
    quality_bitrate INTEGER,
    quality_sample_rate INTEGER,
    quality_bit_depth INTEGER,
    advertised_queue_depth INTEGER,
    queue_position_start INTEGER,
    queue_position_end INTEGER,
    remote_queued INTEGER NOT NULL DEFAULT 0 CHECK (remote_queued IN (0, 1)),
    preferred_quality_fallback_at INTEGER,
    quality_pool_key TEXT,
    attempt_number INTEGER NOT NULL DEFAULT 0,
    attempt_total INTEGER NOT NULL DEFAULT 0,
    has_next_source INTEGER NOT NULL DEFAULT 0 CHECK (has_next_source IN (0, 1)),
    quality_snapshot_json TEXT,
    quality_preference_step INTEGER,
    quality_certainty TEXT,
    quality_provenance TEXT,
    manual_quality_override INTEGER NOT NULL DEFAULT 0
        CHECK (manual_quality_override IN (0, 1)),
    staging_path TEXT,
    final_path TEXT,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_polled_at INTEGER,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    completed_at INTEGER,
    cancelled_at INTEGER,
    updated_at INTEGER NOT NULL,
    wrong_product_verdict_at INTEGER,
    wrong_product_detail TEXT,
    outcome_code TEXT,
    plan_json TEXT NOT NULL DEFAULT '{}',
    picked_by_person INTEGER NOT NULL DEFAULT 0 CHECK (picked_by_person IN (0, 1))
) STRICT;

CREATE TABLE drop_import_items (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    job_id TEXT NOT NULL
        REFERENCES drop_import_jobs(id) ON DELETE CASCADE,
    folder_name TEXT NOT NULL,
    status TEXT NOT NULL
        CHECK(status IN ('processing','imported','skipped',
                         'needs_review','failed','discarded')),
    release_group_mbid TEXT,
    album_title TEXT,
    artist_name TEXT,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_imported INTEGER NOT NULL DEFAULT 0,
    detail TEXT,
    staging_paths TEXT NOT NULL DEFAULT '[]',
    updated_at INTEGER NOT NULL,
    reason_code TEXT
) STRICT;

CREATE TABLE "drop_import_jobs" (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    user_name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL
        CHECK(status IN ('processing','completed','failed')),
    created_at INTEGER NOT NULL,
    upload_name TEXT NOT NULL,
    staging_dir TEXT NOT NULL,
    error TEXT
) STRICT;

CREATE TABLE "personal_mix_approvals" (
    user_id TEXT NOT NULL PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending'
        CHECK(state IN ('pending','approved','rejected','revoked')),
    requested_at INTEGER NOT NULL,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER
) STRICT;

CREATE TABLE "request_history" (
    musicbrainz_id_lower TEXT PRIMARY KEY,
    musicbrainz_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    requested_at INTEGER NOT NULL,
    completed_at INTEGER,
    status TEXT NOT NULL CHECK(status IN ('pending','queued','downloading',
        'awaiting_approval','cancelling','cancelled','failed','imported',
        'incomplete','rejected')),
    monitor_artist INTEGER NOT NULL DEFAULT 0,
    auto_download_artist INTEGER NOT NULL DEFAULT 0,
    user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    requested_by_name TEXT,
    reviewed_by_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    reviewed_by_name TEXT,
    reviewed_at INTEGER,
    download_task_id TEXT,
    release_mbid TEXT,
    request_kind TEXT NOT NULL DEFAULT 'album' CHECK(request_kind IN ('album','track')),
    origin TEXT NOT NULL DEFAULT 'user' CHECK(origin IN ('user','approval','retry',
        'wanted','upgrade','edition','follow')),
    track_title TEXT,
    duration_seconds INTEGER,
    track_release_group_mbid TEXT,
    dispatch_authorized INTEGER NOT NULL DEFAULT 0,
    generation INTEGER NOT NULL DEFAULT 1
, watch_removed_generation INTEGER) STRICT;

CREATE TABLE "request_history_dismissals" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    musicbrainz_id_lower TEXT NOT NULL
        REFERENCES request_history(musicbrainz_id_lower) ON DELETE CASCADE,
    dismissed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, musicbrainz_id_lower)
) STRICT;

CREATE TABLE "request_history_requesters" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    musicbrainz_id_lower TEXT NOT NULL
        REFERENCES request_history(musicbrainz_id_lower) ON DELETE CASCADE,
    requested_at INTEGER NOT NULL,
    requested_by_name TEXT,
    PRIMARY KEY (user_id, musicbrainz_id_lower)
) STRICT;

CREATE TABLE "search_jobs" (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    year INTEGER,
    track_count INTEGER,
    release_group_mbid TEXT,
    artist_mbid TEXT,
    search_query TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'searching'
        CHECK(status IN ('searching','matched','completed','failed','cancelled')),
    candidates_blob TEXT NOT NULL DEFAULT '[]',
    error_message TEXT,
    quality_snapshot_json TEXT,
    created_at INTEGER NOT NULL,
    completed_at INTEGER,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE TABLE "spotify_auth_requests" (
    state        TEXT PRIMARY KEY,
    user_id      TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    redirect_uri TEXT NOT NULL,
    expires_at   INTEGER NOT NULL
) STRICT;

CREATE TABLE "user_quotas" (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    request_quota_count INTEGER,
    request_quota_days INTEGER,
    storage_quota_gb INTEGER
) STRICT;

CREATE TABLE "wanted_seen_candidates" (
    release_group_mbid_lower TEXT NOT NULL
        REFERENCES wanted_watches(release_group_mbid_lower) ON DELETE CASCADE,
    source TEXT NOT NULL,
    identity TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL,
    PRIMARY KEY (release_group_mbid_lower, source, identity)
) STRICT;

CREATE TABLE "wanted_watches" (
    release_group_mbid_lower TEXT PRIMARY KEY,
    release_group_mbid TEXT NOT NULL,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('missing','partial')),
    state TEXT NOT NULL DEFAULT 'watching' CHECK(state IN ('watching','stopped','fulfilled')),
    created_at INTEGER NOT NULL,
    first_release_date TEXT,
    check_count INTEGER NOT NULL DEFAULT 0,
    quiet_streak INTEGER NOT NULL DEFAULT 0,
    last_checked_at INTEGER,
    next_check_at INTEGER NOT NULL,
    last_outcome TEXT,
    new_candidate_count INTEGER NOT NULL DEFAULT 0
) STRICT;

-- Tables: compat

CREATE TABLE "compat_bookmarks" (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, file_id)
) STRICT;

-- One column names rows of several tables and an alias outlives its row,
-- so there is no foreign key. Rows are only added, never deleted.
CREATE TABLE compat_jellyfin_ids (
    jellyfin_id TEXT NOT NULL PRIMARY KEY
        CHECK (length(jellyfin_id) = 32 AND jellyfin_id NOT GLOB '*[^0-9a-f]*'),
    kind TEXT NOT NULL
        CHECK (kind IN ('album', 'artist', 'track', 'genre', 'playlist', 'library')),
    internal_id TEXT NOT NULL,
    current INTEGER NOT NULL CHECK (current IN (0, 1))
) STRICT, WITHOUT ROWID;

CREATE TABLE "compat_play_queue_items" (
    user_id TEXT NOT NULL REFERENCES compat_play_queues(user_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    PRIMARY KEY (user_id, item_index)
) STRICT;

CREATE TABLE "compat_play_queues" (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    updated_at INTEGER NOT NULL,
    changed_by_client TEXT NOT NULL DEFAULT ''
) STRICT;

CREATE TABLE compat_user_settings (
    user_id TEXT PRIMARY KEY NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    include_public_playlists INTEGER NOT NULL CHECK (include_public_playlists IN (0, 1))
) STRICT;

-- Tables: app

CREATE TABLE app_section_prefs (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    page TEXT NOT NULL,
    section_key TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (user_id, page, section_key)
) STRICT;

-- Tables: tool

CREATE TABLE "import_pending_links" (
    target_table TEXT NOT NULL,
    target_key TEXT NOT NULL,
    ref_column TEXT NOT NULL,
    ref_kind TEXT NOT NULL
        CHECK (ref_kind IN ('track', 'album', 'artist', 'tombstone')),
    v2_id TEXT NOT NULL,
    mode TEXT NOT NULL
        CHECK (mode IN ('as_written', 'until_resolved', 'link_only')),
    PRIMARY KEY (target_table, target_key, ref_column)
) STRICT;

CREATE TABLE "import_progress" (
    instance_id TEXT NOT NULL,
    section TEXT NOT NULL,
    export_digest TEXT NOT NULL,
    rows_written INTEGER NOT NULL,
    applied_at INTEGER NOT NULL,
    PRIMARY KEY (instance_id, section)
) STRICT;

CREATE TABLE "import_runs" (
    id TEXT PRIMARY KEY NOT NULL,
    instance_id TEXT NOT NULL,
    exported_at INTEGER NOT NULL,
    exit_code TEXT NOT NULL CHECK (exit_code IN ('OK', 'OK_WITH_DROPS', 'FAILED_VALIDATION',
        'ENVELOPE_AUTH_FAILED', 'FAILED_INTERNAL')),
    entity_counts TEXT NOT NULL,
    applied_at INTEGER NOT NULL
) STRICT;

-- Seed rows: library

INSERT INTO library_catalog_revision (singleton, value) VALUES
    (1, 1);

INSERT INTO library_event_stream_revisions (stream_kind, value) VALUES
    ('scan', 0),
    ('identification', 0),
    ('operation', 0);

INSERT INTO library_repair_watermarks (singleton, identified_at, identified_job_id) VALUES
    (1, 0, '');

INSERT INTO library_scan_fences (singleton, revision) VALUES
    (1, 0);

INSERT INTO library_work_control (queue_kind, state, requested_at, requested_by_user_id, high_priority_claim_count, row_revision) VALUES
    ('identification', 'running', NULL, NULL, 0, 1);

INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, kind, retired_into_artist_id, created_at, updated_at, row_revision, converged_mbid) VALUES
    ('00000000-0000-4000-8000-000000000001', 'Various Artists', NULL, 'various artists', 'various artists', 'various_artists', NULL, 0, 0, 1, NULL),
    ('00000000-0000-4000-8000-000000000002', 'Unknown Artist', NULL, 'unknown artist', 'unknown artist', 'unknown', NULL, 0, 0, 1, NULL);

-- Seed rows: catalog

INSERT INTO catalog_page_generations (singleton, generation) VALUES
    (1, 0);

-- Seed rows: acquire

INSERT INTO download_activity_global_revision (singleton, revision) VALUES
    (1, 0);

-- AUTOINCREMENT counters. A rebuild of one of these tables carries its
-- counter, so an id the API handed out is never given to a new row.

INSERT INTO sqlite_sequence (name, seq) VALUES
    ('download_import_decisions', 0),
    ('download_quarantine', 0),
    ('drop_import_items', 0);

-- Indexes: kernel

CREATE INDEX durable_jobs_by_key ON durable_jobs (kind, idempotency_key, id);

CREATE INDEX durable_jobs_claim ON durable_jobs (kind, state, not_before_at, id);

CREATE UNIQUE INDEX durable_jobs_one_queued ON durable_jobs (kind, idempotency_key)
    WHERE state = 'queued';

-- Indexes: auth

CREATE INDEX idx_auth_app_passwords_user
    ON auth_app_passwords(user_id);

CREATE INDEX idx_auth_password_recovery_expires
    ON auth_password_recovery_codes(expires_at);

CREATE INDEX idx_auth_providers_user
    ON auth_providers(user_id);

CREATE INDEX idx_auth_tokens_expires
    ON auth_tokens(expires_at);

CREATE INDEX idx_auth_tokens_user
    ON auth_tokens(user_id);

CREATE UNIQUE INDEX idx_auth_users_username
    ON auth_users(username) WHERE username IS NOT NULL;

-- Indexes: library

CREATE INDEX idx_library_album_departures_album
    ON library_album_departures(previous_album_id);

CREATE INDEX idx_library_album_looks_wrong_user ON library_album_looks_wrong(user_id);

CREATE INDEX idx_album_match_state_state
    ON library_album_match_state(state, updated_at DESC);

CREATE INDEX idx_artist_credit_proof_album
    ON library_artist_credit_proofs(local_album_id, subject_kind, subject_id);

CREATE INDEX idx_artist_credit_proof_mbid
    ON library_artist_credit_proofs(artist_mbid, source_local_artist_id);

CREATE INDEX idx_artist_credit_proof_resolved
    ON library_artist_credit_proofs(local_artist_id, artist_mbid);

CREATE INDEX idx_artist_credit_proof_source
    ON library_artist_credit_proofs(source_local_artist_id, artist_mbid);

CREATE INDEX idx_artist_credit_proof_track
    ON library_artist_credit_proofs(local_track_id);

CREATE INDEX idx_artist_reconciliation_dismissal_user
ON library_artist_reconciliation_dismissals(dismissed_by_user_id, updated_at DESC);

CREATE INDEX idx_library_artist_reconciliation_dismissals_right_artist_id
    ON library_artist_reconciliation_dismissals(right_artist_id);

CREATE INDEX idx_artist_reconciliation_state_status
    ON library_artist_reconciliation_state(state);

CREATE INDEX idx_library_automatic_edition_undo_job_id
    ON library_automatic_edition_undo(job_id);

CREATE INDEX idx_library_catalog_actions_actor_user_id
    ON library_catalog_actions(actor_user_id);

CREATE INDEX idx_library_catalog_actions_album_kind
    ON library_catalog_actions(local_album_id, action_kind);

CREATE INDEX idx_library_catalog_actions_local_artist_id
    ON library_catalog_actions(local_artist_id);

CREATE INDEX idx_library_catalog_actions_local_track_id
    ON library_catalog_actions(local_track_id);

CREATE INDEX idx_library_catalog_actions_operation_job_id
    ON library_catalog_actions(operation_job_id);

CREATE INDEX idx_library_catalog_actions_undoes
    ON library_catalog_actions(json_extract(before_json, '$.undoes'))
    WHERE action_kind = 'files_put_back';

CREATE INDEX idx_contribution_callback_expiry
ON library_contribution_callback_tokens(expires_at, consumed_at);

CREATE INDEX idx_library_contribution_callback_tokens_contribution_id
    ON library_contribution_callback_tokens(contribution_id);

CREATE INDEX idx_library_contribution_callback_tokens_requested_by_user_id
    ON library_contribution_callback_tokens(requested_by_user_id);

CREATE UNIQUE INDEX idx_contribution_active_album
ON library_contribution_drafts(local_album_id)
WHERE state NOT IN ('linked', 'cancelled', 'stale');

CREATE INDEX idx_contribution_album_updated
ON library_contribution_drafts(local_album_id, updated_at DESC);

CREATE INDEX idx_library_contribution_drafts_created_by_user_id
    ON library_contribution_drafts(created_by_user_id);

CREATE INDEX idx_library_contribution_drafts_updated_by_user_id
    ON library_contribution_drafts(updated_by_user_id);

CREATE INDEX idx_custom_edition_album
ON library_custom_edition_manifests(local_album_id, version DESC);

CREATE INDEX idx_library_custom_edition_manifests_sealed_by_user_id
    ON library_custom_edition_manifests(sealed_by_user_id);

CREATE INDEX idx_custom_edition_track_local
ON library_custom_edition_tracks(local_track_id);

CREATE INDEX idx_library_edition_remap_queue_chosen_by_user_id
    ON library_edition_remap_queue(chosen_by_user_id);

CREATE INDEX idx_identification_attempt_subject_album
    ON library_identification_attempts(local_album_id, completed_at);

CREATE INDEX idx_identification_attempt_subject_track
    ON library_identification_attempts(local_track_id, completed_at);

CREATE INDEX idx_library_identification_attempts_requested_by_user_id
    ON library_identification_attempts(requested_by_user_id);

CREATE INDEX idx_identify_artist_id_checks_canonical
    ON library_identify_artist_id_checks(canonical_mbid) WHERE canonical_mbid IS NOT NULL;

CREATE INDEX idx_identify_artist_id_checks_state
    ON library_identify_artist_id_checks(state, nominated_at);

CREATE INDEX idx_identify_jobs_album
    ON library_identify_jobs(local_album_id);

CREATE INDEX idx_identify_jobs_claim
    ON library_identify_jobs(state, priority, not_before_at);

CREATE UNIQUE INDEX idx_identify_jobs_live_key
    ON library_identify_jobs(idempotency_key)
    WHERE state IN ('queued','running','deferred');

CREATE INDEX idx_library_identify_jobs_requested_by_user_id
    ON library_identify_jobs(requested_by_user_id);

CREATE INDEX idx_identify_reviews_album
    ON library_identify_reviews(local_album_id, state);

CREATE INDEX idx_identify_reviews_state
    ON library_identify_reviews(state, updated_at DESC);

CREATE INDEX idx_library_identify_reviews_resolved_by_user_id
    ON library_identify_reviews(resolved_by_user_id);

CREATE INDEX idx_repair_findings_album
    ON library_identity_repair_findings(local_album_id);

CREATE INDEX idx_repair_findings_cursor
    ON library_identity_repair_findings(job_id, finding_code, updated_at DESC, id DESC);

CREATE INDEX idx_library_management_automatic_queue_due
    ON library_management_automatic_queue(not_before_at);

CREATE INDEX idx_library_management_baseline_purges_actor_user_id
    ON library_management_baseline_purges(actor_user_id);

CREATE INDEX idx_library_management_baselines_semantic_snapshot_blob_sha256
    ON library_management_baselines(semantic_snapshot_blob_sha256);

CREATE INDEX idx_management_baseline_track
ON library_management_baselines(local_track_id);

CREATE INDEX idx_library_management_exclusions_excluded_by_user_id
    ON library_management_exclusions(excluded_by_user_id);

CREATE INDEX idx_library_management_job_snapshots_linked_operation_job_id
    ON library_management_job_snapshots(linked_operation_job_id);

CREATE UNIQUE INDEX idx_management_apply_idempotency
ON library_management_job_snapshots(apply_idempotency_key)
WHERE apply_idempotency_key IS NOT NULL;

CREATE INDEX idx_management_metadata_expiry
ON library_management_metadata_snapshots(expires_at, id);

CREATE INDEX idx_management_metadata_lookup
ON library_management_metadata_snapshots(provider, entity_kind, entity_id, input_hash);

CREATE INDEX idx_library_management_overrides_actor_user_id
    ON library_management_overrides(actor_user_id);

CREATE INDEX idx_library_management_overrides_local_album_id
    ON library_management_overrides(local_album_id);

CREATE INDEX idx_library_management_overrides_local_track_id
    ON library_management_overrides(local_track_id);

CREATE UNIQUE INDEX idx_management_override_album_field
ON library_management_overrides(local_album_id, field_name)
WHERE subject_kind = 'album';

CREATE UNIQUE INDEX idx_management_override_track_field
ON library_management_overrides(local_track_id, field_name)
WHERE subject_kind = 'track';

CREATE INDEX idx_library_management_plan_items_local_album_id
    ON library_management_plan_items(local_album_id);

CREATE INDEX idx_library_management_plan_items_local_track_id
    ON library_management_plan_items(local_track_id);

CREATE INDEX idx_management_plan_cursor
ON library_management_plan_items(job_id, ordinal);

CREATE INDEX idx_management_plan_destination
    ON library_management_plan_items(
        job_id, destination_root_id, destination_collision_key, ordinal);

CREATE INDEX idx_management_plan_eligibility
ON library_management_plan_items(job_id, eligibility, ordinal);

CREATE INDEX idx_management_plan_track
ON library_management_plan_items(job_id, local_track_id);

CREATE INDEX idx_library_operation_control_idempotency_job_id
    ON library_operation_control_idempotency(job_id);

CREATE INDEX idx_library_operation_jobs_requested_by_user_id
    ON library_operation_jobs(requested_by_user_id);

CREATE INDEX idx_operation_jobs_claim ON library_operation_jobs(state, created_at);

CREATE INDEX idx_operation_jobs_lease ON library_operation_jobs(state, lease_expires_at);

CREATE INDEX idx_library_operation_work_bundle_id
    ON library_operation_work(bundle_id) WHERE bundle_id IS NOT NULL;

CREATE INDEX idx_library_operation_work_local_album_id
    ON library_operation_work(local_album_id);

CREATE INDEX idx_library_operation_work_local_track_id
    ON library_operation_work(local_track_id);

CREATE INDEX idx_operation_work_claim ON library_operation_work(job_id, state, ordinal);

CREATE INDEX idx_library_publish_journal_bundle
    ON library_publish_journal(bundle_id);

CREATE INDEX idx_library_publish_journal_fence
    ON library_publish_journal(fence_revision) WHERE fence_revision IS NOT NULL;

CREATE INDEX idx_library_publish_journal_state
    ON library_publish_journal(state);

CREATE INDEX idx_library_publish_journal_track
    ON library_publish_journal(track_id) WHERE track_id IS NOT NULL;

CREATE INDEX idx_library_publish_operations_applied_by_user_id
    ON library_publish_operations(applied_by_user_id);

CREATE INDEX idx_library_publish_outputs_track ON library_publish_outputs(track_id);

CREATE INDEX idx_library_publish_replacements_due
    ON library_publish_replacements(recycled_at, journal_id) WHERE expired_at IS NULL;

CREATE INDEX idx_library_publish_replacements_track
    ON library_publish_replacements(replaced_track_id)
    WHERE replaced_track_id IS NOT NULL;

CREATE INDEX idx_library_publish_resolutions_resolved_by_user_id
    ON library_publish_resolutions(resolved_by_user_id);

CREATE INDEX idx_library_publish_snapshots_bundle
    ON library_publish_snapshots(bundle_id);

CREATE INDEX idx_library_recycle_bin_empties_actor_user_id
    ON library_recycle_bin_empties(actor_user_id);

CREATE INDEX idx_library_reidentification_snapshots_local_album_id
    ON library_reidentification_snapshots(local_album_id);

CREATE INDEX idx_library_retag_pairs_new ON library_retag_pairs(new_album_id);

CREATE INDEX idx_library_review_exclusions_local_track_id
    ON library_review_exclusions(local_track_id);

CREATE INDEX idx_library_scan_album_moves_from
    ON library_scan_album_moves(run_id, previous_album_id);

CREATE INDEX idx_scan_failures_guard ON library_scan_failures(run_id, root_id)
    WHERE failure_code = 'MASS_MISSING_GUARD';

CREATE INDEX idx_scan_failures_run ON library_scan_failures(run_id);

CREATE INDEX idx_scan_inventory_management_candidates
    ON library_scan_inventory(run_id, processing_state, comparison_result, local_track_id);

CREATE INDEX idx_scan_inventory_processing
    ON library_scan_inventory(run_id, processing_state, root_id, relative_path);

CREATE INDEX idx_scan_inventory_track ON library_scan_inventory(local_track_id);

CREATE INDEX idx_library_scan_runs_requested_by_user_id
    ON library_scan_runs(requested_by_user_id);

CREATE UNIQUE INDEX idx_scan_runs_single_active
ON library_scan_runs((1))
WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping');

CREATE UNIQUE INDEX idx_scan_runs_single_queued
ON library_scan_runs((1)) WHERE state = 'queued';

CREATE INDEX idx_scan_runs_state ON library_scan_runs(state, queued_at);

CREATE INDEX idx_library_track_hand_matches_user
    ON library_track_hand_matches(matched_by_user_id);

CREATE INDEX idx_library_track_management_state_baseline_id
    ON library_track_management_state(baseline_id);

CREATE INDEX idx_library_track_management_state_last_operation_job_id
    ON library_track_management_state(last_operation_job_id);

CREATE INDEX idx_library_work_control_requested_by_user_id
    ON library_work_control(requested_by_user_id);

CREATE INDEX idx_album_alias_target ON local_album_aliases(local_album_id);

CREATE INDEX idx_local_album_artists_artist_mbid
    ON local_album_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;

CREATE INDEX idx_local_album_artists_mbid
    ON local_album_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;

CREATE INDEX idx_local_album_artists_reverse ON local_album_artists(local_artist_id, local_album_id);

CREATE INDEX idx_local_album_external_identities_attempt_id
    ON local_album_external_identities(attempt_id);

CREATE INDEX idx_local_album_external_identities_selected_by_user_id
    ON local_album_external_identities(selected_by_user_id);

CREATE INDEX idx_local_album_identity_release
    ON local_album_external_identities(release_mbid);

CREATE INDEX idx_local_album_identity_release_lower ON local_album_external_identities(lower(release_mbid));

CREATE INDEX idx_local_album_identity_rg ON local_album_external_identities(release_group_mbid);

CREATE INDEX idx_local_album_identity_rg_lower ON local_album_external_identities(lower(release_group_mbid));

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

CREATE INDEX idx_artist_alias_target ON local_artist_aliases(local_artist_id);

CREATE INDEX idx_local_artist_external_identities_attempt_id
    ON local_artist_external_identities(attempt_id);

CREATE INDEX idx_local_artist_external_identities_selected_by_user_id
    ON local_artist_external_identities(selected_by_user_id);

CREATE INDEX idx_local_artist_identity_provider_lower ON local_artist_external_identities(lower(provider_artist_id));

CREATE INDEX idx_local_artist_merge_candidates_right_artist_id
    ON local_artist_merge_candidates(right_artist_id);

CREATE INDEX idx_local_artists_folded ON local_artists(folded_name, kind);

CREATE INDEX idx_local_artists_normalized ON local_artists(normalized_name, kind);

CREATE INDEX idx_local_artists_retired ON local_artists(retired_into_artist_id);

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

CREATE INDEX idx_local_track_aliases_track ON local_track_aliases(local_track_id);

CREATE INDEX idx_local_track_artists_artist_mbid
    ON local_track_artists(local_artist_id, artist_mbid) WHERE artist_mbid IS NOT NULL;

CREATE INDEX idx_local_track_artists_mbid
    ON local_track_artists(artist_mbid, local_artist_id) WHERE artist_mbid IS NOT NULL;

CREATE INDEX idx_local_track_artists_reverse ON local_track_artists(local_artist_id, local_track_id);

CREATE INDEX idx_local_track_external_identities_attempt_id
    ON local_track_external_identities(attempt_id);

CREATE INDEX idx_local_track_identity_recording ON local_track_external_identities(recording_mbid);

CREATE INDEX idx_local_track_identity_recording_lower
    ON local_track_external_identities(lower(recording_mbid));

CREATE INDEX idx_local_track_identity_release_track
    ON local_track_external_identities(release_track_mbid);

CREATE INDEX idx_local_track_genres_folded ON local_track_genres(folded_name, local_track_id);

CREATE INDEX idx_local_track_genres_source ON local_track_genres(source, local_track_id);

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

-- Indexes: plugins

CREATE INDEX idx_plugin_library_writes_plugin
    ON plugin_library_writes(plugin, written_at);

CREATE INDEX idx_plugin_tick_state_plugin ON plugin_tick_state(plugin);

-- Indexes: media

CREATE INDEX idx_play_history_user_album_no_track
    ON library_play_history(user_id, local_album_id, played_at)
    WHERE local_track_id IS NULL AND local_album_id IS NOT NULL;

CREATE INDEX idx_play_history_user_track
    ON library_play_history(user_id, local_track_id, played_at);

CREATE INDEX idx_target_history_album ON library_play_history(local_album_id);

CREATE INDEX idx_target_history_artist ON library_play_history(local_artist_id);

CREATE INDEX idx_target_history_track ON library_play_history(local_track_id);

CREATE INDEX idx_target_history_user_played ON library_play_history(user_id, played_at DESC);

-- Indexes: catalog

CREATE INDEX idx_catalog_artist_genres_learned
    ON catalog_artist_genres(learned_at);

CREATE INDEX idx_catalog_discover_activity_due
    ON catalog_discover_activity(source, retry_at, serviced_at);

CREATE INDEX idx_catalog_discover_activity_used
    ON catalog_discover_activity(used_at);

CREATE INDEX idx_catalog_ignored_releases_ignored
    ON catalog_ignored_releases(ignored_at);

CREATE INDEX idx_catalog_live_event_feed_artist ON catalog_live_event_feed(artist_mbid_lower);

CREATE INDEX idx_catalog_live_event_feed_date ON catalog_live_event_feed(local_date);

CREATE INDEX idx_catalog_playlist_tracks_library_file
    ON catalog_playlist_tracks(library_file_id) WHERE library_file_id IS NOT NULL;

CREATE INDEX idx_catalog_playlists_user ON catalog_playlists(user_id);

CREATE UNIQUE INDEX idx_catalog_playlists_user_source_ref
    ON catalog_playlists(user_id, source_ref) WHERE source_ref IS NOT NULL;

CREATE INDEX idx_catalog_user_favorites_user_kind ON catalog_user_favorites(user_id, item_kind);

CREATE INDEX idx_catalog_user_play_marks_track ON catalog_user_play_marks(local_track_id);

CREATE INDEX idx_catalog_user_ratings_item ON catalog_user_ratings(item_kind, item_id);

CREATE INDEX idx_artist_album_credits_album ON library_artist_album_credits(local_album_id);

CREATE INDEX idx_artist_album_credits_appearances ON library_artist_album_credits(local_artist_id)
    WHERE appearance = 1;

CREATE INDEX idx_artist_counts_albums
    ON library_artist_counts(album_count, folded_name, local_artist_id);

CREATE INDEX idx_artist_counts_albums_desc
    ON library_artist_counts(album_count_neg, folded_name, local_artist_id);

CREATE INDEX idx_artist_counts_appearances
    ON library_artist_counts(appearance_count, folded_name, local_artist_id, album_count);

CREATE INDEX idx_artist_counts_appearances_desc
    ON library_artist_counts(appearance_count_neg, folded_name, local_artist_id, album_count);

CREATE INDEX idx_user_album_plays_album ON library_user_album_plays(local_album_id);

CREATE INDEX idx_user_album_plays_count
    ON library_user_album_plays(user_id, plays, local_album_id);

CREATE INDEX idx_user_album_plays_last_played
    ON library_user_album_plays(user_id, last_played_at, local_album_id);

-- Indexes: acquire

CREATE INDEX idx_acquire_discovery_batches_user
    ON acquire_discovery_batches(user_id, created_at DESC);

CREATE INDEX idx_acquire_edition_acquires_task ON acquire_edition_acquires(task_id);

CREATE INDEX idx_acquire_follow_cursors_due
    ON acquire_follow_cursors(next_poll_at);

CREATE INDEX idx_acquire_followed_artists_auto_download
    ON acquire_followed_artists(auto_download) WHERE auto_download = 1;

CREATE INDEX idx_acquire_followed_artists_mbid ON acquire_followed_artists(artist_mbid_lower);

CREATE INDEX idx_acquire_followed_artists_user ON acquire_followed_artists(user_id);

CREATE INDEX idx_acquire_held_entries_drop_item ON acquire_held_entries(drop_item_id);

CREATE INDEX idx_acquire_held_entries_expiry ON acquire_held_entries(state, expires_at);

CREATE INDEX idx_acquire_held_entries_group
    ON acquire_held_entries(release_group_mbid, state);

CREATE UNIQUE INDEX idx_acquire_held_entries_open
    ON acquire_held_entries(user_id, group_key) WHERE state IN ('held', 'placing');

CREATE INDEX idx_acquire_held_entries_retry ON acquire_held_entries(state, retry_at);

CREATE INDEX idx_acquire_held_entries_user ON acquire_held_entries(user_id, state);

CREATE INDEX idx_acquire_held_files_entry ON acquire_held_files(entry_id, state);

CREATE INDEX idx_acquire_held_files_task ON acquire_held_files(source_task_id, state);

CREATE INDEX idx_acquire_new_release_feed_artist ON acquire_new_release_feed(artist_mbid_lower);

CREATE INDEX idx_acquire_new_release_feed_date
    ON acquire_new_release_feed(first_release_date DESC);

CREATE INDEX idx_akr_auto_policy
    ON artist_known_releases(artist_mbid_lower, auto_policy_revision)
    WHERE auto_policy_revision IS NOT NULL;

CREATE INDEX idx_ada_batch ON auto_download_approvals(batch_id) WHERE batch_id IS NOT NULL;

CREATE INDEX idx_ada_pending ON auto_download_approvals(state) WHERE state = 'pending';

CREATE INDEX idx_ada_reviewed_by ON auto_download_approvals(reviewed_by_id);

CREATE INDEX idx_download_attempts_cleanup
    ON download_attempts(state, next_retry_at, lease_expires_at);

CREATE INDEX idx_download_attempts_job
    ON download_attempts(source, job_name);

CREATE INDEX idx_download_attempts_task
    ON download_attempts(task_id, candidate_index, created_at);

CREATE INDEX idx_download_idempotency_task
    ON download_idempotency_keys(task_id);

CREATE INDEX idx_download_import_decisions_attempt
    ON download_import_decisions(attempt_id);

CREATE INDEX idx_download_import_decisions_task
    ON download_import_decisions(task_id, decided_at DESC);

CREATE INDEX idx_quarantine_lookup ON download_quarantine(source, identity);

CREATE INDEX idx_quarantine_quarantined_at ON download_quarantine(quarantined_at);

CREATE INDEX idx_download_activity_landed
    ON download_tasks(release_group_mbid,
        COALESCE(completed_at, updated_at) DESC, user_id)
    WHERE status IN ('completed','partial') AND release_group_mbid != '';

CREATE INDEX idx_download_activity_owner_status
    ON download_tasks(user_id, status);

CREATE INDEX idx_download_tasks_created ON download_tasks(created_at DESC);

CREATE INDEX idx_download_tasks_rgmbid ON download_tasks(release_group_mbid);

CREATE INDEX idx_download_tasks_status ON download_tasks(status);

CREATE INDEX idx_download_tasks_type ON download_tasks(download_type);

CREATE INDEX idx_download_tasks_user ON download_tasks(user_id);

CREATE INDEX idx_download_tasks_username ON download_tasks(source_username);

CREATE INDEX idx_drop_import_items_job
    ON drop_import_items(job_id);

CREATE INDEX idx_drop_import_jobs_user
    ON drop_import_jobs(user_id, created_at);

CREATE INDEX idx_pma_pending
    ON personal_mix_approvals(state) WHERE state = 'pending';

CREATE INDEX idx_pma_reviewed_by ON personal_mix_approvals(reviewed_by_id);

CREATE INDEX idx_request_history_download_task ON request_history(download_task_id);

CREATE INDEX idx_request_history_retrying_keyset
    ON request_history(status, requested_at DESC, musicbrainz_id_lower DESC);

CREATE INDEX idx_request_history_reviewed_by ON request_history(reviewed_by_id);

CREATE INDEX idx_request_history_user ON request_history(user_id);

CREATE INDEX idx_request_history_dismissals_mbid
    ON request_history_dismissals(musicbrainz_id_lower);

CREATE INDEX idx_request_history_requesters_mbid
    ON request_history_requesters(musicbrainz_id_lower);

CREATE INDEX idx_request_history_requesters_user_time
    ON request_history_requesters(user_id, requested_at DESC, musicbrainz_id_lower DESC);

CREATE INDEX idx_search_jobs_rgmbid ON search_jobs(release_group_mbid);

CREATE INDEX idx_search_jobs_status ON search_jobs(status);

CREATE INDEX idx_search_jobs_user ON search_jobs(user_id);

CREATE INDEX idx_spotify_auth_requests_expires
    ON spotify_auth_requests(expires_at);

CREATE INDEX idx_spotify_auth_requests_user_id
    ON spotify_auth_requests(user_id);

CREATE INDEX idx_wanted_due ON wanted_watches(state, next_check_at);

CREATE INDEX idx_wanted_watches_user ON wanted_watches(user_id);

-- Indexes: compat

CREATE UNIQUE INDEX idx_compat_jellyfin_ids_current
    ON compat_jellyfin_ids (kind, internal_id) WHERE current = 1;

-- Indexes: tool

CREATE INDEX idx_import_pending_links_v2 ON import_pending_links(ref_kind, v2_id);

CREATE INDEX idx_import_runs_instance ON import_runs(instance_id);

-- Views: library
--
-- Read models kept by triggers, never by code. Inserting a key into a
-- *_refresh view works that key's rows out again from the source rows;
-- the backfill and every trigger go through it, and a *_rule view holds
-- a rule the refresh reads.
--   library_user_album_plays and local_albums.has_indexed_track follow
--     library_play_history, catalog_user_play_marks and local_tracks;
--   library_artist_album_credits and library_artist_counts follow
--     local_track_artists, local_tracks, local_albums,
--     local_album_artists and local_artists;
--   library_publish_outputs follows library_publish_journal.
-- The followed tables are never written with REPLACE, which removes rows
-- without firing their delete triggers. A migration that rebuilds one of
-- them drops its triggers with it and must make them again (the schema
-- test counts them). Changing a rule means making its refresh again and
-- running the backfill again.

CREATE VIEW library_user_album_plays_refresh (user_id, local_album_id) AS
    SELECT NULL, NULL WHERE 0;

CREATE VIEW library_artist_album_credits_refresh (local_artist_id, local_album_id) AS
    SELECT NULL, NULL WHERE 0;

CREATE VIEW library_artist_counts_refresh (local_artist_id) AS SELECT NULL WHERE 0;

CREATE VIEW library_publish_outputs_refresh (journal_id) AS SELECT NULL WHERE 0;

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

CREATE VIEW library_publish_outputs_rule
    (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
     sha256, size, kind) AS
SELECT j.id, j.bundle_id, i.key, x.key, json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id'),
       j.dest_root, j.dest_rel, j.staged_sha256, json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size'),
       json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind')
FROM library_publish_journal j
JOIN library_publish_operations o ON o.bundle_id = j.bundle_id
JOIN json_each(CASE WHEN json_valid(o.bundle_json) THEN o.bundle_json ELSE '{}' END,
               '$.items') i
JOIN json_each(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.outputs') x
WHERE j.kind = 'output'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_root') = j.dest_root
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_rel') = j.dest_rel
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.sha256') = j.staged_sha256
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind') IN ('artwork', 'lyrics')
  AND typeof(json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size')) = 'integer'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size') >= 0
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.put_back') IS NOT 1
  AND typeof(json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id')) = 'text'
  AND EXISTS (SELECT 1 FROM library_publish_journal k
              WHERE k.bundle_id = j.bundle_id
                AND k.state IN ('committed', 'cleanup_pending', 'cleaned'));

-- Triggers: library
--
-- Some refreshes name their index with INDEXED BY:
-- trg_user_album_plays_refresh (idx_play_history_user_track and
-- idx_play_history_user_album_no_track), trg_artist_album_credits_refresh,
-- trg_genre_artwork_album_update and the three trg_genre_artwork_artwork_*
-- triggers (idx_local_tracks_album_availability), and the view
-- library_artist_counts_rule (idx_local_albums_owned_leader).
-- Before the database has planner statistics SQLite can pick another
-- index and walk every track for each album written. Dropping or renaming
-- one of these indexes makes every write that fires the trigger fail.

CREATE TRIGGER trg_management_metadata_snapshot_immutable
BEFORE UPDATE ON library_management_metadata_snapshots
BEGIN
    SELECT RAISE(ABORT, 'management metadata snapshots are immutable');
END;

CREATE TRIGGER trg_library_publish_journal_fence_insert
AFTER INSERT ON library_publish_journal
WHEN NEW.track_id IS NOT NULL OR NEW.kind = 'remove'
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;

CREATE TRIGGER trg_library_publish_journal_fence_state
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.state IS NOT OLD.state AND (NEW.track_id IS NOT NULL OR NEW.kind = 'remove')
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE id = NEW.id;
END;

CREATE TRIGGER trg_library_publish_resolution_fence
AFTER INSERT ON library_publish_resolutions
WHEN EXISTS (
    SELECT 1 FROM library_publish_journal
    WHERE bundle_id = NEW.bundle_id
      AND (track_id IS NOT NULL OR kind = 'remove')
)
BEGIN
    UPDATE library_scan_fences SET revision = revision + 1 WHERE singleton = 1;
    UPDATE library_publish_journal
        SET fence_revision = (SELECT revision FROM library_scan_fences WHERE singleton = 1)
        WHERE bundle_id = NEW.bundle_id
          AND (track_id IS NOT NULL OR kind = 'remove');
END;

CREATE TRIGGER library_identify_reviews_revision
AFTER UPDATE OF state, candidates_json, reason_code, local_album_id
ON library_identify_reviews
FOR EACH ROW
WHEN OLD.state IS NOT NEW.state
    OR OLD.candidates_json IS NOT NEW.candidates_json
    OR OLD.reason_code IS NOT NEW.reason_code
    OR OLD.local_album_id IS NOT NEW.local_album_id
BEGIN
    UPDATE library_identify_reviews
    SET row_revision = OLD.row_revision + 1
    WHERE id = NEW.id;
END;

CREATE TRIGGER trg_custom_edition_manifest_immutable_update
BEFORE UPDATE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;

CREATE TRIGGER trg_custom_edition_manifest_immutable_delete
BEFORE DELETE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;

CREATE TRIGGER trg_custom_edition_tracks_immutable_update
BEFORE UPDATE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
END;

CREATE TRIGGER trg_custom_edition_tracks_immutable_delete
BEFORE DELETE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
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

CREATE TRIGGER trg_library_publish_outputs_refresh
INSTEAD OF INSERT ON library_publish_outputs_refresh
WHEN NEW.journal_id IS NOT NULL
BEGIN
    INSERT INTO library_publish_outputs
        (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
         sha256, size, kind)
    SELECT journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
           sha256, size, kind
    FROM library_publish_outputs_rule WHERE journal_id = NEW.journal_id
    ON CONFLICT DO NOTHING;
END;

CREATE TRIGGER trg_library_publish_outputs_landed
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.kind = 'output'
 AND NEW.state IN ('committed', 'cleanup_pending', 'cleaned')
 AND OLD.state NOT IN ('committed', 'cleanup_pending', 'cleaned')
BEGIN
    INSERT INTO library_publish_outputs_refresh (journal_id) VALUES (NEW.id);
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

CREATE TRIGGER trg_library_identification_attempts_immutable
BEFORE UPDATE ON library_identification_attempts
BEGIN SELECT RAISE(ABORT, 'identification attempts are immutable'); END;

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
                           OR h.played_at / 1000 > m.marked_unplayed_at / 1000)) AS counted,
                   (SELECT MAX(h.played_at / 1000)
                    FROM library_play_history h INDEXED BY idx_play_history_user_track
                    WHERE h.user_id = NEW.user_id AND h.local_track_id = t.id
                      AND (m.marked_unplayed_at IS NULL
                           OR h.played_at / 1000 > m.marked_unplayed_at / 1000)) AS latest
            FROM local_tracks t
            LEFT JOIN catalog_user_play_marks m
                ON m.user_id = NEW.user_id AND m.local_track_id = t.id
            WHERE t.local_album_id = NEW.local_album_id
        )
        UNION ALL
        SELECT 0, COUNT(*), MAX(h.played_at / 1000)
        FROM library_play_history h INDEXED BY idx_play_history_user_album_no_track
        WHERE h.user_id = NEW.user_id AND h.local_track_id IS NULL
          AND h.local_album_id = NEW.local_album_id AND h.local_album_id IS NOT NULL
    )
    HAVING SUM(plays) > 0;
END;

-- Triggers: media

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

-- Triggers: catalog

CREATE TRIGGER trg_artist_counts_pair_insert
AFTER INSERT ON library_artist_album_credits
WHEN NEW.appearance = 1
  OR NOT EXISTS (SELECT 1 FROM library_artist_counts
                 WHERE local_artist_id = NEW.local_artist_id)
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_pair_delete
AFTER DELETE ON library_artist_album_credits
WHEN OLD.live = 1
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (OLD.local_artist_id);
END;

CREATE TRIGGER trg_artist_counts_pair_update
AFTER UPDATE OF live, appearance ON library_artist_album_credits
WHEN OLD.live IS NOT NEW.live OR OLD.appearance IS NOT NEW.appearance
BEGIN
    INSERT INTO library_artist_counts_refresh (local_artist_id) VALUES (NEW.local_artist_id);
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

-- Triggers: acquire

CREATE TRIGGER download_activity_held_entry_insert
AFTER INSERT ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_state
AFTER UPDATE OF state ON acquire_held_entries
WHEN OLD.state IS NOT NEW.state
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_delete
AFTER DELETE ON acquire_held_entries
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_entry_retry
AFTER UPDATE OF retry_at ON acquire_held_entries
WHEN OLD.retry_at IS NOT NEW.retry_at
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_insert
AFTER INSERT ON acquire_held_files
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries WHERE id = NEW.entry_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_held_file_state
AFTER UPDATE OF state, reason ON acquire_held_files
WHEN OLD.state IS NOT NEW.state OR OLD.reason IS NOT NEW.reason
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM acquire_held_entries WHERE id = NEW.entry_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_insert
AFTER INSERT ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_landed
AFTER UPDATE OF release_group_mbid, completed_at, updated_at ON download_tasks
WHEN NEW.status IN ('completed', 'partial')
    AND OLD.status IN ('completed', 'partial')
    AND (OLD.release_group_mbid IS NOT NEW.release_group_mbid
         OR COALESCE(OLD.completed_at, OLD.updated_at)
            IS NOT COALESCE(NEW.completed_at, NEW.updated_at))
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_status
AFTER UPDATE OF status ON download_tasks
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_search_link
AFTER UPDATE OF search_job_id, candidate_index ON download_tasks
WHEN OLD.search_job_id IS NOT NEW.search_job_id
    OR OLD.candidate_index IS NOT NEW.candidate_index
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_owner
AFTER UPDATE OF user_id ON download_tasks
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_task_delete
AFTER DELETE ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_attempt_insert
AFTER INSERT ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_attempt_state
AFTER UPDATE OF state ON download_attempts
WHEN OLD.state IS NOT NEW.state
    AND EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER download_activity_attempt_delete
AFTER DELETE ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = OLD.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = OLD.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

PRAGMA user_version = 135;
