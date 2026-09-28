-- 0001_baseline.sql - v3 empty-data baseline: consolidated v2-final schema.
--
-- One owner per table. The v2 tree created 12 tables from two places each
-- (a small store plus the native schema file, first writer won); each of
-- those is defined exactly once below in section 2, with the merge noted.
--
-- Excluded at baseline, per the approved drop list:
--   D13  rebuild leftovers: download_quarantine_legacy, download_attempts_new,
--        youtube_links_old, youtube_track_links_old, ignored_releases_legacy,
--        and the native transient swap tables (*__management_v1,
--        *__edition_tier_v1, *__resolved_v1). Each was dropped or renamed away
--        by its own migration, so a fresh baseline never creates them.
--   D14  legacy catalog tables: cache_meta, library_artists, library_albums,
--        library_files, manual_review_queue, library_album_meta. v3 has no
--        bounded migrator; cutover goes through export/import (stage 11).
--   D1   no lidarr tombstone. The v2 lidarr directory stays empty; v3 carries
--        no marker table for it.
--
-- Also intentionally not carried over:
--   - trg_fk_validation_* triggers. v2 generated one per table to detect
--     writes made with foreign_keys=OFF. v3 enforces foreign_keys=ON on every
--     connection, so the detector can never fire. The state table
--     (library_foreign_key_validation_state) stays for the import gate.
--   - Data backfills (genre backfill, follow_due enrollment select, landed
--     projection backfill). They only moved rows on existing databases; the
--     baseline starts empty. Singleton seeds (INSERT OR IGNORE) are kept.
--
-- Section 7 holds the only new v3 tables: the durable-worker fabric.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Section 1: auth-owned tables (owner: auth).
-- Sessions live here: auth_tokens carries last_seen_at, written on a
-- throttle by stage 3. Compat app passwords: connect_app_passwords.

CREATE TABLE IF NOT EXISTS auth_users (
    id TEXT PRIMARY KEY,
    display_name TEXT NOT NULL,
    email TEXT UNIQUE,
    avatar_url TEXT,
    role TEXT NOT NULL DEFAULT 'user',
    created_at TEXT NOT NULL,
    last_login_at TEXT,
    username TEXT,
    username_display TEXT
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_auth_users_username
    ON auth_users(username) WHERE username IS NOT NULL;

CREATE TABLE IF NOT EXISTS auth_providers (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    provider_uid TEXT NOT NULL,
    provider_data TEXT,
    created_at TEXT NOT NULL,
    UNIQUE (provider, provider_uid)
);
CREATE INDEX IF NOT EXISTS idx_auth_providers_user
    ON auth_providers(user_id);

CREATE TABLE IF NOT EXISTS auth_tokens (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    issued_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    last_seen_at TEXT NOT NULL,
    revoked INTEGER NOT NULL DEFAULT 0,
    user_agent TEXT,
    session_kind TEXT NOT NULL DEFAULT 'standard'
);
CREATE INDEX IF NOT EXISTS idx_auth_tokens_user
    ON auth_tokens(user_id);
CREATE INDEX IF NOT EXISTS idx_auth_tokens_hash
    ON auth_tokens(token_hash);
CREATE INDEX IF NOT EXISTS idx_auth_tokens_expires
    ON auth_tokens(expires_at);

CREATE TABLE IF NOT EXISTS auth_password_recovery_codes (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    code_hash TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_auth_password_recovery_expires
    ON auth_password_recovery_codes(expires_at);

CREATE TABLE IF NOT EXISTS auth_oidc_states (
    state TEXT PRIMARY KEY,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    code_verifier TEXT
);

CREATE TABLE IF NOT EXISTS spotify_oauth_states (
    state TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS connect_app_passwords (
    id               TEXT PRIMARY KEY,
    user_id          TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    name             TEXT NOT NULL,
    secret_sha256    TEXT NOT NULL UNIQUE,
    secret_encrypted TEXT NOT NULL,
    created_at       TEXT NOT NULL,
    last_used_at     TEXT,
    last_client      TEXT,
    revoked          INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_cap_user
    ON connect_app_passwords(user_id);

-- Section 2: dual-owner tables, one definition each.
-- Merge rule: identical copies collapse; where copies differ the store copy
-- wins on auth_users FKs (the native copy declared none) and CHECKs merge.

-- album_release_pins: identical in both copies. Owner: library catalog.
CREATE TABLE IF NOT EXISTS album_release_pins (
    release_group_mbid TEXT PRIMARY KEY,
    release_mbid       TEXT NOT NULL,
    set_by_user_id     TEXT,
    set_at             TEXT
);

-- artist_genres / artist_genre_lookup: identical in both copies, including
-- the index. Owner: genre index.
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

-- compat_bookmarks: store copy wins (FK to auth_users). Owner: compat.
CREATE TABLE IF NOT EXISTS compat_bookmarks (
    user_id TEXT NOT NULL
        REFERENCES auth_users(id) ON DELETE CASCADE,
    file_id TEXT NOT NULL,
    position_ms INTEGER NOT NULL,
    comment TEXT NOT NULL DEFAULT '',
    created_at REAL NOT NULL,
    changed_at REAL NOT NULL,
    PRIMARY KEY (user_id, file_id)
);

-- compat_id_map: identical in both copies. Owner: compat.
CREATE TABLE IF NOT EXISTS compat_id_map (
    jf_id       TEXT PRIMARY KEY,
    kind        TEXT NOT NULL,
    internal_id TEXT NOT NULL,
    UNIQUE (kind, internal_id)
);

-- compat_play_queues: store copy wins (FK to auth_users). Owner: compat.
CREATE TABLE IF NOT EXISTS compat_play_queues (
    user_id TEXT PRIMARY KEY
        REFERENCES auth_users(id) ON DELETE CASCADE,
    current_index INTEGER,
    position_ms INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    changed_by_client TEXT NOT NULL DEFAULT ''
);
-- compat_play_queue_items: identical in both copies. Owner: compat.
CREATE TABLE IF NOT EXISTS compat_play_queue_items (
    user_id TEXT NOT NULL
        REFERENCES compat_play_queues(user_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL,
    file_id TEXT NOT NULL,
    PRIMARY KEY (user_id, item_index)
);

-- play_history: store copy wins (FK to auth_users plus index).
-- Owner: play history.
CREATE TABLE IF NOT EXISTS play_history (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    track_name TEXT NOT NULL, artist_name TEXT NOT NULL, album_name TEXT,
    recording_mbid TEXT, release_group_mbid TEXT, duration_ms INTEGER,
    source TEXT,
    played_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_play_history_user_played
    ON play_history(user_id, played_at DESC);

-- user_favorites: store copy wins (FK to auth_users plus index).
-- Owner: favorites.
CREATE TABLE IF NOT EXISTS user_favorites (
    user_id    TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    item_kind  TEXT NOT NULL,
    item_id    TEXT NOT NULL,
    created_at REAL NOT NULL,
    PRIMARY KEY (user_id, item_kind, item_id)
);
CREATE INDEX IF NOT EXISTS idx_fav_user_kind
    ON user_favorites(user_id, item_kind);

-- library_catalog_revision: merged, discovery DEFAULT 0 plus native range
-- CHECK. Seeded by INSERT OR IGNORE in section 6. Owner: library catalog.
CREATE TABLE IF NOT EXISTS library_catalog_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    value INTEGER NOT NULL DEFAULT 0
        CHECK(value BETWEEN 0 AND 9223372036854775807)
);

-- playlists: repo base plus its three ratchets equals the native copy.
-- Owner: persistence (moved in from repositories/).
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
CREATE UNIQUE INDEX IF NOT EXISTS idx_playlists_user_source_ref
    ON playlists(user_id, source_ref) WHERE source_ref IS NOT NULL;
-- playlist_tracks: repo base plus rename plus three ratchets equals the
-- native copy. Owner: persistence.
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
CREATE INDEX IF NOT EXISTS idx_playlist_tracks_playlist_position
    ON playlist_tracks(playlist_id, position);

-- Section 3: discovery (owner: discovery).

CREATE TABLE IF NOT EXISTS discovery_snapshots (
    snapshot_key TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    payload BLOB NOT NULL,
    saved_at REAL NOT NULL,
    stale INTEGER NOT NULL DEFAULT 0,
    catalog_revision INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_discovery_snapshots_user
    ON discovery_snapshots(user_id);

CREATE TABLE IF NOT EXISTS discovery_activity (
    user_id TEXT NOT NULL, feature TEXT NOT NULL,
    artist_mbid TEXT NOT NULL DEFAULT '', section TEXT NOT NULL DEFAULT '',
    provider TEXT NOT NULL DEFAULT '', source TEXT NOT NULL,
    last_used REAL NOT NULL, last_success REAL NOT NULL DEFAULT 0,
    retry_at REAL NOT NULL DEFAULT 0, serviced_at REAL NOT NULL DEFAULT 0,
    PRIMARY KEY(user_id, feature, artist_mbid, section, provider)
);
CREATE INDEX IF NOT EXISTS idx_discovery_activity_due
    ON discovery_activity(retry_at, serviced_at, last_used);
CREATE INDEX IF NOT EXISTS idx_discovery_activity_expiry
    ON discovery_activity(last_used);

CREATE TABLE IF NOT EXISTS discovery_optional_progress (
    user_id TEXT NOT NULL, work_key TEXT NOT NULL,
    revision TEXT NOT NULL, cursor INTEGER NOT NULL DEFAULT 0,
    updated_at REAL NOT NULL,
    PRIMARY KEY(user_id, work_key)
);
CREATE INDEX IF NOT EXISTS idx_discovery_progress_expiry
    ON discovery_optional_progress(updated_at);

CREATE TABLE IF NOT EXISTS discovery_batches (
    id             TEXT PRIMARY KEY,
    user_id        TEXT NOT NULL,
    name           TEXT NOT NULL,
    source_section TEXT NOT NULL DEFAULT '',
    created_at     TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS discovery_batch_items (
    batch_id            TEXT NOT NULL,
    release_group_mbid  TEXT NOT NULL,
    artist_mbid         TEXT NOT NULL DEFAULT '',
    album_name          TEXT NOT NULL DEFAULT '',
    artist_name         TEXT NOT NULL DEFAULT '',
    outcome             TEXT NOT NULL DEFAULT 'requested',
    added_at            TEXT NOT NULL,
    PRIMARY KEY (batch_id, release_group_mbid)
);
CREATE INDEX IF NOT EXISTS idx_discovery_batches_user
    ON discovery_batches(user_id, created_at DESC);

-- Section 4: acquisition (owner: acquisition).
-- Includes the revision triggers, the landed-groups projection, and the
-- activity triggers. No task FK on download_attempts by design: deleting
-- queue rows must not erase cleanup debt.

CREATE TABLE IF NOT EXISTS download_tasks (
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
    origin TEXT NOT NULL DEFAULT 'user',
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
    remote_queued INTEGER NOT NULL DEFAULT 0,
    preferred_quality_fallback_at REAL,
    quality_pool_key TEXT,
    attempt_number INTEGER NOT NULL DEFAULT 0,
    attempt_total INTEGER NOT NULL DEFAULT 0,
    has_next_source INTEGER NOT NULL DEFAULT 0,
    quality_snapshot_json TEXT,
    quality_snapshot_hash TEXT,
    quality_snapshot_summary TEXT,
    quality_preference_step INTEGER,
    quality_certainty TEXT,
    quality_provenance TEXT,
    manual_quality_override INTEGER NOT NULL DEFAULT 0,
    staging_path TEXT,
    final_path TEXT,
    error_message TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    last_polled_at REAL,
    created_at REAL NOT NULL,
    started_at REAL,
    completed_at REAL,
    cancelled_at REAL,
    updated_at REAL NOT NULL,
    wrong_product_verdict_at REAL,
    wrong_product_detail TEXT
);
CREATE INDEX IF NOT EXISTS idx_download_tasks_status ON download_tasks(status);
CREATE INDEX IF NOT EXISTS idx_download_tasks_user ON download_tasks(user_id);
CREATE INDEX IF NOT EXISTS idx_download_tasks_rgmbid ON download_tasks(release_group_mbid);
CREATE INDEX IF NOT EXISTS idx_download_tasks_type ON download_tasks(download_type);
CREATE INDEX IF NOT EXISTS idx_download_tasks_username ON download_tasks(source_username);
CREATE INDEX IF NOT EXISTS idx_download_tasks_created ON download_tasks(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_download_activity_owner_status
    ON download_tasks(user_id, status);
CREATE INDEX IF NOT EXISTS idx_download_activity_landed
    ON download_tasks(release_group_mbid,
        COALESCE(completed_at, updated_at) DESC, user_id)
    WHERE status IN ('completed','partial') AND release_group_mbid != '';

CREATE TABLE IF NOT EXISTS search_jobs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
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
    quality_snapshot_hash TEXT,
    quality_snapshot_summary TEXT,
    created_at REAL NOT NULL,
    completed_at REAL,
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_search_jobs_user ON search_jobs(user_id);
CREATE INDEX IF NOT EXISTS idx_search_jobs_status ON search_jobs(status);
CREATE INDEX IF NOT EXISTS idx_search_jobs_rgmbid ON search_jobs(release_group_mbid);

CREATE TABLE IF NOT EXISTS download_quarantine (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source TEXT NOT NULL DEFAULT 'soulseek',
    identity TEXT NOT NULL,
    release_group_mbid TEXT,
    reason TEXT NOT NULL
        CHECK(reason IN ('verify_failed','corrupt','fingerprint_mismatch',
                         'duration_mismatch','download_failed','manual')),
    quarantined_at REAL NOT NULL,
    UNIQUE (source, identity, release_group_mbid)
);
CREATE INDEX IF NOT EXISTS idx_quarantine_lookup ON download_quarantine(source, identity);
CREATE INDEX IF NOT EXISTS idx_quarantine_quarantined_at ON download_quarantine(quarantined_at);

CREATE TABLE IF NOT EXISTS held_imports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    release_group_mbid TEXT,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    track_title TEXT,
    artist_name TEXT,
    artist_mbid TEXT,
    album_title TEXT,
    year INTEGER,
    held_path TEXT NOT NULL,
    original_filename TEXT,
    file_format TEXT,
    duration_seconds REAL,
    expected_duration_seconds REAL,
    reason TEXT NOT NULL,
    reason_detail TEXT,
    evidence_title TEXT,
    evidence_artist TEXT,
    evidence_score REAL,
    source TEXT NOT NULL DEFAULT 'soulseek',
    source_task_id TEXT,
    origin TEXT NOT NULL DEFAULT 'user',
    naming_template TEXT,
    management_retry_count INTEGER NOT NULL DEFAULT 0,
    management_next_retry_at REAL,
    status TEXT NOT NULL DEFAULT 'held'
        CHECK(status IN ('held','imported','discarded')),
    created_at REAL NOT NULL,
    resolved_at REAL,
    file_cleanup_completed_at REAL
);
CREATE INDEX IF NOT EXISTS idx_held_user ON held_imports(user_id, status);
CREATE INDEX IF NOT EXISTS idx_held_rg ON held_imports(release_group_mbid, status);
CREATE INDEX IF NOT EXISTS idx_held_task ON held_imports(source_task_id, status);
CREATE INDEX IF NOT EXISTS idx_held_dedup
    ON held_imports(release_group_mbid, disc_number, track_number, status);
CREATE INDEX IF NOT EXISTS idx_held_management_retry
    ON held_imports(management_next_retry_at, status);

CREATE TABLE IF NOT EXISTS download_attempts (
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
    next_retry_at REAL NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at REAL,
    error_code TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    completed_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);
CREATE INDEX IF NOT EXISTS idx_download_attempts_task
    ON download_attempts(task_id, candidate_index, created_at);
CREATE INDEX IF NOT EXISTS idx_download_attempts_cleanup
    ON download_attempts(state, next_retry_at, lease_expires_at);
CREATE INDEX IF NOT EXISTS idx_download_attempts_job
    ON download_attempts(source, job_name);

CREATE TABLE IF NOT EXISTS download_cleanup_reconciliation (
    mount_key TEXT PRIMARY KEY,
    mount_root TEXT NOT NULL,
    pending_directories_json TEXT NOT NULL,
    current_directory TEXT,
    last_entry TEXT,
    completed INTEGER NOT NULL DEFAULT 0 CHECK(completed IN (0,1)),
    updated_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS download_activity_global_revision (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    revision INTEGER NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO download_activity_global_revision (singleton, revision)
VALUES (1, 0);

CREATE TABLE IF NOT EXISTS download_activity_user_revisions (
    user_id TEXT PRIMARY KEY,
    revision INTEGER NOT NULL DEFAULT 0
);

CREATE TRIGGER IF NOT EXISTS download_activity_task_insert
AFTER INSERT ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_landed
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

CREATE TRIGGER IF NOT EXISTS download_activity_task_status
AFTER UPDATE OF status ON download_tasks
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_search_link
AFTER UPDATE OF search_job_id, candidate_index ON download_tasks
WHEN OLD.search_job_id IS NOT NEW.search_job_id
    OR OLD.candidate_index IS NOT NEW.candidate_index
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_owner
AFTER UPDATE OF user_id ON download_tasks
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_task_delete
AFTER DELETE ON download_tasks
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_insert
AFTER INSERT ON held_imports
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_status
AFTER UPDATE OF status ON held_imports
WHEN OLD.status IS NOT NEW.status
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_owner
AFTER UPDATE OF user_id ON held_imports
WHEN OLD.user_id IS NOT NEW.user_id
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (NEW.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_held_delete
AFTER DELETE ON held_imports
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision) VALUES (OLD.user_id, 1)
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_attempt_insert
AFTER INSERT ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_attempt_state
AFTER UPDATE OF state ON download_attempts
WHEN OLD.state IS NOT NEW.state
    AND EXISTS (SELECT 1 FROM download_tasks WHERE id = NEW.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = NEW.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TRIGGER IF NOT EXISTS download_activity_attempt_delete
AFTER DELETE ON download_attempts
WHEN EXISTS (SELECT 1 FROM download_tasks WHERE id = OLD.task_id)
BEGIN
    UPDATE download_activity_global_revision SET revision = revision + 1 WHERE singleton = 1;
    INSERT INTO download_activity_user_revisions (user_id, revision)
    SELECT user_id, 1 FROM download_tasks WHERE id = OLD.task_id
    ON CONFLICT(user_id) DO UPDATE SET revision = revision + 1;
END;

CREATE TABLE IF NOT EXISTS download_landed_groups (
    scope TEXT NOT NULL, release_group_mbid TEXT NOT NULL, landed_at REAL NOT NULL,
    PRIMARY KEY(scope, release_group_mbid));
CREATE INDEX IF NOT EXISTS idx_download_landed_order
    ON download_landed_groups(scope, landed_at DESC, release_group_mbid);

CREATE TRIGGER IF NOT EXISTS download_landed_insert
AFTER INSERT ON download_tasks WHEN NEW.status IN ('completed','partial') BEGIN
    DELETE FROM download_landed_groups WHERE scope = 'global'
        AND release_group_mbid = NEW.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'global', release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = NEW.release_group_mbid GROUP BY release_group_mbid;
    DELETE FROM download_landed_groups WHERE scope = 'user:' || NEW.user_id
        AND release_group_mbid = NEW.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'user:' || NEW.user_id, release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = NEW.release_group_mbid
        AND user_id = NEW.user_id GROUP BY release_group_mbid;
END;

CREATE TRIGGER IF NOT EXISTS download_landed_delete
AFTER DELETE ON download_tasks WHEN OLD.status IN ('completed','partial') BEGIN
    DELETE FROM download_landed_groups WHERE scope = 'global'
        AND release_group_mbid = OLD.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'global', release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = OLD.release_group_mbid GROUP BY release_group_mbid;
    DELETE FROM download_landed_groups WHERE scope = 'user:' || OLD.user_id
        AND release_group_mbid = OLD.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'user:' || OLD.user_id, release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = OLD.release_group_mbid
        AND user_id = OLD.user_id GROUP BY release_group_mbid;
END;

CREATE TRIGGER IF NOT EXISTS download_landed_update
AFTER UPDATE ON download_tasks
WHEN (OLD.status IN ('completed','partial') OR NEW.status IN ('completed','partial'))
AND (OLD.status IS NOT NEW.status OR OLD.user_id IS NOT NEW.user_id
OR OLD.release_group_mbid IS NOT NEW.release_group_mbid
OR COALESCE(OLD.completed_at,OLD.updated_at) IS NOT
COALESCE(NEW.completed_at,NEW.updated_at)) BEGIN
    DELETE FROM download_landed_groups WHERE scope = 'global'
        AND release_group_mbid = OLD.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'global', release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = OLD.release_group_mbid GROUP BY release_group_mbid;
    DELETE FROM download_landed_groups WHERE scope = 'user:' || OLD.user_id
        AND release_group_mbid = OLD.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'user:' || OLD.user_id, release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = OLD.release_group_mbid
        AND user_id = OLD.user_id GROUP BY release_group_mbid;
    DELETE FROM download_landed_groups WHERE scope = 'global'
        AND release_group_mbid = NEW.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'global', release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = NEW.release_group_mbid GROUP BY release_group_mbid;
    DELETE FROM download_landed_groups WHERE scope = 'user:' || NEW.user_id
        AND release_group_mbid = NEW.release_group_mbid;
    INSERT INTO download_landed_groups
        SELECT 'user:' || NEW.user_id, release_group_mbid, MAX(COALESCE(completed_at, updated_at))
        FROM download_tasks
        WHERE status IN ('completed','partial') AND release_group_mbid != ''
        AND release_group_mbid = NEW.release_group_mbid
        AND user_id = NEW.user_id GROUP BY release_group_mbid;
END;

CREATE TABLE IF NOT EXISTS acquisition_snapshot_backfill (
    id INTEGER PRIMARY KEY CHECK(id=1),
    completed_at REAL NOT NULL,
    native_tasks INTEGER NOT NULL,
    search_jobs INTEGER NOT NULL
);

-- Section 5: imports, follows, requests, caches, per-user state.

-- Drop import (owner: acquisition).
CREATE TABLE IF NOT EXISTS drop_import_jobs (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    user_name TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL
        CHECK(status IN ('processing','completed','failed')),
    created_at REAL NOT NULL,
    upload_name TEXT NOT NULL,
    staging_dir TEXT NOT NULL,
    error TEXT
);
CREATE INDEX IF NOT EXISTS idx_drop_import_jobs_user
    ON drop_import_jobs(user_id, created_at);

CREATE TABLE IF NOT EXISTS drop_import_items (
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
    updated_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_drop_import_items_job
    ON drop_import_items(job_id);

-- Free music (owner: acquisition).
CREATE TABLE IF NOT EXISTS free_music_tasks (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK(kind IN ('album','track')),
    mbid TEXT NOT NULL,
    artist TEXT NOT NULL DEFAULT '',
    title TEXT NOT NULL DEFAULT '',
    status TEXT NOT NULL CHECK(status IN
        ('searching','downloading','importing','completed','failed','cancelled')),
    identifier TEXT,
    licence_url TEXT,
    format TEXT,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_completed INTEGER NOT NULL DEFAULT 0,
    bytes_total INTEGER NOT NULL DEFAULT 0,
    bytes_downloaded INTEGER NOT NULL DEFAULT 0,
    attempts INTEGER NOT NULL DEFAULT 0,
    error TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    track_count INTEGER NOT NULL DEFAULT 0 CHECK(track_count >= 0),
    origin TEXT NOT NULL DEFAULT 'user',
    release_group_mbid TEXT,
    release_mbid TEXT,
    release_track_mbid TEXT,
    recording_mbid TEXT,
    duration_seconds REAL,
    album_title TEXT,
    track_number INTEGER,
    disc_number INTEGER,
    quality_snapshot_json TEXT,
    quality_snapshot_hash TEXT,
    quality_snapshot_summary TEXT,
    tried_candidates_json TEXT NOT NULL DEFAULT '[]'
);
CREATE INDEX IF NOT EXISTS idx_free_music_user
    ON free_music_tasks(user_id, created_at);
CREATE INDEX IF NOT EXISTS idx_free_music_mbid
    ON free_music_tasks(mbid);

-- Events (owner: events).
CREATE TABLE IF NOT EXISTS live_event_feed (
    source            TEXT NOT NULL CHECK(source IN ('ticketmaster','skiddle')),
    source_event_id   TEXT NOT NULL,
    artist_mbid_lower TEXT NOT NULL,
    artist_name       TEXT NOT NULL,
    event_name        TEXT NOT NULL,
    venue_name        TEXT,
    city              TEXT,
    region            TEXT,
    country_code      TEXT,
    latitude          REAL,
    longitude         REAL,
    starts_at         TEXT,
    local_date        TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'scheduled'
        CHECK(status IN ('scheduled','cancelled','rescheduled')),
    ticket_url        TEXT,
    match_confidence  TEXT NOT NULL CHECK(match_confidence IN ('mbid','name')),
    discovered_at     REAL NOT NULL,
    updated_at        REAL NOT NULL,
    PRIMARY KEY (source, source_event_id, artist_mbid_lower)
);
CREATE INDEX IF NOT EXISTS idx_lef_artist ON live_event_feed(artist_mbid_lower);
CREATE INDEX IF NOT EXISTS idx_lef_date ON live_event_feed(local_date);

CREATE TABLE IF NOT EXISTS artist_event_check (
    artist_mbid_lower TEXT PRIMARY KEY,
    last_checked_at   REAL,
    last_status       TEXT,
    last_error        TEXT
);

CREATE TABLE IF NOT EXISTS artist_tm_attraction (
    artist_mbid_lower TEXT PRIMARY KEY,
    attraction_id     TEXT,
    match_basis       TEXT NOT NULL
        CHECK(match_basis IN ('mbid','exact_name','none')),
    resolved_at       REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS artist_skiddle_ids (
    artist_mbid_lower TEXT NOT NULL,
    skiddle_artistid  TEXT NOT NULL,
    PRIMARY KEY (artist_mbid_lower, skiddle_artistid)
);

CREATE TABLE IF NOT EXISTS artist_skiddle_resolution (
    artist_mbid_lower TEXT PRIMARY KEY,
    resolved_at       REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS user_event_cities (
    user_id      TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    city_name    TEXT NOT NULL,
    country_code TEXT,
    latitude     REAL NOT NULL,
    longitude    REAL NOT NULL,
    radius_km    REAL NOT NULL,
    position     INTEGER NOT NULL,
    PRIMARY KEY (user_id, latitude, longitude)
);

CREATE TABLE IF NOT EXISTS user_event_seen (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at REAL NOT NULL
);

-- Follows (owner: follows). The triggers keep follow_due and the inventory
-- in step across every producer.
CREATE TABLE IF NOT EXISTS user_followed_artists (
    user_id           TEXT    NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid       TEXT    NOT NULL,
    artist_mbid_lower TEXT    NOT NULL,
    artist_name       TEXT    NOT NULL,
    auto_download     INTEGER NOT NULL DEFAULT 0,
    followed_at       REAL    NOT NULL,
    updated_at        REAL    NOT NULL,
    PRIMARY KEY (user_id, artist_mbid_lower)
);
CREATE INDEX IF NOT EXISTS idx_ufa_user ON user_followed_artists(user_id);
CREATE INDEX IF NOT EXISTS idx_ufa_mbid ON user_followed_artists(artist_mbid_lower);
CREATE INDEX IF NOT EXISTS idx_ufa_autodl
    ON user_followed_artists(auto_download) WHERE auto_download = 1;

CREATE TABLE IF NOT EXISTS auto_download_approvals (
    user_id           TEXT    NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    artist_mbid       TEXT    NOT NULL,
    artist_mbid_lower TEXT    NOT NULL,
    artist_name       TEXT    NOT NULL,
    state             TEXT    NOT NULL DEFAULT 'pending',
    requested_at      REAL    NOT NULL,
    reviewed_by_id    TEXT,
    reviewed_by_name  TEXT,
    reviewed_at       REAL,
    batch_id          TEXT,
    source            TEXT,
    PRIMARY KEY (user_id, artist_mbid_lower)
);
CREATE INDEX IF NOT EXISTS idx_ada_pending
    ON auto_download_approvals(state) WHERE state = 'pending';
CREATE INDEX IF NOT EXISTS idx_ada_batch
    ON auto_download_approvals(batch_id) WHERE batch_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS new_release_feed (
    release_group_mbid_lower TEXT    PRIMARY KEY,
    release_group_mbid       TEXT    NOT NULL,
    artist_mbid_lower        TEXT    NOT NULL,
    artist_name              TEXT    NOT NULL,
    title                    TEXT    NOT NULL,
    primary_type             TEXT,
    secondary_types          TEXT,
    first_release_date       TEXT,
    discovered_at            REAL    NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_nrf_artist ON new_release_feed(artist_mbid_lower);
CREATE INDEX IF NOT EXISTS idx_nrf_date ON new_release_feed(first_release_date DESC);

CREATE TABLE IF NOT EXISTS artist_release_check (
    artist_mbid_lower             TEXT PRIMARY KEY,
    last_checked_at               REAL,
    last_status                   TEXT,
    last_error                    TEXT,
    release_type_policy_revision INTEGER
);

CREATE TABLE IF NOT EXISTS artist_known_releases (
    artist_mbid_lower   TEXT NOT NULL,
    rg_mbid_lower       TEXT NOT NULL,
    auto_policy_revision INTEGER,
    PRIMARY KEY (artist_mbid_lower, rg_mbid_lower)
);
CREATE INDEX IF NOT EXISTS idx_akr_auto_policy
    ON artist_known_releases(artist_mbid_lower, auto_policy_revision)
    WHERE auto_policy_revision IS NOT NULL;

CREATE TABLE IF NOT EXISTS user_new_release_seen (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    seen_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS follow_due (
    artist_mbid_lower TEXT PRIMARY KEY,
    due_at REAL NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    last_serviced REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_follow_due
    ON follow_due(due_at, last_serviced, artist_mbid_lower);

CREATE TABLE IF NOT EXISTS follow_inventory (
    artist_mbid_lower TEXT PRIMARY KEY,
    source TEXT NOT NULL,
    policy INTEGER NOT NULL,
    phase TEXT NOT NULL DEFAULT 'collecting',
    offset INTEGER NOT NULL DEFAULT 0,
    total INTEGER,
    process TEXT NOT NULL,
    inflight INTEGER NOT NULL DEFAULT 0,
    progressed_at REAL NOT NULL,
    diverge_count INTEGER NOT NULL DEFAULT 0,
    observation TEXT
);
CREATE TABLE IF NOT EXISTS follow_inventory_rows (
    artist_mbid_lower TEXT NOT NULL REFERENCES follow_inventory
        ON DELETE CASCADE,
    phase TEXT NOT NULL,
    rg TEXT NOT NULL,
    payload TEXT NOT NULL,
    PRIMARY KEY(artist_mbid_lower, phase, rg)
);
CREATE TABLE IF NOT EXISTS follow_inventory_pages (
    artist_mbid_lower TEXT NOT NULL REFERENCES follow_inventory
        ON DELETE CASCADE,
    phase TEXT NOT NULL,
    offset INTEGER NOT NULL,
    fingerprint TEXT NOT NULL,
    PRIMARY KEY(artist_mbid_lower, phase, offset)
);

CREATE TRIGGER IF NOT EXISTS follow_enroll_insert
AFTER INSERT ON user_followed_artists BEGIN
    INSERT INTO follow_due(artist_mbid_lower) VALUES(NEW.artist_mbid_lower)
    ON CONFLICT(artist_mbid_lower) DO UPDATE SET due_at=0, failures=0;
END;
CREATE TRIGGER IF NOT EXISTS follow_enroll_update
AFTER UPDATE ON user_followed_artists BEGIN
    INSERT INTO follow_due(artist_mbid_lower) VALUES(NEW.artist_mbid_lower)
    ON CONFLICT(artist_mbid_lower) DO UPDATE SET due_at=0, failures=0;
END;
CREATE TRIGGER IF NOT EXISTS follow_success_insert
AFTER INSERT ON artist_release_check WHEN NEW.last_status='ok' BEGIN
    INSERT INTO follow_due(artist_mbid_lower,due_at)
    VALUES(NEW.artist_mbid_lower,NEW.last_checked_at+86400)
    ON CONFLICT(artist_mbid_lower) DO UPDATE SET
        due_at=excluded.due_at,failures=0;
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_success_update
AFTER UPDATE ON artist_release_check WHEN NEW.last_status='ok' BEGIN
    INSERT INTO follow_due(artist_mbid_lower,due_at)
    VALUES(NEW.artist_mbid_lower,NEW.last_checked_at+86400)
    ON CONFLICT(artist_mbid_lower) DO UPDATE SET
        due_at=excluded.due_at,failures=0;
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_inventory_insert_fence
AFTER INSERT ON user_followed_artists BEGIN
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_inventory_update_fence
AFTER UPDATE ON user_followed_artists BEGIN
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_inventory_delete_fence
AFTER DELETE ON user_followed_artists BEGIN
    DELETE FROM follow_inventory WHERE artist_mbid_lower=OLD.artist_mbid_lower;
    UPDATE follow_due SET due_at=0,failures=0 WHERE artist_mbid_lower=OLD.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_approval_insert_due
AFTER INSERT ON auto_download_approvals BEGIN
    UPDATE follow_due SET due_at=0,failures=0 WHERE artist_mbid_lower=NEW.artist_mbid_lower;
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;
CREATE TRIGGER IF NOT EXISTS follow_approval_update_due
AFTER UPDATE ON auto_download_approvals BEGIN
    UPDATE follow_due SET due_at=0,failures=0 WHERE artist_mbid_lower=NEW.artist_mbid_lower;
    DELETE FROM follow_inventory WHERE artist_mbid_lower=NEW.artist_mbid_lower;
END;

-- Wanted (owner: acquisition).
CREATE TABLE IF NOT EXISTS wanted_watches (
    release_group_mbid_lower TEXT PRIMARY KEY,
    release_group_mbid TEXT NOT NULL,
    user_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    kind TEXT NOT NULL CHECK(kind IN ('missing','partial')),
    state TEXT NOT NULL DEFAULT 'watching'
        CHECK(state IN ('watching','dormant','stopped','fulfilled')),
    created_at REAL NOT NULL,
    first_release_date TEXT,
    check_count INTEGER NOT NULL DEFAULT 0,
    quiet_streak INTEGER NOT NULL DEFAULT 0,
    last_checked_at REAL,
    next_check_at REAL NOT NULL,
    last_outcome TEXT,
    new_candidate_count INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_wanted_due
    ON wanted_watches(state, next_check_at);

CREATE TABLE IF NOT EXISTS wanted_seen_candidates (
    release_group_mbid_lower TEXT NOT NULL,
    source TEXT NOT NULL,
    identity TEXT NOT NULL,
    first_seen_at REAL NOT NULL,
    PRIMARY KEY (release_group_mbid_lower, source, identity)
);

-- Request history (owner: acquisition).
CREATE TABLE IF NOT EXISTS request_history (
    musicbrainz_id_lower TEXT PRIMARY KEY,
    musicbrainz_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    album_title TEXT NOT NULL,
    artist_mbid TEXT,
    year INTEGER,
    cover_url TEXT,
    requested_at TEXT NOT NULL,
    completed_at TEXT,
    status TEXT NOT NULL,
    monitor_artist INTEGER NOT NULL DEFAULT 0,
    auto_download_artist INTEGER NOT NULL DEFAULT 0,
    user_id TEXT,
    requested_by_name TEXT,
    reviewed_by_id TEXT,
    reviewed_by_name TEXT,
    reviewed_at TEXT,
    download_task_id TEXT,
    release_mbid TEXT,
    request_kind TEXT NOT NULL DEFAULT 'album',
    track_title TEXT,
    duration_seconds INTEGER,
    track_release_group_mbid TEXT,
    dispatch_authorized INTEGER NOT NULL DEFAULT 0,
    generation INTEGER NOT NULL DEFAULT 1
);
CREATE TABLE IF NOT EXISTS request_history_dismissals (
    user_id TEXT NOT NULL,
    musicbrainz_id_lower TEXT NOT NULL,
    dismissed_at TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (user_id, musicbrainz_id_lower)
);
CREATE TABLE IF NOT EXISTS request_history_requesters (
    user_id TEXT NOT NULL,
    musicbrainz_id_lower TEXT NOT NULL,
    requested_at TEXT NOT NULL,
    requested_by_name TEXT,
    PRIMARY KEY (user_id, musicbrainz_id_lower)
);
CREATE INDEX IF NOT EXISTS idx_request_history_retrying_keyset
    ON request_history(status, requested_at DESC, musicbrainz_id_lower DESC);
CREATE INDEX IF NOT EXISTS idx_request_history_requesters_mbid
    ON request_history_requesters(musicbrainz_id_lower);
CREATE INDEX IF NOT EXISTS idx_request_history_requesters_user_time
    ON request_history_requesters(user_id, requested_at DESC, musicbrainz_id_lower DESC);

-- MBID map and remote MBID indexes (owner: metadata).
CREATE TABLE IF NOT EXISTS mbid_resolution_map (
    source_mbid_lower TEXT PRIMARY KEY,
    source_mbid TEXT NOT NULL,
    release_group_mbid TEXT
);
CREATE TABLE IF NOT EXISTS ignored_releases (
    user_id TEXT NOT NULL,
    release_group_mbid_lower TEXT NOT NULL,
    release_group_mbid TEXT NOT NULL,
    artist_mbid TEXT NOT NULL,
    release_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    ignored_at REAL NOT NULL,
    PRIMARY KEY (user_id, release_group_mbid_lower)
);
CREATE TABLE IF NOT EXISTS jellyfin_mbid_index (
    mbid_lower TEXT PRIMARY KEY,
    mbid TEXT NOT NULL,
    item_id TEXT NOT NULL,
    saved_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS navidrome_album_mbid_index (
    cache_key TEXT PRIMARY KEY,
    mbid TEXT,
    saved_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS navidrome_artist_mbid_index (
    cache_key TEXT PRIMARY KEY,
    mbid TEXT,
    saved_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS plex_album_mbid_index (
    cache_key TEXT PRIMARY KEY,
    mbid TEXT,
    saved_at REAL NOT NULL
);
CREATE TABLE IF NOT EXISTS plex_artist_mbid_index (
    cache_key TEXT PRIMARY KEY,
    mbid TEXT,
    saved_at REAL NOT NULL
);

-- Canonical MB store (owner: metadata).
CREATE TABLE IF NOT EXISTS canonical_redirect (
    entity_kind TEXT NOT NULL,
    from_mbid_lower TEXT NOT NULL,
    to_mbid_lower TEXT NOT NULL,
    source TEXT NOT NULL,
    source_host TEXT NOT NULL,
    first_seen_at REAL NOT NULL,
    last_confirmed_at REAL NOT NULL,
    source_mode TEXT NOT NULL DEFAULT '',
    source_id TEXT NOT NULL DEFAULT '',
    source_generation INTEGER NOT NULL DEFAULT 0,
    official_evidence INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (entity_kind, from_mbid_lower)
);
CREATE TABLE IF NOT EXISTS release_to_rg (
    release_mbid_lower TEXT PRIMARY KEY,
    rg_mbid TEXT NOT NULL DEFAULT '',
    source TEXT NOT NULL,
    source_host TEXT NOT NULL,
    official_evidence INTEGER NOT NULL DEFAULT 0,
    saved_at REAL NOT NULL,
    source_mode TEXT NOT NULL DEFAULT '',
    source_id TEXT NOT NULL DEFAULT '',
    source_generation INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS recording_isrc (
    isrc TEXT NOT NULL,
    recording_mbid_lower TEXT NOT NULL,
    first_seen_at REAL NOT NULL,
    source_mode TEXT NOT NULL DEFAULT '',
    source_id TEXT NOT NULL DEFAULT '',
    source_generation INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (isrc, recording_mbid_lower)
);

-- MB response cache (owner: metadata).
CREATE TABLE IF NOT EXISTS mb_response_epoch (
    singleton INTEGER PRIMARY KEY CHECK(singleton=1), epoch INTEGER NOT NULL
);
INSERT OR IGNORE INTO mb_response_epoch VALUES(1,0);
CREATE TABLE IF NOT EXISTS mb_responses (
    key TEXT PRIMARY KEY, source_mode TEXT NOT NULL, source_id TEXT NOT NULL,
    generation INTEGER NOT NULL, payload BLOB NOT NULL,
    fetched REAL NOT NULL, fresh REAL NOT NULL, retention REAL NOT NULL,
    accessed REAL NOT NULL, logical_bytes INTEGER NOT NULL,
    speculative INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS idx_mb_responses_retention ON mb_responses(retention);
CREATE INDEX IF NOT EXISTS idx_mb_responses_access ON mb_responses(accessed);

-- YouTube links (owner: enrichment).
CREATE TABLE IF NOT EXISTS youtube_links (
    album_id TEXT PRIMARY KEY,
    video_id TEXT,
    album_name TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    embed_url TEXT,
    cover_url TEXT,
    created_at TEXT NOT NULL,
    is_manual INTEGER DEFAULT 0,
    track_count INTEGER DEFAULT 0
);
CREATE TABLE IF NOT EXISTS youtube_track_links (
    album_id TEXT NOT NULL,
    track_number INTEGER NOT NULL,
    disc_number INTEGER NOT NULL DEFAULT 1,
    album_name TEXT NOT NULL,
    track_name TEXT NOT NULL,
    video_id TEXT NOT NULL,
    artist_name TEXT NOT NULL,
    embed_url TEXT NOT NULL,
    created_at TEXT NOT NULL,
    PRIMARY KEY (album_id, disc_number, track_number)
);

-- Sync state (owner: sync).
CREATE TABLE IF NOT EXISTS processed_items (
    item_type TEXT NOT NULL,
    mbid_lower TEXT NOT NULL,
    mbid TEXT NOT NULL,
    PRIMARY KEY (item_type, mbid_lower)
);
CREATE TABLE IF NOT EXISTS sync_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    state_json TEXT NOT NULL,
    updated_at REAL NOT NULL
);

-- Per-user connections, prefs, quotas (owner: users).
CREATE TABLE IF NOT EXISTS user_connections (
    user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE CASCADE,
    service TEXT NOT NULL,
    connection_data TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (user_id, service)
);

CREATE TABLE IF NOT EXISTS user_listening_prefs (
    user_id TEXT PRIMARY KEY REFERENCES auth_users(id) ON DELETE CASCADE,
    scrobble_to_lastfm INTEGER NOT NULL DEFAULT 0,
    scrobble_to_listenbrainz INTEGER NOT NULL DEFAULT 0,
    navidrome_handles_external_scrobbles INTEGER NOT NULL DEFAULT 1,
    primary_music_source TEXT NOT NULL DEFAULT 'listenbrainz',
    now_playing_visibility TEXT NOT NULL DEFAULT 'full',
    updated_at TEXT NOT NULL,
    auto_request_personal_mix INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS personal_mix_approvals (
    user_id          TEXT NOT NULL PRIMARY KEY
                     REFERENCES auth_users(id) ON DELETE CASCADE,
    state            TEXT NOT NULL DEFAULT 'pending',
    requested_at     REAL NOT NULL,
    reviewed_by_id   TEXT,
    reviewed_by_name TEXT,
    reviewed_at      REAL
);
CREATE INDEX IF NOT EXISTS idx_pma_pending
    ON personal_mix_approvals(state) WHERE state = 'pending';

CREATE TABLE IF NOT EXISTS user_quotas (
    user_id             TEXT PRIMARY KEY
                        REFERENCES auth_users(id) ON DELETE CASCADE,
    request_quota_count INTEGER,
    request_quota_days  INTEGER,
    storage_quota_gb    INTEGER
);

CREATE TABLE IF NOT EXISTS user_section_prefs (
    user_id     TEXT NOT NULL,
    page        TEXT NOT NULL,
    section_key TEXT NOT NULL,
    enabled     INTEGER NOT NULL DEFAULT 0,
    updated_at  TEXT NOT NULL,
    PRIMARY KEY (user_id, page, section_key)
);

CREATE TABLE IF NOT EXISTS user_navidrome_folder_preferences (
    user_id TEXT PRIMARY KEY
        REFERENCES auth_users(id) ON DELETE CASCADE,
    mode TEXT NOT NULL CHECK (mode IN ('all', 'selected')),
    selected_ids_json TEXT NOT NULL DEFAULT '[]',
    server_identity TEXT,
    updated_at REAL NOT NULL
);

-- Section 6: native catalog (owner: library).
-- Verbatim from v2 native_library_schema.py SCHEMA_SQL except:
-- the 12 dual-owner tables above (defined once in section 2), and
-- 15 ratchet-only columns folded in (provider_reset_count and
-- attention_cause on identification jobs; management_schedule_pending
-- on albums; release_type, tag_album_title, and
-- tag_album_artist_name on tracks; suggested_release_mbid,
-- suggested_release_group_mbid, suggested_edition_json on repair
-- findings; phase_started_at and phase_timings_json on scan runs;
-- ancillary_snapshot_json and before_management_state_json on
-- operation snapshots; provider_base_url on album and track
-- external identities).

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
    management_schedule_pending INTEGER NOT NULL DEFAULT 0,
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
    title_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(title_provenance IN ('tag','parsed','placeholder','absent')),
    album_title_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(album_title_provenance IN ('tag','parsed','placeholder','absent')),
    album_artist_provenance TEXT NOT NULL DEFAULT 'absent' CHECK(album_artist_provenance IN ('tag','parsed','placeholder','absent')),
    UNIQUE(root_id, relative_path)
);

CREATE TABLE IF NOT EXISTS local_album_artists (
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    join_phrase TEXT NOT NULL DEFAULT '',
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(local_album_id, position)
);

CREATE TABLE IF NOT EXISTS local_track_artists (
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    position INTEGER NOT NULL CHECK(position >= 0),
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    role TEXT NOT NULL,
    credited_name TEXT,
    join_phrase TEXT NOT NULL DEFAULT '',
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
    input_identity_revision TEXT NOT NULL DEFAULT '',
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
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
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
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
    provider_base_url TEXT,
    PRIMARY KEY(local_album_id, provider)
);

CREATE TABLE IF NOT EXISTS local_track_external_identities (
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
    selected_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    provider_source_mode TEXT,
    provider_source_id TEXT,
    provider_source_generation INTEGER,
    provider_base_url TEXT,
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

CREATE TABLE IF NOT EXISTS library_artist_credit_proofs (
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('album','track')),
    subject_id TEXT NOT NULL,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    credit_position INTEGER NOT NULL CHECK(credit_position >= 0),
    source_local_artist_id TEXT REFERENCES local_artists(id) ON DELETE RESTRICT,
    local_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    artist_mbid TEXT NOT NULL CHECK(length(trim(artist_mbid)) > 0),
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
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
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
);

CREATE TABLE IF NOT EXISTS library_artist_reconciliation_state (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    input_revision TEXT NOT NULL,
    evidence_hash TEXT,
    state TEXT NOT NULL CHECK(state IN (
        'waiting_for_identity','provider_conflict','ambiguous_credit_structure',
        'projected','resolved_automatically','provider_deferred'
    )),
    projected_album_credit_count INTEGER NOT NULL DEFAULT 0
        CHECK(projected_album_credit_count >= 0),
    projected_track_credit_count INTEGER NOT NULL DEFAULT 0
        CHECK(projected_track_credit_count >= 0),
    retired_artist_count INTEGER NOT NULL DEFAULT 0
        CHECK(retired_artist_count >= 0),
    operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_artist_reconciliation_dismissals (
    left_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    right_artist_id TEXT NOT NULL REFERENCES local_artists(id) ON DELETE RESTRICT,
    left_artist_revision INTEGER NOT NULL
        CHECK(left_artist_revision BETWEEN 1 AND 9223372036854775807),
    right_artist_revision INTEGER NOT NULL
        CHECK(right_artist_revision BETWEEN 1 AND 9223372036854775807),
    dismissed_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    reason_code TEXT NOT NULL DEFAULT 'MARKED_DISTINCT',
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(left_artist_id, right_artist_id),
    CHECK(left_artist_id < right_artist_id)
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
    partial_decode INTEGER NOT NULL DEFAULT 0 CHECK(partial_decode IN (0,1)),
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
    state TEXT NOT NULL CHECK(state IN ('needs_review','keep_tagged','excluded','resolved','edition_to_confirm')),
    reason_code TEXT NOT NULL,
    attempt_id TEXT REFERENCES library_identification_attempts(id) ON DELETE RESTRICT,
    input_revision TEXT NOT NULL,
    decision_revision INTEGER NOT NULL DEFAULT 1 CHECK(decision_revision BETWEEN 1 AND 9223372036854775807),
    decided_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    decided_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    edition_uncertain INTEGER NOT NULL DEFAULT 0 CHECK(edition_uncertain IN (0,1)),
    ranked_edition_keys_json TEXT NOT NULL DEFAULT '[]',
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
    provider_reset_count INTEGER NOT NULL DEFAULT 0,
    attention_cause TEXT,
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
    lease_expires_at REAL,
    heartbeat_at REAL,
    next_attempt_at REAL,
    reidentification_attempt_count INTEGER NOT NULL DEFAULT 0
        CHECK(reidentification_attempt_count >= 0),
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

CREATE TABLE IF NOT EXISTS library_operation_control_idempotency (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    job_id TEXT NOT NULL REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    control TEXT NOT NULL CHECK(control IN ('pause','resume','stop')),
    requested_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS local_track_genres (
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
);

CREATE TABLE IF NOT EXISTS library_management_blobs (
    sha256 TEXT PRIMARY KEY
        CHECK(length(sha256) = 64 AND sha256 = lower(sha256)
              AND sha256 NOT GLOB '*[^0-9a-f]*'),
    kind TEXT NOT NULL CHECK(kind IN (
        'tag_snapshot','image','sidecar_manifest','metadata_document'
    )),
    byte_length INTEGER NOT NULL CHECK(byte_length >= 0),
    relative_path TEXT NOT NULL CHECK(length(trim(relative_path)) > 0),
    media_metadata_json TEXT NOT NULL DEFAULT '{}',
    created_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_management_blob_references (
    blob_sha256 TEXT NOT NULL REFERENCES library_management_blobs(sha256) ON DELETE RESTRICT,
    reference_kind TEXT NOT NULL CHECK(reference_kind IN (
        'baseline','operation_snapshot','artwork','sidecar','metadata_snapshot'
    )),
    reference_id TEXT NOT NULL CHECK(length(trim(reference_id)) > 0),
    created_at REAL NOT NULL,
    PRIMARY KEY(blob_sha256, reference_kind, reference_id)
);

CREATE TABLE IF NOT EXISTS library_management_baselines (
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
    created_at REAL NOT NULL,
    restore_status TEXT NOT NULL DEFAULT 'available' CHECK(restore_status IN (
        'available','restoring','restored','stale','purged'
    )),
    last_verified_at REAL,
    catalog_document_json TEXT,
    catalog_document_hash TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_track_management_state (
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
    last_managed_at REAL,
    last_outcome TEXT,
    last_reason_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_management_baseline_purges (
    idempotency_key TEXT PRIMARY KEY CHECK(length(trim(idempotency_key)) > 0),
    impact_token TEXT NOT NULL CHECK(length(trim(impact_token)) > 0),
    actor_user_id TEXT,
    purged_baseline_count INTEGER NOT NULL CHECK(purged_baseline_count >= 0),
    detached_reference_count INTEGER NOT NULL CHECK(detached_reference_count >= 0),
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_management_overrides (
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
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(
        (subject_kind = 'album' AND local_album_id IS NOT NULL AND local_track_id IS NULL)
        OR
        (subject_kind = 'track' AND local_track_id IS NOT NULL AND local_album_id IS NULL)
    )
);

CREATE TABLE IF NOT EXISTS library_management_metadata_snapshots (
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
    fetched_at REAL NOT NULL,
    expires_at REAL,
    provider_version_notes TEXT,
    UNIQUE(provider, entity_kind, entity_id, input_hash, payload_sha256)
);

CREATE TABLE IF NOT EXISTS library_management_job_snapshots (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
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
    preview_created_at REAL,
    preview_expires_at REAL,
    apply_idempotency_key TEXT,
    target_root_id TEXT,
    linked_operation_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    intent_json TEXT NOT NULL DEFAULT '{}',
    summary_json TEXT NOT NULL DEFAULT '{}',
    warnings_json TEXT NOT NULL DEFAULT '[]',
    staging_cursor TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_management_apply_idempotency
ON library_management_job_snapshots(apply_idempotency_key)
WHERE apply_idempotency_key IS NOT NULL;

CREATE TABLE IF NOT EXISTS library_management_external_refresh_deliveries (
    id TEXT PRIMARY KEY,
    operation_job_id TEXT NOT NULL
        REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    target TEXT NOT NULL CHECK(target IN ('plex','jellyfin','navidrome')),
    state TEXT NOT NULL CHECK(state IN (
        'pending','delivering','retry_wait','succeeded','failed','unavailable'
    )),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 21),
    max_attempts INTEGER NOT NULL CHECK(max_attempts BETWEEN 1 AND 21),
    retry_delay_seconds INTEGER NOT NULL CHECK(retry_delay_seconds BETWEEN 1 AND 3600),
    not_before REAL NOT NULL DEFAULT 0,
    lease_owner TEXT,
    lease_expires_at REAL,
    failure_code TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    completed_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(operation_job_id, target)
);

CREATE TABLE IF NOT EXISTS library_management_import_bundles (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE CHECK(length(trim(idempotency_key)) > 0),
    origin TEXT NOT NULL CHECK(origin IN ('acquisition','drop_import')),
    policy_revision TEXT NOT NULL,
    request_json TEXT NOT NULL,
    request_hash TEXT NOT NULL
        CHECK(length(request_hash) = 64 AND request_hash = lower(request_hash)
              AND request_hash NOT GLOB '*[^0-9a-f]*'),
    state TEXT NOT NULL CHECK(state IN (
        'preparing','publishing','catalog_committed','cleanup_pending','completed',
        'rolled_back','needs_attention','resolved'
    )),
    result_json TEXT NOT NULL DEFAULT '{}',
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_management_import_journal (
    bundle_id TEXT NOT NULL
        REFERENCES library_management_import_bundles(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    state TEXT NOT NULL CHECK(state IN (
        'planned','staged','validated','replacement_backed_up','published',
        'catalog_committed','cleanup_pending','completed','rollback_pending',
        'rolled_back','needs_attention','resolved'
    )),
    source_fingerprint TEXT NOT NULL
        CHECK(length(source_fingerprint) = 64
              AND source_fingerprint = lower(source_fingerprint)
              AND source_fingerprint NOT GLOB '*[^0-9a-f]*'),
    source_size INTEGER NOT NULL CHECK(source_size >= 0),
    source_mtime_ns INTEGER NOT NULL,
    temporary_relative_path TEXT NOT NULL,
    destination_root_id TEXT NOT NULL,
    destination_relative_path TEXT NOT NULL,
    staged_fingerprint TEXT,
    replacement_fingerprint TEXT,
    replacement_backup_relative_path TEXT,
    baseline_blob_sha256 TEXT REFERENCES library_management_blobs(sha256) ON DELETE RESTRICT,
    baseline_format TEXT,
    baseline_adapter_version TEXT,
    baseline_stat_revision TEXT,
    baseline_tag_revision TEXT,
    baseline_image_snapshot_json TEXT NOT NULL DEFAULT '[]',
    baseline_ancillary_snapshot_json TEXT NOT NULL DEFAULT '[]',
    baseline_file_mtime_ns INTEGER,
    baseline_file_mode INTEGER,
    failure_code TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    PRIMARY KEY(bundle_id, ordinal)
);

CREATE TABLE IF NOT EXISTS library_management_job_metadata_snapshots (
    job_id TEXT NOT NULL
        REFERENCES library_management_job_snapshots(job_id) ON DELETE CASCADE,
    metadata_snapshot_id TEXT NOT NULL
        REFERENCES library_management_metadata_snapshots(id) ON DELETE RESTRICT,
    PRIMARY KEY(job_id, metadata_snapshot_id)
);

CREATE TABLE IF NOT EXISTS library_management_plan_items (
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
    created_at REAL NOT NULL,
    PRIMARY KEY(job_id, ordinal),
    CHECK(local_album_id IS NOT NULL OR local_track_id IS NOT NULL)
);

CREATE TABLE IF NOT EXISTS library_management_operation_snapshots (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    work_ordinal INTEGER NOT NULL,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    before_root_id TEXT NOT NULL,
    before_relative_path TEXT NOT NULL,
    after_root_id TEXT,
    after_relative_path TEXT,
    format TEXT NOT NULL,
    adapter_version TEXT NOT NULL,
    semantic_snapshot_blob_sha256 TEXT NOT NULL
        REFERENCES library_management_blobs(sha256) ON DELETE RESTRICT,
    image_snapshot_json TEXT NOT NULL DEFAULT '[]',
    file_mtime_ns INTEGER,
    file_mode INTEGER,
    ancillary_snapshot_json TEXT NOT NULL DEFAULT '[]',
    before_management_state_json TEXT NOT NULL DEFAULT '{}',
    source_fingerprint TEXT NOT NULL,
    created_at REAL NOT NULL,
    expires_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, work_ordinal, local_track_id),
    CHECK(expires_at >= created_at),
    FOREIGN KEY(job_id, work_ordinal)
        REFERENCES library_operation_work(job_id, ordinal) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS library_file_mutation_journal (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    plan_item_ordinal INTEGER NOT NULL,
    subject_kind TEXT NOT NULL CHECK(subject_kind IN ('audio','sidecar','external_art')),
    subject_key TEXT NOT NULL,
    local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    source_root_id TEXT,
    source_relative_path TEXT,
    temporary_root_id TEXT,
    temporary_relative_path TEXT,
    backup_root_id TEXT,
    backup_relative_path TEXT,
    destination_root_id TEXT,
    destination_relative_path TEXT,
    source_fingerprint TEXT,
    staged_fingerprint TEXT,
    baseline_id TEXT REFERENCES library_management_baselines(id) ON DELETE RESTRICT,
    operation_snapshot_id TEXT
        REFERENCES library_management_operation_snapshots(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK(state IN (
        'planned','snapshot_saved','staged','validated','source_backed_up','published',
        'catalog_committed','cleanup_pending','completed','rollback_pending',
        'rolled_back','needs_attention'
    )),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK(attempts >= 0),
    failure_code TEXT,
    recovery_evidence_json TEXT NOT NULL DEFAULT '{}',
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, plan_item_ordinal, subject_kind, subject_key),
    CHECK(subject_kind != 'audio' OR local_track_id IS NOT NULL),
    FOREIGN KEY(job_id, plan_item_ordinal)
        REFERENCES library_management_plan_items(job_id, ordinal) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS library_management_collision_evidence (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    plan_item_ordinal INTEGER NOT NULL,
    classification TEXT NOT NULL CHECK(classification IN (
        'same_catalog_track_same_content','same_path_same_content',
        'same_path_different_content','same_release_position_different_content',
        'normalized_path_collision','sidecar_collision','destination_created_after_preview'
    )),
    existing_local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    destination_root_id TEXT NOT NULL,
    destination_relative_path TEXT NOT NULL,
    evidence_json TEXT NOT NULL,
    created_at REAL NOT NULL,
    FOREIGN KEY(job_id, plan_item_ordinal)
        REFERENCES library_management_plan_items(job_id, ordinal) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_bulk_review_snapshots (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    selection_json TEXT NOT NULL,
    normalized_filter_json TEXT,
    preview_token TEXT NOT NULL,
    staging_state TEXT NOT NULL DEFAULT 'ready' CHECK(staging_state IN ('staging','ready')),
    staging_cursor INTEGER NOT NULL DEFAULT -1 CHECK(staging_cursor >= -1),
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_bulk_review_previews (
    preview_token TEXT PRIMARY KEY,
    action TEXT NOT NULL,
    selection_json TEXT NOT NULL,
    normalized_filter_json TEXT,
    catalog_revision INTEGER,
    requires_local_metadata_confirmation INTEGER NOT NULL DEFAULT 0 CHECK(requires_local_metadata_confirmation IN (0,1)),
    state TEXT NOT NULL DEFAULT 'ready' CHECK(state IN ('staging','ready')),
    summary_json TEXT NOT NULL DEFAULT '{}',
    cursor_updated_at REAL,
    cursor_review_id TEXT,
    subject_count INTEGER NOT NULL DEFAULT 0 CHECK(subject_count >= 0),
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
    expected_identity_revision TEXT NOT NULL DEFAULT '',
    one_off_local_metadata INTEGER NOT NULL DEFAULT 0 CHECK(one_off_local_metadata IN (0,1)),
    requested_release_mbid TEXT,
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

-- (GH-293) Durable keyset materialization state for catalog-wide repair jobs.
-- The job header is created first; work rows are then materialized in pages of
-- at most 500 subjects per transaction, each page atomically advancing the
-- keyset cursor, the staged ordinal/count, and the sealed marker. A crash before
-- or after a page commit resumes from the cursor without omission or
-- duplication. Sealing fixes the materialized subject set: catalog changes after
-- the pinned boundary require a new or versioned job.
CREATE TABLE IF NOT EXISTS library_repair_materialization (
    job_id TEXT PRIMARY KEY REFERENCES library_operation_jobs(id) ON DELETE CASCADE,
    pinned_catalog_revision INTEGER NOT NULL
        CHECK(pinned_catalog_revision BETWEEN 0 AND 9223372036854775807),
    eligibility_version TEXT NOT NULL CHECK(length(trim(eligibility_version)) > 0),
    purpose TEXT NOT NULL CHECK(length(trim(purpose)) > 0),
    staging_cursor TEXT,
    staged_ordinal INTEGER NOT NULL DEFAULT -1 CHECK(staged_ordinal >= -1),
    staged_count INTEGER NOT NULL DEFAULT 0 CHECK(staged_count >= 0),
    sealed INTEGER NOT NULL DEFAULT 0 CHECK(sealed IN (0,1)),
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL
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
    suggested_release_mbid TEXT,
    suggested_release_group_mbid TEXT,
    suggested_edition_json TEXT NOT NULL DEFAULT '{}',
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

CREATE TABLE IF NOT EXISTS library_automatic_edition_undo (
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
    created_at REAL NOT NULL,
    consumed_at REAL,
    consumed_action_id TEXT
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
    phase_started_at REAL,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
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

CREATE TABLE IF NOT EXISTS library_scan_failures (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    root_id TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    failure_code TEXT NOT NULL,
    failure_detail TEXT NOT NULL DEFAULT '',
    phase TEXT NOT NULL CHECK(phase IN ('discovering','indexing','reconciling')),
    recorded_at REAL NOT NULL,
    PRIMARY KEY(run_id, root_id, relative_path, phase, failure_code)
);

CREATE TABLE IF NOT EXISTS library_scan_management_candidates (
    run_id TEXT NOT NULL REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE CASCADE,
    state TEXT NOT NULL DEFAULT 'pending' CHECK(state IN ('pending','completed')),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    next_attempt_at REAL NOT NULL,
    last_attempt_at REAL,
    completed_at REAL,
    PRIMARY KEY(run_id, local_album_id)
);

CREATE TABLE IF NOT EXISTS library_scan_management_staging (
    run_id TEXT PRIMARY KEY REFERENCES library_scan_runs(id) ON DELETE CASCADE,
    staged_at REAL NOT NULL
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

INSERT OR IGNORE INTO library_catalog_revision(singleton, value) VALUES (1, 0);

CREATE TABLE IF NOT EXISTS library_foreign_key_validation_state (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    schema_sha256 TEXT NOT NULL DEFAULT '',
    validator_revision INTEGER NOT NULL DEFAULT 0 CHECK(validator_revision >= 0),
    clean INTEGER NOT NULL DEFAULT 0 CHECK(clean IN (0, 1)),
    validated_at REAL
);

INSERT OR IGNORE INTO library_foreign_key_validation_state(
    singleton, schema_sha256, validator_revision, clean, validated_at
) VALUES (1, '', 0, 0, NULL);

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

CREATE TABLE IF NOT EXISTS local_entity_source_links (
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
    verified_at REAL NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    CHECK(
        (local_artist_id IS NOT NULL) +
        (local_album_id IS NOT NULL) +
        (local_track_id IS NOT NULL) = 1
    )
);

CREATE TABLE IF NOT EXISTS library_contribution_drafts (
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
    provider_snapshot_expires_at REAL,
    duplicate_result_json TEXT,
    duplicate_checked_at REAL,
    duplicate_input_revision TEXT,
    result_release_mbid TEXT,
    result_source TEXT CHECK(result_source IN ('callback', 'manual') OR result_source IS NULL),
    result_received_at REAL,
    seed_snapshot_json TEXT,
    seed_hash TEXT,
    seeded_at REAL,
    terminal_at REAL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807)
);

CREATE TABLE IF NOT EXISTS library_contribution_callback_tokens (
    token_hash TEXT PRIMARY KEY,
    contribution_id TEXT NOT NULL
        REFERENCES library_contribution_drafts(id) ON DELETE CASCADE,
    requested_by_user_id TEXT NOT NULL
        REFERENCES auth_users(id) ON DELETE CASCADE,
    expires_at REAL NOT NULL,
    consumed_at REAL,
    created_at REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS library_contribution_verification_jobs (
    id TEXT PRIMARY KEY,
    contribution_id TEXT NOT NULL
        REFERENCES library_contribution_drafts(id) ON DELETE CASCADE,
    state TEXT NOT NULL CHECK(state IN (
        'queued', 'running', 'succeeded', 'needs_review', 'failed', 'cancelled'
    )),
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK(attempt_count >= 0),
    not_before REAL NOT NULL DEFAULT 0,
    requested_by_user_id TEXT REFERENCES auth_users(id) ON DELETE SET NULL,
    last_failure_code TEXT,
    lease_owner TEXT,
    lease_expires_at REAL,
    heartbeat_at REAL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    terminal_at REAL,
    row_revision INTEGER NOT NULL DEFAULT 1
        CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    event_revision INTEGER NOT NULL DEFAULT 0
        CHECK(event_revision BETWEEN 0 AND 9223372036854775807)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_source_artist_unique
ON local_entity_source_links(local_artist_id, provider, external_entity_type, external_id)
WHERE local_artist_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_source_album_unique
ON local_entity_source_links(local_album_id, provider, external_entity_type, external_id)
WHERE local_album_id IS NOT NULL;

CREATE UNIQUE INDEX IF NOT EXISTS idx_entity_source_track_unique
ON local_entity_source_links(local_track_id, provider, external_entity_type, external_id)
WHERE local_track_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_entity_source_provider
ON local_entity_source_links(provider, external_entity_type, external_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_contribution_active_album
ON library_contribution_drafts(local_album_id)
WHERE state NOT IN ('linked', 'cancelled', 'stale');

CREATE INDEX IF NOT EXISTS idx_contribution_album_updated
ON library_contribution_drafts(local_album_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_contribution_callback_expiry
ON library_contribution_callback_tokens(expires_at, consumed_at);

CREATE UNIQUE INDEX IF NOT EXISTS idx_contribution_job_active
ON library_contribution_verification_jobs(contribution_id)
WHERE state IN ('queued', 'running');

CREATE INDEX IF NOT EXISTS idx_contribution_job_claim
ON library_contribution_verification_jobs(state, not_before, created_at);

CREATE INDEX IF NOT EXISTS idx_contribution_job_lease
ON library_contribution_verification_jobs(state, lease_expires_at);

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

CREATE INDEX IF NOT EXISTS idx_local_tracks_recent ON local_tracks(availability, imported_at DESC, id DESC);

CREATE INDEX IF NOT EXISTS idx_local_album_artists_reverse ON local_album_artists(local_artist_id, local_album_id);

CREATE INDEX IF NOT EXISTS idx_local_track_artists_reverse ON local_track_artists(local_artist_id, local_track_id);

CREATE INDEX IF NOT EXISTS idx_local_album_identity_rg ON local_album_external_identities(release_group_mbid);

CREATE INDEX IF NOT EXISTS idx_local_album_identity_rg_lower ON local_album_external_identities(lower(release_group_mbid));

CREATE INDEX IF NOT EXISTS idx_local_album_identity_release_lower ON local_album_external_identities(lower(release_mbid));

CREATE INDEX IF NOT EXISTS idx_local_artist_identity_provider_lower ON local_artist_external_identities(lower(provider_artist_id));

CREATE INDEX IF NOT EXISTS idx_local_track_identity_recording ON local_track_external_identities(recording_mbid);

CREATE INDEX IF NOT EXISTS idx_local_track_genres_folded ON local_track_genres(folded_name, local_track_id);

CREATE INDEX IF NOT EXISTS idx_local_track_genres_source ON local_track_genres(source, local_track_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_management_override_album_field
ON library_management_overrides(local_album_id, field_name)
WHERE subject_kind = 'album';

CREATE UNIQUE INDEX IF NOT EXISTS idx_management_override_track_field
ON library_management_overrides(local_track_id, field_name)
WHERE subject_kind = 'track';

CREATE INDEX IF NOT EXISTS idx_management_metadata_lookup
ON library_management_metadata_snapshots(provider, entity_kind, entity_id, input_hash);

CREATE INDEX IF NOT EXISTS idx_management_metadata_expiry
ON library_management_metadata_snapshots(expires_at, id);

CREATE INDEX IF NOT EXISTS idx_management_blob_references_reference
ON library_management_blob_references(reference_kind, reference_id, blob_sha256);

CREATE INDEX IF NOT EXISTS idx_management_baseline_track
ON library_management_baselines(local_track_id);

CREATE INDEX IF NOT EXISTS idx_management_operation_snapshot_expiry
ON library_management_operation_snapshots(expires_at, id);

CREATE INDEX IF NOT EXISTS idx_management_plan_cursor
ON library_management_plan_items(job_id, ordinal);

CREATE INDEX IF NOT EXISTS idx_management_plan_eligibility
ON library_management_plan_items(job_id, eligibility, ordinal);

CREATE INDEX IF NOT EXISTS idx_management_plan_track
ON library_management_plan_items(job_id, local_track_id);

CREATE INDEX IF NOT EXISTS idx_management_journal_recovery
ON library_file_mutation_journal(state, updated_at, id);

CREATE INDEX IF NOT EXISTS idx_management_journal_job
ON library_file_mutation_journal(job_id, plan_item_ordinal, state);

CREATE INDEX IF NOT EXISTS idx_management_collision_job
ON library_management_collision_evidence(job_id, plan_item_ordinal, classification);

CREATE INDEX IF NOT EXISTS idx_management_external_refresh_claim
ON library_management_external_refresh_deliveries(state, not_before, created_at, id);

CREATE INDEX IF NOT EXISTS idx_management_external_refresh_lease
ON library_management_external_refresh_deliveries(state, lease_expires_at);

CREATE INDEX IF NOT EXISTS idx_album_alias_target ON local_album_aliases(local_album_id);

CREATE INDEX IF NOT EXISTS idx_artist_alias_target ON local_artist_aliases(local_artist_id);

CREATE INDEX IF NOT EXISTS idx_artist_credit_proof_source
ON library_artist_credit_proofs(source_local_artist_id, artist_mbid);

CREATE INDEX IF NOT EXISTS idx_artist_credit_proof_resolved
ON library_artist_credit_proofs(local_artist_id, artist_mbid);

CREATE INDEX IF NOT EXISTS idx_artist_credit_proof_album
ON library_artist_credit_proofs(local_album_id, subject_kind, subject_id);

CREATE INDEX IF NOT EXISTS idx_artist_reconciliation_state_status
ON library_artist_reconciliation_state(state, updated_at DESC, local_album_id);

CREATE INDEX IF NOT EXISTS idx_artist_reconciliation_dismissal_user
ON library_artist_reconciliation_dismissals(dismissed_by_user_id, updated_at DESC);

CREATE INDEX IF NOT EXISTS idx_identification_attempt_subject_album ON library_identification_attempts(local_album_id, completed_at);

CREATE INDEX IF NOT EXISTS idx_identification_attempt_subject_track ON library_identification_attempts(local_track_id, completed_at);

CREATE INDEX IF NOT EXISTS idx_identification_evidence_attempt ON library_identification_evidence(attempt_id);

CREATE INDEX IF NOT EXISTS idx_identification_jobs_claim ON library_identification_jobs(state, not_before, priority, enqueue_sequence);

CREATE INDEX IF NOT EXISTS idx_identification_jobs_lease ON library_identification_jobs(state, lease_expires_at);

CREATE INDEX IF NOT EXISTS idx_identification_jobs_terminal
ON library_identification_jobs(state, terminal_at DESC, id DESC);

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

CREATE INDEX IF NOT EXISTS idx_scan_failures_run ON library_scan_failures(run_id);

CREATE INDEX IF NOT EXISTS idx_scan_inventory_management_candidates ON library_scan_inventory(run_id, processing_state, comparison_result, local_track_id);

CREATE INDEX IF NOT EXISTS idx_scan_management_candidates_due ON library_scan_management_candidates(state, next_attempt_at, run_id, local_album_id);

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

CREATE TABLE IF NOT EXISTS library_custom_edition_manifests (
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
    sealed_at REAL NOT NULL,
    UNIQUE(local_album_id, version),
    UNIQUE(local_album_id, content_hash)
);

CREATE TABLE IF NOT EXISTS library_custom_edition_tracks (
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
);

CREATE TABLE IF NOT EXISTS library_custom_edition_active (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    manifest_id TEXT NOT NULL UNIQUE REFERENCES library_custom_edition_manifests(id) ON DELETE RESTRICT,
    activated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
);

CREATE TABLE IF NOT EXISTS library_management_exclusions (
    local_album_id TEXT PRIMARY KEY REFERENCES local_albums(id) ON DELETE RESTRICT,
    reason TEXT NOT NULL,
    excluded_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    excluded_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
);

CREATE TABLE IF NOT EXISTS library_edition_conversion_jobs (
    id TEXT PRIMARY KEY,
    local_album_id TEXT NOT NULL REFERENCES local_albums(id) ON DELETE RESTRICT,
    target_release_group_mbid TEXT NOT NULL,
    target_release_mbid TEXT NOT NULL,
    target_album_title TEXT NOT NULL,
    target_artist_name TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN (
        'preflight','acquiring','ready','needs_recheck','cancelled','failed','applied'
    )),
    expected_album_revision INTEGER NOT NULL CHECK(expected_album_revision > 0),
    expected_input_revision TEXT NOT NULL,
    expected_identity_revision TEXT NOT NULL,
    preflight_token_hash TEXT NOT NULL,
    download_source_ready INTEGER NOT NULL CHECK(download_source_ready IN (0,1)),
    required_temporary_bytes INTEGER NOT NULL DEFAULT 0 CHECK(required_temporary_bytes >= 0),
    kept_count INTEGER NOT NULL DEFAULT 0 CHECK(kept_count >= 0),
    acquire_count INTEGER NOT NULL DEFAULT 0 CHECK(acquire_count >= 0),
    recycle_count INTEGER NOT NULL DEFAULT 0 CHECK(recycle_count >= 0),
    staged_count INTEGER NOT NULL DEFAULT 0 CHECK(staged_count >= 0),
    failed_count INTEGER NOT NULL DEFAULT 0 CHECK(failed_count >= 0),
    final_preview_job_id TEXT REFERENCES library_operation_jobs(id) ON DELETE RESTRICT,
    final_preview_token_hash TEXT,
    final_bundle_json TEXT,
    final_bundle_hash TEXT,
    requested_by_user_id TEXT NOT NULL REFERENCES auth_users(id) ON DELETE RESTRICT,
    error_code TEXT,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0)
);

CREATE TABLE IF NOT EXISTS library_edition_conversion_targets (
    job_id TEXT NOT NULL REFERENCES library_edition_conversion_jobs(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL CHECK(ordinal >= 0),
    disc_number INTEGER NOT NULL CHECK(disc_number > 0),
    track_number INTEGER NOT NULL CHECK(track_number > 0),
    release_track_mbid TEXT NOT NULL,
    recording_mbid TEXT NOT NULL,
    title TEXT NOT NULL,
    duration_seconds REAL,
    state TEXT NOT NULL CHECK(state IN ('kept','pending','downloading','staged','failed')),
    kept_local_track_id TEXT REFERENCES local_tracks(id) ON DELETE RESTRICT,
    staged_artifact_id TEXT,
    failure_code TEXT,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision > 0),
    PRIMARY KEY(job_id, ordinal),
    UNIQUE(job_id, disc_number, track_number),
    UNIQUE(job_id, release_track_mbid)
);

CREATE TABLE IF NOT EXISTS library_edition_conversion_local_files (
    job_id TEXT NOT NULL REFERENCES library_edition_conversion_jobs(id) ON DELETE CASCADE,
    local_track_id TEXT NOT NULL REFERENCES local_tracks(id) ON DELETE RESTRICT,
    action TEXT NOT NULL CHECK(action IN (
        'keep','recycle_conflict','recycle_duplicate','recycle_extra'
    )),
    target_ordinal INTEGER,
    evidence_kind TEXT NOT NULL,
    expected_track_revision INTEGER NOT NULL CHECK(expected_track_revision > 0),
    expected_identity_revision INTEGER CHECK(expected_identity_revision IS NULL OR expected_identity_revision > 0),
    expected_stat_revision TEXT NOT NULL,
    PRIMARY KEY(job_id, local_track_id),
    FOREIGN KEY(job_id, target_ordinal)
        REFERENCES library_edition_conversion_targets(job_id, ordinal) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS library_edition_conversion_artifacts (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    target_ordinal INTEGER NOT NULL,
    held_path TEXT NOT NULL UNIQUE,
    file_sha256 TEXT NOT NULL,
    fingerprint TEXT,
    release_track_mbid TEXT NOT NULL,
    recording_mbid TEXT NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('download','free_music','retained_copy')),
    source_task_id TEXT,
    file_size_bytes INTEGER NOT NULL CHECK(file_size_bytes >= 0),
    created_at REAL NOT NULL,
    UNIQUE(job_id, target_ordinal),
    FOREIGN KEY(job_id, target_ordinal)
        REFERENCES library_edition_conversion_targets(job_id, ordinal) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS library_edition_conversion_downloads (
    job_id TEXT NOT NULL,
    target_ordinal INTEGER NOT NULL,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('download','free_music')),
    task_id TEXT NOT NULL,
    state TEXT NOT NULL,
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    PRIMARY KEY(job_id, target_ordinal, task_id),
    UNIQUE(source_kind, task_id),
    FOREIGN KEY(job_id, target_ordinal)
        REFERENCES library_edition_conversion_targets(job_id, ordinal) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_custom_edition_album
ON library_custom_edition_manifests(local_album_id, version DESC);

CREATE INDEX IF NOT EXISTS idx_custom_edition_track_local
ON library_custom_edition_tracks(local_track_id);

CREATE UNIQUE INDEX IF NOT EXISTS idx_edition_conversion_active_album
ON library_edition_conversion_jobs(local_album_id)
WHERE state IN ('preflight','acquiring','ready','needs_recheck');

CREATE INDEX IF NOT EXISTS idx_edition_conversion_download_task
ON library_edition_conversion_downloads(source_kind, task_id);

CREATE TRIGGER IF NOT EXISTS trg_custom_edition_manifest_immutable_update
BEFORE UPDATE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_custom_edition_manifest_immutable_delete
BEFORE DELETE ON library_custom_edition_manifests
BEGIN
    SELECT RAISE(ABORT, 'custom edition manifests are immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_custom_edition_tracks_immutable_update
BEFORE UPDATE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_custom_edition_tracks_immutable_delete
BEFORE DELETE ON library_custom_edition_tracks
BEGIN
    SELECT RAISE(ABORT, 'custom edition tracks are immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_management_metadata_snapshot_immutable
BEFORE UPDATE ON library_management_metadata_snapshots
BEGIN
    SELECT RAISE(ABORT, 'management metadata snapshots are immutable');
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_normalized_insert
AFTER INSERT ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (NEW.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_normalized_delete
AFTER DELETE ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_normalized_update
AFTER UPDATE OF name, folded_name ON local_track_genres
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (OLD.folded_name, 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT NEW.folded_name, 1 WHERE NEW.folded_name != OLD.folded_name
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_normalized_genres_legacy_scalar_update
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

-- Section 8: native extras kept outside the schema file in v2.
-- Three indexes the store created after its ratchets (the fourth,
-- idx_management_apply_idempotency, already lives in section 6), the seven
-- genre-artwork revision triggers, the two migration staging tables with
-- their indexes, and the two sentinel artists.

CREATE INDEX IF NOT EXISTS idx_management_plan_destination
    ON library_management_plan_items(
        job_id, destination_root_id, destination_collision_key, ordinal);
CREATE INDEX IF NOT EXISTS idx_local_tracks_genre_artwork
    ON local_tracks(genre_folded, availability, local_album_id);
CREATE INDEX IF NOT EXISTS idx_local_track_identity_release_track
    ON local_track_external_identities(release_track_mbid);

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_insert
AFTER INSERT ON local_tracks
WHEN NEW.genre IS NOT NULL AND trim(NEW.genre) != ''
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(NEW.genre_folded, lower(trim(NEW.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_delete
AFTER DELETE ON local_tracks
WHEN OLD.genre IS NOT NULL AND trim(OLD.genre) != ''
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    VALUES (COALESCE(OLD.genre_folded, lower(trim(OLD.genre))), 1)
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_track_update
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

CREATE TRIGGER IF NOT EXISTS trg_genre_artwork_album_update
AFTER UPDATE OF title, album_artist_id, album_artist_name, retired_into_album_id
ON local_albums
BEGIN
    INSERT INTO library_genre_artwork_revisions(genre_folded, value)
    SELECT DISTINCT genre.folded_name, 1
    FROM local_tracks t
    JOIN local_track_genres genre ON genre.local_track_id = t.id
    WHERE t.local_album_id = NEW.id
      AND t.availability = 'indexed'
    ON CONFLICT(genre_folded) DO UPDATE SET value = value + 1;
END;

CREATE TABLE IF NOT EXISTS library_migration_file_staging (
    source_id TEXT PRIMARY KEY, group_key TEXT NOT NULL,
    root_id TEXT NOT NULL, relative_path TEXT NOT NULL,
    directory_key TEXT NOT NULL, album_key TEXT NOT NULL,
    artist_key TEXT NOT NULL, track_number INTEGER NOT NULL,
    legacy_release_key TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_group
    ON library_migration_file_staging(group_key, source_id);
CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_directory
    ON library_migration_file_staging(
        root_id, directory_key, album_key, group_key, track_number);
CREATE INDEX IF NOT EXISTS idx_library_migration_file_staging_release
    ON library_migration_file_staging(legacy_release_key, group_key);

CREATE TABLE IF NOT EXISTS library_migration_review_staging (
    source_rowid INTEGER PRIMARY KEY, group_key TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS idx_library_migration_review_staging_group
    ON library_migration_review_staging(group_key, source_rowid);

INSERT OR IGNORE INTO local_artists
    (id, display_name, folded_name, normalized_name, kind, created_at, updated_at)
VALUES
    ('00000000-0000-4000-8000-000000000001', 'Various Artists',
     'various artists', 'various artists', 'various_artists', 0, 0),
    ('00000000-0000-4000-8000-000000000002', 'Unknown Artist',
     'unknown artist', 'unknown artist', 'unknown', 0, 0);

-- Section 7: durable-worker fabric (new in v3, no v2 predecessor).
-- Owner: runtime. The wakeup channels (scan, identification, operation,
-- contribution) are in-process signals between the runtime and its workers;
-- the wakeups table records the latest request per channel so restarts and
-- the admin health endpoint can observe pending demand. The registry names
-- every background job and its liveness; durable jobs keep their progress
-- in their domain tables, this table only tracks that they are alive.

CREATE TABLE IF NOT EXISTS durable_work_wakeups (
    channel TEXT PRIMARY KEY
        CHECK(channel IN ('scan','identification','operation','contribution')),
    seq INTEGER NOT NULL DEFAULT 0 CHECK(seq >= 0),
    requested_at REAL,
    consumed_seq INTEGER NOT NULL DEFAULT 0 CHECK(consumed_seq >= 0),
    updated_at REAL NOT NULL DEFAULT 0
);
INSERT OR IGNORE INTO durable_work_wakeups (channel, updated_at) VALUES
    ('scan', 0), ('identification', 0), ('operation', 0), ('contribution', 0);

CREATE TABLE IF NOT EXISTS durable_job_registry (
    name TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK(kind IN ('durable','ephemeral')),
    wakeup_channel TEXT
        CHECK(wakeup_channel IN ('scan','identification','operation','contribution')),
    state TEXT NOT NULL DEFAULT 'idle'
        CHECK(state IN ('idle','running','stopped','failed')),
    last_heartbeat_at REAL,
    updated_at REAL NOT NULL DEFAULT 0
);

-- Migration high-water mark. The boot assertion compares this against the
-- newest embedded migration and refuses to serve on mismatch.
PRAGMA user_version = 1;
