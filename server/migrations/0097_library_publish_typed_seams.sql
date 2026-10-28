-- 0097_library_publish_typed_seams.sql - typed management states and the
-- generated files of each publish.
--
--   library_publish_journal.commit_as (new, replaces mgmt_state)
--       how the catalog commit records a file: 'managed' (a library track
--       the publish moved or retagged) or 'imported' (a file the commit
--       adds to the catalog). mgmt_state held 'managed:<revision>' or
--       'imported:<revision>'; the revision was never read. Any value but
--       an 'imported:' one was read as managed, and still is.
--   library_track_management_state.last_outcome
--       'managed:<revision>' and 'imported:<revision>' become 'managed'
--       and 'imported'. Nothing reads either value.
--   library_publish_outputs (new)
--       one row per generated file (a cover, an .lrc) a publish placed,
--       written when its journal row reaches the commit. Undo and baseline
--       restore find a track's generated files here instead of searching
--       every stored bundle's text. Rows are only added: a row that is
--       flagged after its bundle committed keeps its file's undo.
--
-- Stored bundles and before-state documents keep their text: the typed
-- readers decode today's shapes as they are.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

ALTER TABLE library_publish_journal
    ADD COLUMN commit_as TEXT CHECK (commit_as IN ('managed', 'imported'));

UPDATE library_publish_journal
SET commit_as = CASE WHEN substr(mgmt_state, 1, 9) = 'imported:' THEN 'imported'
                     ELSE 'managed' END
WHERE mgmt_state IS NOT NULL;

ALTER TABLE library_publish_journal DROP COLUMN mgmt_state;

UPDATE library_track_management_state
SET last_outcome = CASE WHEN substr(last_outcome, 1, 9) = 'imported:' THEN 'imported'
                        ELSE 'managed' END
WHERE substr(last_outcome, 1, 8) = 'managed:' OR substr(last_outcome, 1, 9) = 'imported:';

CREATE TABLE library_publish_outputs (
    journal_id TEXT PRIMARY KEY
        REFERENCES library_publish_journal(id) ON DELETE CASCADE,
    bundle_id TEXT NOT NULL
        REFERENCES library_publish_operations(bundle_id) ON DELETE CASCADE,
    item_index INTEGER NOT NULL CHECK (item_index >= 0),
    output_index INTEGER NOT NULL CHECK (output_index >= 0),
    track_id TEXT NOT NULL,
    dest_root TEXT NOT NULL,
    dest_rel TEXT NOT NULL,
    sha256 TEXT NOT NULL,
    size INTEGER NOT NULL CHECK (size >= 0),
    kind TEXT NOT NULL CHECK (kind IN ('artwork', 'lyrics')),
    UNIQUE (bundle_id, item_index, output_index)
) STRICT;

CREATE INDEX idx_library_publish_outputs_track ON library_publish_outputs(track_id);

-- The rule: every output journal row whose bundle reached the commit,
-- matched to the output its bundle planned by destination and bytes. A
-- bundle whose text does not read as JSON, or an item or output that is
-- not an object, has none.
CREATE VIEW library_publish_outputs_rule
    (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
     sha256, size, kind) AS
SELECT j.id, j.bundle_id, i.key, x.key, json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id'),
       j.dest_root, j.dest_rel, j.staged_sha256, json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size'),
       json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind')
FROM library_publish_journal j
JOIN library_publish_operations o ON o.bundle_id = j.bundle_id
JOIN json_each(CASE WHEN json_valid(o.bundle_json) THEN o.bundle_json ELSE '{}' END,
               '$.items') i
JOIN json_each(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.outputs') x
WHERE j.kind = 'output'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_root') = j.dest_root
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.dest_rel') = j.dest_rel
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.sha256') = j.staged_sha256
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.kind') IN ('artwork', 'lyrics')
  AND typeof(json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size')) = 'integer'
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.size') >= 0
  AND typeof(json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id')) = 'text'
  AND EXISTS (SELECT 1 FROM library_publish_journal k
              WHERE k.bundle_id = j.bundle_id
                AND k.state IN ('committed', 'cleanup_pending', 'cleaned'));

-- The refresh: the rule's row for one journal row, added when missing.
CREATE VIEW library_publish_outputs_refresh (journal_id) AS SELECT NULL WHERE 0;

CREATE TRIGGER trg_library_publish_outputs_refresh
INSTEAD OF INSERT ON library_publish_outputs_refresh
WHEN NEW.journal_id IS NOT NULL
BEGIN
    INSERT INTO library_publish_outputs
        (journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
         sha256, size, kind)
    SELECT journal_id, bundle_id, item_index, output_index, track_id, dest_root, dest_rel,
           sha256, size, kind
    FROM library_publish_outputs_rule WHERE journal_id = NEW.journal_id
    ON CONFLICT DO NOTHING;
END;

-- A generated file's row reaching the commit adds it.
CREATE TRIGGER trg_library_publish_outputs_landed
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.kind = 'output'
 AND NEW.state IN ('committed', 'cleanup_pending', 'cleaned')
 AND OLD.state NOT IN ('committed', 'cleanup_pending', 'cleaned')
BEGIN
    INSERT INTO library_publish_outputs_refresh (journal_id) VALUES (NEW.id);
END;

-- Backfill through the same refresh.
INSERT INTO library_publish_outputs_refresh (journal_id)
SELECT id FROM library_publish_journal WHERE kind = 'output';

-- Migration high-water mark.
PRAGMA user_version = 97;
