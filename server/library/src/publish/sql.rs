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
    /// Another bundle (not `?2`) still unsettled for a track (`?1`).
    pub(super) UNSETTLED_FOR_TRACK = "SELECT bundle_id FROM library_publish_journal \
             WHERE track_id = ?1 AND bundle_id <> ?2 \
             AND state NOT IN ('cleaned', 'compensated', 'needs_attention') LIMIT 1";
    /// Bundles with unsettled rows, in order.
    pub(super) ACTIVE_BUNDLES = "SELECT DISTINCT bundle_id FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention') ORDER BY bundle_id";
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
