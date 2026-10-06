//! SQLite-backed scan store: durable runs, scopes, inventory, failures,
//! and the track catalog over the 0001 baseline tables.
//!
//! [`SqliteScanStore`] implements the scan ports with the v2 disposition,
//! transition, and verdict semantics: the cover rule, union
//! normalization, idempotent control answers, the legacy-mtime band with
//! promotion, and the deferred re-offer. `runs` holds the run lifecycle,
//! `inventory` the discovery inventory, `catalog` the track catalog.
//! The catalog lives in `local_tracks` (keyed by `root_id, relative_path`),
//! so [`commit_indexed`](ScanStore::commit_indexed) also writes the
//! `local_*` rows the reads layer shows (`availability = 'indexed'`).
//!
//! Two intended simplifications versus the memory store:
//!
//! * Deferred markers stay in-process. They re-arm whenever tag-read
//!   exhaustion recurs, so a restart only loses the re-offer shortcut, and
//!   the persisted `TAG_READ_DEFERRED` failure rows still show the history.
//! * Inventory upserts on `(run_id, root_id, relative_path)`: resume
//!   generations refresh the row instead of keeping stale duplicates. The
//!   current-generation reads are identical either way.
//!
//! The store holds one rusqlite connection behind a mutex, so every trait
//! method runs serialized; failures inside `Result` methods surface as
//! [`ScanStoreError::Internal`], while void methods log and degrade safe
//! (no work claimed, empty listings, classify misses read as new).

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};
use sha2::{Digest as _, Sha256};

use super::models::{
    Counters, Disposition, EffectivePolicy, RequestedControl, ScanControl, ScanFailureRecord,
    ScanInventoryItem, ScanKind, ScanPhase, ScanRequest, ScanRequestResult, ScanRun, ScanScope,
    ScanState, ScanTrigger, ScopeDiscoveryState, Verdict, counter_names, failure_codes,
    scope_covers_path,
};
use super::revision::{exact_stat_revision, legacy_mtime_eps_seconds};
use super::store::{
    CatalogEntry, CatalogStore, ClassifyInput, CommitIndexedItem, IndexWindow, InventoryPage,
    InventoryStore, RevisionKind, RunStore, ScanStoreError, WindowOutcome,
};
use crate::db::fold_text;

/// Stale-inventory rows deleted per cleanup call (v2 page size).
const STALE_PAGE: usize = 5_000;

/// Terminal runs kept past cleanup before history pruning.
const HISTORY_KEPT: usize = 50;

/// Commit attempts before a transient lock error propagates. The sqlx pool
/// holds the only other writer (session touches), so contention is rare
/// and short; a bounded retry turns a lost batch into a slow one.
const BUSY_ATTEMPTS: u32 = 8;

/// Base backoff between commit attempts; linear, no jitter needed for a
/// single scan worker.
const BUSY_BACKOFF: Duration = Duration::from_millis(25);

/// Terminal checkpoint attempts before the run settles with frames still
/// in the WAL. A lost race with the checkpoint service sleeps and
/// retries; the service's own pass folds the rest within its cadence.
const TERMINAL_CHECKPOINT_ATTEMPTS: u32 = 6;

/// Pause between terminal checkpoint attempts.
const TERMINAL_CHECKPOINT_RETRY: Duration = Duration::from_millis(500);

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

struct Inner {
    conn: Connection,
    deferred: HashSet<(String, String)>,
    catalog_dirty: bool,
    catalog_version: u64,
    catalog_cache: HashMap<String, CachedCatalog>,
}

/// SQLite [`ScanStore`] over the application database file.
pub struct SqliteScanStore {
    inner: Mutex<Inner>,
}

impl SqliteScanStore {
    /// Open against a migrated database file through the database
    /// factory, with the canonical connection pragmas. The store creates
    /// no schema itself.
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = crate::db::open_connection(path).map_err(|error| error.to_string())?;
        Ok(Self::wrap(connection))
    }

    /// In-memory store with every migration applied, for unit tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn open_ephemeral() -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
        crate::db::apply_connection_pragmas(&connection).map_err(|error| error.to_string())?;
        crate::schema::apply_migrations_blocking(&connection).map_err(|error| error.to_string())?;
        Ok(Self::wrap(connection))
    }

    fn wrap(conn: Connection) -> Self {
        Self {
            inner: Mutex::new(Inner {
                conn,
                deferred: HashSet::new(),
                catalog_dirty: false,
                catalog_version: 0,
                catalog_cache: HashMap::new(),
            }),
        }
    }

    /// Raw SQL for tests: seeds rows no store method writes (legacy
    /// revisions, bare catalog albums). Production has no use for this.
    #[cfg(any(test, feature = "test-support"))]
    pub fn execute_batch_for_tests(&self, sql: &str) -> rusqlite::Result<()> {
        self.lock().conn.execute_batch(sql)
    }

    /// One integer from raw SQL, for test assertions on stored rows.
    #[cfg(any(test, feature = "test-support"))]
    pub fn query_i64_for_tests(&self, sql: &str) -> rusqlite::Result<i64> {
        self.lock().conn.query_row(sql, [], |row| row.get(0))
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn internal(error: rusqlite::Error) -> ScanStoreError {
    ScanStoreError::Internal {
        message: format!("scan store failed: {error}"),
    }
}

/// True for lock-contention failures worth retrying: another connection
/// holds the write lock (the sqlx pool's session touches) or a commit
/// raced it. Everything else (constraint, schema, I/O) fails fast.
fn is_transient(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::DatabaseBusy
                || failure.code == rusqlite::ErrorCode::DatabaseLocked
    )
}

/// Run a write closure, retrying transient lock errors with linear backoff.
/// Returns the last error when attempts run out or the error is persistent.
fn retry_on_busy<T>(
    what: &str,
    mut attempt: impl FnMut() -> rusqlite::Result<T>,
) -> rusqlite::Result<T> {
    let mut round = 0u32;
    loop {
        match attempt() {
            Ok(value) => {
                if round > 0 {
                    tracing::info!(
                        what,
                        attempts = round + 1,
                        "scan store write won its lock race"
                    );
                }
                return Ok(value);
            }
            Err(error) if is_transient(&error) && round + 1 < BUSY_ATTEMPTS => {
                round += 1;
                std::thread::sleep(BUSY_BACKOFF * round);
            }
            Err(error) => return Err(error),
        }
    }
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
// Enum spellings. Writer-controlled values; unknown strings fall back to a
// fixed default so a stray row degrades instead of failing the read.
// ---------------------------------------------------------------------------

fn kind_to_str(kind: ScanKind) -> &'static str {
    match kind {
        ScanKind::Incremental => "incremental",
        ScanKind::RescanFiles => "rescan_files",
        ScanKind::PolicyReconcile => "policy_reconcile",
    }
}

fn kind_from_str(raw: &str) -> ScanKind {
    match raw {
        "rescan_files" => ScanKind::RescanFiles,
        "policy_reconcile" => ScanKind::PolicyReconcile,
        _ => ScanKind::Incremental,
    }
}

fn trigger_to_str(trigger: ScanTrigger) -> &'static str {
    match trigger {
        ScanTrigger::Manual => "manual",
        ScanTrigger::Automatic => "automatic",
        ScanTrigger::Subsonic => "subsonic",
        ScanTrigger::StartupResume => "startup_resume",
        ScanTrigger::PolicyApply => "policy_apply",
    }
}

fn trigger_from_str(raw: &str) -> ScanTrigger {
    match raw {
        "automatic" => ScanTrigger::Automatic,
        "subsonic" => ScanTrigger::Subsonic,
        "startup_resume" => ScanTrigger::StartupResume,
        "policy_apply" => ScanTrigger::PolicyApply,
        _ => ScanTrigger::Manual,
    }
}

fn state_to_str(state: ScanState) -> &'static str {
    match state {
        ScanState::Queued => "queued",
        ScanState::Discovering => "discovering",
        ScanState::Indexing => "indexing",
        ScanState::Reconciling => "reconciling",
        ScanState::Pausing => "pausing",
        ScanState::Paused => "paused",
        ScanState::Stopping => "stopping",
        ScanState::Completed => "completed",
        ScanState::Cancelled => "cancelled",
        ScanState::SupersededPolicyChanged => "superseded_policy_changed",
        ScanState::Failed => "failed",
    }
}

fn state_from_str(raw: &str) -> ScanState {
    match raw {
        "queued" => ScanState::Queued,
        "discovering" => ScanState::Discovering,
        "indexing" => ScanState::Indexing,
        "reconciling" => ScanState::Reconciling,
        "pausing" => ScanState::Pausing,
        "paused" => ScanState::Paused,
        "stopping" => ScanState::Stopping,
        "completed" => ScanState::Completed,
        "cancelled" => ScanState::Cancelled,
        "superseded_policy_changed" => ScanState::SupersededPolicyChanged,
        _ => ScanState::Failed,
    }
}

fn phase_to_str(phase: ScanPhase) -> &'static str {
    match phase {
        ScanPhase::Queued => "queued",
        ScanPhase::Discovering => "discovering",
        ScanPhase::Indexing => "indexing",
        ScanPhase::Reconciling => "reconciling",
    }
}

fn phase_from_str(raw: &str) -> ScanPhase {
    match raw {
        "discovering" => ScanPhase::Discovering,
        "indexing" => ScanPhase::Indexing,
        "reconciling" => ScanPhase::Reconciling,
        _ => ScanPhase::Queued,
    }
}

fn control_from_str(raw: &str) -> RequestedControl {
    match raw {
        "pause" => RequestedControl::Pause,
        "stop" => RequestedControl::Stop,
        _ => RequestedControl::None,
    }
}

fn policy_to_str(policy: EffectivePolicy) -> &'static str {
    match policy {
        EffectivePolicy::LocalMetadata => "local_metadata",
        EffectivePolicy::Automatic => "automatic",
        EffectivePolicy::Excluded => "excluded",
    }
}

fn policy_from_str(raw: &str) -> EffectivePolicy {
    match raw {
        "local_metadata" => EffectivePolicy::LocalMetadata,
        "excluded" => EffectivePolicy::Excluded,
        _ => EffectivePolicy::Automatic,
    }
}

fn verdict_to_str(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::New => "new",
        Verdict::Changed => "changed",
        Verdict::Unchanged => "unchanged",
        Verdict::Excluded => "excluded",
        Verdict::CandidateMissing => "candidate_missing",
    }
}

fn verdict_from_str(raw: &str) -> Verdict {
    match raw {
        "changed" => Verdict::Changed,
        "unchanged" => Verdict::Unchanged,
        "excluded" => Verdict::Excluded,
        "candidate_missing" => Verdict::CandidateMissing,
        _ => Verdict::New,
    }
}

fn discovery_to_str(state: ScopeDiscoveryState) -> &'static str {
    match state {
        ScopeDiscoveryState::Pending => "pending",
        ScopeDiscoveryState::Completed => "completed",
        ScopeDiscoveryState::PartiallyRead => "partially_read",
        ScopeDiscoveryState::Unavailable => "unavailable",
    }
}

fn discovery_from_str(raw: &str) -> ScopeDiscoveryState {
    match raw {
        "completed" => ScopeDiscoveryState::Completed,
        "partially_read" => ScopeDiscoveryState::PartiallyRead,
        "unavailable" => ScopeDiscoveryState::Unavailable,
        _ => ScopeDiscoveryState::Pending,
    }
}

/// Counter name to its runs-table column. Unknown names have no column.
fn counter_column(name: &str) -> Option<&'static str> {
    match name {
        s if s == counter_names::TOTAL => Some("total_count"),
        s if s == counter_names::DISCOVERED => Some("discovered_count"),
        s if s == counter_names::INSPECTED => Some("inspected_count"),
        s if s == counter_names::NEW => Some("new_count"),
        s if s == counter_names::CHANGED => Some("changed_count"),
        s if s == counter_names::INDEXED => Some("indexed_count"),
        s if s == counter_names::UNCHANGED => Some("unchanged_count"),
        s if s == counter_names::EXCLUDED => Some("excluded_count"),
        s if s == counter_names::MISSING => Some("missing_count"),
        s if s == counter_names::ERRORED => Some("errored_count"),
        s if s == counter_names::IDENTIFICATION_ENQUEUED => Some("identification_enqueued_count"),
        _ => None,
    }
}

const RUN_COLUMNS: &str = "id, kind, trigger, requested_by_user_id, state, phase, \
    resume_phase, requested_control, aggregate_scope, total_count, discovered_count, \
    inspected_count, new_count, changed_count, indexed_count, unchanged_count, \
    excluded_count, missing_count, errored_count, identification_enqueued_count, \
    coalesced_request_count, queued_at, started_at, updated_at, terminal_at, \
    terminal_code, phase_timings_json, row_revision, event_revision";

fn map_run(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanRun> {
    let kind: String = row.get("kind")?;
    let trigger: String = row.get("trigger")?;
    let state: String = row.get("state")?;
    let phase: String = row.get("phase")?;
    let resume_phase: Option<String> = row.get("resume_phase")?;
    let requested_control: String = row.get("requested_control")?;
    let timings: String = row.get("phase_timings_json")?;
    let mut counters = Counters::new();
    for (name, column) in [
        (counter_names::TOTAL, "total_count"),
        (counter_names::DISCOVERED, "discovered_count"),
        (counter_names::INSPECTED, "inspected_count"),
        (counter_names::NEW, "new_count"),
        (counter_names::CHANGED, "changed_count"),
        (counter_names::INDEXED, "indexed_count"),
        (counter_names::UNCHANGED, "unchanged_count"),
        (counter_names::EXCLUDED, "excluded_count"),
        (counter_names::MISSING, "missing_count"),
        (counter_names::ERRORED, "errored_count"),
        (
            counter_names::IDENTIFICATION_ENQUEUED,
            "identification_enqueued_count",
        ),
    ] {
        counters.insert(name.to_owned(), row.get(column)?);
    }
    Ok(ScanRun {
        id: row.get("id")?,
        kind: kind_from_str(&kind),
        trigger: trigger_from_str(&trigger),
        state: state_from_str(&state),
        phase: phase_from_str(&phase),
        requested_by_user_id: row.get("requested_by_user_id")?,
        aggregate_scope: row.get("aggregate_scope")?,
        queued_at: row.get("queued_at")?,
        started_at: row.get("started_at")?,
        updated_at: row.get("updated_at")?,
        terminal_at: row.get("terminal_at")?,
        resume_phase: resume_phase.as_deref().map(phase_from_str),
        requested_control: control_from_str(&requested_control),
        terminal_code: row.get("terminal_code")?,
        coalesced_request_count: row.get("coalesced_request_count")?,
        row_revision: row.get::<_, i64>("row_revision")? as u64,
        event_revision: row.get::<_, i64>("event_revision")? as u64,
        counters,
        phase_timings: serde_json::from_str(&timings).unwrap_or_default(),
    })
}

fn map_scope(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanScope> {
    let policy: String = row.get("effective_policy")?;
    Ok(ScanScope {
        root_id: row.get("root_id")?,
        scope_id: row.get("scope_id")?,
        relative_path: row.get("relative_path")?,
        root_path: row.get("root_path")?,
        effective_policy: policy_from_str(&policy),
        policy_revision: row.get("policy_revision")?,
        estimated_count: row.get("estimated_count")?,
    })
}

fn map_inventory(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanInventoryItem> {
    let policy: String = row.get("effective_policy")?;
    let verdict: String = row.get("comparison_result")?;
    Ok(ScanInventoryItem {
        root_id: row.get("root_id")?,
        relative_path: row.get("relative_path")?,
        absolute_path: row.get("absolute_path")?,
        file_size_bytes: row.get::<_, i64>("file_size_bytes")? as u64,
        file_mtime_ns: row.get("file_mtime_ns")?,
        stat_revision: row.get("stat_revision")?,
        effective_policy: policy_from_str(&policy),
        comparison_result: verdict_from_str(&verdict),
        policy_revision: row.get("policy_revision")?,
        local_track_id: row.get("local_track_id")?,
        scope_relative_path: row.get("scope_relative_path")?,
    })
}

fn map_failure(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanFailureRecord> {
    let phase: String = row.get("phase")?;
    Ok(ScanFailureRecord {
        root_id: row.get("root_id")?,
        relative_path: row.get("relative_path")?,
        failure_code: row.get("failure_code")?,
        recorded_at: row.get("recorded_at")?,
        failure_detail: row.get("failure_detail")?,
        phase: phase_from_str(&phase),
    })
}

fn load_scopes(conn: &Connection, run_id: &str) -> rusqlite::Result<Vec<ScanScope>> {
    let mut stmt = conn.prepare(
        "SELECT root_id, scope_id, relative_path, root_path, effective_policy, \
         policy_revision, estimated_count FROM library_scan_run_scopes \
         WHERE run_id = ?1 ORDER BY scope_sequence",
    )?;
    stmt.query_map(params![run_id], map_scope)?.collect()
}

fn load_run(conn: &Connection, run_id: &str) -> rusqlite::Result<Option<ScanRun>> {
    conn.query_row(
        &format!("SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE id = ?1"),
        params![run_id],
        map_run,
    )
    .optional()
}

fn bump_scan_stream(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO library_event_stream_revisions (stream_kind, value) \
         VALUES ('scan', 0) ON CONFLICT (stream_kind) DO NOTHING",
        [],
    )?;
    conn.execute(
        "UPDATE library_event_stream_revisions SET value = value + 1 \
         WHERE stream_kind = 'scan'",
        [],
    )?;
    Ok(())
}

fn read_scan_stream(conn: &Connection) -> u64 {
    conn.query_row(
        "SELECT value FROM library_event_stream_revisions WHERE stream_kind = 'scan'",
        [],
        |row| row.get::<_, i64>(0),
    )
    .map(|value| value.max(0) as u64)
    .unwrap_or(0)
}

/// True when a stored scope covers a requested one: same root, same policy
/// revision, stored path is the requested path or an ancestor (memory port).
fn scope_covers(existing: &ScanScope, requested_root: &str, requested: &ScanScope) -> bool {
    existing.root_id == requested_root
        && existing.policy_revision == requested.policy_revision
        && scope_covers_path(&existing.relative_path, &requested.relative_path)
}

mod artwork;
pub use artwork::ArtworkSweep;
mod catalog;
mod commit;
mod inventory;
mod runs;
