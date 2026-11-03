-- 0128_library_publish_cover_put_back.sql - covers Library Management
-- replaced, and the ones it put back.
--
--   library_publish_outputs_rule (view rebuilt)
--       skips an output its bundle marks `put_back`: a file Undo or
--       "Restore originals" brings back (a cover the archive's had
--       replaced) is the person's own, not one Library Management made, so
--       a later restore never takes it away. An output without the mark
--       reads as not put back, so every row kept before still matches.
--       Its two triggers are made again over it; no row changes.
--
--   library_publish_cover_baselines (new)
--       the bytes of a cover file the archive's replaced the first time,
--       by the track whose plan carried it and the place it stood, so
--       "Restore originals" can put it back. Like the track baselines it
--       has no foreign key: it outlives the track row, and the baseline
--       purge empties it.
--
-- No down migration. Rollback is restoring a pre-upgrade backup.

DROP TRIGGER trg_library_publish_outputs_landed;
DROP TRIGGER trg_library_publish_outputs_refresh;
DROP VIEW library_publish_outputs_rule;

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
  AND json_extract(CASE WHEN x.type = 'object' THEN x.value ELSE '{}' END, '$.put_back') IS NOT 1
  AND typeof(json_extract(CASE WHEN i.type = 'object' THEN i.value ELSE '{}' END, '$.track_id')) = 'text'
  AND EXISTS (SELECT 1 FROM library_publish_journal k
              WHERE k.bundle_id = j.bundle_id
                AND k.state IN ('committed', 'cleanup_pending', 'cleaned'));

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

CREATE TRIGGER trg_library_publish_outputs_landed
AFTER UPDATE OF state ON library_publish_journal
WHEN NEW.kind = 'output'
 AND NEW.state IN ('committed', 'cleanup_pending', 'cleaned')
 AND OLD.state NOT IN ('committed', 'cleanup_pending', 'cleaned')
BEGIN
    INSERT INTO library_publish_outputs_refresh (journal_id) VALUES (NEW.id);
END;

CREATE TABLE library_publish_cover_baselines (
    track_id TEXT NOT NULL,
    root_id TEXT NOT NULL,
    rel TEXT NOT NULL,
    blob_sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (track_id, root_id, rel)
) STRICT;

-- Migration high-water mark.
PRAGMA user_version = 128;
