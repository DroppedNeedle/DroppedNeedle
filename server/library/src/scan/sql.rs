//! The scan store's newer statements: the failure row a window or page
//! records, and for its chunked clean-ups the rows each one reads before
//! it starts and the one-row deletes its chunks run.

use super::sqlite_store::RUN_COLUMNS;

droppedneedle_kernel::sql! {
    /// Record one failure row of a run; a repeat of the same row is kept once.
    pub(super) RECORD_FAILURE = "INSERT OR IGNORE INTO library_scan_failures \
             (run_id, root_id, relative_path, failure_code, failure_detail, phase, recorded_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";
    /// Note that run `?1` owes its end passes from pass `?2` (code `?3`, at `?4`).
    pub(super) OWE_END_PASSES = "INSERT OR IGNORE INTO library_scan_failures \
             (run_id, root_id, relative_path, failure_code, failure_detail, phase, recorded_at) \
             VALUES (?1, '', ?2, ?3, 'The scan is settling what it found.', 'reconciling', ?4)";
    /// The ended runs that owe end passes (code `?1`): run, next pass, end time.
    pub(super) OWED_END_PASSES = "SELECT run_id, relative_path, recorded_at FROM library_scan_failures \
             WHERE failure_code = ?1 ORDER BY recorded_at, run_id";
    /// The ended runs that still owe end passes (code `?1`), oldest first.
    pub(super) RUNS_OWING_END_PASSES = "SELECT " + RUN_COLUMNS + " FROM library_scan_runs r \
             WHERE terminal_at IS NOT NULL AND EXISTS (SELECT 1 FROM library_scan_failures f \
               WHERE f.run_id = r.id AND f.failure_code = ?1) \
             ORDER BY terminal_at, id";
    /// Move run `?1`'s note on to pass `?2` (code `?3`).
    pub(super) NEXT_END_PASS = "UPDATE library_scan_failures SET relative_path = ?2 \
             WHERE run_id = ?1 AND failure_code = ?3";
    /// Run `?1` owes no more end passes (code `?2`).
    pub(super) END_PASSES_DONE = "DELETE FROM library_scan_failures WHERE run_id = ?1 AND failure_code = ?2";
    /// The oldest finished run whose inventory waits for clean-up and that
    /// owes no end passes (code `?1`).
    pub(super) CLEANUP_TARGET = "SELECT id FROM library_scan_runs r \
             WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 1 \
             AND NOT EXISTS (SELECT 1 FROM library_scan_failures f \
               WHERE f.run_id = r.id AND f.failure_code = ?1) \
             ORDER BY terminal_at, id LIMIT 1";
    /// A run's failure rows, oldest first, without the internal note (code `?2`).
    pub(super) RUN_FAILURES = "SELECT root_id, relative_path, failure_code, recorded_at, failure_detail, phase \
             FROM library_scan_failures WHERE run_id = ?1 AND failure_code != ?2 ORDER BY rowid";
    /// One keyset page of a run's failure rows, without the internal note (code `?4`).
    pub(super) RUN_FAILURES_PAGE = "SELECT rowid, root_id, relative_path, failure_code, recorded_at, \
             failure_detail, phase FROM library_scan_failures \
             WHERE run_id = ?1 AND (?2 IS NULL OR rowid > ?2) AND failure_code != ?4 \
             ORDER BY rowid LIMIT ?3";
    /// The rowids of one finished run's album moves.
    pub(super) ALBUM_MOVE_ROWIDS = "SELECT rowid FROM library_scan_album_moves WHERE run_id = ?1";
    /// The rowids of one finished run's inventory.
    pub(super) INVENTORY_ROWIDS = "SELECT rowid FROM library_scan_inventory WHERE run_id = ?1";
    /// Delete one album move of run `?2`.
    pub(super) DELETE_ALBUM_MOVE = "DELETE FROM library_scan_album_moves WHERE rowid = ?1 AND run_id = ?2";
    /// Delete one inventory row of run `?2`.
    pub(super) DELETE_INVENTORY_ROW = "DELETE FROM library_scan_inventory WHERE rowid = ?1 AND run_id = ?2";
    /// A finished run whose inventory is all gone stops waiting for clean-up.
    pub(super) CLEANUP_DONE = "UPDATE library_scan_runs SET inventory_cleanup_pending = 0 \
             WHERE id = ?1 AND NOT EXISTS (SELECT 1 FROM library_scan_inventory WHERE run_id = ?1)";
    /// Finished, cleaned runs past the newest `?1`, which history pruning removes.
    pub(super) PRUNED_RUNS = "SELECT id FROM library_scan_runs \
             WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 0 \
             ORDER BY terminal_at DESC, id DESC LIMIT -1 OFFSET ?1";
    /// Delete one run.
    pub(super) DELETE_RUN = "DELETE FROM library_scan_runs WHERE id = ?1";
    /// The rowids of a run's inventory rows whose scope is gone or was walked again since.
    pub(super) STALE_INVENTORY_ROWIDS = "SELECT i.rowid FROM library_scan_inventory i \
             LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 \
             AND (s.scope_sequence IS NULL OR s.discovery_generation != i.discovery_generation)";
    /// Delete one inventory row of run `?2` while it is still stale.
    pub(super) DELETE_STALE_INVENTORY_ROW = "DELETE FROM library_scan_inventory \
             WHERE rowid = ?1 AND run_id = ?2 AND NOT EXISTS ( \
             SELECT 1 FROM library_scan_run_scopes s WHERE s.run_id = library_scan_inventory.run_id \
             AND s.root_id = library_scan_inventory.root_id \
             AND s.relative_path = library_scan_inventory.scope_relative_path \
             AND s.discovery_generation = library_scan_inventory.discovery_generation)";
}
