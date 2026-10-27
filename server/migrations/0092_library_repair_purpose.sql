-- 0092_library_repair_purpose.sql - a repair job's purpose is a column,
-- and the cleanup after identification keeps its place across restarts.
--
--   library_repair_snapshots.purpose (new)
--       What the job checks, one of the repairs crate's purposes. Filled
--       from scope_json: its purpose when it names a known one, and
--       existing_matches when it names none (v2's rule for a scope
--       without a purpose). A scope that is not a JSON object, or names
--       an unknown purpose, is left NULL: the job reads as unreadable,
--       never as an identity repair. Every job written from now on has
--       one. The table is not STRICT yet, so the column stays nullable.
--
--   library_repair_watermarks (new)
--       One row: the finished identification job, by (updated ms, id),
--       up to which albums have had their cleanup queued. Seeded at the
--       newest finished identification job, or the start of time when
--       there is none, so the upgrade queues nothing for identifications
--       that were already done.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_repair_snapshots ADD COLUMN purpose TEXT
    CHECK (purpose IN ('existing_matches', 'management_readiness',
                       'catalog_identity_hygiene', 'play_history_filing'));

UPDATE library_repair_snapshots SET purpose = CASE
    WHEN NOT json_valid(scope_json) THEN NULL
    WHEN json_type(scope_json) <> 'object' THEN NULL
    WHEN COALESCE(json_type(scope_json, '$.purpose'), 'null') = 'null' THEN 'existing_matches'
    WHEN json_extract(scope_json, '$.purpose') IN ('existing_matches', 'management_readiness',
         'catalog_identity_hygiene', 'play_history_filing')
        THEN json_extract(scope_json, '$.purpose')
    ELSE NULL
END;

CREATE TABLE library_repair_watermarks (
    singleton INTEGER NOT NULL PRIMARY KEY CHECK (singleton = 1),
    identified_at INTEGER NOT NULL,
    identified_job_id TEXT NOT NULL
) STRICT;

INSERT INTO library_repair_watermarks (singleton, identified_at, identified_job_id)
SELECT 1, COALESCE(newest.updated_ms, 0), COALESCE(newest.id, '')
FROM (SELECT 1) LEFT JOIN (
    SELECT updated_ms, id FROM library_identify_jobs
    WHERE state IN ('succeeded', 'failed', 'attention')
    ORDER BY updated_ms DESC, id DESC LIMIT 1
) AS newest;

-- Migration high-water mark.
PRAGMA user_version = 92;
