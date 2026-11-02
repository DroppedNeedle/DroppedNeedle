//! The rules a scan judges tracks by, as SQL fragments, and the macros
//! that write them for a given run parameter and row alias.

// `sql!` needs a macro call in parentheses to take it as one part.
#![allow(unused_parens, clippy::double_parens)]

/// Whether the folder at the SQL expression `$ancestor` covers the path at
/// `$descendant`: `.` covers everything, otherwise the path itself or
/// anything below it ([`scope_covers_path`](crate::scan::models::scope_covers_path)).
macro_rules! covers {
    ($ancestor:literal, $descendant:literal) => {
        concat!(
            "(",
            $ancestor,
            " = '.' OR ",
            $descendant,
            " = ",
            $ancestor,
            " OR substr(",
            $descendant,
            ", 1, length(",
            $ancestor,
            ") + 1) = ",
            $ancestor,
            " || '/')"
        )
    };
}
pub(crate) use covers;

/// Whether run `$run` refused a scope covering the row aliased `$row` because
/// it is a link or reached through one, asked for or met by a walk
/// (`refuse_link`). A completed scope around it did not walk there, so nothing
/// under it counts as walked or counts toward the mass-missing guard.
macro_rules! refused_scope {
    ($run:literal, $row:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM library_scan_run_scopes x WHERE x.run_id = ",
            $run,
            " AND x.root_id = ",
            $row,
            ".root_id AND x.error_code = 'SCOPE_THROUGH_LINK' AND (",
            $row,
            ".relative_path = x.relative_path OR substr(",
            $row,
            ".relative_path, 1, length(x.relative_path) + 1) = x.relative_path || '/'))"
        )
    };
}
pub(crate) use refused_scope;

/// Whether the track row aliased `$track` is held by the held folder
/// aliased `$hold`: indexed, found by a scan, in that folder of its root,
/// and first indexed at or after the hold began.
macro_rules! held {
    ($track:literal, $hold:literal) => {
        concat!(
            $track,
            ".availability = 'indexed' AND ",
            $track,
            ".ingest_source = 'scan' AND ",
            $track,
            ".root_id = ",
            $hold,
            ".root_id AND ",
            $track,
            ".imported_at >= ",
            $hold,
            ".held_at AND (",
            $hold,
            ".relative_path = '.' OR ",
            $track,
            ".relative_path = ",
            $hold,
            ".relative_path OR substr(",
            $track,
            ".relative_path, 1, length(",
            $hold,
            ".relative_path) + 1) = ",
            $hold,
            ".relative_path || '/')"
        )
    };
}
pub(crate) use held;

/// Whether the track row aliased `$track` keeps its album in the run with
/// id `$run` (a parameter, a column or a subquery): an album keeps a row
/// while it is indexed, or while it went `missing` after that run began.
/// A row that is `excluded`, or `missing` since before the run began,
/// holds nothing.
macro_rules! holds {
    ($track:literal, $run:tt) => {
        droppedneedle_kernel::db::statement::__concatcp!(
            "(",
            $track,
            ".availability = 'indexed' OR (",
            $track,
            ".availability = 'missing' AND ",
            $track,
            ".missing_at >= (SELECT started_at FROM library_scan_runs WHERE id = ",
            $run,
            ")))"
        )
    };
}
pub(crate) use holds;

/// Journal row `j` keeps scope `s` from judging its track: it is still
/// open, or it moved after the scope's walk began. The states listed are
/// the publisher's terminal ones (`JournalState::is_terminal`).
macro_rules! fenced_by {
    () => {
        "(j.state NOT IN ('cleaned', 'compensated', 'needs_attention') \
          OR j.fence_revision > s.walk_revision)"
    };
}
pub(crate) use fenced_by;

/// Journal row `j` needs attention and no administrator has marked its
/// write handled. It holds its track back from being judged gone, and a
/// move's destination back from being listed (`FENCED_PATHS`). A scan
/// still indexes a file it finds at the track's path, unless that path is
/// such a destination.
macro_rules! awaits_admin {
    () => {
        "(j.state = 'needs_attention' \
          AND j.bundle_id NOT IN (SELECT bundle_id FROM library_publish_resolutions))"
    };
}
pub(crate) use awaits_admin;

/// The one rule for "this run walked past the track" (aliased `t`; the
/// run id is the parameter `$run`): the walk did not see the file, and a
/// scope of the run covering its path finished its walk with a recorded
/// walk revision, no scope of the run covering it was refused as a link, and
/// no library write naming the track is still open,
/// needs attention and is not marked handled, or moved after that walk
/// began. A write names a track by its journal row's `track_id`, or, for
/// a removal or a replaced file, by the row's replacement. The journal
/// probe runs last: it is the rarest to match.
macro_rules! walked_past {
    ($run:literal) => {
        concat!(
            "(NOT EXISTS (SELECT 1 FROM library_scan_inventory i WHERE i.run_id = ",
            $run,
            " AND i.root_id = t.root_id AND i.relative_path = t.relative_path) \
             AND EXISTS (SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = ",
            $run,
            " AND s.root_id = t.root_id AND s.discovery_state = 'completed' \
              AND s.walk_revision IS NOT NULL \
              AND (s.relative_path = '.' OR t.relative_path = s.relative_path \
                OR substr(t.relative_path, 1, length(s.relative_path) + 1) = s.relative_path || '/') \
              AND NOT ",
            $crate::scan::sqlite_store::sql::refused_scope!($run, "t"),
            " AND NOT EXISTS (SELECT 1 FROM library_publish_journal j \
                WHERE j.track_id = t.id AND (",
            $crate::scan::sqlite_store::sql::fenced_by!(),
            " OR ",
            $crate::scan::sqlite_store::sql::awaits_admin!(),
            ")) AND NOT EXISTS (SELECT 1 FROM library_publish_replacements r \
                JOIN library_publish_journal j ON j.id = r.journal_id \
                WHERE r.replaced_track_id = t.id AND (",
            $crate::scan::sqlite_store::sql::fenced_by!(),
            " OR ",
            $crate::scan::sqlite_store::sql::awaits_admin!(),
            "))))"
        )
    };
}
pub(crate) use walked_past;

/// The place a moved file must keep for the gone track `t` to continue
/// into it: the same folded album title and album artist, disc and track
/// number, each given as the new file's SQL value.
macro_rules! same_place {
    ($title:literal, $artist:literal, $disc:literal, $track:literal) => {
        concat!(
            "(t.album_title_folded = ",
            $title,
            " AND COALESCE(t.album_artist_name_folded, '') = ",
            $artist,
            " AND t.disc_number = ",
            $disc,
            " AND t.track_number = ",
            $track,
            ")"
        )
    };
}
pub(crate) use same_place;

/// Whether run `$run` held back a folder covering the row aliased `$row`: a
/// mass-missing guard failure on its root at a folder that [`covers`] its
/// path, a scan scope or a mount found empty. Nothing under a held-back
/// folder counts as walked: it is not missing, gone or moved. The guard
/// index is named: SQLite would otherwise read every failure of the run on
/// the root through the primary key, since that one covers the query.
macro_rules! held_back {
    ($run:literal, $row:literal) => {
        concat!(
            "EXISTS (SELECT 1 FROM library_scan_failures g \
             INDEXED BY idx_scan_failures_guard WHERE g.run_id = ",
            $run,
            " AND g.root_id = ",
            $row,
            ".root_id AND g.failure_code = 'MASS_MISSING_GUARD' \
             AND (g.relative_path = '.' OR ",
            $row,
            ".relative_path = g.relative_path OR substr(",
            $row,
            ".relative_path, 1, length(g.relative_path) + 1) = g.relative_path || '/'))"
        )
    };
}
pub(crate) use held_back;

droppedneedle_kernel::sql! {
    /// A track this run walked past, whether or not the mass-missing guard
    /// held its scope back.
    pub(crate) fragment WALKED_PAST = (walked_past!("?1"));
    /// A track is gone when it is already marked missing, or when this run
    /// walked its folder to the end without seeing the file. A folder the
    /// mass-missing guard held back (an unmounted share looks like that), a
    /// scope or a mount found empty, does not count as walked.
    pub(crate) fragment TRACK_GONE = "(t.availability = 'missing' OR (t.availability = 'indexed' AND " + (walked_past!("?1")) + " AND NOT " + (held_back!("?1", "t")) + "))";
    /// A track still counts as present unless it is missing or this run
    /// walked its folder to the end without seeing it, and did not hold back a
    /// folder covering it.
    pub(crate) fragment TRACK_PRESENT = "(t.availability = 'indexed' AND NOT (" + (walked_past!("?1")) + " AND NOT " + (held_back!("?1", "t")) + "))";
    /// How a gone track `t` and an indexed track `n` match: `same_place!`,
    /// then the recording id `n`'s file holds, or with none the same title,
    /// both read from the tags. The lengths of a pair without a recording id
    /// are compared after (`same_length`). As `find_moved_track` reads
    /// `n`'s file, so a file read before its old copy went pairs as one read
    /// after.
    pub(crate) fragment MOVED_MATCH = (same_place!(
        "n.album_title_folded",
        "COALESCE(n.album_artist_name_folded, '')",
        "n.disc_number",
        "n.track_number"
    )) + " AND CASE WHEN trim(COALESCE(n.embedded_recording_mbid, '')) <> '' \
     THEN t.embedded_recording_mbid = trim(n.embedded_recording_mbid) \
     ELSE t.title_folded = n.title_folded AND t.title_provenance = 'tag' \
       AND n.title_provenance = 'tag' END";
    /// What [`MOVED_MATCH`] leaves to compare: the pair's ids and albums,
    /// whether `n` holds a recording id, and the two lengths.
    pub(crate) fragment MOVED_COLUMNS = "t.id AS old_id, t.local_album_id AS old_album_id, n.id AS new_id, \
    n.local_album_id AS new_album_id, \
    trim(COALESCE(n.embedded_recording_mbid, '')) <> '' AS recording, \
    t.duration_seconds AS old_seconds, n.duration_seconds AS new_seconds";
    pub(crate) fragment SAME_PLACE = (same_place!("?3", "?4", "?5", "?6"));
}
