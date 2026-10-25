-- 0083_acquire_typed_reasons.sql - one typed reason for every held,
-- refused or failed download.
--
-- Every hold, refusal, failure and wait of the acquire area is one reason
-- with a stable spelling. Its sentence and suggested action are worked out
-- from the spelling when read, never stored, and nothing reads a reason
-- back out of the worker's log text.
--
--   download_tasks.outcome_code (new)
--       the worker's last outcome as a reason spelling, beside the log
--       text in error_message. Filled once here: from the start of the
--       worker's text where it names an outcome, else from the task's
--       newest import decision when that did not import. No CHECK: it is
--       a label for the queue, not a state the task moves through.
--   download_import_decisions.reason_code
--       the retired upgrade_pending and upgrade_restore_failed read as
--       held_by_earlier_version.
--   drop_import_items.reason_code
--       leftover_files becomes tag_mismatch and unreadable becomes
--       corrupt: each named the same situation as the other spelling.
--   drop_import_items.reason_action,
--   download_import_decisions.reason_text, reason_action (dropped)
--       worked out from the reason when read.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

UPDATE download_import_decisions SET reason_code = 'held_by_earlier_version'
WHERE reason_code IN ('upgrade_pending', 'upgrade_restore_failed');

ALTER TABLE download_tasks ADD COLUMN outcome_code TEXT;

UPDATE download_tasks SET outcome_code = CASE
    WHEN instr(error_message, 'transfer stalled') = 1 THEN 'stalled'
    WHEN instr(error_message, 'stuck in the remote queue') = 1 THEN 'queue_timeout'
    WHEN instr(error_message, 'no transfer materialized after enqueue') = 1
        THEN 'never_started'
    WHEN instr(error_message, 'poll deadline hit') = 1 THEN 'deadline'
    WHEN instr(error_message, 'source batch terminal') = 1 THEN 'source_failed'
    WHEN instr(error_message, 'no source could serve this release') = 1 THEN 'no_source'
    WHEN instr(error_message, 'imported what was found; no source had the rest') = 1
        THEN 'tracks_missing'
    WHEN instr(error_message, 'moved to the next source at your request') = 1
        THEN 'moved_on'
    WHEN instr(error_message, 'album_lookup_unavailable:') = 1
        THEN 'album_lookup_unavailable'
    WHEN instr(error_message, 'recording_unknown:') = 1 THEN 'recording_unknown'
    WHEN instr(error_message, 'no_album_for_recording:') = 1 THEN 'no_album_for_recording'
    WHEN instr(error_message, 'no_album_folder:') = 1 THEN 'no_album_folder'
    WHEN instr(error_message, 'no_album_release:') = 1 THEN 'no_album_release'
    WHEN instr(error_message, 'album_releases_exhausted:') = 1
        THEN 'album_releases_exhausted'
    WHEN instr(error_message, 'track_not_found:') = 1 THEN 'track_not_found'
    WHEN instr(error_message, 'Held for review:') = 1 THEN COALESCE(
        (SELECT d.reason_code FROM download_import_decisions d
         WHERE d.task_id = download_tasks.id AND d.outcome = 'held'
         ORDER BY d.decided_at DESC, d.id DESC LIMIT 1),
        'held_for_review')
    ELSE (SELECT d.reason_code FROM download_import_decisions d
          WHERE d.task_id = download_tasks.id
          ORDER BY d.decided_at DESC, d.id DESC LIMIT 1)
END
WHERE error_message IS NOT NULL;

-- A newest decision that imported names no reason, so it leaves NULL.

UPDATE drop_import_items SET reason_code = 'tag_mismatch'
WHERE reason_code = 'leftover_files';
UPDATE drop_import_items SET reason_code = 'corrupt'
WHERE reason_code = 'unreadable';

ALTER TABLE drop_import_items DROP COLUMN reason_action;
ALTER TABLE download_import_decisions DROP COLUMN reason_text;
ALTER TABLE download_import_decisions DROP COLUMN reason_action;

-- Migration high-water mark.
PRAGMA user_version = 83;
