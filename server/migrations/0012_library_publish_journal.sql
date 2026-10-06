-- 0012_library_publish_journal.sql - the publish journal in the main database.
--
-- Managed file writes (retag, organize, undo, baseline restore) kept their
-- journal, snapshots, baselines, and snapshot blobs in a separate SQLite
-- file inside the first music root. That file moved with root order,
-- churned a live WAL inside the user's library, and left the catalog the
-- reads API serves untouched by a managed move. These tables bring the
-- journal into the application database, next to the catalog it commits
-- against in one transaction.
--
-- - library_publish_journal: one row per file write, with its state
--   machine (prepared, staged, published, committed, cleanup_pending,
--   cleaned, compensated, needs_attention).
-- - library_publish_operations: the sealed bundle each operation published,
--   for undo.
-- - library_publish_blobs: content-addressed before-state documents.
-- - library_publish_blob_refs: who still needs each blob.
-- - library_publish_snapshots: per-operation before states (expiring).
-- - library_publish_baselines: first-management states (kept until purge).
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS library_publish_journal (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    source_root TEXT,
    source_rel TEXT,
    dest_root TEXT NOT NULL,
    dest_rel TEXT NOT NULL,
    staged TEXT NOT NULL,
    backup TEXT,
    source_sha256 TEXT,
    staged_sha256 TEXT NOT NULL,
    track_id TEXT,
    catalog_revision INTEGER,
    mgmt_state TEXT,
    state TEXT NOT NULL,
    seq INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_library_publish_journal_bundle
    ON library_publish_journal(bundle_id);

CREATE INDEX IF NOT EXISTS idx_library_publish_journal_state
    ON library_publish_journal(state);

CREATE TABLE IF NOT EXISTS library_publish_operations (
    bundle_id TEXT PRIMARY KEY,
    bundle_json TEXT NOT NULL,
    created_day INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS library_publish_blobs (
    sha256 TEXT PRIMARY KEY,
    bytes BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS library_publish_blob_refs (
    sha256 TEXT NOT NULL,
    owner_kind TEXT NOT NULL,
    owner_id TEXT NOT NULL,
    PRIMARY KEY (sha256, owner_kind, owner_id)
);

CREATE TABLE IF NOT EXISTS library_publish_snapshots (
    id TEXT PRIMARY KEY,
    bundle_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    blob_sha256 TEXT NOT NULL,
    created_day INTEGER NOT NULL,
    expires_day INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_library_publish_snapshots_bundle
    ON library_publish_snapshots(bundle_id);

CREATE TABLE IF NOT EXISTS library_publish_baselines (
    track_id TEXT PRIMARY KEY,
    blob_sha256 TEXT NOT NULL,
    original_root TEXT NOT NULL,
    original_rel TEXT NOT NULL,
    created_day INTEGER NOT NULL
);

-- Migration high-water mark.
PRAGMA user_version = 12;
