//! SQLite-backed scan store: durable runs, scopes, inventory, failures,
//! and the track catalog over the 0001 baseline tables.
//!
//! [`SqliteScanStore`] implements [`ScanStore`] with the exact disposition,
//! transition, and verdict semantics of [`MemoryScanStore`](super::store::MemoryScanStore):
//! same cover rule, same union normalization, same idempotent control
//! answers, same legacy-mtime band with promotion, same deferred re-offer.
//! The catalog lives in `local_tracks` (keyed by `root_id, relative_path`),
//! so [`commit_indexed`](ScanStore::commit_indexed) also writes the
//! `local_*` rows the reads layer shows (`availability = 'indexed'`).
//!
//! Two deliberate simplifications versus the memory store:
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
    CatalogEntry, ClassifyInput, CommitIndexedItem, InventoryPage, RevisionKind, ScanStore,
    ScanStoreError,
};
use crate::db::fold_text;

/// Artist every scan-committed track credits until identify reconciles it.
const UNKNOWN_ARTIST_ID: &str = "scan-unknown-artist";

/// Title for tracks sitting directly under a root (no parent directory).
const ROOT_ALBUM_TITLE: &str = "Unknown Album";

/// Bulk catalog load backing in-memory classify: same row shape as
/// [`ScanStore::catalog_entries`], keyed for verdict lookups.
fn load_catalog_map(
    conn: &Connection,
    root_id: &str,
) -> rusqlite::Result<HashMap<String, CatalogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
         file_mtime_ns, tags_read_at FROM local_tracks \
         WHERE root_id = ?1 AND availability = 'indexed'",
    )?;
    let rows = stmt.query_map(params![root_id], |row| {
        let kind: String = row.get("stat_revision_kind")?;
        Ok((
            row.get::<_, String>("relative_path")?,
            CatalogEntry {
                track_id: row.get("id")?,
                revision: row.get("stat_revision")?,
                size_bytes: row.get::<_, i64>("file_size_bytes")? as u64,
                mtime_ns: row.get("file_mtime_ns")?,
                revision_kind: if kind == "exact" {
                    RevisionKind::Exact
                } else {
                    RevisionKind::LegacyFloat
                },
                tags_read_at: row.get("tags_read_at")?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (key, entry) = row?;
        map.insert(key, entry);
    }
    Ok(map)
}

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
/// the snapshot to the catalog mutation counter; terminal invalidation
/// drops the snapshot so idle scans hold no catalog memory.
struct CachedCatalog {
    version: u64,
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
    /// Open against a migrated database file. Production calls this after
    /// migrations; the store creates no schema itself.
    pub fn open(path: &Path) -> Result<Self, String> {
        let connection = Connection::open(path).map_err(|error| error.to_string())?;
        apply_pragmas(&connection).map_err(|error| error.to_string())?;
        Ok(Self::wrap(connection))
    }

    /// Ephemeral store for test bundles: an in-memory database with the
    /// baseline schema applied. Production never uses this.
    pub fn open_ephemeral() -> Result<Self, String> {
        let connection = Connection::open_in_memory().map_err(|error| error.to_string())?;
        apply_pragmas(&connection).map_err(|error| error.to_string())?;
        connection
            .execute_batch(include_str!("../../../migrations/0001_baseline.sql"))
            .map_err(|error| error.to_string())?;
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
    /// revisions). Production has no use for this.
    #[cfg(test)]
    pub fn execute_batch_for_tests(&self, sql: &str) -> rusqlite::Result<()> {
        self.lock().conn.execute_batch(sql)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn apply_pragmas(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::from_millis(5000))?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=NORMAL;
         PRAGMA foreign_keys=ON;
         PRAGMA mmap_size=16777216;
         PRAGMA temp_store=MEMORY;
         PRAGMA cache_size=-16384;
         /* Bounded inline autocheckpoint: 4000 frames (~16 MB at 4 KiB
            pages, 4x under the 64 MB backpressure high water). At 1000
            frames every batch paid a checkpoint fsync (13 ms mean,
            179 ms p99 on disk); 4000 keeps ~7/8 of that cost off the
            scan wall while the log wraps instead of growing to
            gigabytes (autocheckpoint=0 peaked at 2.8 GB mid-scan).
            The terminal TRUNCATE still reclaims the rest. Durability is
            unaffected: frames are durable in the WAL before checkpoint. */
         PRAGMA wal_autocheckpoint=4000;",
    )
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

/// Escape a scope path for a prefix `LIKE`: backslash, percent, and
/// underscore match literally so the scope rule never widens.
fn escape_like_prefix(scope: &str) -> String {
    let mut escaped = String::with_capacity(scope.len() + 1);
    for ch in scope.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped.push('/');
    escaped
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

impl ScanStore for SqliteScanStore {
    fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> ScanRequestResult {
        let mut guard = self.lock();
        let outcome = (|| -> rusqlite::Result<ScanRequestResult> {
            let tx = guard.conn.transaction()?;
            let result = request_run_inner(&tx, request, run_id, requested_at)?;
            tx.commit()?;
            Ok(result)
        })();
        match outcome {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "scan request_run failed");
                ScanRequestResult {
                    run_id: run_id.to_owned(),
                    disposition: Disposition::Conflict,
                    state: ScanState::Queued,
                    row_revision: 0,
                    queued_reason: Some("The scan store is unavailable.".to_owned()),
                    conflicting_kind: None,
                }
            }
        }
    }

    fn get_run(&self, run_id: &str) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        let guard = self.lock();
        let run = load_run(&guard.conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
        let scopes = load_scopes(&guard.conn, run_id).map_err(internal)?;
        Ok((run.clone(), scopes, run.counters.clone()))
    }

    fn list_current(&self) -> Vec<ScanRun> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM library_scan_runs \
             WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
             ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, queued_at, id"
        )) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan list_current failed");
                return Vec::new();
            }
        };
        stmt.query_map([], map_run)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan list_current failed");
                Vec::new()
            })
    }

    fn list_history(&self, limit: usize) -> Vec<ScanRun> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(&format!(
            "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
             ORDER BY terminal_at DESC, id DESC LIMIT ?1"
        )) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan list_history failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![limit.max(1) as i64], map_run)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan list_history failed");
                Vec::new()
            })
    }

    fn latest_filesystem_terminal(&self) -> Option<ScanRun> {
        let guard = self.lock();
        let mut stmt = guard
            .conn
            .prepare(&format!(
                "SELECT {RUN_COLUMNS} FROM library_scan_runs WHERE terminal_at IS NOT NULL \
             ORDER BY terminal_at DESC, id DESC"
            ))
            .ok()?;
        let rows: Vec<ScanRun> = stmt
            .query_map([], map_run)
            .ok()?
            .collect::<rusqlite::Result<Vec<_>>>()
            .ok()?;
        rows.into_iter()
            .find(|run| run.kind != ScanKind::PolicyReconcile || run.aggregate_scope == "all")
    }

    fn claim_next(&self, now: f64) -> Option<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Option<ScanRun>> {
            let tx = guard.conn.transaction()?;
            let run = claim_next_inner(&tx, now)?;
            tx.commit()?;
            Ok(run)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan claim_next failed");
            None
        })
    }

    fn resumable(&self) -> Option<ScanRun> {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                &format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state IN ('discovering','indexing','reconciling') \
                     AND requested_control = 'none' \
                     ORDER BY started_at, id LIMIT 1"
                ),
                [],
                map_run,
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan resumable failed");
                None
            })
    }

    fn transition(
        &self,
        run_id: &str,
        expected_state: ScanState,
        expected_revision: u64,
        new_state: ScanState,
        now: f64,
        terminal_code: Option<&str>,
    ) -> Result<ScanRun, ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard.conn.transaction().map_err(internal)?;
        let run = transition_inner(
            &tx,
            run_id,
            expected_state,
            expected_revision,
            new_state,
            now,
            terminal_code,
        )?;
        tx.commit().map_err(internal)?;
        Ok(run)
    }

    fn request_control(
        &self,
        run_id: &str,
        control: ScanControl,
        resume: bool,
        expected_revision: u64,
        now: f64,
    ) -> Result<(ScanRun, u64), ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard.conn.transaction().map_err(internal)?;
        let outcome = request_control_inner(&tx, run_id, control, resume, expected_revision, now)?;
        tx.commit().map_err(internal)?;
        Ok(outcome)
    }

    fn record_failures(&self, run_id: &str, failures: Vec<ScanFailureRecord>) {
        if failures.is_empty() {
            return;
        }
        let mut guard = self.lock();
        let outcome = (|| -> rusqlite::Result<()> {
            let tx = guard.conn.transaction()?;
            for failure in &failures {
                tx.execute(
                    "INSERT OR IGNORE INTO library_scan_failures \
                     (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                     recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        run_id,
                        failure.root_id,
                        failure.relative_path,
                        failure.failure_code,
                        failure.failure_detail,
                        phase_to_str(failure.phase),
                        failure.recorded_at,
                    ],
                )?;
            }
            tx.commit()
        })();
        match outcome {
            Ok(()) => guard.catalog_dirty = true,
            Err(error) => tracing::error!(%error, "scan record_failures failed"),
        }
    }

    fn failures(&self, run_id: &str) -> Vec<ScanFailureRecord> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT root_id, relative_path, failure_code, recorded_at, failure_detail, phase \
             FROM library_scan_failures WHERE run_id = ?1 ORDER BY rowid",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan failures failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![run_id], map_failure)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan failures failed");
                Vec::new()
            })
    }

    fn scope_discovery_state(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> ScopeDiscoveryState {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT discovery_state FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan scope_discovery_state failed");
                None
            })
            .map(|state| discovery_from_str(&state))
            .unwrap_or(ScopeDiscoveryState::Pending)
    }

    fn scope_discovery_generation(&self, run_id: &str, root_id: &str, relative_path: &str) -> u64 {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT discovery_generation FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![run_id, root_id, relative_path],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan scope_discovery_generation failed");
                None
            })
            .map(|generation| generation.max(1) as u64)
            .unwrap_or(1)
    }

    fn complete_scope_discovery(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
        state: ScopeDiscoveryState,
        error_code: Option<&str>,
    ) {
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = ?1, error_code = ?2 \
             WHERE run_id = ?3 AND root_id = ?4 AND relative_path = ?5",
            params![
                discovery_to_str(state),
                error_code,
                run_id,
                root_id,
                relative_path
            ],
        ) {
            tracing::error!(%error, "scan complete_scope_discovery failed");
        }
    }

    fn restart_scope_discovery(&self, run_id: &str, root_id: &str, relative_path: &str) {
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            "UPDATE library_scan_run_scopes SET discovery_state = 'pending', error_code = NULL, \
             discovery_generation = discovery_generation + 1 \
             WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
            params![run_id, root_id, relative_path],
        ) {
            tracing::error!(%error, "scan restart_scope_discovery failed");
        }
    }

    fn prepare_discovery_resume(&self, run_id: &str) {
        let mut guard = self.lock();
        let outcome = (|| -> rusqlite::Result<()> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "UPDATE library_scan_run_scopes SET discovery_state = 'pending', \
                 error_code = NULL, discovery_generation = discovery_generation + 1 \
                 WHERE run_id = ?1 AND discovery_state != 'completed'",
                params![run_id],
            )?;
            tx.execute(
                "UPDATE library_scan_runs SET discovered_count = ( \
                 SELECT COUNT(*) FROM library_scan_inventory i \
                 JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
                 ), row_revision = row_revision + 1 WHERE id = ?1",
                params![run_id],
            )?;
            tx.commit()
        })();
        if let Err(error) = outcome {
            tracing::error!(%error, "scan prepare_discovery_resume failed");
        }
    }

    fn finalize_discovery(&self, run_id: &str, updated_at: f64) -> Result<ScanRun, ScanStoreError> {
        let mut guard = self.lock();
        let tx = guard.conn.transaction().map_err(internal)?;
        if load_run(&tx, run_id).map_err(internal)?.is_none() {
            return Err(ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            });
        }
        tx.execute(
            "UPDATE library_scan_runs SET total_count = ( \
             SELECT COUNT(*) FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ), discovered_count = ( \
             SELECT COUNT(*) FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ), updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?1",
            params![run_id, updated_at],
        )
        .map_err(internal)?;
        bump_scan_stream(&tx).map_err(internal)?;
        let run =
            load_run(&tx, run_id)
                .map_err(internal)?
                .ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
        tx.commit().map_err(internal)?;
        Ok(run)
    }

    fn cleanup_stale_inventory(&self, run_id: &str) -> usize {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<usize> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "DELETE FROM library_scan_inventory WHERE rowid IN ( \
                 SELECT i.rowid FROM library_scan_inventory i \
                 LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 \
                 AND (s.scope_sequence IS NULL \
                 OR s.discovery_generation != i.discovery_generation) \
                 LIMIT ?2)",
                params![run_id, STALE_PAGE as i64],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM library_scan_inventory i \
                 LEFT JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
                 AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
                 WHERE i.run_id = ?1 \
                 AND (s.scope_sequence IS NULL \
                 OR s.discovery_generation != i.discovery_generation)",
                params![run_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            Ok(pending.max(0) as usize)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan cleanup_stale_inventory failed");
            0
        })
    }

    fn cleanup_terminal_inventory(&self, limit: usize) {
        let guard = self.lock();
        if let Err(error) = (|| -> rusqlite::Result<()> {
            let target: Option<String> = guard
                .conn
                .query_row(
                    "SELECT id FROM library_scan_runs \
                     WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 1 \
                     ORDER BY terminal_at, id LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(target) = target else {
                guard.conn.execute(
                    "DELETE FROM library_scan_runs WHERE id IN ( \
                     SELECT id FROM library_scan_runs \
                     WHERE terminal_at IS NOT NULL AND inventory_cleanup_pending = 0 \
                     ORDER BY terminal_at DESC, id DESC LIMIT -1 OFFSET ?1)",
                    params![HISTORY_KEPT as i64],
                )?;
                return Ok(());
            };
            let page = limit.max(1) as i64;
            guard.conn.execute(
                "DELETE FROM library_scan_failures WHERE rowid IN ( \
                 SELECT rowid FROM library_scan_failures \
                 WHERE run_id = ?1 AND failure_code != ?2 LIMIT ?3)",
                params![target, failure_codes::TAG_READ_DEFERRED, page],
            )?;
            if guard.conn.changes() > 0 {
                return Ok(());
            }
            guard.conn.execute(
                "DELETE FROM library_scan_inventory WHERE rowid IN ( \
                 SELECT rowid FROM library_scan_inventory WHERE run_id = ?1 LIMIT ?2)",
                params![target, page],
            )?;
            let remaining: i64 = guard.conn.query_row(
                "SELECT COUNT(*) FROM library_scan_inventory WHERE run_id = ?1",
                params![target],
                |row| row.get(0),
            )?;
            if remaining == 0 {
                guard.conn.execute(
                    "UPDATE library_scan_runs SET inventory_cleanup_pending = 0 WHERE id = ?1",
                    params![target],
                )?;
            }
            Ok(())
        })() {
            tracing::error!(%error, "scan cleanup_terminal_inventory failed");
        }
    }

    fn add_inventory_batch(
        &self,
        run_id: &str,
        items: Vec<ScanInventoryItem>,
        expected_run_revision: u64,
        updated_at: f64,
        generation: u64,
    ) -> Result<u64, ScanStoreError> {
        let mut guard = self.lock();
        // Revision gate first: this connection is the only scan-table
        // writer and the mutex serializes it, so the revision cannot move
        // between this check and the write below.
        let current = load_run(&guard.conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
        if current.row_revision != expected_run_revision {
            return Err(ScanStoreError::StaleRevision {
                message: "The scan run changed before inventory was recorded.".to_owned(),
            });
        }
        // One transaction with reused prepared statements; a lock race
        // against the sqlx pool retries instead of losing the batch.
        retry_on_busy("add_inventory_batch", || {
            let tx = guard.conn.transaction()?;
            let mut insert = tx.prepare(
                "INSERT INTO library_scan_inventory (run_id, root_id, relative_path, \
                 scope_relative_path, discovery_generation, absolute_path, file_size_bytes, \
                 file_mtime_ns, stat_revision, policy_revision, effective_policy, \
                 comparison_result, local_track_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
                 ON CONFLICT (run_id, root_id, relative_path) DO UPDATE SET \
                 scope_relative_path = excluded.scope_relative_path, \
                 discovery_generation = excluded.discovery_generation, \
                 absolute_path = excluded.absolute_path, \
                 file_size_bytes = excluded.file_size_bytes, \
                 file_mtime_ns = excluded.file_mtime_ns, \
                 stat_revision = excluded.stat_revision, \
                 policy_revision = excluded.policy_revision, \
                 effective_policy = excluded.effective_policy, \
                 comparison_result = excluded.comparison_result, \
                 local_track_id = excluded.local_track_id",
            )?;
            let mut twin = tx.prepare(
                "INSERT OR IGNORE INTO library_scan_failures \
                 (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                 recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', ?6)",
            )?;
            let mut seen: HashSet<&str> = HashSet::new();
            let mut landed = 0i64;
            for item in &items {
                if !seen.insert(item.relative_path.as_str()) {
                    twin.execute(params![
                        run_id,
                        item.root_id,
                        item.relative_path,
                        failure_codes::NFC_TWIN_COLLISION,
                        "Two on-disk names normalize to the same inventory key; the first file won and this twin was skipped.",
                        updated_at,
                    ])?;
                    continue;
                }
                insert.execute(params![
                    run_id,
                    item.root_id,
                    item.relative_path,
                    item.scope_relative_path,
                    generation as i64,
                    item.absolute_path,
                    item.file_size_bytes as i64,
                    item.file_mtime_ns,
                    item.stat_revision,
                    item.policy_revision,
                    policy_to_str(item.effective_policy),
                    verdict_to_str(item.comparison_result),
                    item.local_track_id,
                ])?;
                landed += 1;
            }
            drop(insert);
            drop(twin);
            tx.execute(
                "UPDATE library_scan_runs SET discovered_count = discovered_count + ?1, \
                 updated_at = ?2, row_revision = row_revision + 1 WHERE id = ?3",
                params![landed, updated_at, run_id],
            )?;
            let revision: i64 = tx.query_row(
                "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
                params![run_id],
                |row| row.get(0),
            )?;
            tx.commit()?;
            Ok(revision as u64)
        })
        .map_err(internal)
    }

    fn inventory_for_run(&self, run_id: &str) -> Vec<ScanInventoryItem> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
             i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
             i.policy_revision, i.local_track_id, i.scope_relative_path \
             FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             ORDER BY i.rowid",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan inventory_for_run failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![run_id], map_inventory)
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan inventory_for_run failed");
                Vec::new()
            })
    }

    fn inventory_page(
        &self,
        run_id: &str,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<InventoryPage, ScanStoreError> {
        // Keyset over the (run_id, root_id, relative_path) primary key:
        // every page seeks and reads only its rows. The generation join
        // probes the tiny scopes table per row; no sort, no offset rescan.
        // Read failures surface as Err: an empty page means end-of-run,
        // and the caller retries, then fails honestly, instead of
        // completing the run short.
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT i.root_id, i.relative_path, i.absolute_path, i.file_size_bytes, \
             i.file_mtime_ns, i.stat_revision, i.effective_policy, i.comparison_result, \
             i.policy_revision, i.local_track_id, i.scope_relative_path \
             FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?1 AND s.discovery_generation = i.discovery_generation \
             AND (i.root_id, i.relative_path) > (?2, ?3) \
             ORDER BY i.root_id, i.relative_path LIMIT ?4",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan inventory_page failed");
                return Err(internal(error));
            }
        };
        let (after_root, after_path) = after.unwrap_or(("", ""));
        let items = stmt
            .query_map(
                params![run_id, after_root, after_path, limit.max(1) as i64],
                map_inventory,
            )
            .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
            .map_err(|error| {
                tracing::error!(%error, "scan inventory_page failed");
                internal(error)
            })?;
        let cursor = items
            .last()
            .map(|item| (item.root_id.clone(), item.relative_path.clone()));
        Ok((items, cursor))
    }

    fn add_counter(&self, run_id: &str, name: &str, delta: i64) {
        let Some(column) = counter_column(name) else {
            return;
        };
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {column} = {column} + ?1 WHERE id = ?2"),
            params![delta, run_id],
        ) {
            tracing::error!(%error, "scan add_counter failed");
        }
    }

    fn add_counters(&self, run_id: &str, deltas: &[(&str, i64)]) {
        // Collapse to one UPDATE: duplicate names sum, unknown names drop
        // exactly like repeated add_counter calls.
        let mut summed: HashMap<&'static str, i64> = HashMap::new();
        for (name, delta) in deltas {
            if let Some(column) = counter_column(name) {
                *summed.entry(column).or_insert(0) += *delta;
            }
        }
        if summed.is_empty() {
            return;
        }
        let mut pairs: Vec<(&'static str, i64)> = summed.into_iter().collect();
        pairs.sort_by_key(|pair| pair.0);
        let assignments = pairs
            .iter()
            .map(|(column, _)| format!("{column} = {column} + ?"))
            .collect::<Vec<_>>()
            .join(", ");
        let mut values: Vec<rusqlite::types::Value> = Vec::with_capacity(pairs.len() + 1);
        for (_, delta) in &pairs {
            values.push(rusqlite::types::Value::Integer(*delta));
        }
        values.push(rusqlite::types::Value::Text(run_id.to_owned()));
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {assignments} WHERE id = ?"),
            params_from_iter(values),
        ) {
            tracing::error!(%error, "scan add_counters failed");
        }
    }

    fn set_counter(&self, run_id: &str, name: &str, value: i64) {
        let Some(column) = counter_column(name) else {
            return;
        };
        let guard = self.lock();
        if let Err(error) = guard.conn.execute(
            &format!("UPDATE library_scan_runs SET {column} = ?1 WHERE id = ?2"),
            params![value, run_id],
        ) {
            tracing::error!(%error, "scan set_counter failed");
        }
    }

    fn commit_indexed(
        &self,
        root_id: &str,
        relative_path: &str,
        size_bytes: u64,
        mtime_ns: i64,
        track_id: String,
        tags_read_at: f64,
    ) {
        self.commit_indexed_batch(&[CommitIndexedItem {
            root_id: root_id.to_owned(),
            relative_path: relative_path.to_owned(),
            size_bytes,
            mtime_ns,
            track_id,
            tags_read_at,
        }]);
    }

    fn commit_indexed_batch(&self, items: &[CommitIndexedItem]) {
        if items.is_empty() {
            return;
        }
        let mut guard = self.lock();
        match retry_on_busy("commit_indexed_batch", || {
            commit_indexed_batch_inner(&mut guard.conn, items)
        }) {
            Ok(()) => {
                // Cleared only on success: a failed batch keeps the
                // re-offer shortcut instead of losing it.
                for item in items {
                    guard
                        .deferred
                        .remove(&(item.root_id.clone(), item.relative_path.clone()));
                }
                guard.catalog_dirty = true;
                guard.catalog_version += 1;
            }
            Err(error) => tracing::error!(%error, "scan commit_indexed_batch failed"),
        }
    }

    fn mark_deferred(&self, root_id: &str, relative_path: &str, deferred: bool) {
        let mut guard = self.lock();
        let key = (root_id.to_owned(), relative_path.to_owned());
        if deferred {
            guard.deferred.insert(key);
        } else {
            guard.deferred.remove(&key);
        }
    }

    fn catalog_entries(&self, root_id: &str) -> Vec<(String, CatalogEntry)> {
        let guard = self.lock();
        let mut stmt = match guard.conn.prepare(
            "SELECT relative_path, id, stat_revision, stat_revision_kind, file_size_bytes, \
             file_mtime_ns, tags_read_at FROM local_tracks \
             WHERE root_id = ?1 AND availability = 'indexed' ORDER BY relative_path",
        ) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan catalog_entries failed");
                return Vec::new();
            }
        };
        stmt.query_map(params![root_id], |row| {
            let kind: String = row.get("stat_revision_kind")?;
            Ok((
                row.get::<_, String>("relative_path")?,
                CatalogEntry {
                    track_id: row.get("id")?,
                    revision: row.get("stat_revision")?,
                    size_bytes: row.get::<_, i64>("file_size_bytes")? as u64,
                    mtime_ns: row.get("file_mtime_ns")?,
                    revision_kind: if kind == "exact" {
                        RevisionKind::Exact
                    } else {
                        RevisionKind::LegacyFloat
                    },
                    tags_read_at: row.get("tags_read_at")?,
                },
            ))
        })
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan catalog_entries failed");
            Vec::new()
        })
    }

    fn remove_catalog(&self, root_id: &str, relative_path: &str) {
        let mut guard = self.lock();
        let outcome = retry_on_busy("remove_catalog", || {
            let tx = guard.conn.transaction()?;
            let found: Option<(String, Option<String>)> = tx
                .query_row(
                    "SELECT id, local_album_id FROM local_tracks \
                     WHERE root_id = ?1 AND relative_path = ?2",
                    params![root_id, relative_path],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            if let Some((track_id, album_id)) = found {
                tx.execute(
                    "UPDATE library_scan_inventory SET local_track_id = NULL \
                     WHERE local_track_id = ?1",
                    params![track_id],
                )?;
                tx.execute(
                    "DELETE FROM local_track_artists WHERE local_track_id = ?1",
                    params![track_id],
                )?;
                tx.execute("DELETE FROM local_tracks WHERE id = ?1", params![track_id])?;
                tx.commit()?;
                if let Some(album_id) = album_id
                    && let Err(error) = cleanup_emptied_album(&mut guard.conn, &album_id)
                {
                    tracing::debug!(%error, "scan kept an emptied album row");
                }
            }
            Ok(())
        });
        match outcome {
            Ok(()) => {
                // Cleared only on success, matching commit_indexed_batch:
                // a failed delete keeps the re-offer shortcut.
                guard
                    .deferred
                    .remove(&(root_id.to_owned(), relative_path.to_owned()));
                guard.catalog_dirty = true;
                guard.catalog_version += 1;
                if let Some(cached) = guard.catalog_cache.get_mut(root_id) {
                    cached.entries.remove(relative_path);
                }
            }
            Err(error) => tracing::error!(%error, "scan remove_catalog failed"),
        }
    }

    fn missing_catalog_paths(
        &self,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Vec<String> {
        // The exact scope_covers_path rule in SQL: "." covers everything,
        // else the path equals the scope or extends it past a slash. LIKE
        // metacharacters in the scope escape so a literal % never widens.
        let guard = self.lock();
        let sql = "SELECT t.relative_path FROM local_tracks t \
             WHERE t.root_id = ?1 AND t.availability = 'indexed' \
             AND (?2 = '.' OR t.relative_path = ?2 OR t.relative_path LIKE ?3 ESCAPE '\\') \
             AND NOT EXISTS ( \
             SELECT 1 FROM library_scan_inventory i \
             JOIN library_scan_run_scopes s ON s.run_id = i.run_id \
             AND s.root_id = i.root_id AND s.relative_path = i.scope_relative_path \
             WHERE i.run_id = ?4 AND i.root_id = ?1 AND i.relative_path = t.relative_path \
             AND s.discovery_generation = i.discovery_generation) \
             ORDER BY t.relative_path";
        let mut stmt = match guard.conn.prepare(sql) {
            Ok(stmt) => stmt,
            Err(error) => {
                tracing::error!(%error, "scan missing_catalog_paths failed");
                return Vec::new();
            }
        };
        let prefix = format!("{}%", escape_like_prefix(scope_relative_path));
        stmt.query_map(
            params![root_id, scope_relative_path, prefix, run_id],
            |row| row.get::<_, String>(0),
        )
        .and_then(|rows| rows.collect::<rusqlite::Result<Vec<_>>>())
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan missing_catalog_paths failed");
            Vec::new()
        })
    }

    fn classify(
        &self,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> HashMap<String, (Verdict, Option<String>)> {
        if paths.is_empty() {
            return HashMap::new();
        }
        let mut guard = self.lock();
        // One bulk load per catalog version: discovery classifies hundreds
        // of batches against an unchanging catalog, and every commit or
        // removal bumps the version, so the snapshot is never stale.
        let version = guard.catalog_version;
        let fresh = guard
            .catalog_cache
            .get(root_id)
            .is_some_and(|cached| cached.version == version);
        if !fresh {
            match load_catalog_map(&guard.conn, root_id) {
                Ok(entries) => {
                    guard
                        .catalog_cache
                        .insert(root_id.to_owned(), CachedCatalog { version, entries });
                }
                Err(error) => {
                    tracing::error!(%error, "scan classify failed");
                    return HashMap::new();
                }
            }
        }
        let mut verdicts = HashMap::with_capacity(paths.len());
        let mut promotions: Vec<(String, ClassifyInput)> = Vec::new();
        let mut skew: Vec<String> = Vec::new();
        {
            let Some(cached) = guard.catalog_cache.get(root_id) else {
                tracing::error!("scan classify lost its snapshot");
                return HashMap::new();
            };
            for input in paths {
                let key = normalize_key(&input.0);
                let Some(entry) = cached.entries.get(&key) else {
                    verdicts.insert(key, (Verdict::New, None));
                    continue;
                };
                let same_size = entry.size_bytes == input.1;
                let unchanged = match entry.revision_kind {
                    RevisionKind::Exact => entry.revision == input.4,
                    RevisionKind::LegacyFloat => {
                        let current_mtime = input.3;
                        let saved_mtime = entry.mtime_ns as f64 / 1_000_000_000.0;
                        let band = legacy_mtime_eps_seconds(current_mtime);
                        if let Some(tags_read_at) = entry.tags_read_at
                            && (current_mtime - tags_read_at).abs() > band
                        {
                            skew.push(key.clone());
                        }
                        same_size && (current_mtime - saved_mtime).abs() <= band
                    }
                };
                if unchanged && guard.deferred.contains(&(root_id.to_owned(), key.clone())) {
                    verdicts.insert(key, (Verdict::Changed, Some(entry.track_id.clone())));
                    continue;
                }
                if unchanged && entry.revision_kind != RevisionKind::Exact {
                    promotions.push((key.clone(), input.clone()));
                }
                verdicts.insert(
                    key,
                    (
                        if unchanged {
                            Verdict::Unchanged
                        } else {
                            Verdict::Changed
                        },
                        Some(entry.track_id.clone()),
                    ),
                );
            }
        }
        // Promotions mutate the row the verdict just read: write through to
        // the snapshot so later batches in this scan see exact revisions.
        for (key, input) in &promotions {
            let outcome = retry_on_busy("classify promotion", || {
                guard.conn.execute(
                    "UPDATE local_tracks SET stat_revision = ?1, file_size_bytes = ?2, \
                     file_mtime_ns = ?3, stat_revision_kind = 'exact' \
                     WHERE root_id = ?4 AND relative_path = ?5",
                    params![input.4, input.1 as i64, input.2, root_id, key],
                )
            });
            if let Err(error) = outcome {
                tracing::error!(%error, "scan classify promotion failed");
                continue;
            }
            if let Some(entry) = guard
                .catalog_cache
                .get_mut(root_id)
                .and_then(|cached| cached.entries.get_mut(key))
            {
                entry.revision.clone_from(&input.4);
                entry.size_bytes = input.1;
                entry.mtime_ns = input.2;
                entry.revision_kind = RevisionKind::Exact;
            }
        }
        if let Some(run_id) = run_id {
            for relative_path in &skew {
                let outcome = retry_on_busy("classify skew evidence", || {
                    guard.conn.execute(
                        "INSERT OR IGNORE INTO library_scan_failures \
                         (run_id, root_id, relative_path, failure_code, failure_detail, phase, \
                         recorded_at) VALUES (?1, ?2, ?3, ?4, ?5, 'discovering', 0.0)",
                        params![
                            run_id,
                            root_id,
                            relative_path,
                            failure_codes::MTIME_SKEW,
                            "Legacy mtime drift beyond tolerance; the file clock and the read clock disagree.",
                        ],
                    )
                });
                if let Err(error) = outcome {
                    tracing::error!(%error, "scan classify skew evidence failed");
                }
            }
        }
        verdicts
    }

    fn stream_revision(&self, kind: &str) -> u64 {
        let guard = self.lock();
        guard
            .conn
            .query_row(
                "SELECT value FROM library_event_stream_revisions WHERE stream_kind = ?1",
                params![kind],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .unwrap_or_else(|error| {
                tracing::error!(%error, "scan stream_revision failed");
                None
            })
            .map(|value| value.max(0) as u64)
            .unwrap_or(0)
    }

    fn checkpoint_terminal(&self) {
        // The bounded inline autocheckpoint wraps the log mid-scan; fold
        // what remains back now, off the batch path and off the observed
        // scan wall. TRUNCATE also resets the log so the file never
        // accumulates folded frames across scans (RESTART will not reset
        // while the pool holds the WAL open). A lost race with the
        // checkpoint service (or an in-flight pool read) sleeps and
        // retries; whatever remains after attempts run out stays in the
        // WAL for the service, and the next scan's terminal fold retries.
        let guard = self.lock();
        for attempt in 0..TERMINAL_CHECKPOINT_ATTEMPTS {
            let checkpointed = guard
                .conn
                .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                    Ok((
                        row.get::<_, i32>(0)?,
                        row.get::<_, i32>(1)?,
                        row.get::<_, i32>(2)?,
                    ))
                });
            match checkpointed {
                Ok((_, 0, folded)) => {
                    tracing::debug!(
                        folded,
                        attempts = attempt + 1,
                        "scan terminal checkpoint drained its log"
                    );
                    break;
                }
                Ok((busy, log, folded)) => {
                    tracing::debug!(
                        busy,
                        log,
                        folded,
                        attempt = attempt + 1,
                        "scan terminal checkpoint retrying"
                    );
                    std::thread::sleep(TERMINAL_CHECKPOINT_RETRY);
                }
                Err(error) => {
                    tracing::debug!(%error, "scan terminal checkpoint failed");
                    break;
                }
            }
        }
    }

    fn flush_invalidation(&self, terminal: bool) {
        let mut guard = self.lock();
        if terminal {
            // The classify snapshot is a within-scan accelerator: drop it
            // at every terminal flush so idle scans hold no catalog memory.
            guard.catalog_cache.clear();
        }
        if !terminal || !guard.catalog_dirty {
            return;
        }
        let outcome = (|| -> rusqlite::Result<()> {
            guard.conn.execute(
                "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0) \
                 ON CONFLICT (singleton) DO NOTHING",
                [],
            )?;
            guard.conn.execute(
                "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1",
                [],
            )?;
            Ok(())
        })();
        match outcome {
            Ok(()) => guard.catalog_dirty = false,
            Err(error) => tracing::error!(%error, "scan flush_invalidation failed"),
        }
    }

    fn recover(&self, now: f64) -> Vec<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Vec<ScanRun>> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', terminal_at = ?1, \
                 updated_at = ?1, requested_control = 'none', \
                 row_revision = row_revision + 1, event_revision = event_revision + 1 \
                 WHERE state = 'stopping' OR requested_control = 'stop'",
                params![now],
            )?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'paused', requested_control = 'none', \
                 updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 \
                 WHERE state = 'pausing' OR requested_control = 'pause'",
                params![now],
            )?;
            let runs: Vec<ScanRun> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state IN ('queued','discovering','indexing','reconciling','paused') \
                     ORDER BY queued_at, id"
                ))?;
                stmt.query_map([], map_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            if !runs.is_empty() {
                bump_scan_stream(&tx)?;
            }
            tx.commit()?;
            Ok(runs)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan recover failed");
            Vec::new()
        })
    }

    fn recover_stopping(&self, now: f64) -> Vec<ScanRun> {
        let mut guard = self.lock();
        (|| -> rusqlite::Result<Vec<ScanRun>> {
            let tx = guard.conn.transaction()?;
            tx.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', terminal_at = ?1, \
                 updated_at = ?1, requested_control = 'none', \
                 row_revision = row_revision + 1, event_revision = event_revision + 1 \
                 WHERE state = 'stopping' OR requested_control = 'stop'",
                params![now],
            )?;
            let runs: Vec<ScanRun> = {
                let mut stmt = tx.prepare(&format!(
                    "SELECT {RUN_COLUMNS} FROM library_scan_runs \
                     WHERE state = 'cancelled' AND terminal_at = ?1 ORDER BY id"
                ))?;
                stmt.query_map(params![now], map_run)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            };
            tx.commit()?;
            Ok(runs)
        })()
        .unwrap_or_else(|error| {
            tracing::error!(%error, "scan recover_stopping failed");
            Vec::new()
        })
    }
}

/// Catalog-commit path: upsert the track row plus its album, artist, and
/// join rows so the reads layer shows the file as indexed. Metadata comes
/// from the path alone (the coordinator passes no tags); identify owns
/// real grouping and reconciliation later. One transaction with reused
/// prepared statements for the whole batch: same rows as one transaction
/// per file, one commit instead of hundreds.
fn commit_indexed_batch_inner(
    conn: &mut Connection,
    items: &[CommitIndexedItem],
) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    let mut artist = tx.prepare(
        "INSERT INTO local_artists (id, display_name, folded_name, normalized_name, kind, \
         created_at, updated_at) VALUES (?1, 'Unknown Artist', ?2, '', 'unknown', ?3, ?3) \
         ON CONFLICT (id) DO NOTHING",
    )?;
    let mut album = tx.prepare(
        "INSERT INTO local_albums (id, root_id, grouping_key, title, title_folded, \
         album_artist_id, grouping_source, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'automatic', ?7, ?7) \
         ON CONFLICT (id) DO NOTHING",
    )?;
    let mut track = tx.prepare(
        "INSERT INTO local_tracks (id, local_album_id, root_id, file_path, relative_path, \
         path_hash, file_size_bytes, file_mtime_ns, stat_revision, stat_revision_kind, \
         tags_read_at, title, title_folded, album_title, album_title_folded, disc_number, \
         track_number, file_format, availability, ingest_source, imported_at, \
         membership_source, title_provenance, album_title_provenance, album_artist_provenance) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'exact', ?10, ?11, ?12, ?13, ?14, 1, 0, \
         ?15, 'indexed', 'scan', ?10, 'automatic', 'parsed', ?16, 'absent') \
         ON CONFLICT (root_id, relative_path) DO UPDATE SET \
         local_album_id = excluded.local_album_id, file_path = excluded.file_path, \
         path_hash = excluded.path_hash, file_size_bytes = excluded.file_size_bytes, \
         file_mtime_ns = excluded.file_mtime_ns, stat_revision = excluded.stat_revision, \
         stat_revision_kind = excluded.stat_revision_kind, \
         tags_read_at = excluded.tags_read_at, title = excluded.title, \
         title_folded = excluded.title_folded, album_title = excluded.album_title, \
         album_title_folded = excluded.album_title_folded, \
         file_format = excluded.file_format, availability = 'indexed', missing_since = NULL, \
         excluded_at = NULL",
    )?;
    let mut track_artist = tx.prepare(
        "INSERT INTO local_track_artists (local_track_id, position, local_artist_id, role) \
         VALUES (?1, 0, ?2, 'main') ON CONFLICT (local_track_id, position) DO NOTHING",
    )?;
    let mut album_artist = tx.prepare(
        "INSERT INTO local_album_artists (local_album_id, position, local_artist_id, role) \
         VALUES (?1, 0, ?2, 'main') ON CONFLICT (local_album_id, position) DO NOTHING",
    )?;
    let unknown_folded = fold_text("Unknown Artist");
    for item in items {
        let file_name = item
            .relative_path
            .rsplit('/')
            .next()
            .unwrap_or(item.relative_path.as_str());
        let (stem, extension) = match file_name.rsplit_once('.') {
            Some((stem, extension)) if !stem.is_empty() && !extension.is_empty() => {
                (stem, extension)
            }
            _ => (file_name, ""),
        };
        let title = if stem.is_empty() {
            "Unknown Track"
        } else {
            stem
        };
        let file_format = if extension.is_empty() {
            "unknown".to_owned()
        } else {
            extension.to_ascii_lowercase()
        };
        let parent = item
            .relative_path
            .rsplit_once('/')
            .map(|(parent, _)| parent)
            .unwrap_or(".");
        let parent = if parent.is_empty() { "." } else { parent };
        let album_title = if parent == "." {
            ROOT_ALBUM_TITLE
        } else {
            parent.rsplit('/').next().unwrap_or(parent)
        };
        let album_provenance = if parent == "." {
            "placeholder"
        } else {
            "parsed"
        };
        let album_id = format!(
            "scan-album-{}",
            &sha256_hex(&format!(
                "scan-album-v1\0{root_id}\0{parent}",
                root_id = item.root_id
            ))[..32]
        );
        let grouping_key = format!("scan:{root_id}:{parent}", root_id = item.root_id);
        let path_hash = sha256_hex(&format!(
            "scan-track-v1\0{root_id}\0{relative_path}",
            root_id = item.root_id,
            relative_path = item.relative_path
        ));
        artist.execute(params![
            UNKNOWN_ARTIST_ID,
            unknown_folded,
            item.tags_read_at
        ])?;
        album.execute(params![
            album_id,
            item.root_id,
            grouping_key,
            album_title,
            fold_text(album_title),
            UNKNOWN_ARTIST_ID,
            item.tags_read_at,
        ])?;
        track.execute(params![
            item.track_id,
            album_id,
            item.root_id,
            item.relative_path,
            item.relative_path,
            path_hash,
            item.size_bytes as i64,
            item.mtime_ns,
            exact_stat_revision(item.size_bytes, item.mtime_ns),
            item.tags_read_at,
            title,
            fold_text(title),
            album_title,
            fold_text(album_title),
            file_format,
            album_provenance,
        ])?;
        // The join uses the passed track id directly: the upsert keeps a
        // conflicting row's id, and that id is always the passed one.
        // Changed files carry their classify-provided id (no catalog
        // writer runs between classify and commit on the single worker),
        // new files insert it (the walk dedupes keys, so no twin can
        // claim the row first), and re-reads follow the same two cases.
        track_artist.execute(params![item.track_id, UNKNOWN_ARTIST_ID])?;
        album_artist.execute(params![album_id, UNKNOWN_ARTIST_ID])?;
    }
    drop(artist);
    drop(album);
    drop(track);
    drop(track_artist);
    drop(album_artist);
    tx.commit()
}

/// Delete an album left with no tracks, with its scan-owned joins. Other
/// slices' identity rows block the delete (all-or-nothing) and the album
/// stays; the caller logs that at debug.
fn cleanup_emptied_album(conn: &mut Connection, album_id: &str) -> rusqlite::Result<()> {
    let remaining: i64 = conn.query_row(
        "SELECT COUNT(*) FROM local_tracks WHERE local_album_id = ?1",
        params![album_id],
        |row| row.get(0),
    )?;
    if remaining > 0 {
        return Ok(());
    }
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM local_album_artists WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute(
        "DELETE FROM local_album_artwork WHERE local_album_id = ?1",
        params![album_id],
    )?;
    tx.execute("DELETE FROM local_albums WHERE id = ?1", params![album_id])?;
    tx.commit()
}

fn transition_inner(
    conn: &Connection,
    run_id: &str,
    expected_state: ScanState,
    expected_revision: u64,
    new_state: ScanState,
    now: f64,
    terminal_code: Option<&str>,
) -> Result<ScanRun, ScanStoreError> {
    let current =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    if current.state != expected_state || current.row_revision != expected_revision {
        return Err(ScanStoreError::StaleRevision {
            message: "The scan run changed before the transition was applied.".to_owned(),
        });
    }
    let phase = match new_state {
        ScanState::Discovering => Some(ScanPhase::Discovering),
        ScanState::Indexing => Some(ScanPhase::Indexing),
        ScanState::Reconciling => Some(ScanPhase::Reconciling),
        _ => None,
    };
    if new_state.is_terminal() {
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, terminal_at = ?2, terminal_code = ?3, \
             updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?4",
            params![state_to_str(new_state), now, terminal_code, run_id],
        )
        .map_err(internal)?;
    } else {
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?3",
            params![state_to_str(new_state), now, run_id],
        )
        .map_err(internal)?;
    }
    if let Some(phase) = phase {
        conn.execute(
            "UPDATE library_scan_runs SET phase = ?1 WHERE id = ?2",
            params![phase_to_str(phase), run_id],
        )
        .map_err(internal)?;
    }
    bump_scan_stream(conn).map_err(internal)?;
    load_run(conn, run_id)
        .map_err(internal)?
        .ok_or_else(|| ScanStoreError::NotFound {
            run_id: run_id.to_owned(),
        })
}

fn request_control_inner(
    conn: &Connection,
    run_id: &str,
    control: ScanControl,
    resume: bool,
    expected_revision: u64,
    now: f64,
) -> Result<(ScanRun, u64), ScanStoreError> {
    let snapshot =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    let run_state = snapshot.state;
    let current_stream = read_scan_stream(conn);
    if !resume
        && control == ScanControl::Pause
        && matches!(run_state, ScanState::Pausing | ScanState::Paused)
    {
        return Ok((snapshot, current_stream));
    }
    if !resume
        && control == ScanControl::Stop
        && matches!(run_state, ScanState::Stopping | ScanState::Cancelled)
    {
        return Ok((snapshot, current_stream));
    }
    if resume
        && matches!(
            run_state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        )
        && snapshot.requested_control == RequestedControl::None
    {
        return Ok((snapshot, current_stream));
    }
    if snapshot.row_revision != expected_revision {
        return Err(ScanStoreError::StaleRevision {
            message: "The scan run changed before the control was applied.".to_owned(),
        });
    }
    if resume {
        let Some(resume_phase) = snapshot.resume_phase else {
            return Err(ScanStoreError::InvalidControl {
                message: "Only a paused scan can be resumed.".to_owned(),
            });
        };
        if run_state != ScanState::Paused {
            return Err(ScanStoreError::InvalidControl {
                message: "Only a paused scan can be resumed.".to_owned(),
            });
        }
        let state = match resume_phase {
            ScanPhase::Discovering => ScanState::Discovering,
            ScanPhase::Indexing => ScanState::Indexing,
            ScanPhase::Reconciling => ScanState::Reconciling,
            ScanPhase::Queued => ScanState::Queued,
        };
        conn.execute(
            "UPDATE library_scan_runs SET state = ?1, requested_control = 'none', \
             resume_phase = NULL, updated_at = ?2, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?3",
            params![state_to_str(state), now, run_id],
        )
        .map_err(internal)?;
    } else if control == ScanControl::Pause {
        if !matches!(
            run_state,
            ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
        ) {
            return Err(ScanStoreError::InvalidControl {
                message: "This scan cannot be paused in its current state.".to_owned(),
            });
        }
        conn.execute(
            "UPDATE library_scan_runs SET state = 'pausing', requested_control = 'pause', \
             resume_phase = phase, updated_at = ?1, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?2",
            params![now, run_id],
        )
        .map_err(internal)?;
    } else if run_state == ScanState::Paused {
        conn.execute(
            "UPDATE library_scan_runs SET state = 'cancelled', requested_control = 'none', \
             terminal_at = ?1, updated_at = ?1, row_revision = row_revision + 1, \
             event_revision = event_revision + 1 WHERE id = ?2",
            params![now, run_id],
        )
        .map_err(internal)?;
    } else if matches!(
        run_state,
        ScanState::Queued
            | ScanState::Discovering
            | ScanState::Indexing
            | ScanState::Reconciling
            | ScanState::Pausing
    ) {
        if run_state == ScanState::Queued {
            conn.execute(
                "UPDATE library_scan_runs SET state = 'cancelled', requested_control = 'stop', \
                 terminal_at = ?1, updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 WHERE id = ?2",
                params![now, run_id],
            )
            .map_err(internal)?;
        } else {
            conn.execute(
                "UPDATE library_scan_runs SET state = 'stopping', requested_control = 'stop', \
                 updated_at = ?1, row_revision = row_revision + 1, \
                 event_revision = event_revision + 1 WHERE id = ?2",
                params![now, run_id],
            )
            .map_err(internal)?;
        }
    } else {
        return Err(ScanStoreError::InvalidControl {
            message: "This scan cannot be stopped in its current state.".to_owned(),
        });
    }
    bump_scan_stream(conn).map_err(internal)?;
    let stream_revision = read_scan_stream(conn);
    let run =
        load_run(conn, run_id)
            .map_err(internal)?
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
    Ok((run, stream_revision))
}

fn request_run_inner(
    conn: &Connection,
    request: &ScanRequest,
    run_id: &str,
    requested_at: f64,
) -> rusqlite::Result<ScanRequestResult> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {RUN_COLUMNS} FROM library_scan_runs \
         WHERE state NOT IN ('completed','cancelled','superseded_policy_changed','failed') \
         ORDER BY CASE WHEN state = 'queued' THEN 1 ELSE 0 END, rowid"
    ))?;
    let current: Vec<ScanRun> = stmt
        .query_map([], map_run)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let has_active = current.iter().any(|run| run.state != ScanState::Queued);
    let queued_id = current
        .iter()
        .find(|run| run.state == ScanState::Queued)
        .map(|run| run.id.clone());

    let mut scopes_by_run: HashMap<String, Vec<ScanScope>> = HashMap::new();
    for run in &current {
        scopes_by_run.insert(run.id.clone(), load_scopes(conn, &run.id)?);
    }
    // F-SCAN-02: only a queued run may cover a request.
    let covering = current.iter().find(|run| {
        if run.state != ScanState::Queued || run.kind != request.kind {
            return false;
        }
        let Some(scopes) = scopes_by_run.get(&run.id) else {
            return false;
        };
        if scopes.is_empty() {
            return false;
        }
        request.scopes.iter().all(|requested| {
            scopes
                .iter()
                .any(|existing| scope_covers(existing, &requested.root_id, requested))
        })
    });
    if let Some(covering) = covering {
        conn.execute(
            "UPDATE library_scan_runs SET coalesced_request_count = coalesced_request_count + 1, \
             updated_at = ?1, row_revision = row_revision + 1, event_revision = event_revision + 1 \
             WHERE id = ?2",
            params![requested_at, covering.id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
            params![covering.id],
            |row| row.get(0),
        )?;
        return Ok(ScanRequestResult {
            run_id: covering.id.clone(),
            disposition: Disposition::Coalesced,
            state: ScanState::Queued,
            row_revision: revision as u64,
            queued_reason: None,
            conflicting_kind: None,
        });
    }

    if let Some(queued_id) = queued_id {
        let Some(queued) = current.iter().find(|run| run.id == queued_id) else {
            return fresh_run(conn, request, run_id, requested_at, has_active);
        };
        let queued_scopes = scopes_by_run.get(&queued_id).cloned().unwrap_or_default();
        let incompatible = queued.kind != request.kind
            || queued_scopes
                .iter()
                .any(|scope| scope.policy_revision != request.policy_revision);
        if incompatible {
            return Ok(ScanRequestResult {
                run_id: queued_id,
                disposition: Disposition::Conflict,
                state: ScanState::Queued,
                row_revision: queued.row_revision,
                queued_reason: Some(
                    "The follow-up slot already contains incompatible work.".to_owned(),
                ),
                conflicting_kind: Some(queued.kind),
            });
        }
        // F-INDEXREC-01: normalize the union; per root keep the broadest
        // ancestor and drop its descendants.
        let additions: Vec<ScanScope> = request
            .scopes
            .iter()
            .filter(|requested| {
                !queued_scopes.iter().any(|existing| {
                    existing.root_id == requested.root_id
                        && scope_covers_path(&existing.relative_path, &requested.relative_path)
                })
            })
            .cloned()
            .collect();
        let mut sequence: i64 = conn.query_row(
            "SELECT COALESCE(MAX(scope_sequence), -1) FROM library_scan_run_scopes WHERE run_id = ?1",
            params![queued_id],
            |row| row.get(0),
        )?;
        for existing in queued_scopes.iter().filter(|existing| {
            additions.iter().any(|addition| {
                existing.root_id == addition.root_id
                    && scope_covers_path(&addition.relative_path, &existing.relative_path)
                    && existing.relative_path != addition.relative_path
            })
        }) {
            conn.execute(
                "DELETE FROM library_scan_run_scopes \
                 WHERE run_id = ?1 AND root_id = ?2 AND relative_path = ?3",
                params![queued_id, existing.root_id, existing.relative_path],
            )?;
        }
        for scope in &additions {
            sequence += 1;
            conn.execute(
                "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, scope_id, \
                 relative_path, root_path, effective_policy, policy_revision, estimated_count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    queued_id,
                    sequence,
                    scope.root_id,
                    scope.scope_id,
                    scope.relative_path,
                    scope.root_path,
                    policy_to_str(scope.effective_policy),
                    scope.policy_revision,
                    scope.estimated_count,
                ],
            )?;
        }
        // Quirk port: aggregate_scope derives from the request alone.
        let aggregate = if request
            .scopes
            .iter()
            .any(|scope| scope.relative_path == ".")
        {
            "all"
        } else {
            "selected"
        };
        conn.execute(
            "UPDATE library_scan_runs SET aggregate_scope = ?1, updated_at = ?2, \
             row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?3",
            params![aggregate, requested_at, queued_id],
        )?;
        bump_scan_stream(conn)?;
        let revision: i64 = conn.query_row(
            "SELECT row_revision FROM library_scan_runs WHERE id = ?1",
            params![queued_id],
            |row| row.get(0),
        )?;
        return Ok(ScanRequestResult {
            run_id: queued_id,
            disposition: Disposition::Expanded,
            state: ScanState::Queued,
            row_revision: revision as u64,
            queued_reason: None,
            conflicting_kind: None,
        });
    }

    fresh_run(conn, request, run_id, requested_at, has_active)
}

fn fresh_run(
    conn: &Connection,
    request: &ScanRequest,
    run_id: &str,
    requested_at: f64,
    has_active: bool,
) -> rusqlite::Result<ScanRequestResult> {
    let aggregate = if request
        .scopes
        .iter()
        .any(|scope| scope.relative_path == ".")
    {
        "all"
    } else {
        "selected"
    };
    conn.execute(
        "INSERT INTO library_scan_runs (id, kind, trigger, requested_by_user_id, state, phase, \
         aggregate_scope, queued_at, updated_at, inventory_cleanup_pending, \
         row_revision, event_revision) \
         VALUES (?1, ?2, ?3, ?4, 'queued', 'queued', ?5, ?6, ?6, 1, 1, 0)",
        params![
            run_id,
            kind_to_str(request.kind),
            trigger_to_str(request.trigger),
            request.requested_by_user_id,
            aggregate,
            requested_at,
        ],
    )?;
    for (sequence, scope) in request.scopes.iter().enumerate() {
        conn.execute(
            "INSERT INTO library_scan_run_scopes (run_id, scope_sequence, root_id, scope_id, \
             relative_path, root_path, effective_policy, policy_revision, estimated_count) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                run_id,
                sequence as i64,
                scope.root_id,
                scope.scope_id,
                scope.relative_path,
                scope.root_path,
                policy_to_str(scope.effective_policy),
                scope.policy_revision,
                scope.estimated_count,
            ],
        )?;
    }
    bump_scan_stream(conn)?;
    Ok(ScanRequestResult {
        run_id: run_id.to_owned(),
        disposition: if has_active {
            Disposition::Queued
        } else {
            Disposition::Started
        },
        state: ScanState::Queued,
        row_revision: 1,
        queued_reason: if has_active {
            Some("Another scan is active.".to_owned())
        } else {
            None
        },
        conflicting_kind: None,
    })
}

fn claim_next_inner(conn: &Connection, now: f64) -> rusqlite::Result<Option<ScanRun>> {
    let blocked: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_scan_runs \
         WHERE state IN ('discovering','indexing','reconciling','pausing','paused','stopping')",
        [],
        |row| row.get(0),
    )?;
    if blocked > 0 {
        return Ok(None);
    }
    let candidate: Option<String> = conn
        .query_row(
            "SELECT id FROM library_scan_runs WHERE state = 'queued' ORDER BY rowid LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let Some(candidate) = candidate else {
        return Ok(None);
    };
    conn.execute(
        "UPDATE library_scan_runs SET state = 'discovering', phase = 'discovering', \
         started_at = COALESCE(started_at, ?1), updated_at = ?1, \
         row_revision = row_revision + 1, event_revision = event_revision + 1 WHERE id = ?2",
        params![now, candidate],
    )?;
    bump_scan_stream(conn)?;
    load_run(conn, &candidate)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::super::coordinator::{LibraryScanCoordinator, StaticResolver};
    use super::super::models::EffectivePolicy;
    use super::super::pool::BlockingPool;
    use super::super::roots::{LibraryRoot, RootRegistry};
    use super::super::seams::{NullIdentifyQueue, NullTagReader};
    use super::super::watcher::WorkWakeups;
    use super::*;
    use crate::reads::library::sqlite::{LibraryDb, SqliteCatalog};
    use crate::reads::library::stores::{AlbumFilter, AlbumSort, LibraryCatalog, TrackFilter};

    static SEQ: AtomicU64 = AtomicU64::new(0);

    fn scratch_dir(tag: &str) -> PathBuf {
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-scan-sqlite-{tag}-{seq}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    async fn migrated_db(tag: &str) -> (crate::db::DbRuntime, PathBuf) {
        let dir = scratch_dir(tag);
        let db_path = dir.join("app.db");
        let runtime = crate::db::open_runtime(&crate::db::DbConfig::new(&db_path))
            .await
            .expect("scratch runtime");
        (runtime, db_path)
    }

    fn scope(root: &str) -> ScanScope {
        ScanScope {
            root_id: root.to_owned(),
            scope_id: Some(root.to_owned()),
            relative_path: ".".to_owned(),
            root_path: Some(format!("/{root}")),
            effective_policy: EffectivePolicy::Automatic,
            policy_revision: "rev-1".to_owned(),
            estimated_count: None,
        }
    }

    fn request(kind: ScanKind) -> ScanRequest {
        ScanRequest {
            kind,
            trigger: super::super::models::ScanTrigger::Manual,
            scopes: vec![scope("r1")],
            requested_by_user_id: None,
            policy_revision: "rev-1".to_owned(),
        }
    }

    fn inventory_item(rel: &str) -> ScanInventoryItem {
        ScanInventoryItem {
            root_id: "r1".to_owned(),
            relative_path: rel.to_owned(),
            absolute_path: format!("/m/{rel}"),
            file_size_bytes: 10,
            file_mtime_ns: 5,
            stat_revision: "10:5".to_owned(),
            effective_policy: EffectivePolicy::Automatic,
            comparison_result: Verdict::New,
            policy_revision: "rev-1".to_owned(),
            local_track_id: None,
            scope_relative_path: ".".to_owned(),
        }
    }

    /// A restart keeps the run, its inventory, its failures, and the
    /// catalog row; untouched files still classify unchanged.
    #[tokio::test]
    async fn restart_keeps_run_inventory_failures_and_catalog() {
        let (_runtime, db_path) = migrated_db("restart").await;
        let run_id = {
            let store = SqliteScanStore::open(&db_path).expect("store opens");
            let created = store.request_run(&request(ScanKind::Incremental), "run-1", 1.0);
            assert_eq!(created.disposition, Disposition::Started);
            let claimed = store.claim_next(2.0).expect("claimed");
            assert_eq!(claimed.state, ScanState::Discovering);
            let revision = store
                .add_inventory_batch("run-1", vec![inventory_item("a.flac")], 2, 3.0, 1)
                .expect("batch lands");
            assert_eq!(revision, 3);
            store.record_failures(
                "run-1",
                vec![ScanFailureRecord {
                    root_id: "r1".to_owned(),
                    relative_path: "b.flac".to_owned(),
                    failure_code: failure_codes::TAG_READ_FAILED.to_owned(),
                    recorded_at: 4.0,
                    failure_detail: "boom".to_owned(),
                    phase: ScanPhase::Indexing,
                }],
            );
            store.commit_indexed("r1", "a.flac", 10, 5, "t-1".to_owned(), 99.0);
            store.flush_invalidation(true);
            created.run_id
        };
        // Restart: reopen against the same file.
        let store = SqliteScanStore::open(&db_path).expect("store reopens");
        let (run, scopes, _) = store.get_run(&run_id).expect("run survives");
        assert_eq!(run.state, ScanState::Discovering);
        assert_eq!(scopes.len(), 1);
        let inventory = store.inventory_for_run(&run_id);
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].relative_path, "a.flac");
        let failures = store.failures(&run_id);
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].failure_code, failure_codes::TAG_READ_FAILED);
        let entries = store.catalog_entries("r1");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].1.track_id, "t-1");
        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned())],
            None,
        );
        assert_eq!(
            verdicts.get("a.flac"),
            Some(&(Verdict::Unchanged, Some("t-1".to_owned())))
        );
        assert!(store.stream_revision("scan") > 0);
    }

    /// Rescan verdicts match the memory store: new for unknown files,
    /// changed for touched ones, and legacy rows promote to exact on
    /// an unchanged verdict with skew evidence recorded.
    #[tokio::test]
    async fn rescan_verdicts_match_memory_semantics() {
        let store = SqliteScanStore::open_ephemeral().expect("ephemeral opens");
        let created = store.request_run(&request(ScanKind::Incremental), "run-9", 1.0);
        assert_eq!(created.disposition, Disposition::Started);
        store.commit_indexed(
            "r1",
            "same.flac",
            10,
            100_000_000_000,
            "t-1".to_owned(),
            100.0,
        );
        store.commit_indexed("r1", "touched.flac", 10, 5, "t-2".to_owned(), 99.0);
        // One legacy row with a drifted read clock.
        store
            .execute_batch_for_tests(
                "UPDATE local_tracks SET stat_revision_kind = 'legacy_float', tags_read_at = 90.0 \
             WHERE root_id = 'r1' AND relative_path = 'same.flac'",
            )
            .expect("legacy seed");
        let verdicts = store.classify(
            "r1",
            &[
                (
                    "same.flac".to_owned(),
                    10,
                    100_000_000_000,
                    100.0,
                    "10:100000000000".to_owned(),
                ),
                ("touched.flac".to_owned(), 12, 5, 5e-9, "12:5".to_owned()),
                ("unknown.flac".to_owned(), 1, 1, 1e-9, "1:1".to_owned()),
            ],
            Some("run-9"),
        );
        assert_eq!(
            verdicts.get("same.flac"),
            Some(&(Verdict::Unchanged, Some("t-1".to_owned())))
        );
        assert_eq!(
            verdicts.get("touched.flac"),
            Some(&(Verdict::Changed, Some("t-2".to_owned())))
        );
        assert_eq!(verdicts.get("unknown.flac"), Some(&(Verdict::New, None)));
        // The legacy row promoted to exact and left skew evidence.
        let entries = store.catalog_entries("r1");
        let promoted = entries
            .iter()
            .find(|(rel, _)| rel == "same.flac")
            .expect("promoted row");
        assert_eq!(promoted.1.revision_kind, RevisionKind::Exact);
        let failures = store.failures("run-9");
        assert!(
            failures
                .iter()
                .any(|failure| failure.failure_code == failure_codes::MTIME_SKEW),
            "skew evidence recorded"
        );
    }

    /// A commit batch lands every row and survives a restart with its
    /// exact revisions; untouched files still classify unchanged.
    #[tokio::test]
    async fn commit_batch_persists_and_survives_restart() {
        use super::super::store::CommitIndexedItem;

        let (_runtime, db_path) = migrated_db("batch").await;
        {
            let store = SqliteScanStore::open(&db_path).expect("store opens");
            store.commit_indexed_batch(&[
                CommitIndexedItem {
                    root_id: "r1".to_owned(),
                    relative_path: "a.flac".to_owned(),
                    size_bytes: 10,
                    mtime_ns: 5,
                    track_id: "t-1".to_owned(),
                    tags_read_at: 99.0,
                },
                CommitIndexedItem {
                    root_id: "r1".to_owned(),
                    relative_path: "sub/b.flac".to_owned(),
                    size_bytes: 20,
                    mtime_ns: 6,
                    track_id: "t-2".to_owned(),
                    tags_read_at: 99.0,
                },
            ]);
            assert_eq!(store.catalog_entries("r1").len(), 2);
        }
        let store = SqliteScanStore::open(&db_path).expect("store reopens");
        let entries = store.catalog_entries("r1");
        assert_eq!(entries.len(), 2);
        let verdicts = store.classify(
            "r1",
            &[
                ("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned()),
                ("sub/b.flac".to_owned(), 20, 6, 6e-9, "20:6".to_owned()),
            ],
            None,
        );
        assert_eq!(
            verdicts.get("a.flac"),
            Some(&(Verdict::Unchanged, Some("t-1".to_owned())))
        );
        assert_eq!(
            verdicts.get("sub/b.flac"),
            Some(&(Verdict::Unchanged, Some("t-2".to_owned())))
        );
    }

    /// Classify serves a snapshot per catalog version: repeated calls
    /// agree, and a commit between calls reloads instead of answering
    /// stale.
    #[tokio::test]
    async fn classify_cache_reloads_after_commit() {
        let store = SqliteScanStore::open_ephemeral().expect("ephemeral opens");
        store.commit_indexed("r1", "a.flac", 10, 5, "t-1".to_owned(), 99.0);
        let input = ("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned());
        let first = store.classify("r1", std::slice::from_ref(&input), None);
        let second = store.classify("r1", std::slice::from_ref(&input), None);
        assert_eq!(first, second);
        assert_eq!(
            first.get("a.flac"),
            Some(&(Verdict::Unchanged, Some("t-1".to_owned())))
        );
        // A commit bumps the catalog version; the unknown file answered
        // New from the old snapshot now classifies from a fresh one.
        let before = store.classify(
            "r1",
            &[("b.flac".to_owned(), 1, 1, 1e-9, "1:1".to_owned())],
            None,
        );
        assert_eq!(before.get("b.flac"), Some(&(Verdict::New, None)));
        store.commit_indexed("r1", "b.flac", 1, 1, "t-2".to_owned(), 99.0);
        let after = store.classify(
            "r1",
            &[("b.flac".to_owned(), 1, 1, 1e-9, "1:1".to_owned())],
            None,
        );
        assert_eq!(
            after.get("b.flac"),
            Some(&(Verdict::Unchanged, Some("t-2".to_owned())))
        );
    }

    /// Missing detection follows the exact scope-cover rule, including
    /// sibling prefixes and LIKE metacharacters in directory names.
    #[tokio::test]
    async fn missing_paths_match_scope_rule() {
        let store = SqliteScanStore::open_ephemeral().expect("ephemeral opens");
        let created = store.request_run(&request(ScanKind::Incremental), "run-1", 1.0);
        assert_eq!(created.disposition, Disposition::Started);
        store.claim_next(2.0).expect("claimed");
        for (path, track) in [
            ("sub/seen.flac", "t-seen"),
            ("sub/gone.flac", "t-gone"),
            ("other/gone.flac", "t-other"),
            ("100%_x/gone.flac", "t-wild"),
        ] {
            store.commit_indexed("r1", path, 10, 5, track.to_owned(), 99.0);
        }
        store
            .add_inventory_batch("run-1", vec![inventory_item("sub/seen.flac")], 2, 3.0, 1)
            .expect("batch lands");
        let mut missing = store.missing_catalog_paths("run-1", "r1", ".");
        missing.sort_unstable();
        assert_eq!(
            missing,
            vec![
                "100%_x/gone.flac".to_owned(),
                "other/gone.flac".to_owned(),
                "sub/gone.flac".to_owned(),
            ]
        );
        assert_eq!(
            store.missing_catalog_paths("run-1", "r1", "sub"),
            vec!["sub/gone.flac".to_owned()]
        );
        assert!(store.missing_catalog_paths("run-1", "r1", "su").is_empty());
        assert!(
            store
                .missing_catalog_paths("run-1", "r1", "sub/")
                .is_empty()
        );
    }

    /// Keyset pages cover the run exactly once in path order.
    #[tokio::test]
    async fn inventory_pages_cover_run_in_order() {
        let store = SqliteScanStore::open_ephemeral().expect("ephemeral opens");
        let created = store.request_run(&request(ScanKind::Incremental), "run-1", 1.0);
        assert_eq!(created.disposition, Disposition::Started);
        store.claim_next(2.0).expect("claimed");
        let revision = store
            .add_inventory_batch(
                "run-1",
                vec![
                    inventory_item("b.flac"),
                    inventory_item("a.flac"),
                    inventory_item("sub/c.flac"),
                ],
                2,
                3.0,
                1,
            )
            .expect("batch lands");
        assert_eq!(revision, 3);
        let (page, cursor) = store.inventory_page("run-1", None, 2).expect("page reads");
        let paths: Vec<&str> = page
            .iter()
            .map(|item| item.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["a.flac", "b.flac"]);
        let cursor = cursor.expect("full page carries a cursor");
        let after = (cursor.0.as_str(), cursor.1.as_str());
        let (page, _) = store
            .inventory_page("run-1", Some(after), 2)
            .expect("page reads");
        let paths: Vec<&str> = page
            .iter()
            .map(|item| item.relative_path.as_str())
            .collect();
        assert_eq!(paths, vec!["sub/c.flac"]);
        let (page, _) = store
            .inventory_page("run-1", Some(("r1", "sub/c.flac")), 2)
            .expect("page reads");
        assert!(page.is_empty());
    }

    /// Full scan, then restart: the reads layer shows the catalog.
    #[tokio::test]
    async fn scan_then_restart_reads_show_catalog() {
        let root = scratch_dir("library");
        std::fs::write(root.join("a.flac"), b"fake-audio-a").expect("fixture");
        std::fs::create_dir_all(root.join("sub")).expect("subdir");
        std::fs::write(root.join("sub").join("b.mp3"), b"fake-audio-b").expect("fixture");
        let (runtime, db_path) = migrated_db("catalog").await;
        let registry = RootRegistry::new(
            vec![LibraryRoot::new(
                "music",
                root.clone(),
                EffectivePolicy::Automatic,
            )],
            true,
            "rev-1",
        );
        let mut root_paths = HashMap::new();
        root_paths.insert("music".to_owned(), root.clone());
        {
            let store = Arc::new(SqliteScanStore::open(&db_path).expect("store opens"));
            let coordinator = LibraryScanCoordinator::new(
                store.clone(),
                BlockingPool::new(4),
                Arc::new(NullTagReader::new()),
                Arc::new(NullIdentifyQueue::new()),
                Arc::new(StaticResolver::new(registry)),
            )
            .with_wakeups(WorkWakeups::new());
            let request = ScanRequest {
                kind: ScanKind::Incremental,
                trigger: super::super::models::ScanTrigger::Manual,
                scopes: vec![ScanScope::root(
                    "music",
                    &root.display().to_string(),
                    "rev-1",
                )],
                requested_by_user_id: None,
                policy_revision: "rev-1".to_owned(),
            };
            let created = coordinator.request_run(&request).expect("accepted");
            assert_eq!(created.disposition, Disposition::Started);
            let run = coordinator
                .run_once(&root_paths)
                .await
                .expect("worker drove the run");
            assert_eq!(run.state, ScanState::Completed);
            let (_, _, counters) = store.get_run(&run.id).expect("run readable");
            assert_eq!(counters.get(counter_names::INDEXED), Some(&2));
        }
        // Restart: fresh store, same file.
        let store = SqliteScanStore::open(&db_path).expect("store reopens");
        assert_eq!(store.catalog_entries("music").len(), 2);
        let catalog = SqliteCatalog::new(&LibraryDb::new(runtime.pool()));
        let (tracks, total) = catalog
            .list_tracks(
                &TrackFilter::default(),
                crate::reads::library::stores::TrackSort::Title,
                false,
                100,
                0,
            )
            .await
            .expect("tracks list");
        assert_eq!(total, 2);
        assert_eq!(tracks.len(), 2);
        let titles: Vec<&str> = tracks.iter().map(|track| track.title.as_str()).collect();
        assert!(titles.contains(&"a"), "titles: {titles:?}");
        assert!(titles.contains(&"b"), "titles: {titles:?}");
        let (albums, album_total) = catalog
            .list_albums(&AlbumFilter::default(), AlbumSort::Name, false, 100, 0)
            .await
            .expect("albums list");
        assert_eq!(album_total, 2);
        assert_eq!(albums.len(), 2);
    }
}
