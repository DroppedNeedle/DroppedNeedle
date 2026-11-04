//! The publish journal's statements, the operation record, and the
//! maintenance tick's purge and sweep.

// `sql!` needs a macro call in parentheses to take it as one part.
#![allow(unused_parens, clippy::double_parens)]

use super::journal::{JournalKind, JournalState, MgmtState};
use crate::scan::models::Availability;

/// Where a file is in the recycle bin, over a replacement alias and its
/// journal row's alias: its copy's row is not marked as gone, and its copy
/// reached the bin or its row waits for an admin, the state
/// `$needs_attention` names (a bind, or the state quoted).
#[macro_export]
macro_rules! in_recycle_bin {
    ($replacement:literal, $journal:literal, $needs_attention:literal) => {
        concat!(
            $replacement,
            ".expired_at IS NULL AND (",
            $replacement,
            ".recycled_at IS NOT NULL OR ",
            $journal,
            ".state = ",
            $needs_attention,
            ")"
        )
    };
}

macro_rules! in_bin {
    ($needs_attention:literal) => {
        $crate::in_recycle_bin!("r", "j", $needs_attention)
    };
}

/// Everything of an album, a column holding its id, went through the
/// recycle bin and has left it: the album has tracks, each of them is
/// missing because its file went to the bin (a copy reached the bin no
/// earlier than the track went missing: a removal marks the track missing
/// in its commit and its copy lands after), and none is there now (put
/// back, expired or emptied). A track whose file vanished before the
/// album's last copy reached the bin counts as removed with it: the
/// person removed the album with that track already gone. A track that
/// went missing later (its folder vanished, even after an earlier trip to
/// the bin and back) or one excluded keeps the album out of this.
#[macro_export]
macro_rules! album_gone_through_bin {
    ($album_id:expr) => {
        concat!(
            "EXISTS (SELECT 1 FROM local_tracks gbt WHERE gbt.local_album_id = ",
            $album_id,
            ") AND NOT EXISTS (SELECT 1 FROM local_tracks gbt WHERE gbt.local_album_id = ",
            $album_id,
            " AND (gbt.availability <> 'missing' OR gbt.missing_at IS NULL \
             OR (NOT EXISTS (SELECT 1 FROM library_publish_replacements gbr \
             WHERE gbr.replaced_track_id = gbt.id AND gbr.recycled_at >= gbt.missing_at) \
             AND gbt.missing_at > IFNULL((SELECT MAX(gbl.recycled_at) \
             FROM library_publish_replacements gbl \
             JOIN local_tracks gbo ON gbo.id = gbl.replaced_track_id \
             WHERE gbo.local_album_id = gbt.local_album_id AND gbo.availability = 'missing' \
             AND gbl.recycled_at >= gbo.missing_at), -1)) \
             OR EXISTS (SELECT 1 FROM library_publish_replacements gbr \
             JOIN library_publish_journal gbj ON gbj.id = gbr.journal_id \
             WHERE gbr.replaced_track_id = gbt.id AND ",
            $crate::in_recycle_bin!("gbr", "gbj", "'needs_attention'"),
            ")))"
        )
    };
}

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
    /// When a replaced file reached the recycle bin (`?2`), with its
    /// copy's identity there (`?3`, JSON, or NULL when it could not be read).
    pub(super) JOURNAL_SET_RECYCLED = "UPDATE library_publish_replacements SET recycled_at = ?2, \
             binned_stat_json = ?3 WHERE journal_id = ?1";
    /// A replaced or removed file's copy identity on landing in its bin
    /// (`?1` journal id), JSON or NULL.
    pub(super) REPLACEMENT_BINNED = "SELECT binned_stat_json FROM library_publish_replacements \
             WHERE journal_id = ?1";
    /// A fresh bin entry (`?2`) for a replaced file not yet recycled.
    pub(super) JOURNAL_SET_RECYCLE_TO = "UPDATE library_publish_replacements \
             SET recycle_to = ?2 WHERE journal_id = ?1 AND recycled_at IS NULL";
    /// Retired track `?1`, whose path still names bin entry `?2`, now
    /// names `?3`.
    pub(super) TRACK_FOLLOW_REBIN = "UPDATE local_tracks SET file_path = ?3, \
             row_revision = row_revision + 1 WHERE id = ?1 AND file_path = ?2";
    /// One page of recycled copies due to expire: copies that reached the
    /// bin before `?1` (unix milliseconds), are not expired yet and whose row
    /// is done, after the cursor (`?2` recycled at, `?3` journal id), at
    /// most `?4`, each with its track and its copy's identity on landing.
    /// `?5` is the state of a row that is done.
    pub(super) EXPIRY_DUE = "SELECT r.journal_id, r.recycle_to, r.replaced_sha256, \
             r.recycled_at, r.replaced_track_id, r.binned_stat_json \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             WHERE r.expired_at IS NULL AND r.recycled_at IS NOT NULL AND r.recycled_at < ?1 \
             AND j.state = ?5 \
             AND (r.recycled_at > ?2 OR (r.recycled_at = ?2 AND r.journal_id > ?3)) \
             ORDER BY r.recycled_at, r.journal_id LIMIT ?4";
    /// The put-back rows not yet settled, with their bundles and tracks:
    /// `?1` to `?6` are the unsettled states, `?7` the put-back kind.
    pub(super) EXPIRY_HELD_PUT_BACKS = "SELECT id, bundle_id, track_id \
             FROM library_publish_journal \
             WHERE state IN (?1, ?2, ?3, ?4, ?5, ?6) AND kind = ?7";
    /// Whether row `?1`'s copy was of a track on an album with no files.
    pub(super) EXPIRY_OF_FILELESS_ALBUM = "SELECT EXISTS (SELECT 1 \
             FROM library_publish_replacements r \
             JOIN local_tracks t ON t.id = r.replaced_track_id \
             JOIN local_albums a ON a.id = t.local_album_id \
             WHERE r.journal_id = ?1 AND a.has_indexed_track = 0)";
    /// Record that row `?1`'s copy left the bin at `?2` (unix
    /// milliseconds): expired, found gone, or put back.
    pub(super) EXPIRY_MARK = "UPDATE library_publish_replacements SET expired_at = ?2 \
             WHERE journal_id = ?1 AND expired_at IS NULL";
    /// Every bin entry a row names, with whether that row's copy has left
    /// the bin.
    pub(super) EXPIRY_ENTRIES = "SELECT recycle_to, expired_at IS NOT NULL AS left_bin \
             FROM library_publish_replacements";
    /// Another bundle (not `?2`) still unsettled for a track (`?1`): one
    /// writing the track's file, or one that set the file aside to replace
    /// or remove it and has not committed yet. A committed row waiting for
    /// its bin holds no track. `?3` to `?5` are the settled states, `?6` to
    /// `?8` those before the commit.
    pub(super) UNSETTLED_FOR_TRACK = "SELECT bundle_id FROM library_publish_journal \
             WHERE track_id = ?1 AND bundle_id <> ?2 \
             AND state NOT IN (?3, ?4, ?5) \
             UNION ALL SELECT j.bundle_id FROM library_publish_journal j \
             JOIN library_publish_replacements r ON r.journal_id = j.id \
             WHERE r.replaced_track_id = ?1 AND j.bundle_id <> ?2 \
             AND j.state IN (?6, ?7, ?8) LIMIT 1";
    /// Bundles with unsettled rows, or with folders an unwind has not
    /// removed yet, in order. `?1` to `?3` are the settled states.
    pub(super) ACTIVE_BUNDLES = "SELECT bundle_id FROM library_publish_journal \
             WHERE state NOT IN (?1, ?2, ?3) \
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
    /// How many bundles have a row that has not committed yet: `?1` to `?3`
    /// are the states before the commit.
    pub(super) PRECOMMIT_BUNDLE_COUNT = "SELECT COUNT(DISTINCT bundle_id) \
             FROM library_publish_journal WHERE state IN (?1, ?2, ?3)";
    /// Mark an indexed track (`?1`) missing since `?2`; answers its new
    /// row revision.
    pub(crate) TRACK_MARK_MISSING = "UPDATE local_tracks SET availability = 'missing', \
             missing_at = ?2, row_revision = row_revision + 1 \
             WHERE id = ?1 AND availability = 'indexed' RETURNING row_revision";
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
    /// held row never expires. `?2` is the removal kind, `?3` the held state
    /// and `?4` to `?7` the states of a row still being written.
    pub(crate) ALBUM_REMOVALS = "WITH RECURSIVE albums(id, hops) AS ( \
             SELECT ?1, 0 UNION SELECT b.id, albums.hops + 1 FROM local_albums b \
             JOIN albums ON b.retired_into_album_id = albums.id WHERE albums.hops < 8) \
             SELECT id, reason_code, created_at, files, held FROM ( \
             SELECT a.id, a.reason_code, a.created_at, \
             (SELECT count(*) FROM library_publish_journal j \
              JOIN library_publish_replacements r ON r.journal_id = j.id \
              WHERE j.bundle_id = a.id AND j.kind = ?2 \
              AND (r.expired_at IS NULL OR j.state = ?3)) AS files, \
             EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
              AND j.kind = ?2 AND j.state = ?3) AS held \
             FROM library_catalog_actions a \
             WHERE a.action_kind = 'files_recycled' \
             AND a.local_album_id IN (SELECT id FROM albums) \
             AND NOT EXISTS (SELECT 1 FROM library_catalog_actions p \
              WHERE p.action_kind = 'files_put_back' \
              AND json_extract(p.before_json, '$.undoes') = a.id) \
             AND NOT EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
              AND j.kind = ?2 \
              AND j.state IN (?4, ?5, ?6, ?7))) \
             WHERE files > 0 ORDER BY created_at DESC, id";
    /// One page of what the recycle bin holds, grouped by the removal or
    /// change that put it there, newest first, before the cursor (`?1`
    /// unix ms, then bundle id `?2`), at most `?3`: each bundle with when
    /// it was made, when its first copy reached the bin (unix milliseconds),
    /// and whether its rows are removals (`?4`, the removal kind; `?5`, the
    /// state of a row that waits for an admin).
    pub(super) BIN_PAGE = "SELECT bundle_id, removed_at, first_binned, removal FROM ( \
             SELECT j.bundle_id AS bundle_id, \
             COALESCE(o.created_at, MIN(r.recycled_at), 0) AS removed_at, \
             MIN(r.recycled_at) AS first_binned, MAX(j.kind = ?4) AS removal \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             LEFT JOIN library_publish_operations o ON o.bundle_id = j.bundle_id \
             WHERE " + (in_bin!("?5")) + " GROUP BY j.bundle_id) \
             WHERE removed_at < ?1 OR (removed_at = ?1 AND bundle_id < ?2) \
             ORDER BY removed_at DESC, bundle_id DESC LIMIT ?3";
    /// The files in the bin of the bundles in the JSON array `?1`: each
    /// copy, its row's state and its track's album (for a cover a generated
    /// one replaced, the album of its bundle's tracks). `?2` is the state
    /// of a row that waits for an admin.
    pub(super) BIN_FILES = "SELECT j.bundle_id, r.recycle_to, j.state, t.local_album_id \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             LEFT JOIN local_tracks t ON t.id = COALESCE(r.replaced_track_id, \
                 CASE WHEN j.kind = 'output' THEN (SELECT a.track_id FROM library_publish_journal a \
                 WHERE a.bundle_id = j.bundle_id AND a.kind = 'audio' AND a.track_id IS NOT NULL \
                 ORDER BY a.id LIMIT 1) END) \
             WHERE j.bundle_id IN (SELECT value FROM json_each(?1)) AND " + (in_bin!("?2")) + " \
             ORDER BY j.bundle_id, r.journal_id";
    /// The undo records of the bundles in the JSON array `?1`: reason,
    /// album, and whether a removal row (`?2`, the kind) of it is still being
    /// written (`?3` to `?6`, the states).
    pub(super) BIN_RECORDS = "SELECT a.id, a.reason_code, a.local_album_id, \
             EXISTS (SELECT 1 FROM library_publish_journal j WHERE j.bundle_id = a.id \
             AND j.kind = ?2 \
             AND j.state IN (?3, ?4, ?5, ?6)) AS writing \
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
             WHERE e.local_album_id = a.id AND e.provider = 'musicbrainz') AS release_group_mbid, \
             EXISTS (SELECT 1 FROM local_album_artwork w WHERE w.local_album_id = a.id \
             AND w.source IN ('folder', 'embedded')) \
             AS cover_available \
             FROM chain JOIN local_albums a ON a.id = chain.id WHERE chain.next IS NULL";
    /// How many files the bin holds. `?1` is the state of a row that waits
    /// for an admin.
    pub(crate) BIN_COUNT = "SELECT COUNT(*) FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id WHERE " + (in_bin!("?1"));
    /// The copies an empty at `?1` (unix milliseconds) would try: the expiry's
    /// due rows with no retention, each with its row and track. `?2` is the
    /// state of a row that is done.
    pub(super) BIN_DUE = "SELECT r.journal_id, r.recycle_to, r.replaced_track_id \
             FROM library_publish_replacements r \
             JOIN library_publish_journal j ON j.id = r.journal_id \
             WHERE r.expired_at IS NULL AND r.recycled_at IS NOT NULL AND r.recycled_at < ?1 \
             AND j.state = ?2";
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
    /// or is still settling: a row of the delete kind (`?2`) not unwound
    /// (`?3`).
    pub(super) TAKE_BACK_EXISTS = "SELECT EXISTS (SELECT 1 FROM library_publish_operations o \
             JOIN library_publish_journal j ON j.bundle_id = o.bundle_id \
             WHERE instr(o.bundle_json, ?1) > 0 AND j.kind = ?2 \
             AND j.state <> ?3)";
    /// The landed files plugin `?1` added, newest first, before the point
    /// (`?2` unix milliseconds, then bundle id `?3`), at most `?4`: each
    /// bundle with when it was made and whether an undo took the file
    /// away or is still settling. `?5` and `?6` are the delete kind and the
    /// unwound state, `?7` the add kind, `?8` to `?10` a landed row's states.
    pub(super) PLUGIN_FILES = "SELECT o.bundle_id, o.created_at, o.bundle_json, \
             EXISTS (SELECT 1 FROM library_publish_operations t \
             JOIN library_publish_journal d ON d.bundle_id = t.bundle_id \
             WHERE d.kind = ?5 AND d.state <> ?6 \
             AND instr(t.bundle_json, '\"undoes\":') > 0 \
             AND json_extract(t.bundle_json, '$.added.undoes') = o.bundle_id) AS taken_back \
             FROM library_publish_journal a \
             JOIN library_publish_operations o ON o.bundle_id = a.bundle_id \
             WHERE a.kind = ?7 AND a.state IN (?8, ?9, ?10) \
             AND json_extract(o.bundle_json, '$.added.origin.plugin') = ?1 \
             AND (o.created_at < ?2 OR (o.created_at = ?2 AND o.bundle_id < ?3)) \
             ORDER BY o.created_at DESC, o.bundle_id DESC LIMIT ?4";
    /// Which of the bundles in the JSON array `?1` still stand: each wrote
    /// a journal row that was not unwound.
    pub(super) STANDING_BUNDLES = "SELECT DISTINCT bundle_id FROM library_publish_journal \
             WHERE bundle_id IN (SELECT value FROM json_each(?1)) AND state <> 'compensated'";
    /// A track's (`?1`) root, relative path and availability.
    pub(super) TRACK_PLACE = "SELECT root_id, relative_path, availability FROM local_tracks \
             WHERE id = ?1";
    /// Whether a track is listed in library folder `?1` below folder `?2`:
    /// a path from `?2/` up to `?20`, `0` being the character after `/`.
    pub(super) TRACK_LISTED_BELOW = "SELECT EXISTS (SELECT 1 FROM local_tracks \
             WHERE root_id = ?1 AND relative_path >= ?2 || '/' AND relative_path < ?2 || '0' \
             AND availability = 'indexed')";
    /// How many tracks are listed in library folder `?1` below folder `?2`,
    /// read as [`TRACK_LISTED_BELOW`] reads them.
    pub(super) TRACKS_LISTED_BELOW = "SELECT COUNT(*) FROM local_tracks \
             WHERE root_id = ?1 AND relative_path >= ?2 || '/' AND relative_path < ?2 || '0' \
             AND availability = 'indexed'";
    /// The paths of the tracks listed in library folder `?1` from `?2` up
    /// to, not including, `?3`.
    pub(super) TRACKS_LISTED_FROM = "SELECT relative_path FROM local_tracks \
             WHERE root_id = ?1 AND relative_path >= ?2 AND relative_path < ?3 \
             AND availability = 'indexed'";
    /// The track at root `?1`, path `?2`, with its availability.
    pub(super) TRACK_AT = "SELECT id, availability FROM local_tracks \
             WHERE root_id = ?1 AND relative_path = ?2";
    /// Move a track (`?1`) to path `?2` (absolute `?3`, hash `?4`).
    pub(super) TRACK_SET_PATH = "UPDATE local_tracks SET relative_path = ?2, file_path = ?3, \
             path_hash = ?4, row_revision = row_revision + 1 WHERE id = ?1";
    /// Whether any bundle has unsettled rows, or folders an unwind has not
    /// removed yet. `?1` to `?3` are the settled states.
    pub(crate) ANY_ACTIVE = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
             WHERE state NOT IN (?1, ?2, ?3)) \
             OR EXISTS (SELECT 1 FROM library_publish_created_folders)";
    /// Every hidden name the journal records, with the root and path of
    /// the destination it sits beside.
    pub(super) JOURNAL_NAMES = "SELECT dest_root, dest_rel, staged, backup \
             FROM library_publish_journal";
    /// Every root's last leftover check.
    pub(crate) LEFTOVER_CHECKS = "SELECT root_id, checked_at, file_count, files_json \
             FROM library_leftover_checks";
    /// Keep one root's check (`?1` root, `?2` when, `?3` count, `?4` JSON
    /// array of paths).
    pub(crate) LEFTOVER_KEEP = "INSERT INTO library_leftover_checks \
             (root_id, checked_at, file_count, files_json) VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (root_id) DO UPDATE SET checked_at = excluded.checked_at, \
             file_count = excluded.file_count, files_json = excluded.files_json";
    /// The folders the scan found to be drives or shares of their own.
    pub(crate) SCAN_MOUNTS = "SELECT root_id, relative_path FROM library_scan_mounts";
    /// Drop the checks of roots not in the JSON array `?1`.
    pub(crate) LEFTOVER_FORGET = "DELETE FROM library_leftover_checks \
             WHERE root_id NOT IN (SELECT value FROM json_each(?1))";
    /// The catalog rows a publish is checked against, for the track ids in
    /// the JSON array `?1`: each track's place, availability, stat
    /// revision and album, with its album's and its own MusicBrainz
    /// identity.
    pub(super) OWN_ROWS = "SELECT t.id, t.root_id, t.relative_path, t.availability, \
             t.stat_revision, t.local_album_id, \
             ai.row_revision AS album_revision, ai.release_group_mbid, ai.release_mbid, \
             ti.row_revision AS mapping_revision, ti.recording_mbid, ti.release_track_mbid \
             FROM local_tracks t \
             LEFT JOIN local_album_external_identities ai \
             ON ai.local_album_id = t.local_album_id AND ai.provider = 'musicbrainz' \
             LEFT JOIN local_track_external_identities ti \
             ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
             WHERE t.id IN (SELECT value FROM json_each(?1))";
    /// Record one operation, made at `?3` (unix milliseconds).
    pub(super) OPERATION_INSERT = "INSERT OR IGNORE INTO library_publish_operations \
             (bundle_id, bundle_json, created_at, applied_by_user_id) \
             VALUES (?1, ?2, ?3, ?4)";
    /// One recorded bundle's JSON (`?1`).
    pub(super) OPERATION_JSON = "SELECT bundle_json FROM library_publish_operations \
             WHERE bundle_id = ?1";
    /// The generated files publishes placed for one track (`?1`), oldest
    /// publish first, in plan order within each.
    pub(super) OUTPUTS_FOR_TRACK = "SELECT p.dest_root, p.dest_rel, p.sha256, p.size, p.kind \
             FROM library_publish_outputs p \
             JOIN library_publish_operations o ON o.bundle_id = p.bundle_id \
             WHERE p.track_id = ?1 \
             ORDER BY o.created_at, o.rowid, p.item_index, p.output_index";
    /// The generated files one publish (`?1`) placed, each with its track,
    /// in plan order.
    pub(super) OUTPUTS_OF_BUNDLE = "SELECT track_id, dest_root, dest_rel, sha256, size, kind \
             FROM library_publish_outputs WHERE bundle_id = ?1 \
             ORDER BY item_index, output_index";
    /// Bundles holding a snapshot expired by the day starting at `?1`.
    pub(super) EXPIRED_SNAPSHOT_BUNDLES = "SELECT DISTINCT bundle_id \
             FROM library_publish_snapshots WHERE expires_at <= ?1 ORDER BY bundle_id";
    /// A bundle's (`?1`) operation blob references.
    pub(super) DELETE_OPERATION_REFS = "DELETE FROM library_publish_blob_refs \
             WHERE owner_kind = 'operation' AND owner_id = ?1";
    /// A bundle's (`?1`) snapshots expired by the day starting at `?2`.
    pub(super) DELETE_EXPIRED_SNAPSHOTS = "DELETE FROM library_publish_snapshots \
             WHERE bundle_id = ?1 AND expires_at <= ?2";
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
    /// neither finished nor undone, at most `?2`. `?3` to `?6` are the states
    /// of such a write.
    pub(crate) RETRYING_BUNDLES = "SELECT j.bundle_id, COUNT(*) AS file_count, \
             GROUP_CONCAT(j.dest_rel, char(10)) AS sample_paths, \
             GROUP_CONCAT(j.source_rel, char(10)) AS source_paths \
             FROM library_publish_journal j \
             JOIN library_publish_operations o ON o.bundle_id = j.bundle_id \
             WHERE j.state IN (?3, ?4, ?5, ?6) \
             AND o.created_at <= ?1 \
             GROUP BY j.bundle_id ORDER BY j.bundle_id LIMIT ?2";
    /// When the oldest write still open or needing attention was made: `?1`
    /// to `?3` are the settled states, `?3` also the state that needs attention.
    pub(crate) OLDEST_OPEN_WRITE = "SELECT MIN(o.created_at) FROM library_publish_operations o \
             WHERE o.bundle_id IN (SELECT bundle_id FROM library_publish_journal \
             WHERE state NOT IN (?1, ?2, ?3) \
             OR (state = ?3 AND bundle_id NOT IN \
             (SELECT bundle_id FROM library_publish_resolutions)))";
    /// Whether a bundle that took files from folder `?1` is unsettled: `?2`
    /// to `?4` are the settled states, `?5` the one that needs attention.
    pub(super) STAGING_SETTLED = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
         WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
         WHERE substr(source_rel, 1, length(?1)) = ?1) \
         AND state NOT IN (?2, ?3, ?4) \
         AND NOT (state = ?5 AND bundle_id IN \
         (SELECT bundle_id FROM library_publish_resolutions)))";
    /// Whether a bundle that took files from folder `?1` needs a person: `?2`
    /// is the state that needs attention.
    pub(super) STAGING_NEEDS_ATTENTION = "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
         WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
         WHERE substr(source_rel, 1, length(?1)) = ?1) \
         AND state = ?2 AND bundle_id NOT IN \
         (SELECT bundle_id FROM library_publish_resolutions))";
    pub(super) LOAD_OPERATION = "SELECT bundle_json FROM library_publish_operations WHERE bundle_id = ?1";
    pub(super) APPLIED_BY = "SELECT applied_by_user_id FROM library_publish_operations WHERE bundle_id = ?1";
    pub(super) READ_REVISION = "SELECT value FROM library_catalog_revision WHERE singleton = 1";
    pub(super) LOCATE = "SELECT t.root_id, t.relative_path, \
             COALESCE(m.applied_projection_hash, '') AS applied_projection_hash, \
             m.last_outcome \
             FROM local_tracks t \
             LEFT JOIN library_track_management_state m ON m.local_track_id = t.id \
             WHERE t.id = ?1";
    pub(super) COMMIT_BUNDLE = "INSERT INTO library_track_management_state (local_track_id, managed_root_id, \
                 applied_projection_hash, written_sha256, last_outcome, last_managed_at) \
                 VALUES (?1, ?2, ?3, ?6, ?4, ?5) \
                 ON CONFLICT (local_track_id) DO UPDATE SET \
                 managed_root_id = excluded.managed_root_id, \
                 applied_projection_hash = excluded.applied_projection_hash, \
                 written_sha256 = excluded.written_sha256, \
                 last_outcome = excluded.last_outcome, \
                 last_managed_at = excluded.last_managed_at, \
                 row_revision = library_track_management_state.row_revision + 1";
    pub(super) MARK_MISSING = "UPDATE local_tracks SET availability = 'missing', missing_at = ?2 WHERE id = ?1";
    pub(super) RETIRE_REPLACED_TRACK = "UPDATE local_tracks SET availability = 'missing', missing_at = ?5, \
         relative_path = ?4, file_path = ?6, path_hash = ?7, \
         row_revision = row_revision + 1 \
         WHERE id = ?1 AND root_id = ?2 AND relative_path = ?3";
    pub(super) FORGET_REPLACED_STATE = "UPDATE library_track_management_state SET written_sha256 = NULL, \
         managed_path_revision = NULL, row_revision = row_revision + 1 \
         WHERE local_track_id = ?1";
    pub(super) GIVE_UP_DESTINATION = "SELECT id, availability FROM local_tracks \
             WHERE root_id = ?1 AND relative_path = ?2 AND id <> ?3";
    pub(super) WRITE_TRACK_PLACE = "UPDATE local_tracks SET root_id = ?2, relative_path = ?3, file_path = ?4, \
         path_hash = ?5, row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) MOVE_ALBUM_ROOT = "UPDATE local_albums SET root_id = ?2, row_revision = row_revision + 1 \
         WHERE id = (SELECT local_album_id FROM local_tracks WHERE id = ?1) \
         AND root_id <> ?2 AND NOT EXISTS (SELECT 1 FROM local_tracks t \
         WHERE t.local_album_id = local_albums.id AND t.root_id <> ?2 \
         AND t.availability = 'indexed')";
    pub(super) JOINED_ALBUM = "SELECT b.id, i.release_mbid, i.release_group_mbid FROM local_albums b \
             LEFT JOIN local_album_external_identities i \
             ON i.local_album_id = b.id AND i.provider = 'musicbrainz' \
             WHERE b.id = (SELECT COALESCE(retired_into_album_id, id) FROM local_albums \
             WHERE id = ?1)";
    pub(super) COPY_FOLDER = "SELECT root_id, relative_path, disc_number FROM local_tracks \
             WHERE local_album_id = ?1 AND availability = 'indexed'";
    pub(super) TRACK_AT_PATH = "SELECT id, local_album_id FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) LOCK_TRACK_MEMBERSHIP = "UPDATE local_tracks SET membership_source = 'manual', membership_locked = 1, \
             row_revision = row_revision + 1 WHERE id = ?1";
    pub(super) SET_TRACK_INGEST = "UPDATE local_tracks SET ingest_source = ?4, download_task_id = ?2, \
         source_path = ?3 WHERE id = ?1";
    pub(super) RECORD = "INSERT INTO library_publish_snapshots
             (id, bundle_id, track_id, blob_sha256, created_at, expires_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    pub(super) ADD_REF = "INSERT OR IGNORE INTO library_publish_blob_refs (sha256, owner_kind, owner_id)
             VALUES (?1, ?2, ?3)";
    pub(super) SNAPSHOT_ROW = "SELECT track_id, blob_sha256, expires_at / 86400000 AS expiry_day
                 FROM library_publish_snapshots WHERE id = ?1";
    pub(super) BUNDLE = "SELECT id, track_id, blob_sha256, expires_at / 86400000 AS expiry_day
             FROM library_publish_snapshots WHERE bundle_id = ?1 ORDER BY id";
    pub(super) WRITE_BASELINE = "INSERT OR IGNORE INTO library_publish_baselines
             (track_id, blob_sha256, original_root, original_rel, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)";
    pub(super) WRITE_BASELINE_BLOB_REF = "INSERT OR IGNORE INTO library_publish_blob_refs (sha256, owner_kind, owner_id)
             VALUES (?1, 'baseline', ?2)";
    pub(super) BASELINE_ROW = "SELECT blob_sha256, original_root, original_rel
                 FROM library_publish_baselines WHERE track_id = ?1";
    /// Keep a cover file's first replaced bytes for track `?1` at
    /// `?2`/`?3`; a cover kept there before wins.
    pub(super) WRITE_COVER_BASELINE = "INSERT OR IGNORE INTO library_publish_cover_baselines \
             (track_id, root_id, rel, blob_sha256, created_at) VALUES (?1, ?2, ?3, ?4, ?5)";
    /// The cover files kept as track `?1`'s originals.
    pub(super) COVER_BASELINES = "SELECT root_id, rel, blob_sha256 \
             FROM library_publish_cover_baselines WHERE track_id = ?1 ORDER BY root_id, rel";
    /// Whether bytes `?2` are a file a publish generated for track `?1` or
    /// another track of its album, at any path.
    pub(super) GENERATED_BEFORE = "SELECT EXISTS (SELECT 1 FROM library_publish_outputs \
             WHERE sha256 = ?2 AND (track_id = ?1 OR track_id IN (SELECT t.id FROM local_tracks t \
             WHERE t.local_album_id = (SELECT a.local_album_id FROM local_tracks a WHERE a.id = ?1))))";
    pub(super) V2_ORIGINAL_MISSING = "SELECT (EXISTS(SELECT 1 FROM library_track_management_state m \
                 WHERE m.local_track_id = ?1 AND m.baseline_id IS NOT NULL) \
                 OR EXISTS(SELECT 1 FROM library_management_baselines v \
                 WHERE v.local_track_id = ?1)) \
                 AND NOT EXISTS (SELECT 1 FROM library_publish_baselines b \
                 WHERE b.track_id = ?1)";
    pub(super) BASELINE_COUNT = "SELECT COUNT(*) FROM library_publish_baselines";
    pub(super) BASELINE_BLOB_REFS = "SELECT COUNT(DISTINCT sha256) FROM library_publish_blob_refs WHERE owner_kind = 'baseline'";
    /// True for a state row (alias `s`) that came from v2 with its record.
    /// Every v3 writer of `managed_path_revision` sets the job too, and the
    /// upgrade leaves v2's job behind.
    pub(crate) fragment V2_ROW_SQL = "s.written_sha256 IS NULL AND s.managed_path_revision IS NOT NULL \
     AND s.last_operation_job_id IS NULL AND s.last_outcome IN ('succeeded', 'undone')";
    /// On a `library_track_management_state` row (alias `s`): the app has a
    /// record of the file's bytes.
    pub(crate) fragment HAS_BYTES_RECORD = "(s.written_sha256 IS NOT NULL OR (" + V2_ROW_SQL + "))";
    pub(super) TRACK_WRITTEN_STATE = "SELECT s.written_sha256, CASE WHEN " + V2_ROW_SQL + " THEN s.managed_path_revision END AS v2_path_revision \
             FROM library_track_management_state s WHERE s.local_track_id = ?1";
    /// Forget the record of track `?1`'s file. `managed_path_revision` goes only where no job wrote it: such a value is v2's.
    pub(super) FORGET_RECORD = "UPDATE library_track_management_state AS s SET written_sha256 = NULL, \
             managed_path_revision = CASE WHEN s.last_operation_job_id IS NULL THEN NULL \
             ELSE s.managed_path_revision END, row_revision = s.row_revision + 1 \
             WHERE s.local_track_id = ?1 AND " + HAS_BYTES_RECORD;
    /// Whether another row of track `?1`'s album, whose file went missing, has a record.
    pub(super) MISSING_ROW_STANDS_IN = "SELECT 1 FROM local_tracks t \
             JOIN local_tracks o ON o.local_album_id = t.local_album_id AND o.id <> t.id \
             AND +o.availability = 'missing' \
             JOIN library_track_management_state s ON s.local_track_id = o.id \
             WHERE t.id = ?1 AND " + HAS_BYTES_RECORD + " LIMIT 1";

    pub(super) row JournalRow for [JOURNAL_BY_ID, JOURNAL_BY_BUNDLE] {
        id: String,
        bundle_id: String,
        kind: JournalKind,
        source_root: Option<String>,
        source_rel: Option<String>,
        dest_root: String,
        dest_rel: String,
        staged: String,
        backup: Option<String>,
        source_sha256: Option<String>,
        staged_sha256: String,
        track_id: Option<String>,
        commit_as: Option<MgmtState>,
        state: JournalState,
        seq: i64,
        replaced_rel: Option<String>,
        replaced_sha256: Option<String>,
        replaced_track_id: Option<String>,
        recycle_to: Option<String>,
    }
    pub(super) row ExpiryDueRow for [EXPIRY_DUE] {
        journal_id: String,
        recycle_to: String,
        replaced_sha256: String,
        recycled_at: i64,
        replaced_track_id: Option<String>,
        binned_stat_json: Option<String>,
    }
    pub(super) row ExpiryHeldRow for [EXPIRY_HELD_PUT_BACKS] {
        id: String,
        bundle_id: String,
        track_id: Option<String>,
    }
    pub(super) row BinEntry for [EXPIRY_ENTRIES] {
        recycle_to: String,
        left_bin: bool,
    }
    pub(super) row CreatedFolder for [CREATED_FOLDERS] {
        root_id: String,
        folder_rel: String,
    }
    pub(super) row AlbumRemoval for [ALBUM_REMOVALS] {
        id: String,
        reason_code: Option<String>,
        created_at: i64,
        files: u32,
        held: bool,
    }
    pub(super) row BinPageRow for [BIN_PAGE] {
        bundle_id: String,
        removed_at: i64,
        first_binned: Option<i64>,
        removal: bool,
    }
    pub(super) row BinFile for [BIN_FILES] {
        bundle_id: String,
        recycle_to: String,
        state: String,
        local_album_id: Option<String>,
    }
    pub(super) row BinRecord for [BIN_RECORDS] {
        id: String,
        reason_code: Option<String>,
        local_album_id: Option<String>,
        writing: bool,
    }
    pub(super) row BinAlbumRow for [BIN_ALBUM] {
        id: String,
        title: String,
        album_artist_name: Option<String>,
        has_indexed_track: bool,
        release_group_mbid: Option<String>,
        cover_available: bool,
    }
    pub(super) row BinDueRow for [BIN_DUE] {
        journal_id: String,
        recycle_to: String,
        replaced_track_id: Option<String>,
    }
    pub(crate) row BinEmptiedRow for [BIN_EMPTIED_LAST] {
        finished_at: i64,
        display_name: Option<String>,
        deleted: u32,
        freed_bytes: i64,
        older_folders: u32,
        kept: u32,
        waited: u32,
        kept_json: String,
    }
    pub(super) row PluginFileRow for [PLUGIN_FILES] {
        bundle_id: String,
        created_at: i64,
        bundle_json: String,
        taken_back: bool,
    }
    pub(super) row TrackPlace for [TRACK_PLACE] {
        root_id: String,
        relative_path: String,
        availability: Availability,
    }
    pub(super) row TrackAvailability for [TRACK_AT, GIVE_UP_DESTINATION] {
        id: String,
        availability: Availability,
    }
    pub(super) row JournalName for [JOURNAL_NAMES] {
        dest_root: String,
        dest_rel: String,
        staged: String,
        backup: Option<String>,
    }
    pub(crate) row LeftoverCheck for [LEFTOVER_CHECKS] {
        root_id: String,
        checked_at: i64,
        file_count: i64,
        files_json: String,
    }
    pub(crate) row ScanMount for [SCAN_MOUNTS] {
        root_id: String,
        relative_path: String,
    }
    pub(super) row OwnRowRead for [OWN_ROWS] {
        id: String,
        root_id: String,
        relative_path: String,
        availability: Availability,
        stat_revision: String,
        local_album_id: String,
        album_revision: Option<i64>,
        release_group_mbid: Option<String>,
        release_mbid: Option<String>,
        mapping_revision: Option<i64>,
        recording_mbid: Option<String>,
        release_track_mbid: Option<String>,
    }
    pub(super) row TrackOutput for [OUTPUTS_FOR_TRACK] {
        dest_root: String,
        dest_rel: String,
        sha256: String,
        size: u64,
        kind: String,
    }
    pub(super) row BundleOutput for [OUTPUTS_OF_BUNDLE] {
        track_id: String,
        dest_root: String,
        dest_rel: String,
        sha256: String,
        size: u64,
        kind: String,
    }
    pub(super) row LegacyBlob for [LEGACY_BLOBS_AFTER] {
        sha256: String,
        bytes: Vec<u8>,
    }
    pub(super) row LocateRow for [LOCATE] {
        root_id: String,
        relative_path: String,
        applied_projection_hash: String,
        last_outcome: Option<String>,
    }
    pub(super) row JoinedAlbumRow for [JOINED_ALBUM] {
        id: String,
        release_mbid: Option<String>,
        release_group_mbid: Option<String>,
    }
    pub(super) row CopyFolderRow for [COPY_FOLDER] {
        root_id: String,
        relative_path: String,
        disc_number: i64,
    }
    pub(super) row TrackAtPath for [TRACK_AT_PATH] {
        id: String,
        local_album_id: String,
    }
    pub(super) row SnapshotRow for [SNAPSHOT_ROW] {
        track_id: String,
        blob_sha256: String,
        expiry_day: i64,
    }
    pub(super) row BundleSnapshot for [BUNDLE] {
        id: String,
        track_id: String,
        blob_sha256: String,
        expiry_day: i64,
    }
    pub(super) row CoverBaseline for [COVER_BASELINES] {
        root_id: String,
        rel: String,
        blob_sha256: String,
    }
    pub(super) row BaselineRow for [BASELINE_ROW] {
        blob_sha256: String,
        original_root: String,
        original_rel: String,
    }
    pub(super) row WrittenState for [TRACK_WRITTEN_STATE] {
        written_sha256: Option<String>,
        v2_path_revision: Option<String>,
    }
}

#[cfg(test)]
mod tests {
    use super::JournalState;

    /// [`album_gone_through_bin!`] writes the held state as text.
    #[test]
    fn the_bin_rule_names_the_held_state() {
        let state = format!("gbj.state = '{}'", JournalState::NeedsAttention.as_str());
        assert!(album_gone_through_bin!("a.id").contains(&state));
    }
}
