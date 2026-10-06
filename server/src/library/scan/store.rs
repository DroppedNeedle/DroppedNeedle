//! Durable scan state: the store ports and their shared types.
//!
//! v2 persists runs, scopes, inventory, failures, and the track catalog in
//! SQLite (`NativeLibraryStore`). The ports split along those concerns:
//! [`RunStore`], [`InventoryStore`], and [`CatalogStore`], bundled as
//! [`ScanStore`]. [`super::SqliteScanStore`] implements all three over the
//! application database.
//!
//! Cover and transition rules cite the v2 functions they port.

use std::collections::HashMap;

use super::models::{
    Counters, ScanControl, ScanFailureRecord, ScanInventoryItem, ScanRequest, ScanRequestResult,
    ScanRun, ScanScope, ScanState, ScopeDiscoveryState, Verdict,
};

/// Stat-revision lineage for one catalog row. `exact` rows compare the
/// `size:mtime_ns` string; `legacy_float` rows use the symmetric epsilon
/// band and promote to exact on an unchanged verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionKind {
    Exact,
    LegacyFloat,
}

/// One catalog row: the durable memory of a previously scanned file.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub track_id: String,
    pub revision: String,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub revision_kind: RevisionKind,
    pub tags_read_at: Option<f64>,
}

/// One path offered to [`ScanStore::classify`]: key, size, mtime_ns,
/// mtime seconds float, stat revision.
pub type ClassifyInput = (String, u64, i64, f64, String);

/// One inventory page plus the cursor for the next page (none when the
/// page came up short).
pub type InventoryPage = (Vec<ScanInventoryItem>, Option<(String, String)>);

/// One row for [`ScanStore::commit_indexed_batch`]: the same fields as
/// [`ScanStore::commit_indexed`], bundled so SQLite implementations can
/// land a whole batch in one transaction.
#[derive(Debug, Clone)]
pub struct CommitIndexedItem {
    pub root_id: String,
    pub relative_path: String,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub track_id: String,
    pub tags_read_at: f64,
}

/// Store errors. Spelling mirrors the v2 exception mapping: stale reads
/// fail loudly, missing runs are not-found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanStoreError {
    NotFound { run_id: String },
    StaleRevision { message: String },
    InvalidControl { message: String },
    Internal { message: String },
}

impl std::fmt::Display for ScanStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanStoreError::NotFound { run_id } => write!(f, "scan run not found: {run_id}"),
            ScanStoreError::StaleRevision { message } => write!(f, "{message}"),
            ScanStoreError::InvalidControl { message } => write!(f, "{message}"),
            ScanStoreError::Internal { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ScanStoreError {}

/// Scan runs: requests, claims, state transitions, control, failures,
/// counters, and boot recovery.
pub trait RunStore: Send + Sync {
    /// Record a scan request. A store failure is `Err`, never a
    /// conflict answer naming a run that was never written.
    fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> Result<ScanRequestResult, ScanStoreError>;

    fn get_run(&self, run_id: &str) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError>;

    fn list_current(&self) -> Vec<ScanRun>;

    fn list_history(&self, limit: usize) -> Vec<ScanRun>;

    fn latest_filesystem_terminal(&self) -> Option<ScanRun>;

    fn claim_next(&self, now: f64) -> Option<ScanRun>;

    fn resumable(&self) -> Option<ScanRun>;

    #[allow(clippy::too_many_arguments)]
    fn transition(
        &self,
        run_id: &str,
        expected_state: ScanState,
        expected_revision: u64,
        new_state: ScanState,
        now: f64,
        terminal_code: Option<&str>,
    ) -> Result<ScanRun, ScanStoreError>;

    fn request_control(
        &self,
        run_id: &str,
        control: ScanControl,
        resume: bool,
        expected_revision: u64,
        now: f64,
    ) -> Result<(ScanRun, u64), ScanStoreError>;

    fn record_failures(&self, run_id: &str, failures: Vec<ScanFailureRecord>);

    fn failures(&self, run_id: &str) -> Vec<ScanFailureRecord>;

    fn add_counter(&self, run_id: &str, name: &str, delta: i64);

    /// Add several counter deltas at once. Same per-name semantics as
    /// [`ScanStore::add_counter`]; the default loops, SQLite lands one
    /// statement.
    fn add_counters(&self, run_id: &str, deltas: &[(&str, i64)]);

    fn set_counter(&self, run_id: &str, name: &str, value: i64);

    fn stream_revision(&self, kind: &str) -> u64;

    /// the default is a no-op.
    fn checkpoint_terminal(&self);

    fn flush_invalidation(&self, terminal: bool);

    fn recover(&self, now: f64) -> Vec<ScanRun>;

    fn recover_stopping(&self, now: f64) -> Vec<ScanRun>;

    fn cleanup_terminal_inventory(&self, limit: usize);
}

/// Discovery state and the per-run file inventory.
pub trait InventoryStore: Send + Sync {
    fn scope_discovery_state(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> ScopeDiscoveryState;

    fn scope_discovery_generation(&self, run_id: &str, root_id: &str, relative_path: &str) -> u64;

    fn complete_scope_discovery(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
        state: ScopeDiscoveryState,
        error_code: Option<&str>,
    );

    fn restart_scope_discovery(&self, run_id: &str, root_id: &str, relative_path: &str);

    fn prepare_discovery_resume(&self, run_id: &str);

    fn finalize_discovery(&self, run_id: &str, updated_at: f64) -> Result<ScanRun, ScanStoreError>;

    /// Delete one bounded page of stale-generation inventory. Returns the
    /// number of rows still pending afterwards.
    fn cleanup_stale_inventory(&self, run_id: &str) -> usize;

    fn add_inventory_batch(
        &self,
        run_id: &str,
        items: Vec<ScanInventoryItem>,
        expected_run_revision: u64,
        updated_at: f64,
        generation: u64,
    ) -> Result<u64, ScanStoreError>;

    fn inventory_for_run(&self, run_id: &str) -> Vec<ScanInventoryItem>;

    /// One bounded inventory page in `(root_id, relative_path)` order,
    /// starting after `after` (none for the first page). Returns the page
    /// plus the cursor for the next page (none when the page came up
    /// short). Keyset paging keeps every page O(page): no offset rescan,
    /// no sort. The default sorts the full listing; SQLite seeks its
    /// primary key. Processing order is path order in both. A read
    /// failure is `Err`, never an empty page: an empty page means
    /// end-of-run, and the caller must fail the run on `Err` instead
    /// of completing the run short.
    fn inventory_page(
        &self,
        run_id: &str,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<InventoryPage, ScanStoreError>;
}

/// The track catalog: classification against it, commits into it, and
/// missing detection.
pub trait CatalogStore: Send + Sync {
    fn commit_indexed(
        &self,
        root_id: &str,
        relative_path: &str,
        size_bytes: u64,
        mtime_ns: i64,
        track_id: String,
        tags_read_at: f64,
    );

    /// Commit several catalog rows with the exact per-row semantics of
    /// [`ScanStore::commit_indexed`]. The default loops; SQLite lands one
    /// transaction with reused prepared statements.
    fn commit_indexed_batch(&self, items: &[CommitIndexedItem]);

    fn mark_deferred(&self, root_id: &str, relative_path: &str, deferred: bool);

    fn catalog_entries(&self, root_id: &str) -> Vec<(String, CatalogEntry)>;

    fn remove_catalog(&self, root_id: &str, relative_path: &str);

    /// Catalog paths under `scope_relative_path` that the run's current
    /// inventory does not contain: the missing set for one cleanly-walked
    /// scope. Only indexed catalog rows qualify, and scope matching is the
    /// exact [`scope_covers_path`] rule. The default joins the two full
    /// listings in memory; SQLite answers in one query.
    fn missing_catalog_paths(
        &self,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Vec<String>;

    fn classify(
        &self,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> HashMap<String, (Verdict, Option<String>)>;
}

/// Everything the coordinator and the walker need from durable scan state.
pub trait ScanStore: RunStore + InventoryStore + CatalogStore {}

impl<T: RunStore + InventoryStore + CatalogStore> ScanStore for T {}
