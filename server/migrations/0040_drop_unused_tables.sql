-- 0040_drop_unused_tables.sql - drop tables and triggers nothing uses.
--
-- The baseline carried v2's whole schema. Sixty of its tables are never
-- read by the server: most are never written either, a few were filled
-- only by triggers or by the v2 importer. They go here with their rows
-- and indexes, and so do the triggers that fed them.
--
-- library_identity_repair_findings pointed at one of them through
-- evidence_id, a column no code ever set. SQLite cannot drop a column
-- that carries a foreign key, so the table is rebuilt without it. Nothing
-- references the table, so the swap needs no foreign key juggling.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

-- Triggers on tables that stay. They ran on every download, follow and
-- approval change to maintain tables nothing reads. They go first:
-- renaming a table further down re-checks every trigger in the schema.
DROP TRIGGER IF EXISTS download_landed_insert;
DROP TRIGGER IF EXISTS download_landed_update;
DROP TRIGGER IF EXISTS download_landed_delete;
DROP TRIGGER IF EXISTS follow_enroll_insert;
DROP TRIGGER IF EXISTS follow_enroll_update;
DROP TRIGGER IF EXISTS follow_inventory_insert_fence;
DROP TRIGGER IF EXISTS follow_inventory_update_fence;
DROP TRIGGER IF EXISTS follow_inventory_delete_fence;
DROP TRIGGER IF EXISTS follow_approval_insert_due;
DROP TRIGGER IF EXISTS follow_approval_update_due;

-- Repair findings without evidence_id. The copy casts each value to its
-- column's type, because a STRICT table refuses a mistyped one.
CREATE TABLE library_identity_repair_findings_new (
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
    created_at REAL NOT NULL,
    updated_at REAL NOT NULL,
    row_revision INTEGER NOT NULL DEFAULT 1 CHECK(row_revision BETWEEN 1 AND 9223372036854775807),
    UNIQUE(job_id, local_album_id, finding_code)
) STRICT;

INSERT INTO library_identity_repair_findings_new
    SELECT CAST(id AS TEXT), CAST(job_id AS TEXT), CAST(local_album_id AS TEXT),
           CAST(expected_album_revision AS INTEGER),
           CAST(expected_identity_revision AS INTEGER),
           CAST(finding_code AS TEXT), CAST(confidence AS TEXT), CAST(reason_code AS TEXT),
           CAST(apply_eligible AS INTEGER), CAST(apply_result AS TEXT),
           CAST(suggested_release_mbid AS TEXT), CAST(suggested_release_group_mbid AS TEXT),
           CAST(suggested_edition_json AS TEXT), CAST(state AS TEXT),
           CAST(created_at AS REAL), CAST(updated_at AS REAL),
           CAST(row_revision AS INTEGER)
    FROM library_identity_repair_findings;

DROP TABLE library_identity_repair_findings;
ALTER TABLE library_identity_repair_findings_new RENAME TO library_identity_repair_findings;

CREATE INDEX idx_repair_findings_cursor
    ON library_identity_repair_findings(job_id, finding_code, updated_at DESC, id DESC);
CREATE INDEX idx_repair_findings_album
    ON library_identity_repair_findings(local_album_id);

-- Tables that point at another table in this list go before it.

-- Downloads and acquisition.
DROP TABLE IF EXISTS download_cleanup_reconciliation;
DROP TABLE IF EXISTS download_landed_groups;
DROP TABLE IF EXISTS acquisition_snapshot_backfill;
DROP TABLE IF EXISTS free_music_tasks;
DROP TABLE IF EXISTS acquire_upgrade_worklist;
DROP TABLE IF EXISTS acquire_flow_quarantine;
DROP TABLE IF EXISTS acquire_operations;

-- Follows. artist_release_check takes its two follow_success triggers
-- with it. The follow poll keeps its place in acquire_follow_cursors.
DROP TABLE IF EXISTS artist_release_check;
DROP TABLE IF EXISTS follow_due;
DROP TABLE IF EXISTS follow_inventory_rows;
DROP TABLE IF EXISTS follow_inventory_pages;
DROP TABLE IF EXISTS follow_inventory;

-- Metadata caches and media-server indexes.
DROP TABLE IF EXISTS artist_genre_lookup;
DROP TABLE IF EXISTS jellyfin_mbid_index;
DROP TABLE IF EXISTS navidrome_album_mbid_index;
DROP TABLE IF EXISTS navidrome_artist_mbid_index;
DROP TABLE IF EXISTS plex_album_mbid_index;
DROP TABLE IF EXISTS plex_artist_mbid_index;
DROP TABLE IF EXISTS canonical_redirect;
DROP TABLE IF EXISTS release_to_rg;
DROP TABLE IF EXISTS recording_isrc;
DROP TABLE IF EXISTS mb_response_epoch;
DROP TABLE IF EXISTS mb_responses;
DROP TABLE IF EXISTS processed_items;
DROP TABLE IF EXISTS sync_state;

-- Older copies of tables that live on under another name: play history
-- is library_play_history, the compat tables are compat_*.
DROP TABLE IF EXISTS album_release_pins;
DROP TABLE IF EXISTS compat_id_map;
DROP TABLE IF EXISTS play_history;
DROP TABLE IF EXISTS library_compat_bookmarks;
DROP TABLE IF EXISTS library_compat_play_queue_items;
DROP TABLE IF EXISTS library_compat_play_queues;

-- Identification. The live queue and reviews are library_identify_jobs
-- and library_identify_reviews.
DROP TABLE IF EXISTS library_identification_evidence;
DROP TABLE IF EXISTS library_identification_reviews;
DROP TABLE IF EXISTS library_enqueue_sequence;
DROP TABLE IF EXISTS library_identification_jobs;
DROP TABLE IF EXISTS library_bulk_review_snapshots;
DROP TABLE IF EXISTS library_bulk_review_preview_subjects;
DROP TABLE IF EXISTS library_bulk_review_previews;
DROP TABLE IF EXISTS library_edition_conversion_local_files;
DROP TABLE IF EXISTS library_edition_conversion_artifacts;
DROP TABLE IF EXISTS library_edition_conversion_downloads;
DROP TABLE IF EXISTS library_edition_conversion_targets;

-- Scan.
DROP TABLE IF EXISTS library_scan_run_triggers;
DROP TABLE IF EXISTS library_scan_management_candidates;
DROP TABLE IF EXISTS library_scan_management_staging;
DROP TABLE IF EXISTS library_scan_grouping_evidence;
DROP TABLE IF EXISTS library_scan_grouping_values;
DROP TABLE IF EXISTS library_scan_grouping_edges;
DROP TABLE IF EXISTS library_scan_grouping_old_nodes;
DROP TABLE IF EXISTS library_scan_grouping_new_nodes;

-- Library Management.
DROP TABLE IF EXISTS library_management_import_journal;
DROP TABLE IF EXISTS library_management_import_bundles;
DROP TABLE IF EXISTS library_management_job_metadata_snapshots;
DROP TABLE IF EXISTS library_management_collision_evidence;
DROP TABLE IF EXISTS library_policy_state;
DROP TABLE IF EXISTS library_policy_transitions;
DROP TABLE IF EXISTS library_foreign_key_validation_state;

-- v2's own library upgrade.
DROP TABLE IF EXISTS library_migration_markers;
DROP TABLE IF EXISTS library_migration_file_staging;
DROP TABLE IF EXISTS library_migration_review_staging;

-- Migration high-water mark.
PRAGMA user_version = 40;
