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
    Counters, EffectivePolicy, ScanControl, ScanFailureRecord, ScanInventoryItem, ScanRequest,
    ScanRequestResult, ScanRun, ScanScope, ScanState, ScopeDiscoveryState, Verdict,
};
use super::seams::ScannedTags;

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

/// One file the index phase read, ready for the catalog.
#[derive(Debug, Clone)]
pub struct CommitIndexedItem {
    pub root_id: String,
    pub relative_path: String,
    /// Where the walk found the file, stored as the track's `file_path`.
    pub absolute_path: String,
    pub size_bytes: u64,
    pub mtime_ns: i64,
    pub tags_read_at: f64,
    pub tags: ScannedTags,
    pub effective_policy: EffectivePolicy,
    pub policy_revision: String,
    /// Verdict counter the coordinator bumped for this file (`new_count`,
    /// `changed_count`, `unchanged_count`), undone if the row fails.
    pub verdict_counter: &'static str,
}

/// One index window: every inventory row in `(after, through]` was
/// handled. `items` go to the catalog; `failed` rows take the named
/// processing state (`failed`, `deferred`); every other pending row in
/// the range marks skipped. `counters` land with the rows.
#[derive(Debug, Clone, Default)]
pub struct IndexWindow {
    pub run_id: String,
    pub after: Option<(String, String)>,
    pub through: (String, String),
    pub items: Vec<CommitIndexedItem>,
    pub failed: Vec<(String, String, &'static str)>,
    pub counters: Vec<(&'static str, i64)>,
    pub now: f64,
}

/// What one window commit landed.
#[derive(Debug, Clone, Default)]
pub struct WindowOutcome {
    /// Catalog rows written.
    pub committed: usize,
    /// Identify jobs queued for the albums the window touched.
    pub enqueued: usize,
    /// Files that could not be written: root, path, store error.
    pub failed: Vec<(String, String, String)>,
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

    /// Terminal runs, newest first. `before` is a `(terminal_at, run id)`
    /// keyset cursor: only runs strictly older are listed.
    fn list_history(&self, limit: usize, before: Option<(f64, &str)>) -> Vec<ScanRun>;

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

    /// Delete one page (at most `limit` rows) of a finished run's
    /// inventory or failures, or prune old history when none is left.
    /// Returns true while more cleanup is pending.
    fn cleanup_terminal_inventory(&self, limit: usize) -> bool;
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

    /// Record one inventory page. Returns the run revision after the
    /// write plus how many rows could not be stored: when the page fails
    /// as a whole it lands row by row, and each refused row gets a
    /// `WALK_ERROR` failure naming the store error.
    fn add_inventory_batch(
        &self,
        run_id: &str,
        items: Vec<ScanInventoryItem>,
        expected_run_revision: u64,
        updated_at: f64,
        generation: u64,
    ) -> Result<(u64, usize), ScanStoreError>;

    /// The run's last `limit` current-generation inventory rows in
    /// discovery order, each with the catalog track id its path holds.
    fn inventory_for_run(&self, run_id: &str, limit: usize) -> Vec<ScanInventoryItem>;

    /// One bounded page of the run's unprocessed inventory rows in
    /// `(root_id, relative_path)` order,
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
    /// Commit one index window atomically: catalog rows, inventory
    /// marks, counters, and identify offers land together or not at all.
    /// A row the catalog refuses fails alone and is reported back.
    fn commit_window(&self, window: &IndexWindow) -> Result<WindowOutcome, ScanStoreError>;

    fn mark_deferred(&self, root_id: &str, relative_path: &str, deferred: bool);

    /// Catalog album of one indexed track: the track-to-album join.
    fn album_for_track(&self, track_id: &str) -> Option<String>;

    /// Indexed track at one path.
    fn track_at(&self, root_id: &str, relative_path: &str) -> Option<String>;

    /// Mark catalog rows missing (never delete: plays, playlists, and
    /// identities keep pointing at them) and count them on the run.
    /// Returns rows marked.
    fn mark_missing(
        &self,
        run_id: &str,
        root_id: &str,
        relative_paths: &[String],
        now: f64,
    ) -> Result<usize, ScanStoreError>;

    /// Indexed catalog rows under one scope.
    fn indexed_count(
        &self,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<usize, ScanStoreError>;

    /// For one walked scope before indexing: indexed rows the walk did
    /// not see, and the rows the scope holds once its new files index.
    /// Feeds the mass-missing guard.
    fn vanish_counts(
        &self,
        run_id: &str,
        root_id: &str,
        scope_relative_path: &str,
    ) -> Result<(usize, usize), ScanStoreError>;

    /// True when the run held missing detection back for this scope (the
    /// mass-missing guard). Such a scope counts as not walked: nothing in
    /// it is missing, moved, or taken over.
    fn scope_guarded(&self, run_id: &str, root_id: &str, scope_relative_path: &str) -> bool;

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
pub trait ScanStore: RunStore + InventoryStore + CatalogStore + 'static {}

impl<T: RunStore + InventoryStore + CatalogStore + 'static> ScanStore for T {}
