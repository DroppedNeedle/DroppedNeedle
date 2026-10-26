//! Durable scan state: the types [`super::SqliteScanStore`] takes and
//! returns, and its error.

use droppedneedle_kernel::db::DbError;

use super::models::{Counter, EffectivePolicy, ScanInventoryItem};
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
/// the range marks skipped. `counters` land with the rows, each delta
/// beside the inventory row it counts, so a window split by path carries
/// only its own rows' counts.
#[derive(Debug, Clone, Default)]
pub struct IndexWindow {
    pub run_id: String,
    pub after: Option<(String, String)>,
    pub through: (String, String),
    pub items: Vec<CommitIndexedItem>,
    pub failed: Vec<(String, String, &'static str)>,
    /// Files whose catalog write was given up (root, path, why), reported
    /// back as refused rows are.
    pub refused: Vec<(String, String, String)>,
    pub counters: Vec<RowCounter>,
    /// Unchanged files whose edition hints were never stored (files
    /// carried over from v2): only those rows are written for them.
    pub edition_fills: Vec<EditionFill>,
    pub now: f64,
}

/// The edition hints read from an unchanged file's tags, for a catalog
/// row that holds none.
#[derive(Debug, Clone)]
pub struct EditionFill {
    pub root_id: String,
    pub relative_path: String,
    pub track_id: String,
    /// The file's stat revision when it was read: a row that has moved on
    /// since is left to the scan that reads it in full.
    pub stat_revision: String,
    pub edition: crate::tags::read::EditionTags,
    /// The file's disc number, which a disc total must not fall below.
    pub disc_number: u32,
    pub effective_policy: EffectivePolicy,
}

/// One counter delta and the inventory row (root, path) it counts.
pub type RowCounter = ((String, String), Counter, i64);

/// What one window commit landed.
#[derive(Debug, Clone, Default)]
pub struct WindowOutcome {
    /// Catalog rows written.
    pub committed: usize,
    /// Identify jobs queued for the albums the window touched.
    pub enqueued: usize,
    /// Files that could not be written: root, path, store error. Each is
    /// recorded as `CATALOG_COMMIT_FAILED` in the window's own transaction.
    pub failed: Vec<(String, String, String)>,
}

/// Store errors. Spelling mirrors the v2 exception mapping: stale reads
/// fail loudly, missing runs are not-found. The database's own answers
/// that a caller branches on keep their own variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScanStoreError {
    NotFound {
        run_id: String,
    },
    StaleRevision {
        message: String,
    },
    InvalidControl {
        message: String,
    },
    /// The database was busy: the file lock at `BEGIN`, a full queue, or a
    /// request read that waited out its turn. Nothing landed.
    Busy {
        operation: String,
    },
    /// The database is shutting down. Nothing landed.
    LaneClosed,
    /// A write ran past its time budget and rolled back.
    TxTimeout {
        operation: String,
    },
    /// The run's stop flag ended a long read while it waited.
    ReadCancelled {
        operation: String,
    },
    /// The run was asked to pause or stop: a chunked write stopped at a
    /// chunk boundary, or a busy write stopped waiting. What committed
    /// before stays.
    Stopped,
    Internal {
        message: String,
    },
}

impl From<DbError> for ScanStoreError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { operation } => Self::Busy { operation },
            DbError::LaneClosed => Self::LaneClosed,
            DbError::TxTimeout { operation, .. } => Self::TxTimeout { operation },
            DbError::ReadCancelled { operation } => Self::ReadCancelled { operation },
            other => Self::Internal {
                message: format!("scan store failed: {other}"),
            },
        }
    }
}

impl From<rusqlite::Error> for ScanStoreError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::from(error).into()
    }
}

impl std::fmt::Display for ScanStoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScanStoreError::NotFound { run_id } => write!(f, "scan run not found: {run_id}"),
            ScanStoreError::StaleRevision { message } => write!(f, "{message}"),
            ScanStoreError::InvalidControl { message } => write!(f, "{message}"),
            ScanStoreError::Busy { operation } => {
                write!(f, "the database is busy during {operation}")
            }
            ScanStoreError::LaneClosed => write!(f, "the database is shutting down"),
            ScanStoreError::TxTimeout { operation } => {
                write!(f, "write {operation} ran past its time budget")
            }
            ScanStoreError::ReadCancelled { operation } => {
                write!(f, "read {operation} was cancelled")
            }
            ScanStoreError::Stopped => write!(f, "the scan was asked to stop"),
            ScanStoreError::Internal { message } => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ScanStoreError {}
