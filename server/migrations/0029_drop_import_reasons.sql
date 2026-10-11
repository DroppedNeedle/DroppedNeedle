-- 0029_drop_import_reasons.sql - one clear reason per drop-import item and
-- Free Music download.
--
-- drop_import_items and free_music_tasks (0001) carry a free-text detail or
-- error. Drop imports and Free Music now record a stable reason code next to
-- it, plus what the person can do about it, so the import and download
-- views show a plain sentence and an action instead of internal error text.
--
--   drop_import_items.reason_code     stable code (no_album_match, ...)
--   drop_import_items.reason_action   what to do about it
--   free_music_tasks.error_code       stable code (no_source, ...)
--   free_music_tasks.error_action     what to do about it
--   free_music_tasks.idempotency_key  the requester's key: a repeated ask
--                                     answers the first task (unique)
--
-- The sentence itself stays in the existing detail/error column. Rows written
-- before this migration have no code and keep their text.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE drop_import_items ADD COLUMN reason_code TEXT;
ALTER TABLE drop_import_items ADD COLUMN reason_action TEXT;
ALTER TABLE free_music_tasks ADD COLUMN error_code TEXT;
ALTER TABLE free_music_tasks ADD COLUMN error_action TEXT;
ALTER TABLE free_music_tasks ADD COLUMN idempotency_key TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS idx_free_music_idempotency
    ON free_music_tasks(idempotency_key) WHERE idempotency_key IS NOT NULL;

-- Migration high-water mark.
PRAGMA user_version = 29;
