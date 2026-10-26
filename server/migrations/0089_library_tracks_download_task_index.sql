-- The storage quota reaches a user's tracks through their download tasks.
CREATE INDEX IF NOT EXISTS idx_local_tracks_download_task
    ON local_tracks(download_task_id) WHERE download_task_id IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 89;
