-- 0084_acquire_task_plan.sql - a download's plan lives on its task row.
--
--   download_tasks.plan_json (new)
--       what the task fetches beyond its own columns: the release its
--       lists belong to, the edition's tracklist, the positions "Get
--       missing tracks" asks for, the library copy it was fetched for
--       and the versions it replaces, "Get it from the album", and a
--       single track's album. Written in the transaction that creates
--       the task. '{}' is the empty plan of a plain download, which is
--       what every task already on the queue starts with: the staging
--       manifest files that held this before are not read.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE download_tasks ADD COLUMN plan_json TEXT NOT NULL DEFAULT '{}';

-- Migration high-water mark.
PRAGMA user_version = 84;
