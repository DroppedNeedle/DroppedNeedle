//! Durable scan state: the store trait plus an in-memory implementation.
//!
//! v2 persists runs, scopes, inventory, failures, and the track catalog in
//! SQLite (`NativeLibraryStore`). This slice defines the [`ScanStore`] seam
//! with the exact disposition and transition semantics and ships
//! [`MemoryScanStore`], an honest in-process store the tests and the first
//! runtime use. A SQLite implementation can replace it later without
//! touching the coordinator or the walker.
//!
//! Cover and transition rules cite the v2 functions they port.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use super::models::{
    Counters, Disposition, RequestedControl, ScanControl, ScanFailureRecord, ScanInventoryItem,
    ScanKind, ScanPhase, ScanRequest, ScanRequestResult, ScanRun, ScanScope, ScanState,
    ScopeDiscoveryState, Verdict, counter_names, failure_codes, scope_covers_path,
};
use super::revision::{exact_stat_revision, legacy_mtime_eps_seconds};

/// Stat-revision lineage for one catalog row. `exact` rows compare the
/// `size:mtime_ns` string; `legacy_float` rows use the symmetric epsilon
/// band and promote to exact on an unchanged verdict (v2 F-15/4.12).
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

/// Durable scan-state seam. Synchronous: the memory implementation answers
/// inline, and a future SQLite implementation serializes on its own writer.
pub trait ScanStore: Send + Sync {
    fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> ScanRequestResult;

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

    fn cleanup_terminal_inventory(&self, limit: usize);

    fn classify(
        &self,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> HashMap<String, (Verdict, Option<String>)>;

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
    /// end-of-run, and the caller must fail honestly on `Err` instead
    /// of completing the run short.
    fn inventory_page(
        &self,
        run_id: &str,
        after: Option<(&str, &str)>,
        limit: usize,
    ) -> Result<InventoryPage, ScanStoreError> {
        let mut all = self.inventory_for_run(run_id);
        all.sort_by(|a, b| (&a.root_id, &a.relative_path).cmp(&(&b.root_id, &b.relative_path)));
        let mut items = Vec::new();
        let mut cursor = None;
        for item in all {
            if let Some((root, path)) = after
                && (item.root_id.as_str(), item.relative_path.as_str()) <= (root, path)
            {
                continue;
            }
            if items.len() >= limit.max(1) {
                break;
            }
            cursor = Some((item.root_id.clone(), item.relative_path.clone()));
            items.push(item);
        }
        Ok((items, cursor))
    }

    fn add_counter(&self, run_id: &str, name: &str, delta: i64);

    /// Add several counter deltas at once. Same per-name semantics as
    /// [`ScanStore::add_counter`]; the default loops, SQLite lands one
    /// statement.
    fn add_counters(&self, run_id: &str, deltas: &[(&str, i64)]) {
        for (name, delta) in deltas {
            self.add_counter(run_id, name, *delta);
        }
    }

    fn set_counter(&self, run_id: &str, name: &str, value: i64);

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
    fn commit_indexed_batch(&self, items: &[CommitIndexedItem]) {
        for item in items {
            self.commit_indexed(
                &item.root_id,
                &item.relative_path,
                item.size_bytes,
                item.mtime_ns,
                item.track_id.clone(),
                item.tags_read_at,
            );
        }
    }

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
    ) -> Vec<String> {
        let seen: HashSet<String> = self
            .inventory_for_run(run_id)
            .into_iter()
            .filter(|item| item.root_id == root_id)
            .map(|item| item.relative_path)
            .collect();
        self.catalog_entries(root_id)
            .into_iter()
            .map(|(relative_path, _)| relative_path)
            .filter(|relative_path| scope_covers_path(scope_relative_path, relative_path))
            .filter(|relative_path| !seen.contains(relative_path))
            .collect()
    }

    fn stream_revision(&self, kind: &str) -> u64;

    /// Fold the run's WAL frames back after a terminal outcome. Runs after
    /// the completed transition, off the observed scan wall. SQLite-only;
    /// the default is a no-op.
    fn checkpoint_terminal(&self) {}

    fn flush_invalidation(&self, terminal: bool);

    fn recover(&self, now: f64) -> Vec<ScanRun>;

    fn recover_stopping(&self, now: f64) -> Vec<ScanRun>;
}

#[derive(Debug, Clone)]
struct ScopeDiscovery {
    state: ScopeDiscoveryState,
    error_code: Option<String>,
    generation: u64,
}

#[derive(Debug, Clone)]
struct StoredInventoryItem {
    item: ScanInventoryItem,
    generation: u64,
}

#[derive(Debug)]
struct StoredRun {
    run: ScanRun,
    scopes: Vec<ScanScope>,
    discoveries: HashMap<(String, String), ScopeDiscovery>,
    inventory: Vec<StoredInventoryItem>,
    cleanup_pending: bool,
}

#[derive(Debug, Default)]
struct MemoryState {
    runs: HashMap<String, StoredRun>,
    order: Vec<String>,
    catalog: HashMap<(String, String), CatalogEntry>,
    failures: HashMap<String, Vec<ScanFailureRecord>>,
    deferred: HashSet<(String, String)>,
    stream_revisions: HashMap<String, u64>,
    invalidation_pending: bool,
    catalog_dirty: bool,
}

/// In-memory [`ScanStore`]. Honest about every disposition and transition;
/// persistence across restarts is the one thing it does not offer.
#[derive(Debug, Default)]
pub struct MemoryScanStore {
    state: Mutex<MemoryState>,
}

impl MemoryScanStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn empty_counters() -> Counters {
    [
        counter_names::TOTAL,
        counter_names::DISCOVERED,
        counter_names::INSPECTED,
        counter_names::NEW,
        counter_names::CHANGED,
        counter_names::INDEXED,
        counter_names::UNCHANGED,
        counter_names::EXCLUDED,
        counter_names::MISSING,
        counter_names::ERRORED,
        counter_names::IDENTIFICATION_ENQUEUED,
    ]
    .into_iter()
    .map(|name| (name.to_owned(), 0))
    .collect()
}

/// True when a stored scope covers a requested one: same root, same policy
/// revision, and the stored path is the requested path or an ancestor of it
/// (v2 `_scan_scope_covers` plus the revision check in `covers`).
fn scope_covers(existing: &ScanScope, requested_root: &str, requested: &ScanScope) -> bool {
    existing.root_id == requested_root
        && existing.policy_revision == requested.policy_revision
        && scope_covers_path(&existing.relative_path, &requested.relative_path)
}

fn bump_counters(run: &mut ScanRun, name: &str, delta: i64) {
    *run.counters.entry(name.to_owned()).or_insert(0) += delta;
}

fn normalize_key(raw: &str) -> String {
    // Step 4.13 / F-16 belt-and-braces: NFC-normalize incoming keys so the
    // catalog join matches the walker's normalized keys.
    use unicode_normalization::UnicodeNormalization;
    raw.nfc().collect()
}

impl ScanStore for MemoryScanStore {
    fn request_run(
        &self,
        request: &ScanRequest,
        run_id: &str,
        requested_at: f64,
    ) -> ScanRequestResult {
        let mut state = self.lock();
        let mut current: Vec<String> = state
            .order
            .iter()
            .filter(|id| {
                state
                    .runs
                    .get(*id)
                    .map(|stored| !stored.run.state.is_terminal())
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        // Active runs first, then by queue order (v2 ORDER BY).
        current.sort_by_key(|id| {
            let queued = state
                .runs
                .get(id)
                .map(|stored| stored.run.state == ScanState::Queued)
                .unwrap_or(false);
            (
                queued,
                state
                    .order
                    .iter()
                    .position(|o| o == id)
                    .unwrap_or(usize::MAX),
            )
        });
        let has_active = current.iter().any(|id| {
            state
                .runs
                .get(id)
                .map(|stored| stored.run.state != ScanState::Queued)
                .unwrap_or(false)
        });
        let queued_id: Option<String> = current
            .iter()
            .find(|id| {
                state
                    .runs
                    .get(*id)
                    .map(|stored| stored.run.state == ScanState::Queued)
                    .unwrap_or(false)
            })
            .map(|id| (*id).clone());

        // F-SCAN-02: only a queued run may cover a request. A matching
        // request during active work falls through to the queued follow-up
        // instead of being acknowledged against work that can still fail.
        let covering: Option<String> = current
            .iter()
            .find(|id| {
                let stored = match state.runs.get(*id) {
                    Some(stored) => stored,
                    None => return false,
                };
                if stored.run.state != ScanState::Queued || stored.run.kind != request.kind {
                    return false;
                }
                if stored.scopes.is_empty() {
                    return false;
                }
                request.scopes.iter().all(|requested| {
                    stored
                        .scopes
                        .iter()
                        .any(|existing| scope_covers(existing, &requested.root_id, requested))
                })
            })
            .map(|id| (*id).clone());
        if let Some(covering_id) = covering
            && let Some(stored) = state.runs.get_mut(&covering_id)
        {
            let revision = {
                stored.run.coalesced_request_count += 1;
                stored.run.updated_at = requested_at;
                stored.run.row_revision += 1;
                stored.run.event_revision += 1;
                stored.run.row_revision
            };
            *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
            return ScanRequestResult {
                run_id: covering_id,
                disposition: Disposition::Coalesced,
                state: ScanState::Queued,
                row_revision: revision,
                queued_reason: None,
                conflicting_kind: None,
            };
        }

        if let Some(queued_id) = queued_id {
            let conflict = state.runs.get(&queued_id).and_then(|stored| {
                let incompatible = stored.run.kind != request.kind
                    || stored
                        .scopes
                        .iter()
                        .any(|scope| scope.policy_revision != request.policy_revision);
                incompatible.then_some((stored.run.row_revision, stored.run.kind))
            });
            if let Some((row_revision, kind)) = conflict {
                return ScanRequestResult {
                    run_id: queued_id,
                    disposition: Disposition::Conflict,
                    state: ScanState::Queued,
                    row_revision,
                    queued_reason: Some(
                        "The follow-up slot already contains incompatible work.".to_owned(),
                    ),
                    conflicting_kind: Some(kind),
                };
            }
            // F-INDEXREC-01: normalize the union inside the transaction.
            // For each root retain the broadest ancestor and drop its
            // descendants; a root "." supersedes every scope for that root.
            // A missing row (only if the map changed under its own lock,
            // which cannot happen) falls through to fresh-run creation.
            if let Some(mut stored) = state.runs.remove(&queued_id) {
                let additions: Vec<ScanScope> = request
                    .scopes
                    .iter()
                    .filter(|requested| {
                        !stored.scopes.iter().any(|existing| {
                            existing.root_id == requested.root_id
                                && scope_covers_path(
                                    &existing.relative_path,
                                    &requested.relative_path,
                                )
                        })
                    })
                    .cloned()
                    .collect();
                stored.scopes.retain(|existing| {
                    !additions.iter().any(|addition| {
                        existing.root_id == addition.root_id
                            && scope_covers_path(&addition.relative_path, &existing.relative_path)
                            && existing.relative_path != addition.relative_path
                    })
                });
                for scope in &additions {
                    stored.discoveries.insert(
                        (scope.root_id.clone(), scope.relative_path.clone()),
                        ScopeDiscovery {
                            state: ScopeDiscoveryState::Pending,
                            error_code: None,
                            generation: 1,
                        },
                    );
                }
                stored.scopes.extend(additions);
                // Quirk port: v2 derives aggregate_scope from the request alone,
                // so expanding an "all" run with selected scopes regresses the
                // label to "selected". Preserved exactly.
                stored.run.aggregate_scope = if request
                    .scopes
                    .iter()
                    .any(|scope| scope.relative_path == ".")
                {
                    "all".to_owned()
                } else {
                    "selected".to_owned()
                };
                stored.run.updated_at = requested_at;
                stored.run.row_revision += 1;
                stored.run.event_revision += 1;
                let revision = stored.run.row_revision;
                state.runs.insert(queued_id.clone(), stored);
                *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
                return ScanRequestResult {
                    run_id: queued_id,
                    disposition: Disposition::Expanded,
                    state: ScanState::Queued,
                    row_revision: revision,
                    queued_reason: None,
                    conflicting_kind: None,
                };
            }
        }

        let aggregate_scope = if request
            .scopes
            .iter()
            .any(|scope| scope.relative_path == ".")
        {
            "all"
        } else {
            "selected"
        }
        .to_owned();
        let discoveries = request
            .scopes
            .iter()
            .map(|scope| {
                (
                    (scope.root_id.clone(), scope.relative_path.clone()),
                    ScopeDiscovery {
                        state: ScopeDiscoveryState::Pending,
                        error_code: None,
                        generation: 1,
                    },
                )
            })
            .collect();
        state.runs.insert(
            run_id.to_owned(),
            StoredRun {
                run: ScanRun {
                    id: run_id.to_owned(),
                    kind: request.kind,
                    trigger: request.trigger,
                    state: ScanState::Queued,
                    phase: ScanPhase::Queued,
                    requested_by_user_id: request.requested_by_user_id.clone(),
                    aggregate_scope,
                    queued_at: requested_at,
                    started_at: None,
                    updated_at: requested_at,
                    terminal_at: None,
                    resume_phase: None,
                    requested_control: RequestedControl::None,
                    terminal_code: None,
                    coalesced_request_count: 0,
                    row_revision: 1,
                    event_revision: 0,
                    counters: empty_counters(),
                    phase_timings: HashMap::new(),
                },
                scopes: request.scopes.clone(),
                discoveries,
                inventory: Vec::new(),
                cleanup_pending: true,
            },
        );
        state.order.push(run_id.to_owned());
        *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
        let disposition = if has_active {
            Disposition::Queued
        } else {
            Disposition::Started
        };
        ScanRequestResult {
            run_id: run_id.to_owned(),
            disposition,
            state: ScanState::Queued,
            row_revision: 1,
            queued_reason: if has_active {
                Some("Another scan is active.".to_owned())
            } else {
                None
            },
            conflicting_kind: None,
        }
    }

    fn get_run(&self, run_id: &str) -> Result<(ScanRun, Vec<ScanScope>, Counters), ScanStoreError> {
        let state = self.lock();
        state
            .runs
            .get(run_id)
            .map(|stored| {
                (
                    stored.run.clone(),
                    stored.scopes.clone(),
                    stored.run.counters.clone(),
                )
            })
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })
    }

    fn list_current(&self) -> Vec<ScanRun> {
        let state = self.lock();
        let mut runs: Vec<ScanRun> = state
            .runs
            .values()
            .filter(|stored| !stored.run.state.is_terminal())
            .map(|stored| stored.run.clone())
            .collect();
        runs.sort_by(|a, b| {
            (a.state == ScanState::Queued)
                .cmp(&(b.state == ScanState::Queued))
                .then(
                    a.queued_at
                        .partial_cmp(&b.queued_at)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
        });
        runs
    }

    fn list_history(&self, limit: usize) -> Vec<ScanRun> {
        let state = self.lock();
        let mut runs: Vec<ScanRun> = state
            .runs
            .values()
            .filter(|stored| stored.run.terminal_at.is_some())
            .map(|stored| stored.run.clone())
            .collect();
        runs.sort_by(|a, b| {
            b.terminal_at
                .partial_cmp(&a.terminal_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.id.cmp(&a.id))
        });
        runs.truncate(limit.max(1));
        runs
    }

    fn latest_filesystem_terminal(&self) -> Option<ScanRun> {
        // v2: terminal runs where kind != policy_reconcile OR the run
        // covered everything; newest first.
        self.list_history(usize::MAX)
            .into_iter()
            .find(|run| run.kind != ScanKind::PolicyReconcile || run.aggregate_scope == "all")
    }

    fn claim_next(&self, now: f64) -> Option<ScanRun> {
        let mut state = self.lock();
        let blocked = state
            .runs
            .values()
            .any(|stored| stored.run.state.blocks_claim());
        if blocked {
            return None;
        }
        let candidate = state
            .order
            .iter()
            .filter_map(|id| state.runs.get(id))
            .find(|stored| stored.run.state == ScanState::Queued)
            .map(|stored| stored.run.id.clone())?;
        let run = {
            let stored = state.runs.get_mut(&candidate)?;
            stored.run.state = ScanState::Discovering;
            stored.run.phase = ScanPhase::Discovering;
            if stored.run.started_at.is_none() {
                stored.run.started_at = Some(now);
            }
            stored.run.updated_at = now;
            stored.run.row_revision += 1;
            stored.run.event_revision += 1;
            stored.run.clone()
        };
        *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
        Some(run)
    }

    fn resumable(&self) -> Option<ScanRun> {
        // v2 get_resumable: discovering/indexing/reconciling with no pending
        // control latch, oldest first. Paused runs resume through control.
        let state = self.lock();
        let mut candidates: Vec<&StoredRun> = state
            .runs
            .values()
            .filter(|stored| {
                matches!(
                    stored.run.state,
                    ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
                ) && stored.run.requested_control == RequestedControl::None
            })
            .collect();
        candidates.sort_by(|a, b| {
            a.run
                .started_at
                .partial_cmp(&b.run.started_at)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.run.id.cmp(&b.run.id))
        });
        candidates.first().map(|stored| stored.run.clone())
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
        let mut state = self.lock();
        let run = {
            let stored = state
                .runs
                .get_mut(run_id)
                .ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
            if stored.run.state != expected_state || stored.run.row_revision != expected_revision {
                return Err(ScanStoreError::StaleRevision {
                    message: "The scan run changed before the transition was applied.".to_owned(),
                });
            }
            stored.run.state = new_state;
            match new_state {
                ScanState::Discovering => stored.run.phase = ScanPhase::Discovering,
                ScanState::Indexing => stored.run.phase = ScanPhase::Indexing,
                ScanState::Reconciling => stored.run.phase = ScanPhase::Reconciling,
                _ => {}
            }
            if new_state.is_terminal() {
                stored.run.terminal_at = Some(now);
                stored.run.terminal_code = terminal_code.map(str::to_owned);
            }
            stored.run.updated_at = now;
            stored.run.row_revision += 1;
            stored.run.event_revision += 1;
            stored.run.clone()
        };
        *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
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
        // Port of v2 request_scan_control, including the idempotent
        // no-change answers (pause on pausing/paused, stop on
        // stopping/cancelled, resume on active-without-latch).
        let mut state = self.lock();
        let snapshot = state
            .runs
            .get(run_id)
            .map(|stored| stored.run.clone())
            .ok_or_else(|| ScanStoreError::NotFound {
                run_id: run_id.to_owned(),
            })?;
        let run_state = snapshot.state;
        let current_stream = *state.stream_revisions.get("scan").unwrap_or(&0);
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
        let run = {
            let Some(stored) = state.runs.get_mut(run_id) else {
                return Err(ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                });
            };
            if resume {
                let Some(resume_phase) = stored.run.resume_phase else {
                    return Err(ScanStoreError::InvalidControl {
                        message: "Only a paused scan can be resumed.".to_owned(),
                    });
                };
                if run_state != ScanState::Paused {
                    return Err(ScanStoreError::InvalidControl {
                        message: "Only a paused scan can be resumed.".to_owned(),
                    });
                }
                stored.run.state = match resume_phase {
                    ScanPhase::Discovering => ScanState::Discovering,
                    ScanPhase::Indexing => ScanState::Indexing,
                    ScanPhase::Reconciling => ScanState::Reconciling,
                    ScanPhase::Queued => ScanState::Queued,
                };
                stored.run.requested_control = RequestedControl::None;
                stored.run.resume_phase = None;
            } else if control == ScanControl::Pause {
                if !matches!(
                    run_state,
                    ScanState::Discovering | ScanState::Indexing | ScanState::Reconciling
                ) {
                    return Err(ScanStoreError::InvalidControl {
                        message: "This scan cannot be paused in its current state.".to_owned(),
                    });
                }
                stored.run.state = ScanState::Pausing;
                stored.run.requested_control = RequestedControl::Pause;
                stored.run.resume_phase = Some(stored.run.phase);
            } else if run_state == ScanState::Paused {
                stored.run.state = ScanState::Cancelled;
                stored.run.requested_control = RequestedControl::None;
                stored.run.terminal_at = Some(now);
            } else if matches!(
                run_state,
                ScanState::Queued
                    | ScanState::Discovering
                    | ScanState::Indexing
                    | ScanState::Reconciling
                    | ScanState::Pausing
            ) {
                stored.run.state = if run_state == ScanState::Queued {
                    stored.run.terminal_at = Some(now);
                    ScanState::Cancelled
                } else {
                    ScanState::Stopping
                };
                stored.run.requested_control = RequestedControl::Stop;
            } else {
                return Err(ScanStoreError::InvalidControl {
                    message: "This scan cannot be stopped in its current state.".to_owned(),
                });
            }
            stored.run.updated_at = now;
            stored.run.row_revision += 1;
            stored.run.event_revision += 1;
            stored.run.clone()
        };
        *state.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
        let stream_revision = *state.stream_revisions.get("scan").unwrap_or(&0);
        Ok((run, stream_revision))
    }

    fn record_failures(&self, run_id: &str, failures: Vec<ScanFailureRecord>) {
        if failures.is_empty() {
            return;
        }
        let mut state = self.lock();
        state
            .failures
            .entry(run_id.to_owned())
            .or_default()
            .extend(failures);
        state.catalog_dirty = true;
    }

    fn failures(&self, run_id: &str) -> Vec<ScanFailureRecord> {
        self.lock()
            .failures
            .get(run_id)
            .cloned()
            .unwrap_or_default()
    }

    fn scope_discovery_state(
        &self,
        run_id: &str,
        root_id: &str,
        relative_path: &str,
    ) -> ScopeDiscoveryState {
        self.lock()
            .runs
            .get(run_id)
            .and_then(|stored| {
                stored
                    .discoveries
                    .get(&(root_id.to_owned(), relative_path.to_owned()))
                    .map(|discovery| discovery.state)
            })
            .unwrap_or(ScopeDiscoveryState::Pending)
    }

    fn scope_discovery_generation(&self, run_id: &str, root_id: &str, relative_path: &str) -> u64 {
        self.lock()
            .runs
            .get(run_id)
            .and_then(|stored| {
                stored
                    .discoveries
                    .get(&(root_id.to_owned(), relative_path.to_owned()))
                    .map(|discovery| discovery.generation)
            })
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
        let mut guard = self.lock();
        if let Some(stored) = guard.runs.get_mut(run_id) {
            stored.discoveries.insert(
                (root_id.to_owned(), relative_path.to_owned()),
                ScopeDiscovery {
                    state,
                    error_code: error_code.map(str::to_owned),
                    generation: stored
                        .discoveries
                        .get(&(root_id.to_owned(), relative_path.to_owned()))
                        .map(|discovery| discovery.generation)
                        .unwrap_or(1),
                },
            );
        }
    }

    fn restart_scope_discovery(&self, run_id: &str, root_id: &str, relative_path: &str) {
        let mut guard = self.lock();
        if let Some(stored) = guard.runs.get_mut(run_id) {
            let generation = stored
                .discoveries
                .get(&(root_id.to_owned(), relative_path.to_owned()))
                .map(|discovery| discovery.generation)
                .unwrap_or(1)
                + 1;
            stored.discoveries.insert(
                (root_id.to_owned(), relative_path.to_owned()),
                ScopeDiscovery {
                    state: ScopeDiscoveryState::Pending,
                    error_code: None,
                    generation,
                },
            );
        }
    }

    fn prepare_discovery_resume(&self, run_id: &str) {
        // v2 prepare_scan_discovery_resume: every incomplete scope starts a
        // new generation, and discovered_count recounts current-generation
        // rows only.
        let mut guard = self.lock();
        let Some(stored) = guard.runs.get_mut(run_id) else {
            return;
        };
        for discovery in stored.discoveries.values_mut() {
            if discovery.state != ScopeDiscoveryState::Completed {
                discovery.state = ScopeDiscoveryState::Pending;
                discovery.generation += 1;
                discovery.error_code = None;
            }
        }
        let current: HashSet<(String, String, u64)> = stored
            .discoveries
            .iter()
            .map(|((root, rel), discovery)| (root.clone(), rel.clone(), discovery.generation))
            .collect();
        let discovered = stored
            .inventory
            .iter()
            .filter(|row| {
                current.contains(&(
                    row.item.root_id.clone(),
                    row.item.scope_relative_path.clone(),
                    row.generation,
                ))
            })
            .count() as i64;
        stored
            .run
            .counters
            .insert(counter_names::DISCOVERED.to_owned(), discovered);
        stored.run.row_revision += 1;
    }

    fn finalize_discovery(&self, run_id: &str, updated_at: f64) -> Result<ScanRun, ScanStoreError> {
        let mut guard = self.lock();
        let run = {
            let stored = guard
                .runs
                .get_mut(run_id)
                .ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
            let current: HashSet<(String, String, u64)> = stored
                .discoveries
                .iter()
                .map(|((root, rel), discovery)| (root.clone(), rel.clone(), discovery.generation))
                .collect();
            let total = stored
                .inventory
                .iter()
                .filter(|row| {
                    current.contains(&(
                        row.item.root_id.clone(),
                        row.item.scope_relative_path.clone(),
                        row.generation,
                    ))
                })
                .count() as i64;
            stored
                .run
                .counters
                .insert(counter_names::TOTAL.to_owned(), total);
            stored
                .run
                .counters
                .insert(counter_names::DISCOVERED.to_owned(), total);
            stored.run.updated_at = updated_at;
            stored.run.row_revision += 1;
            stored.run.event_revision += 1;
            stored.run.clone()
        };
        *guard.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
        Ok(run)
    }

    fn cleanup_stale_inventory(&self, run_id: &str) -> usize {
        // One bounded page (5,000 rows) of older-generation inventory.
        // Returns rows still pending so the caller can keep draining.
        let mut guard = self.lock();
        let Some(stored) = guard.runs.get_mut(run_id) else {
            return 0;
        };
        let current: HashSet<(String, String, u64)> = stored
            .discoveries
            .iter()
            .map(|((root, rel), discovery)| (root.clone(), rel.clone(), discovery.generation))
            .collect();
        let mut removed = 0usize;
        stored.inventory.retain(|row| {
            let stale = !current.contains(&(
                row.item.root_id.clone(),
                row.item.scope_relative_path.clone(),
                row.generation,
            ));
            if stale && removed < 5_000 {
                removed += 1;
                return false;
            }
            true
        });
        let pending = stored
            .inventory
            .iter()
            .filter(|row| {
                !current.contains(&(
                    row.item.root_id.clone(),
                    row.item.scope_relative_path.clone(),
                    row.generation,
                ))
            })
            .count();
        let _ = removed;
        pending
    }

    fn cleanup_terminal_inventory(&self, limit: usize) {
        // Oldest terminal run with pending cleanup loses one page of
        // failures (TAG_READ_DEFERRED markers survive: they are the
        // persisted re-offer marker for the next run's classify) and then
        // its inventory, before history prunes past 50 terminal runs.
        let mut guard = self.lock();
        let target = guard
            .runs
            .values()
            .filter(|stored| stored.run.terminal_at.is_some() && stored.cleanup_pending)
            .min_by(|a, b| {
                a.run
                    .terminal_at
                    .partial_cmp(&b.run.terminal_at)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(a.run.id.cmp(&b.run.id))
            })
            .map(|stored| stored.run.id.clone());
        let Some(target) = target else {
            // History prune past 50 terminal runs.
            let mut terminal: Vec<(f64, String)> = guard
                .runs
                .values()
                .filter(|stored| stored.run.terminal_at.is_some() && !stored.cleanup_pending)
                .map(|stored| (stored.run.terminal_at.unwrap_or(0.0), stored.run.id.clone()))
                .collect();
            terminal.sort_by(|a, b| {
                b.0.partial_cmp(&a.0)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then(b.1.cmp(&a.1))
            });
            for (_, id) in terminal.into_iter().skip(50) {
                guard.runs.remove(&id);
                guard.failures.remove(&id);
            }
            let live: HashSet<String> = guard.runs.keys().cloned().collect();
            guard.order.retain(|id| live.contains(id));
            return;
        };
        let page = limit.max(1);
        if let Some(failures) = guard.failures.get_mut(&target) {
            let mut removed = 0usize;
            failures.retain(|failure| {
                if failure.failure_code != failure_codes::TAG_READ_DEFERRED && removed < page {
                    removed += 1;
                    return false;
                }
                true
            });
            if removed > 0 {
                return;
            }
        }
        if let Some(stored) = guard.runs.get_mut(&target) {
            let drain = stored.inventory.len().min(page);
            stored.inventory.drain(..drain);
            if stored.inventory.is_empty() {
                stored.cleanup_pending = false;
            }
        }
    }

    fn classify(
        &self,
        root_id: &str,
        paths: &[ClassifyInput],
        run_id: Option<&str>,
    ) -> HashMap<String, (Verdict, Option<String>)> {
        // Port of v2 classify_scan_paths: new when no catalog row, exact
        // revision compare, legacy symmetric band with promotion, deferred
        // re-offer, and MTIME_SKEW evidence only when run_id is passed.
        if paths.is_empty() {
            return HashMap::new();
        }
        let mut guard = self.lock();
        let mut verdicts = HashMap::with_capacity(paths.len());
        let mut promotions: Vec<(String, ClassifyInput)> = Vec::new();
        let mut skew: Vec<(String, &str)> = Vec::new();
        for input in paths {
            let key = normalize_key(&input.0);
            let entry = guard
                .catalog
                .get(&(root_id.to_owned(), key.clone()))
                .cloned();
            let Some(entry) = entry else {
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
                        skew.push((key.clone(), "legacy_float"));
                    }
                    same_size && (current_mtime - saved_mtime).abs() <= band
                }
            };
            if unchanged && guard.deferred.contains(&(root_id.to_owned(), key.clone())) {
                // F-12: a file deferred by tag-read exhaustion still carries
                // the marker, so it re-offers as changed; a clean read
                // clears the marker in commit_indexed.
                verdicts.insert(key, (Verdict::Changed, Some(entry.track_id)));
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
                    Some(entry.track_id),
                ),
            );
        }
        for (key, input) in promotions {
            if let Some(entry) = guard.catalog.get_mut(&(root_id.to_owned(), key)) {
                entry.revision = input.4.clone();
                entry.size_bytes = input.1;
                entry.mtime_ns = input.2;
                entry.revision_kind = RevisionKind::Exact;
            }
        }
        if let Some(run_id) = run_id {
            // Skew evidence lands as discovering-phase failure rows, the
            // same family as the WALK codes.
            let records: Vec<ScanFailureRecord> = skew
                .into_iter()
                .map(|(relative_path, _kind)| ScanFailureRecord {
                    root_id: root_id.to_owned(),
                    relative_path,
                    failure_code: failure_codes::MTIME_SKEW.to_owned(),
                    recorded_at: 0.0,
                    failure_detail: "Legacy mtime drift beyond tolerance; the file clock and the read clock disagree."
                        .to_owned(),
                    phase: ScanPhase::Discovering,
                })
                .collect();
            if !records.is_empty() {
                guard
                    .failures
                    .entry(run_id.to_owned())
                    .or_default()
                    .extend(records);
            }
        }
        verdicts
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
        let (revision, twin_losers) = {
            let stored = guard
                .runs
                .get_mut(run_id)
                .ok_or_else(|| ScanStoreError::NotFound {
                    run_id: run_id.to_owned(),
                })?;
            if stored.run.row_revision != expected_run_revision {
                return Err(ScanStoreError::StaleRevision {
                    message: "The scan run changed before inventory was recorded.".to_owned(),
                });
            }
            let mut seen: HashSet<String> = HashSet::new();
            let mut twin_losers: Vec<ScanFailureRecord> = Vec::new();
            let mut landed = 0i64;
            for item in items {
                // Same first-wins twin rule as the walk-time dedup: the batch
                // writer is reachable without the walk, so it dedups here too
                // instead of letting one key reach the PK twice.
                if !seen.insert(item.relative_path.clone()) {
                    twin_losers.push(ScanFailureRecord {
                        root_id: item.root_id.clone(),
                        relative_path: item.relative_path.clone(),
                        failure_code: failure_codes::NFC_TWIN_COLLISION.to_owned(),
                        recorded_at: updated_at,
                        failure_detail: "Two on-disk names normalize to the same inventory key; the first file won and this twin was skipped.".to_owned(),
                        phase: ScanPhase::Discovering,
                    });
                    continue;
                }
                // Discovery bumps discovered_count only; verdict counters
                // (new/changed/unchanged/excluded/errored/inspected/indexed)
                // land at index time in commit_scan_index_batch.
                stored
                    .inventory
                    .push(StoredInventoryItem { item, generation });
                landed += 1;
            }
            bump_counters(&mut stored.run, counter_names::DISCOVERED, landed);
            stored.run.updated_at = updated_at;
            stored.run.row_revision += 1;
            (stored.run.row_revision, twin_losers)
        };
        if !twin_losers.is_empty() {
            guard
                .failures
                .entry(run_id.to_owned())
                .or_default()
                .extend(twin_losers);
        }
        Ok(revision)
    }

    fn inventory_for_run(&self, run_id: &str) -> Vec<ScanInventoryItem> {
        let guard = self.lock();
        let Some(stored) = guard.runs.get(run_id) else {
            return Vec::new();
        };
        let current: HashSet<(String, String, u64)> = stored
            .discoveries
            .iter()
            .map(|((root, rel), discovery)| (root.clone(), rel.clone(), discovery.generation))
            .collect();
        stored
            .inventory
            .iter()
            .filter(|row| {
                current.contains(&(
                    row.item.root_id.clone(),
                    row.item.scope_relative_path.clone(),
                    row.generation,
                ))
            })
            .map(|row| row.item.clone())
            .collect()
    }

    fn add_counter(&self, run_id: &str, name: &str, delta: i64) {
        let mut guard = self.lock();
        if let Some(stored) = guard.runs.get_mut(run_id) {
            bump_counters(&mut stored.run, name, delta);
        }
    }

    fn set_counter(&self, run_id: &str, name: &str, value: i64) {
        let mut guard = self.lock();
        if let Some(stored) = guard.runs.get_mut(run_id) {
            stored.run.counters.insert(name.to_owned(), value);
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
        // A clean read clears the TAG_READ_DEFERRED marker (F-12) and
        // upserts the catalog row as exact.
        let mut guard = self.lock();
        guard
            .deferred
            .remove(&(root_id.to_owned(), relative_path.to_owned()));
        guard.catalog.insert(
            (root_id.to_owned(), relative_path.to_owned()),
            CatalogEntry {
                track_id,
                revision: exact_stat_revision(size_bytes, mtime_ns),
                size_bytes,
                mtime_ns,
                revision_kind: RevisionKind::Exact,
                tags_read_at: Some(tags_read_at),
            },
        );
        guard.catalog_dirty = true;
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
        self.lock()
            .catalog
            .iter()
            .filter(|((root, _), _)| root == root_id)
            .map(|((_, rel), entry)| (rel.clone(), entry.clone()))
            .collect()
    }

    fn remove_catalog(&self, root_id: &str, relative_path: &str) {
        let mut guard = self.lock();
        guard
            .catalog
            .remove(&(root_id.to_owned(), relative_path.to_owned()));
        guard
            .deferred
            .remove(&(root_id.to_owned(), relative_path.to_owned()));
        guard.catalog_dirty = true;
    }

    fn stream_revision(&self, kind: &str) -> u64 {
        *self.lock().stream_revisions.get(kind).unwrap_or(&0)
    }

    fn flush_invalidation(&self, terminal: bool) {
        // v2 flush_scan_invalidation: terminal runs invalidate when the
        // catalog moved; mid-run flushes only fire when pending.
        let mut guard = self.lock();
        if terminal && guard.catalog_dirty {
            guard.catalog_dirty = false;
            guard.invalidation_pending = false;
            return;
        }
        if guard.invalidation_pending {
            guard.invalidation_pending = false;
        }
    }

    fn recover(&self, now: f64) -> Vec<ScanRun> {
        // v2 recover_scan_runs: stopping/stop-latched runs cancel, pausing
        // runs settle to paused, and everything resumable is returned.
        let mut guard = self.lock();
        for stored in guard.runs.values_mut() {
            if stored.run.state == ScanState::Stopping
                || stored.run.requested_control == RequestedControl::Stop
            {
                stored.run.state = ScanState::Cancelled;
                stored.run.terminal_at = Some(now);
                stored.run.updated_at = now;
                stored.run.requested_control = RequestedControl::None;
                stored.run.row_revision += 1;
                stored.run.event_revision += 1;
            }
        }
        for stored in guard.runs.values_mut() {
            if stored.run.state == ScanState::Pausing
                || stored.run.requested_control == RequestedControl::Pause
            {
                stored.run.state = ScanState::Paused;
                stored.run.requested_control = RequestedControl::None;
                stored.run.updated_at = now;
                stored.run.row_revision += 1;
                stored.run.event_revision += 1;
            }
        }
        let mut runs: Vec<ScanRun> = guard
            .runs
            .values()
            .filter(|stored| {
                matches!(
                    stored.run.state,
                    ScanState::Queued
                        | ScanState::Discovering
                        | ScanState::Indexing
                        | ScanState::Reconciling
                        | ScanState::Paused
                )
            })
            .map(|stored| stored.run.clone())
            .collect();
        runs.sort_by(|a, b| {
            a.queued_at
                .partial_cmp(&b.queued_at)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        if !runs.is_empty() {
            *guard.stream_revisions.entry("scan".to_owned()).or_insert(0) += 1;
        }
        runs
    }

    fn recover_stopping(&self, now: f64) -> Vec<ScanRun> {
        // v2 recover_stopping_scan_runs: only the stopping runs cancel (the
        // library-disabled path); everything else is left alone.
        let mut guard = self.lock();
        for stored in guard.runs.values_mut() {
            if stored.run.state == ScanState::Stopping
                || stored.run.requested_control == RequestedControl::Stop
            {
                stored.run.state = ScanState::Cancelled;
                stored.run.terminal_at = Some(now);
                stored.run.updated_at = now;
                stored.run.requested_control = RequestedControl::None;
                stored.run.row_revision += 1;
                stored.run.event_revision += 1;
            }
        }
        guard
            .runs
            .values()
            .filter(|stored| {
                stored.run.state == ScanState::Cancelled && stored.run.terminal_at == Some(now)
            })
            .map(|stored| stored.run.clone())
            .collect()
    }
}

/// Seed helper for tests: insert a catalog row directly.
pub fn seed_catalog_entry(
    store: &MemoryScanStore,
    root_id: &str,
    relative_path: &str,
    entry: CatalogEntry,
) {
    store
        .lock()
        .catalog
        .insert((root_id.to_owned(), relative_path.to_owned()), entry);
}

#[cfg(test)]
mod tests {
    use super::super::models::EffectivePolicy;
    use super::*;

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

    #[test]
    fn request_coalesce_expand_conflict_flow() {
        let store = MemoryScanStore::new();
        let first = store.request_run(&request(ScanKind::Incremental), "run-1", 1.0);
        assert_eq!(first.disposition, Disposition::Started);
        let second = store.request_run(&request(ScanKind::Incremental), "run-2", 2.0);
        assert_eq!(second.disposition, Disposition::Coalesced);
        assert_eq!(second.run_id, "run-1");

        // A narrower scope on another root expands the queued follow-up.
        let mut sub = request(ScanKind::Incremental);
        sub.scopes = vec![ScanScope {
            root_id: "r2".to_owned(),
            scope_id: Some("r2".to_owned()),
            relative_path: "sub".to_owned(),
            root_path: Some("/r2".to_owned()),
            effective_policy: EffectivePolicy::Automatic,
            policy_revision: "rev-1".to_owned(),
            estimated_count: None,
        }];
        let expanded = store.request_run(&sub, "run-3", 3.0);
        assert_eq!(expanded.disposition, Disposition::Expanded);
        assert_eq!(expanded.run_id, "run-1");

        // A different kind against the queued follow-up conflicts.
        let conflict = store.request_run(&request(ScanKind::RescanFiles), "run-4", 4.0);
        assert_eq!(conflict.disposition, Disposition::Conflict);
        assert_eq!(conflict.conflicting_kind, Some(ScanKind::Incremental));
    }

    #[test]
    fn expanded_union_keeps_broadest_ancestor() {
        let store = MemoryScanStore::new();
        let mut narrow = request(ScanKind::Incremental);
        narrow.scopes[0].relative_path = "a/b".to_owned();
        let first = store.request_run(&narrow, "run-1", 1.0);
        assert_eq!(first.disposition, Disposition::Started);
        // Claim it so the next request lands on a fresh queued run... instead
        // assert on the union directly: requesting the ancestor expands.
        let mut wide = request(ScanKind::Incremental);
        wide.scopes[0].relative_path = "a".to_owned();
        let expanded = store.request_run(&wide, "run-2", 2.0);
        assert_eq!(expanded.disposition, Disposition::Expanded);
        let (_, scopes, _) = store.get_run(&expanded.run_id).expect("run");
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].relative_path, "a");
    }

    #[test]
    fn classify_new_changed_unchanged_and_deferred_reoffer() {
        let store = MemoryScanStore::new();
        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned())],
            Some("run-1"),
        );
        assert_eq!(verdicts["a.flac"].0, Verdict::New);

        store.commit_indexed("r1", "a.flac", 10, 5, "t-1".to_owned(), 99.0);
        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned())],
            Some("run-1"),
        );
        assert_eq!(
            verdicts["a.flac"],
            (Verdict::Unchanged, Some("t-1".to_owned()))
        );

        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 11, 6, 6e-9, "11:6".to_owned())],
            Some("run-1"),
        );
        assert_eq!(verdicts["a.flac"].0, Verdict::Changed);

        // Deferred marker re-offers unchanged stat as changed.
        store.mark_deferred("r1", "a.flac", true);
        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned())],
            Some("run-1"),
        );
        assert_eq!(verdicts["a.flac"].0, Verdict::Changed);
        // A clean read clears the marker.
        store.commit_indexed("r1", "a.flac", 10, 5, "t-1".to_owned(), 100.0);
        let verdicts = store.classify(
            "r1",
            &[("a.flac".to_owned(), 10, 5, 5e-9, "10:5".to_owned())],
            Some("run-1"),
        );
        assert_eq!(verdicts["a.flac"].0, Verdict::Unchanged);
    }

    #[test]
    fn legacy_float_promotes_and_records_skew() {
        let store = MemoryScanStore::new();
        seed_catalog_entry(
            &store,
            "r1",
            "old.flac",
            CatalogEntry {
                track_id: "t-old".to_owned(),
                revision: "stale".to_owned(),
                size_bytes: 100,
                mtime_ns: 1_700_000_000_000_000_000,
                revision_kind: RevisionKind::LegacyFloat,
                tags_read_at: Some(1_700_000_100.0),
            },
        );
        // Same size, mtime within the band, tags_read_at far away: unchanged
        // with MTIME_SKEW evidence, and the row promotes to exact.
        let verdicts = store.classify(
            "r1",
            &[(
                "old.flac".to_owned(),
                100,
                1_700_000_000_000_000_100,
                1_700_000_000.000_000_1,
                "100:1700000000000000100".to_owned(),
            )],
            Some("run-9"),
        );
        assert_eq!(verdicts["old.flac"].0, Verdict::Unchanged);
        let failures = store.failures("run-9");
        let skew = failures
            .iter()
            .find(|f| f.failure_code == failure_codes::MTIME_SKEW)
            .expect("skew row");
        assert_eq!(
            skew.failure_detail,
            "Legacy mtime drift beyond tolerance; the file clock and the read clock disagree."
        );
        let entries = store.catalog_entries("r1");
        assert_eq!(entries[0].1.revision_kind, RevisionKind::Exact);
    }

    #[test]
    fn control_pause_resume_stop_flow() {
        let store = MemoryScanStore::new();
        store.request_run(&request(ScanKind::Incremental), "run-1", 1.0);
        let claimed = store.claim_next(2.0).expect("claimed");
        assert_eq!(claimed.state, ScanState::Discovering);
        let (pausing, _) = store
            .request_control(
                "run-1",
                ScanControl::Pause,
                false,
                claimed.row_revision,
                3.0,
            )
            .expect("pause");
        assert_eq!(pausing.state, ScanState::Pausing);
        // Idempotent pause on pausing.
        let (still, _) = store
            .request_control("run-1", ScanControl::Pause, false, 999, 3.5)
            .expect("idempotent pause");
        assert_eq!(still.state, ScanState::Pausing);
        let settled = store
            .transition(
                "run-1",
                ScanState::Pausing,
                pausing.row_revision,
                ScanState::Paused,
                4.0,
                None,
            )
            .expect("settle paused");
        let (resumed, _) = store
            .request_control("run-1", ScanControl::Pause, true, settled.row_revision, 5.0)
            .expect("resume");
        assert_eq!(resumed.state, ScanState::Discovering);
        let (stopping, _) = store
            .request_control("run-1", ScanControl::Stop, false, resumed.row_revision, 6.0)
            .expect("stop");
        assert_eq!(stopping.state, ScanState::Stopping);
    }
}
