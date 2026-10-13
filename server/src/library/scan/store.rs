//! Durable scan state: the types [`super::SqliteScanStore`] takes and
//! returns, and its error.

use super::models::{EffectivePolicy, ScanInventoryItem};
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

/// One path offered to [`super::SqliteScanStore::classify`]: key, size, mtime_ns,
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
