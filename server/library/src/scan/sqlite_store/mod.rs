//! SQLite-backed scan store: durable runs, scopes, inventory, failures,
//! and the track catalog over the 0001 baseline tables.
//!
//! [`SqliteScanStore`] keeps the disposition, transition, and
//! verdict semantics: the cover rule, union
//! normalization, idempotent control answers, the legacy-mtime band with
//! promotion, and the deferred re-offer. `runs` holds the run lifecycle,
//! `inventory` the discovery inventory, `catalog` the track catalog.
//! The catalog lives in `local_tracks` (keyed by `root_id, relative_path`),
//! so [`commit_window`](SqliteScanStore::commit_window) also writes the
//! `local_*` rows the catalog shows (`availability = 'indexed'`).
//!
//! The store holds the shared [`Db`] and no connection. The request path
//! reads through the request pool and writes in the foreground class; the
//! scan worker reads through the long pool with the run's stop flag and
//! writes in the background or bulk class, answering a busy database by
//! [`retry_busy`]. Many rows go in chunks. Every method returns a
//! `Result`: a read that fails is an error, never an empty answer.
//!
//! Deferred markers stay in-process. They re-arm whenever tag-read
//! exhaustion recurs, so a restart only loses the re-offer shortcut, and
//! the persisted `TAG_READ_DEFERRED` failure rows still show the history.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};
use sha2::{Digest as _, Sha256};

use super::models::{
    Counter, Counters, Disposition, EffectivePolicy, Event, FailureCode, PhaseTimings, RunState,
    ScanFailureRecord, ScanInventoryItem, ScanKind, ScanPhase, ScanRequest, ScanRequestResult,
    ScanRun, ScanScope, ScanState, ScopeDiscoveryState, Verdict, scope_covers_path,
};
use super::revision::{exact_stat_revision, legacy_mtime_eps_seconds};
use super::stop::{Backoff, RunStop, retry_busy};
use super::store::{
    CatalogEntry, ClassifyInput, CommitIndexedItem, IndexWindow, InventoryPage, RevisionKind,
    ScanStoreError, UnreadRows, WindowOutcome,
};
use crate::scan::sql::{FailureRow, InventoryRow, RunRow, ScopeRow};
pub use artwork::ArtworkSweep;
use droppedneedle_kernel::db::writer::Lane;
use droppedneedle_kernel::db::{ChunkedError, Db, fold_text};
pub(crate) use held_scopes::{holds_new_files, judge_remembered, remembered};
pub use lyrics::LyricsSweep;

/// Terminal runs kept past cleanup before history pruning.
const HISTORY_KEPT: usize = 50;

/// Rows a chunked scan write commits per turn.
const CHUNK_ROWS: usize = 500;

/// One root's catalog snapshot for in-memory classify. Discovery issues
/// hundreds of classify calls against an unchanging catalog; one bulk load
/// replaces hundreds of random-probe `IN (...)` queries. The version ties
/// the snapshot to this store's mutation counter, and the shared catalog
/// revision to writes from elsewhere (a managed move commits through the
/// publisher's own connection); terminal invalidation drops the snapshot
/// so idle scans hold no catalog memory.
struct CachedCatalog {
    version: u64,
    shared_revision: i64,
    entries: HashMap<String, CatalogEntry>,
}

/// What the store keeps in memory. Its mutex is never held while a call
/// waits for a lane turn or a pool connection.
struct Inner {
    deferred: HashSet<(String, String)>,
    catalog_version: u64,
    catalog_cache: HashMap<String, CachedCatalog>,
}

/// The scan store, over the application database.
pub struct SqliteScanStore {
    db: Db,
    /// Wall time for the artwork and lyrics checks.
    clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
    inner: Mutex<Inner>,
}

impl SqliteScanStore {
    /// The store over `db`, dating its checks by `clock`. It creates no
    /// schema itself.
    pub fn new(db: Db, clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>) -> Self {
        Self {
            db,
            clock,
            inner: Mutex::new(Inner {
                deferred: HashSet::new(),
                catalog_version: 0,
                catalog_cache: HashMap::new(),
            }),
        }
    }

    /// The database the store reads and writes. The activity feed, the
    /// pause switch, the diagnostics export and tests reach it through here.
    pub fn db(&self) -> &Db {
        &self.db
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A request-path read on the request pool.
    fn read<T>(
        &self,
        name: &'static str,
        op: impl FnOnce(&Connection) -> Result<T, ScanStoreError>,
    ) -> Result<T, ScanStoreError> {
        self.db.read_blocking(name, op)
    }

    /// A scan-worker read on the long pool, ended by the run's stop flag.
    fn read_long<T>(
        &self,
        stop: &RunStop,
        name: &'static str,
        op: impl FnOnce(&Connection) -> Result<T, ScanStoreError>,
    ) -> Result<T, ScanStoreError> {
        self.db.read_long(name, stop.flag(), op)
    }

    /// One write, tried once: the request path answers a busy database
    /// with 503.
    fn write_once<T>(
        &self,
        lane: Lane,
        name: &'static str,
        op: impl FnOnce(&Transaction<'_>) -> Result<T, ScanStoreError>,
    ) -> Result<T, ScanStoreError> {
        self.db.write_blocking(lane, name, op)
    }

    /// One scan-worker write, tried again while the database is busy.
    fn write<T>(
        &self,
        stop: &RunStop,
        lane: Lane,
        name: &'static str,
        mut op: impl FnMut(&Transaction<'_>) -> Result<T, ScanStoreError>,
    ) -> Result<T, ScanStoreError> {
        retry_busy(stop, name, || self.db.write_blocking(lane, name, &mut op))
    }

    /// Delete or update `items` in chunks, each its own bulk turn and each
    /// a complete change, ended early by the run's stop flag. A chunk the
    /// database was too busy for is tried again from where it stopped.
    /// Returns what `chunk` counted over the chunks that committed.
    fn write_chunked<T>(
        &self,
        stop: &RunStop,
        name: &'static str,
        items: &[T],
        chunk: impl FnMut(&[T], &Transaction<'_>) -> Result<usize, ScanStoreError>,
    ) -> Result<usize, ScanStoreError> {
        self.write_chunked_by(stop, name, items, CHUNK_ROWS, chunk)
    }

    /// [`Self::write_chunked`] with `chunk_rows` items a turn, for chunks
    /// whose items each cost more than a row.
    fn write_chunked_by<T>(
        &self,
        stop: &RunStop,
        name: &'static str,
        items: &[T],
        chunk_rows: usize,
        mut chunk: impl FnMut(&[T], &Transaction<'_>) -> Result<usize, ScanStoreError>,
    ) -> Result<usize, ScanStoreError> {
        let mut done = 0usize;
        let mut counted = 0usize;
        let mut backoff = Backoff::new();
        loop {
            // One count per chunk the closure finished; a chunk that then
            // failed to commit is past the committed ones and not summed.
            let mut counts: Vec<usize> = Vec::new();
            let written = self.db.write_chunked_blocking(
                name,
                &items[done..],
                chunk_rows,
                stop.flag(),
                |rows: &[T], tx| -> Result<(), ScanStoreError> {
                    counts.push(chunk(rows, tx)?);
                    Ok(())
                },
            );
            let committed =
                |rows: usize| -> usize { counts.iter().take(rows.div_ceil(chunk_rows)).sum() };
            match written {
                Ok(_) => return Ok(counted + counts.iter().sum::<usize>()),
                Err(ChunkedError::Cancelled { .. }) => return Err(ScanStoreError::Stopped),
                Err(ChunkedError::Failed {
                    rows,
                    error: ScanStoreError::Busy { .. },
                }) => {
                    counted += committed(rows);
                    done += rows;
                    backoff.pause(stop, name)?;
                }
                Err(ChunkedError::Failed { error, .. }) => return Err(error),
            }
        }
    }
}

/// A store on a private copy of the migrated template, for unit tests.
/// The directory goes when the returned guard drops, so keep it.
#[cfg(test)]
pub(crate) fn scratch_store() -> (
    droppedneedle_kernel::db::scratch::ScratchDir,
    SqliteScanStore,
) {
    let dir = droppedneedle_kernel::db::scratch::ScratchDir::new("scan-store").unwrap();
    let path = dir.path().join("app.db");
    droppedneedle_kernel::test_support::template::copy_to(&path).unwrap();
    let db = Db::open_for_tests(&path).unwrap();
    (
        dir,
        SqliteScanStore::new(
            db,
            std::sync::Arc::new(droppedneedle_kernel::clock::SystemClock),
        ),
    )
}

fn normalize_key(raw: &str) -> String {
    use unicode_normalization::UnicodeNormalization;
    raw.nfc().collect()
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// Reading rows back. A stored value no release wrote fails the read: it is
// never taken for a default that looks real.
// ---------------------------------------------------------------------------

/// The read error for a column holding a value no release wrote.
fn unknown(column: &str, value: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("{column} holds a value this release does not know: {value:?}").into(),
    )
}

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanRun> {
    let row = RunRow::read(row)?;
    let run_state = RunState::from_columns(&row.state, &row.phase)
        .ok_or_else(|| unknown("state", &format!("{} in phase {}", row.state, row.phase)))?;
    // Both follow from the state; reading the row fails on a word no release
    // wrote, and nothing else looks at them.
    let (_resume_phase, _requested_control) = (row.resume_phase, row.requested_control);
    let phase_timings: PhaseTimings = serde_json::from_str(&row.phase_timings_json)
        .map_err(|_| unknown("phase_timings_json", &row.phase_timings_json))?;
    Ok(ScanRun {
        id: row.id,
        kind: row.kind,
        trigger: row.trigger,
        state: run_state,
        requested_by_user_id: row.requested_by_user_id,
        aggregate_scope: row.aggregate_scope,
        queued_at: crate::clock::at_secs(row.queued_at),
        started_at: row.started_at.map(crate::clock::at_secs),
        updated_at: crate::clock::at_secs(row.updated_at),
        terminal_at: row.terminal_at.map(crate::clock::at_secs),
        terminal_code: row.terminal_code,
        coalesced_request_count: row.coalesced_request_count,
        row_revision: row.row_revision as u64,
        event_revision: row.event_revision as u64,
        counters: Counters {
            total_count: row.total_count,
            discovered_count: row.discovered_count,
            inspected_count: row.inspected_count,
            new_count: row.new_count,
            changed_count: row.changed_count,
            indexed_count: row.indexed_count,
            unchanged_count: row.unchanged_count,
            excluded_count: row.excluded_count,
            missing_count: row.missing_count,
            errored_count: row.errored_count,
            identification_enqueued_count: row.identification_enqueued_count,
        },
        phase_timings,
    })
}

fn map_scope(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanScope> {
    let row = ScopeRow::read(row)?;
    Ok(ScanScope {
        root_id: row.root_id,
        scope_id: row.scope_id,
        relative_path: row.relative_path,
        root_path: row.root_path,
        effective_policy: row.effective_policy,
        policy_revision: row.policy_revision,
        estimated_count: row.estimated_count,
    })
}

fn map_inventory(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanInventoryItem> {
    let row = InventoryRow::read(row)?;
    Ok(ScanInventoryItem {
        root_id: row.root_id,
        relative_path: row.relative_path,
        absolute_path: row.absolute_path,
        file_size_bytes: row.file_size_bytes as u64,
        file_mtime_ns: row.file_mtime_ns,
        stat_revision: row.stat_revision,
        effective_policy: row.effective_policy,
        comparison_result: row.comparison_result,
        policy_revision: row.policy_revision,
        local_track_id: row.local_track_id,
        scope_relative_path: row.scope_relative_path,
    })
}

fn map_failure(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanFailureRecord> {
    let row = FailureRow::read(row)?;
    Ok(ScanFailureRecord {
        root_id: row.root_id,
        relative_path: row.relative_path,
        failure_code: row.failure_code,
        recorded_at: crate::clock::at_secs(row.recorded_at),
        failure_detail: row.failure_detail,
        phase: row.phase,
    })
}

fn load_scopes(conn: &Connection, run_id: &str) -> rusqlite::Result<Vec<ScanScope>> {
    let mut stmt = conn.prepare(crate::scan::sql::LOAD_SCOPES.text())?;
    stmt.query_map(params![run_id], map_scope)?.collect()
}

fn load_run(conn: &Connection, run_id: &str) -> rusqlite::Result<Option<ScanRun>> {
    conn.query_row(crate::scan::sql::LOAD_RUN.text(), params![run_id], map_run)
        .optional()
}

fn bump_scan_stream(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(crate::scan::sql::ENSURE_SCAN_STREAM.text(), [])?;
    conn.execute(crate::scan::sql::BUMP_SCAN_STREAM.text(), [])?;
    Ok(())
}

fn read_scan_stream(conn: &Connection) -> rusqlite::Result<u64> {
    conn.query_row(crate::scan::sql::READ_SCAN_STREAM.text(), [], |row| {
        row.get::<_, i64>(0)
    })
    .optional()
    .map(|value| value.unwrap_or(0).max(0) as u64)
}

/// True when a stored scope covers a requested one: same root, same policy
/// revision, stored path is the requested path or an ancestor.
fn scope_covers(existing: &ScanScope, requested_root: &str, requested: &ScanScope) -> bool {
    existing.root_id == requested_root
        && existing.policy_revision == requested.policy_revision
        && scope_covers_path(&existing.relative_path, &requested.relative_path)
}

pub(crate) mod artists;
mod artwork;
mod catalog;
mod commit;
mod folder_digests;
mod hands_off;
mod held_scopes;
mod inventory;
mod lyrics;
mod mounts;
pub mod retag_pairs;
mod runs;
pub(crate) mod sql;

pub(crate) use commit::{
    Adopted, RetagPair, adopt_file, adoptable_track_id, automatic_album_for, awaits_retag, tried,
    waits_for_the_rest,
};
