//! The publish journal's statements, the operation record, and the
//! maintenance tick's purge and sweep.

droppedneedle_kernel::sql! {
    /// One journal row with the file it replaces, if any.
    pub(super) fragment JOURNAL_ROWS = "SELECT j.id, j.bundle_id, j.kind, j.source_root, \
             j.source_rel, j.dest_root, j.dest_rel, j.staged, j.backup, j.source_sha256, \
             j.staged_sha256, j.track_id, j.commit_as, j.state, j.seq, \
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
             source_sha256, staged_sha256, track_id, commit_as, state, seq) \
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
    /// Move a row (`?2`) from `?3` to `?1` only, recording the hash of its
    /// staged bytes (`?4`).
    pub(super) JOURNAL_STAGE = "UPDATE library_publish_journal \
             SET state = ?1, staged_sha256 = ?4, seq = seq + 1 WHERE id = ?2 AND state = ?3";
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
    /// most `?4`, each with its track.
    pub(super) EXPIRY_DUE = "SELECT r.journal_id, r.recycle_to, r.replaced_sha256, \
             r.recycled_at, r.replaced_track_id FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             WHERE r.expired_at IS NULL AND r.recycled_at IS NOT NULL AND r.recycled_at < ?1 \
             AND j.state = 'cleaned' \
             AND (r.recycled_at > ?2 OR (r.recycled_at = ?2 AND r.journal_id > ?3)) \
             ORDER BY r.recycled_at, r.journal_id LIMIT ?4";
    /// The tracks a put-back not yet settled holds.
    pub(super) EXPIRY_HELD_TRACKS = "SELECT DISTINCT track_id FROM library_publish_journal \
             WHERE state IN ('prepared', 'staged', 'published', 'committed', \
             'cleanup_pending', 'needs_attention') AND kind = 'put_back' \
             AND track_id IS NOT NULL";
    /// Record that row `?1`'s copy left the bin at `?2` (unix
    /// milliseconds): expired, found gone, or put back.
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
    /// Bundles with unsettled rows, or with folders an unwind has not
    /// removed yet, in order.
    pub(super) ACTIVE_BUNDLES = "SELECT bundle_id FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention') \
             UNION SELECT bundle_id FROM library_publish_created_folders ORDER BY bundle_id";
    /// Record a folder a bundle (`?1`) made under root `?2` at `?3`.
    pub(super) CREATED_FOLDER_INSERT = "INSERT OR IGNORE INTO library_publish_created_folders \
             (bundle_id, root_id, folder_rel) VALUES (?1, ?2, ?3)";
    /// The folders a bundle (`?1`) made.
    pub(super) CREATED_FOLDERS = "SELECT root_id, folder_rel FROM library_publish_created_folders \
             WHERE bundle_id = ?1";
    /// Forget the folders a bundle (`?1`) made.
    pub(super) CREATED_FOLDERS_DELETE = "DELETE FROM library_publish_created_folders \
             WHERE bundle_id = ?1";
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
    /// Where a file is in the recycle bin: its copy's row is not marked as
    /// gone, and its copy reached the bin or its row waits for an admin.
    pub(super) fragment IN_BIN = "r.expired_at IS NULL \
             AND (r.recycled_at IS NOT NULL OR j.state = 'needs_attention')";
    /// One page of what the recycle bin holds, grouped by the removal or
    /// change that put it there, newest first, before the cursor (`?1`
    /// unix ms, then bundle id `?2`), at most `?3`: each bundle with when
    /// it was made, when its first copy reached the bin (unix seconds),
    /// and whether its rows are removals.
    pub(super) BIN_PAGE = "SELECT bundle_id, removed_at, first_binned, removal FROM ( \
             SELECT j.bundle_id AS bundle_id, \
             COALESCE(o.created_at, CAST(MIN(r.recycled_at) * 1000 AS INTEGER), 0) AS removed_at, \
             MIN(r.recycled_at) AS first_binned, MAX(j.kind = 'remove') AS removal \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             LEFT JOIN library_publish_operations o ON o.bundle_id = j.bundle_id \
             WHERE " + IN_BIN + " GROUP BY j.bundle_id) \
             WHERE removed_at < ?1 OR (removed_at = ?1 AND bundle_id < ?2) \
             ORDER BY removed_at DESC, bundle_id DESC LIMIT ?3";
    /// The files in the bin of the bundles in the JSON array `?1`: each
    /// copy, its row's state and its track's album.
    pub(super) BIN_FILES = "SELECT j.bundle_id, r.recycle_to, j.state, t.local_album_id \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             LEFT JOIN local_tracks t ON t.id = r.replaced_track_id \
             WHERE j.bundle_id IN (SELECT value FROM json_each(?1)) AND " + IN_BIN + " \
             ORDER BY j.bundle_id, r.journal_id";
    /// The undo records of the bundles in the JSON array `?1`: reason,
    /// album, and whether a removal row of it is still being written.
    pub(super) BIN_RECORDS = "SELECT a.id, a.reason_code, a.local_album_id, \
             EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
             AND j.kind = 'remove' \
             AND j.state IN ('prepared', 'staged', 'published', 'compensated')) \
             FROM library_catalog_actions a WHERE a.action_kind = 'files_recycled' \
             AND a.id IN (SELECT value FROM json_each(?1))";
    /// Which of the undo records in the JSON array `?1` a put-back spent.
    pub(super) BIN_SPENT = "SELECT json_extract(before_json, '$.undoes') \
             FROM library_catalog_actions WHERE action_kind = 'files_put_back' \
             AND json_extract(before_json, '$.undoes') IN (SELECT value FROM json_each(?1))";
    /// Album `?1`, or the album it was merged into: its id, title, artist,
    /// whether it has an indexed track, its MusicBrainz release group, and
    /// whether it has art of its own.
    pub(super) BIN_ALBUM = "WITH RECURSIVE chain(id, next, hops) AS ( \
             SELECT id, retired_into_album_id, 0 FROM local_albums WHERE id = ?1 \
             UNION ALL SELECT b.id, b.retired_into_album_id, chain.hops + 1 FROM local_albums b \
             JOIN chain ON b.id = chain.next WHERE chain.hops < 8) \
             SELECT a.id, a.title, a.album_artist_name, a.has_indexed_track, \
             (SELECT e.release_group_mbid FROM local_album_external_identities e \
             WHERE e.local_album_id = a.id AND e.provider = 'musicbrainz'), \
             EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id) \
             FROM chain JOIN local_albums a ON a.id = chain.id WHERE chain.next IS NULL";
    /// How many files the bin holds.
    pub(super) BIN_COUNT = "SELECT COUNT(*) FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id WHERE " + IN_BIN;
    /// The copies an empty at `?1` (unix seconds) would try: the expiry's
    /// due rows with no retention, each with its track.
    pub(super) BIN_DUE = "SELECT r.recycle_to, r.replaced_track_id \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             WHERE r.expired_at IS NULL AND r.recycled_at IS NOT NULL AND r.recycled_at < ?1 \
             AND j.state = 'cleaned'";
    /// What one attempt of an empty of the bin did, added to its run's row
    /// (`?1`, the job's id): deletes, bytes and older folders sum; kept,
    /// waited and their names are the latest attempt's; `?4` is set when it
    /// finished.
    pub(crate) BIN_EMPTIED_RECORD = "INSERT INTO library_recycle_bin_empties (id, \
             actor_user_id, before_at, finished_at, deleted, freed_bytes, older_folders, kept, \
             waited, kept_json) VALUES (?1, (SELECT id FROM auth_users WHERE id = ?2), ?3, ?4, \
             ?5, ?6, ?7, ?8, ?9, ?10) ON CONFLICT(id) DO UPDATE SET \
             finished_at = excluded.finished_at, deleted = deleted + excluded.deleted, \
             freed_bytes = freed_bytes + excluded.freed_bytes, \
             older_folders = older_folders + excluded.older_folders, kept = excluded.kept, \
             waited = excluded.waited, kept_json = excluded.kept_json";
    /// The last finished empty of the bin, with who asked.
    pub(crate) BIN_EMPTIED_LAST = "SELECT e.finished_at, u.display_name, e.deleted, \
             e.freed_bytes, e.older_folders, e.kept, e.waited, e.kept_json \
             FROM library_recycle_bin_empties e \
             LEFT JOIN auth_users u ON u.id = e.actor_user_id \
             WHERE e.finished_at IS NOT NULL ORDER BY e.finished_at DESC, e.id DESC LIMIT 1";
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
    /// Whether any bundle has unsettled rows, or folders an unwind has not
    /// removed yet.
    pub(crate) ANY_ACTIVE = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
             WHERE state NOT IN ('cleaned', 'compensated', 'needs_attention')) \
             OR EXISTS (SELECT 1 FROM library_publish_created_folders)";
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
    /// The generated files publishes placed for one track (`?1`), oldest
    /// publish first, in plan order within each.
    pub(super) OUTPUTS_FOR_TRACK = "SELECT p.dest_root, p.dest_rel, p.sha256, p.size, p.kind \
             FROM library_publish_outputs p \
             JOIN library_publish_operations o ON o.bundle_id = p.bundle_id \
             WHERE p.track_id = ?1 \
             ORDER BY o.created_day, o.rowid, p.item_index, p.output_index";
    /// The generated files one publish (`?1`) placed, each with its track,
    /// in plan order.
    pub(super) OUTPUTS_OF_BUNDLE = "SELECT track_id, dest_root, dest_rel, sha256, size, kind \
             FROM library_publish_outputs WHERE bundle_id = ?1 \
             ORDER BY item_index, output_index";
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
    /// The bytes of one blob (`?1`) still kept in the database.
    pub(super) LEGACY_BLOB = "SELECT bytes FROM library_publish_blobs WHERE sha256 = ?1";
    /// The blobs still kept in the database after hash `?1`, in order.
    pub(super) LEGACY_BLOBS_AFTER = "SELECT sha256, bytes FROM library_publish_blobs \
             WHERE sha256 > ?1 ORDER BY sha256";
    /// One blob (`?1`) the undo folder now holds.
    pub(super) DELETE_LEGACY_BLOB = "DELETE FROM library_publish_blobs WHERE sha256 = ?1";
    /// Which of the hashes in the JSON array `?1` some row refers to.
    pub(super) REFERENCED_AMONG = "SELECT DISTINCT sha256 FROM library_publish_blob_refs \
             WHERE sha256 IN (SELECT value FROM json_each(?1))";
    /// A marker row, written and deleted in one durable write so that the
    /// write has something to sync.
    pub(super) SYNC_MARK_INSERT = "INSERT INTO library_publish_blobs (sha256, bytes) \
             VALUES ('sync', x'')";
    /// The marker row again.
    pub(super) SYNC_MARK_DELETE = "DELETE FROM library_publish_blobs WHERE sha256 = 'sync'";
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
