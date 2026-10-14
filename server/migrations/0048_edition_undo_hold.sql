-- 0048_edition_undo_hold.sql - the edition undo keeps the album's hold.
--
-- library_edition_choice_undo (0026) keeps what an edition change
-- replaced: the album identity, its track rows and its match state. A
-- change can also set or clear the album's "leave unmanaged"
-- (library_management_exclusions) and its active custom edition
-- (library_custom_edition_active). prior_hold_json keeps both, as they
-- were before the change and as the change left them, so undo can put
-- them back:
--
--   {"before": {"exclusion": {...} | null, "custom_manifest_id": "..." | null},
--    "after":  {...the same shape...}}
--
-- A row written before this migration has none, and its undo restores
-- no hold.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_edition_choice_undo ADD COLUMN prior_hold_json TEXT;

-- Migration high-water mark.
PRAGMA user_version = 48;
