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
    CatalogEntry, CatalogStore, ClassifyInput, CommitIndexedItem, InventoryPage, InventoryStore,
    RevisionKind, RunStore, ScanStoreError,
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

mod catalog;
mod inventory;
mod runs;

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
            let created = store
                .request_run(&request(ScanKind::Incremental), "run-1", 1.0)
                .expect("request recorded");
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
        let created = store
            .request_run(&request(ScanKind::Incremental), "run-9", 1.0)
            .expect("request recorded");
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
        let created = store
            .request_run(&request(ScanKind::Incremental), "run-1", 1.0)
            .expect("request recorded");
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
        let created = store
            .request_run(&request(ScanKind::Incremental), "run-1", 1.0)
            .expect("request recorded");
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
