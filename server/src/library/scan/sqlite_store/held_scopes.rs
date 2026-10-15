//! Library folders the mass-missing guard holds back, and the new files in
//! them that automatic management leaves alone meanwhile.
//!
//! A scan that finds most of a folder's files gone at once marks none of
//! them missing (an unmounted share looks like that). If the files were a
//! person's retag and rename instead, the old rows stay indexed, so no
//! check can see that the new album replaced the old one, and automatic
//! management would write the edition over the retag. So the folder is
//! held: each run end records the folders it held back
//! (`library_scan_held_scopes`, kept past the run history), and an album
//! with a scanned file first indexed in a held folder since the hold began
//! is not written by itself. Rows that were there before the hold are not
//! held: a retag that keeps them keeps their records too, which the
//! takeover checks see.
//!
//! A hold lifts at the end of a completed run that walked a folder
//! covering it to the end without holding that folder back, unless a
//! file under it went missing in that run from an album that holds no
//! indexed file and is not merged into another: the retag join may still
//! take such an album over after its one-scan wait, so the hold stays one
//! more clean run. Albums parked on the hold are then looked at again.

use rusqlite::{Connection, Transaction, params};

use crate::library::management::automatic;

/// Whether the folder at the SQL expression `ancestor` covers the path at
/// `descendant`: `.` covers everything, otherwise the path itself or
/// anything below it ([`scope_covers_path`](crate::library::scan::models::scope_covers_path)).
fn covers(ancestor: &str, descendant: &str) -> String {
    format!(
        "({ancestor} = '.' OR {descendant} = {ancestor} \
         OR substr({descendant}, 1, length({ancestor}) + 1) = {ancestor} || '/')"
    )
}

/// Lift the holds run `run_id` settled, then record the folders it held
/// back, then wake the albums parked on a lifted hold. Part of the run-end
/// transaction.
pub(super) fn settle(tx: &Transaction<'_>, run_id: &str, now: f64) -> rusqlite::Result<()> {
    let lifted = tx.execute(
        &format!(
            "DELETE FROM library_scan_held_scopes AS h \
             WHERE EXISTS (SELECT 1 FROM library_scan_runs r \
               JOIN library_scan_run_scopes s ON s.run_id = r.id \
               WHERE r.id = ?1 AND r.state = 'completed' AND s.root_id = h.root_id \
               AND s.discovery_state = 'completed' AND {scope_covers_held} \
               AND NOT EXISTS (SELECT 1 FROM library_scan_failures f WHERE f.run_id = r.id \
                 AND f.root_id = s.root_id AND f.relative_path = s.relative_path \
                 AND f.failure_code = 'MASS_MISSING_GUARD')) \
             AND NOT EXISTS (SELECT 1 FROM local_tracks t \
               JOIN local_albums a ON a.id = t.local_album_id \
               WHERE t.availability = 'missing' AND t.root_id = h.root_id \
               AND t.missing_since >= (SELECT COALESCE(started_at, ?2) \
                 FROM library_scan_runs WHERE id = ?1) \
               AND {held_covers_track} AND a.retired_into_album_id IS NULL \
               AND NOT EXISTS (SELECT 1 FROM local_tracks k \
                 WHERE k.local_album_id = a.id AND k.availability = 'indexed'))",
            scope_covers_held = covers("s.relative_path", "h.relative_path"),
            held_covers_track = covers("h.relative_path", "t.relative_path"),
        ),
        params![run_id, now],
    )?;
    tx.execute(
        "INSERT INTO library_scan_held_scopes (root_id, relative_path, held_at) \
         SELECT DISTINCT f.root_id, f.relative_path, \
           CAST(COALESCE(r.started_at, ?2) * 1000 AS INTEGER) \
         FROM library_scan_failures f JOIN library_scan_runs r ON r.id = f.run_id \
         WHERE f.run_id = ?1 AND f.failure_code = 'MASS_MISSING_GUARD' \
         ON CONFLICT (root_id, relative_path) DO NOTHING",
        params![run_id, now],
    )?;
    if lifted > 0 {
        automatic::wake(tx, Some(automatic::SCAN_HELD_BACK), now)?;
        tracing::info!(
            run_id,
            folders = lifted,
            "scan found held-back folders as expected; their new files may be managed again"
        );
    }
    Ok(())
}

/// Whether automatic management leaves the album alone because the scan
/// holds a folder back: one of its scanned files sits in a held folder
/// and was first indexed after the hold began.
pub(crate) fn holds_new_files(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    conn.prepare_cached(&format!(
        "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         CROSS JOIN library_scan_held_scopes h ON h.root_id = t.root_id \
         WHERE t.local_album_id = ?1 AND t.availability = 'indexed' \
         AND t.ingest_source = 'scan' \
         AND CAST(t.imported_at * 1000 AS INTEGER) >= h.held_at AND {})",
        covers("h.relative_path", "t.relative_path")
    ))?
    .query_row(params![album_id], |row| row.get(0))
}
