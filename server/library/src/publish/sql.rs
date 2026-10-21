//! The publish journal's statements, the operation record, and the
//! maintenance tick's purge and sweep.

droppedneedle_kernel::sql! {
    /// One journal row with the file it replaces, if any.
    pub(super) fragment JOURNAL_ROWS = "SELECT j.id, j.bundle_id, j.kind, j.source_root, \
             j.source_rel, j.dest_root, j.dest_rel, j.staged, j.backup, j.source_sha256, \
             j.staged_sha256, j.track_id, j.mgmt_state, j.state, j.seq, \
             r.replaced_rel, r.replaced_sha256, r.replaced_track_id, r.recycle_to \
             FROM library_publish_journal j \
             LEFT JOIN library_publish_replacements r ON r.journal_id = j.id";
    /// One row by id (`?1`).
    pub(super) JOURNAL_BY_ID = JOURNAL_ROWS + " WHERE j.id = ?1";
    /// Every row of one bundle (`?1`), by id.
    pub(super) JOURNAL_BY_BUNDLE = JOURNAL_ROWS + " WHERE j.bundle_id = ?1 ORDER BY j.id";
    /// One row's state (`?1`).
    pub(super) JOURNAL_STATE = "SELECT state FROM library_publish_journal WHERE id = ?1";
    /// A fresh intent.
    pub(super) JOURNAL_INSERT = "INSERT INTO library_publish_journal \
             (id, bundle_id, kind, source_root, source_rel, dest_root, dest_rel, staged, backup, \
             source_sha256, staged_sha256, track_id, mgmt_state, state, seq) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, 0)";
    /// The file a fresh intent replaces.
    pub(super) REPLACEMENT_INSERT = "INSERT INTO library_publish_replacements \
             (journal_id, replaced_rel, replaced_sha256, replaced_track_id, recycle_to) \
             VALUES (?1, ?2, ?3, ?4, ?5)";
    /// Move a row (`?2`) from `?3` to `?1`, or touch it when it is already
    /// at `?1`, so the write always changes a row.
    pub(super) JOURNAL_ADVANCE = "UPDATE library_publish_journal SET state = ?1, seq = seq + 1 \
             WHERE id = ?2 AND state IN (?3, ?1)";
    /// Move a row (`?2`) from `?3` to `?1` only.
    pub(super) JOURNAL_TRANSITION = "UPDATE library_publish_journal SET state = ?1, seq = seq + 1 \
             WHERE id = ?2 AND state = ?3";
    /// Record a row's (`?2`) backup path (`?1`), unless it names another.
    pub(super) JOURNAL_SET_BACKUP = "UPDATE library_publish_journal SET backup = ?1 \
             WHERE id = ?2 AND (backup IS NULL OR backup = ?1)";
    /// When a replaced file reached the recycle bin.
    pub(super) JOURNAL_SET_RECYCLED = "UPDATE library_publish_replacements SET recycled_at = ?2 \
             WHERE journal_id = ?1";
    /// A fresh bin entry (`?2`) for a replaced file not yet recycled.
    pub(super) JOURNAL_SET_RECYCLE_TO = "UPDATE library_publish_replacements \
             SET recycle_to = ?2 WHERE journal_id = ?1 AND recycled_at IS NULL";
    /// One page of recycled copies due to expire: copies that reached the
    /// bin before `?1` (unix seconds), are not expired yet and whose row
    /// is done, after the cursor (`?2` recycled at, `?3` journal id), at
    /// most `?4`, skipping any whose track a put-back still holds.
    pub(super) EXPIRY_DUE = "SELECT r.journal_id, r.recycle_to, r.replaced_sha256, \
             r.recycled_at FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             WHERE r.expired_at IS NULL AND r.recycled_at IS NOT NULL AND r.recycled_at < ?1 \
             AND j.state = 'cleaned' \
             AND (r.recycled_at > ?2 OR (r.recycled_at = ?2 AND r.journal_id > ?3)) \
             AND NOT EXISTS (SELECT 1 FROM library_publish_journal p \
             WHERE p.kind = 'put_back' AND p.track_id = r.replaced_track_id \
             AND p.state NOT IN ('cleaned', 'compensated')) \
             ORDER BY r.recycled_at, r.journal_id LIMIT ?4";
    /// Whether a put-back still holds the track of row `?1`'s copy.
    pub(super) EXPIRY_HELD = "SELECT EXISTS (SELECT 1 FROM library_publish_replacements r \
             JOIN library_publish_journal p ON p.track_id = r.replaced_track_id \
             WHERE r.journal_id = ?1 AND p.kind = 'put_back' \
             AND p.state NOT IN ('cleaned', 'compensated'))";
    /// Record that row `?1`'s copy was expired at `?2` (unix milliseconds).
    pub(super) EXPIRY_MARK = "UPDATE library_publish_replacements SET expired_at = ?2 \
             WHERE journal_id = ?1 AND expired_at IS NULL";
    /// Every bin entry a row names, expired or not.
    pub(super) EXPIRY_ENTRIES = "SELECT recycle_to FROM library_publish_replacements";
    /// Another bundle (not `?2`) still unsettled for a track (`?1`): one
    /// writing the track's file, or one that set the file aside to replace
    /// or remove it and has not committed yet. A committed row waiting for
    /// its bin holds no track.
    pub(super) UNSETTLED_FOR_TRACK = "SELECT bundle_id FROM library_publish_journal \
             WHERE track_id = ?1 AND bundle_id <> ?2 \
             AND state NOT IN ('cleaned', 'compensated', 'needs_attention') \
             UNION ALL SELECT j.bundle_id FROM library_publish_journal j \
             JOIN library_publish_replacements r ON r.journal_id = j.id \
             WHERE r.replaced_track_id = ?1 AND j.bundle_id <> ?2 \
             AND j.state IN ('prepared', 'staged', 'published') LIMIT 1";
    /// Bundles with unsettled rows, in order.
    pub(super) ACTIVE_BUNDLES = "SELECT DISTINCT bundle_id FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention') ORDER BY bundle_id";
    /// How many bundles have a row that has not committed yet.
    pub(super) PRECOMMIT_BUNDLE_COUNT = "SELECT COUNT(DISTINCT bundle_id) \
             FROM library_publish_journal WHERE state IN ('prepared', 'staged', 'published')";
    /// Mark an indexed track (`?1`) missing since `?2`; answers its new
    /// row revision.
    pub(crate) TRACK_MARK_MISSING = "UPDATE local_tracks SET availability = 'missing', \
             missing_since = ?2, row_revision = row_revision + 1 \
             WHERE id = ?1 AND availability = 'indexed' RETURNING row_revision";
    /// One catalog action. The actor (`?2`) stays NULL when it is no
    /// account.
    pub(crate) CATALOG_ACTION_INSERT = "INSERT INTO library_catalog_actions (id, actor_user_id, \
             action_kind, local_album_id, local_track_id, before_json, after_json, reason_code, \
             created_at) VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, ?5, ?6, \
             ?7, ?8, ?9)";
    /// A removal's undo record (`?1`) on album `?2`, or on an album merged
    /// into it, that no put-back has spent yet: its `after_json`.
    pub(super) RECYCLED_ACTION_OPEN = "WITH RECURSIVE albums(id, hops) AS ( \
             SELECT ?2, 0 UNION SELECT b.id, albums.hops + 1 FROM local_albums b \
             JOIN albums ON b.retired_into_album_id = albums.id WHERE albums.hops < 8) \
             SELECT a.after_json FROM library_catalog_actions a \
             WHERE a.id = ?1 AND a.action_kind = 'files_recycled' \
             AND a.local_album_id IN (SELECT id FROM albums) \
             AND NOT EXISTS (SELECT 1 FROM library_catalog_actions p \
             WHERE p.action_kind = 'files_put_back' \
             AND json_extract(p.before_json, '$.undoes') = a.id)";
    /// Whether a put-back spent the undo record `?1`.
    pub(super) PUT_BACK_EXISTS = "SELECT EXISTS (SELECT 1 FROM library_catalog_actions \
             WHERE action_kind = 'files_put_back' AND json_extract(before_json, '$.undoes') = ?1)";
    /// The removals of album `?1` (or of an album merged into it) that can
    /// still be put back: unspent, every file row settled or held for an
    /// admin, newest first. `files` counts the copies still in the bin; a
    /// held row never expires.
    pub(crate) ALBUM_REMOVALS = "WITH RECURSIVE albums(id, hops) AS ( \
             SELECT ?1, 0 UNION SELECT b.id, albums.hops + 1 FROM local_albums b \
             JOIN albums ON b.retired_into_album_id = albums.id WHERE albums.hops < 8) \
             SELECT id, reason_code, created_at, files, held FROM ( \
             SELECT a.id, a.reason_code, a.created_at, \
             (SELECT count(*) FROM library_publish_journal j \
              JOIN library_publish_replacements r ON r.journal_id = j.id \
              WHERE j.bundle_id = a.id AND j.kind = 'remove' \
              AND (r.expired_at IS NULL OR j.state = 'needs_attention')) AS files, \
             EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
              AND j.kind = 'remove' AND j.state = 'needs_attention') AS held \
             FROM library_catalog_actions a \
             WHERE a.action_kind = 'files_recycled' \
             AND a.local_album_id IN (SELECT id FROM albums) \
             AND NOT EXISTS (SELECT 1 FROM library_catalog_actions p \
              WHERE p.action_kind = 'files_put_back' \
              AND json_extract(p.before_json, '$.undoes') = a.id) \
             AND NOT EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
              AND j.kind = 'remove' \
              AND j.state IN ('prepared', 'staged', 'published', 'compensated'))) \
             WHERE files > 0 ORDER BY created_at DESC, id";
    /// Whether an undo of an added file, recorded with the text `?1`, landed
    /// or is still settling.
    pub(super) TAKE_BACK_EXISTS = "SELECT EXISTS (SELECT 1 FROM library_publish_operations o \
             JOIN library_publish_journal j ON j.bundle_id = o.bundle_id \
             WHERE instr(o.bundle_json, ?1) > 0 AND j.kind = 'delete' \
             AND j.state <> 'compensated')";
    /// The landed files plugin `?1` added, newest first, before the point
    /// (`?2` unix milliseconds, then bundle id `?3`), at most `?4`: each
    /// bundle with when it was made and whether an undo took the file
    /// away or is still settling.
    pub(super) PLUGIN_FILES = "SELECT o.bundle_id, o.created_at, o.bundle_json, \
             EXISTS (SELECT 1 FROM library_publish_operations t \
             JOIN library_publish_journal d ON d.bundle_id = t.bundle_id \
             WHERE d.kind = 'delete' AND d.state <> 'compensated' \
             AND instr(t.bundle_json, '\"undoes\":') > 0 \
             AND json_extract(t.bundle_json, '$.added.undoes') = o.bundle_id) \
             FROM library_publish_journal a \
             JOIN library_publish_operations o ON o.bundle_id = a.bundle_id \
             WHERE a.kind = 'add' AND a.state IN ('committed', 'cleanup_pending', 'cleaned') \
             AND json_extract(o.bundle_json, '$.added.origin.plugin') = ?1 \
             AND (o.created_at < ?2 OR (o.created_at = ?2 AND o.bundle_id < ?3)) \
             ORDER BY o.created_at DESC, o.bundle_id DESC LIMIT ?4";
    /// A track's (`?1`) root, relative path and availability.
    pub(super) TRACK_PLACE = "SELECT root_id, relative_path, availability FROM local_tracks \
             WHERE id = ?1";
    /// The track at root `?1`, path `?2`, with its availability.
    pub(super) TRACK_AT = "SELECT id, availability FROM local_tracks \
             WHERE root_id = ?1 AND relative_path = ?2";
    /// Move a track (`?1`) to path `?2` (absolute `?3`, hash `?4`).
    pub(super) TRACK_SET_PATH = "UPDATE local_tracks SET relative_path = ?2, file_path = ?3, \
             path_hash = ?4, row_revision = row_revision + 1 WHERE id = ?1";
    /// Whether any bundle has unsettled rows.
    pub(crate) ANY_ACTIVE = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention'))";
    /// Every hidden name the journal records.
    pub(super) JOURNAL_NAMES = "SELECT staged, backup FROM library_publish_journal";
    /// The catalog rows a publish is checked against, for the track ids in
    /// the JSON array `?1`: each track's place, availability, stat
    /// revision and album, with its album's and its own MusicBrainz
    /// identity.
    pub(super) OWN_ROWS = "SELECT t.id, t.root_id, t.relative_path, t.availability, \
             t.stat_revision, t.local_album_id, \
             ai.row_revision, ai.release_group_mbid, ai.release_mbid, \
             ti.row_revision, ti.recording_mbid, ti.release_track_mbid \
             FROM local_tracks t \
             LEFT JOIN local_album_external_identities ai \
             ON ai.local_album_id = t.local_album_id AND ai.provider = 'musicbrainz' \
             LEFT JOIN local_track_external_identities ti \
             ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
             WHERE t.id IN (SELECT value FROM json_each(?1))";
    /// Record one operation, made at `?4` (unix milliseconds).
    pub(super) OPERATION_INSERT = "INSERT OR IGNORE INTO library_publish_operations \
             (bundle_id, bundle_json, created_day, created_at, applied_by_user_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)";
    /// One recorded bundle's JSON (`?1`).
    pub(super) OPERATION_JSON = "SELECT bundle_json FROM library_publish_operations \
             WHERE bundle_id = ?1";
    /// Bundles holding a snapshot expired by day `?1`.
    pub(super) EXPIRED_SNAPSHOT_BUNDLES = "SELECT DISTINCT bundle_id \
             FROM library_publish_snapshots WHERE expires_day <= ?1 ORDER BY bundle_id";
    /// A bundle's (`?1`) operation blob references.
    pub(super) DELETE_OPERATION_REFS = "DELETE FROM library_publish_blob_refs \
             WHERE owner_kind = 'operation' AND owner_id = ?1";
    /// A bundle's (`?1`) snapshots expired by day `?2`.
    pub(super) DELETE_EXPIRED_SNAPSHOTS = "DELETE FROM library_publish_snapshots \
             WHERE bundle_id = ?1 AND expires_day <= ?2";
    /// The blob references of previews whose job has ended.
    pub(crate) DROP_PREVIEW_REFS = "DELETE FROM library_publish_blob_refs \
             WHERE owner_kind = 'preview' AND owner_id NOT IN (SELECT id \
             FROM library_operation_jobs WHERE state NOT IN ('succeeded', 'cancelled'))";
    /// Blobs nothing refers to.
    pub(super) UNREFERENCED_BLOBS = "SELECT sha256 FROM library_publish_blobs b \
             WHERE NOT EXISTS (SELECT 1 FROM library_publish_blob_refs r \
             WHERE r.sha256 = b.sha256)";
    /// One blob (`?1`), unless something refers to it by now.
    pub(super) DELETE_UNREFERENCED_BLOB = "DELETE FROM library_publish_blobs \
             WHERE sha256 = ?1 AND NOT EXISTS (SELECT 1 FROM library_publish_blob_refs r \
             WHERE r.sha256 = ?1)";
    /// Writes made at or before `?1` (unix milliseconds) that recovery has
    /// neither finished nor undone, at most `?2`.
    pub(crate) RETRYING_BUNDLES = "SELECT j.bundle_id, COUNT(*), \
             GROUP_CONCAT(j.dest_rel, char(10)) FROM library_publish_journal j \
             JOIN library_publish_operations o ON o.bundle_id = j.bundle_id \
             WHERE j.state IN ('prepared', 'staged', 'published', 'committed') \
             AND o.created_at <= ?1 \
             GROUP BY j.bundle_id ORDER BY j.bundle_id LIMIT ?2";
    /// When the oldest write still open or needing attention was made.
    pub(crate) OLDEST_OPEN_WRITE = "SELECT MIN(o.created_at) FROM library_publish_operations o \
             WHERE o.bundle_id IN (SELECT bundle_id FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention') \
             OR (state = 'needs_attention' AND bundle_id NOT IN \
             (SELECT bundle_id FROM library_publish_resolutions)))";
}
