-- 0025_download_import_decisions.sql - what the importer decided about a
-- finished download, and why.
--
-- When a download lands, the importer reads the files, runs its checks
-- (wrong album or edition, quality, samples, track count, how closely the
-- files match the requested release) and then imports, holds or rejects
-- them. One row per landing records the outcome and every check's verdict,
-- so the download and held-import views can explain a hold or a failover.
--
-- held_imports (0001) already carries the per-file holds; this table is the
-- per-landing summary next to them. Rows are append-only history: a reimport
-- or a resumed landing adds a row instead of rewriting the old one.
-- missing_positions lists the release's [disc, track] positions still
-- missing after a landing, so a failover asks the next source only for those.
-- reason_text and reason_action are the plain sentence and the suggested
-- action for reason_code, on decisions and on held files alike, so the views
-- never show internal error text.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

CREATE TABLE IF NOT EXISTS download_import_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL,
    attempt_id TEXT,
    outcome TEXT NOT NULL
        CHECK(outcome IN ('imported','partial','held','rejected','deferred')),
    reason_code TEXT,
    reason_text TEXT,
    reason_action TEXT,
    detail TEXT,
    release_mbid TEXT,
    distance REAL,
    files_total INTEGER NOT NULL DEFAULT 0,
    files_imported INTEGER NOT NULL DEFAULT 0,
    files_held INTEGER NOT NULL DEFAULT 0,
    checks_json TEXT NOT NULL DEFAULT '[]',
    missing_positions TEXT NOT NULL DEFAULT '[]',
    decided_at REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_download_import_decisions_task
    ON download_import_decisions(task_id, decided_at DESC);

ALTER TABLE held_imports ADD COLUMN reason_text TEXT;
ALTER TABLE held_imports ADD COLUMN reason_action TEXT;

-- Migration high-water mark.
PRAGMA user_version = 25;
