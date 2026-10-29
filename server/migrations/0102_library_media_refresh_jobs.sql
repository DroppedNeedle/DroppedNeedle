-- 0102_library_media_refresh_jobs.sql - media-server rescans move to the
-- jobs table.
--
--   library_management_external_refresh_deliveries
--       dropped. A rescan asked for after a management run is a job on
--       the jobs table now, one per server, which a burst of runs joins.
--       Rescans still waiting at the upgrade are dropped with it.
--
--   library_management_job_snapshots.{jellyfin,plex,navidrome}_refresh_job_id
--       the jobs-table row each server's rescan was queued as when the
--       run finished, read back for the run's page. No foreign key: the
--       row is pruned after a week, or folded into a newer request, and
--       then reads as no record.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP INDEX IF EXISTS idx_management_external_refresh_claim;
DROP INDEX IF EXISTS idx_management_external_refresh_lease;
DROP TABLE library_management_external_refresh_deliveries;

ALTER TABLE library_management_job_snapshots
    ADD COLUMN jellyfin_refresh_job_id INTEGER;
ALTER TABLE library_management_job_snapshots
    ADD COLUMN plex_refresh_job_id INTEGER;
ALTER TABLE library_management_job_snapshots
    ADD COLUMN navidrome_refresh_job_id INTEGER;

-- Migration high-water mark.
PRAGMA user_version = 102;
