//! Library scan: roots, discovery, scheduling, and supervision.
//!
//! Stage-8 scan slice. Owns the library root registry (replacing the
//! stage-6 provisional `<root>/music`), the filesystem walk, scan runs,
//! the rolling scheduler, the filesystem watcher, worker supervision, and
//! the library-revision poller. v2 reference: `backend/services/native/`
//! (`library_inventory_scanner`, `library_scan_coordinator`,
//! `library_scan_scheduler`, `library_scan_supervisor`,
//! `library_filesystem_watcher`, `library_revision_poller`,
//! `library_filesystem_coordinator`, `library_indexer`,
//! `library_reconciler`, `file_revision`, `library_scan_events`) plus the
//! scan half of `NativeLibraryStore` and `models/library_work.py`.
//!
//! ## Boundary
//!
//! * Tag parsing and provider identification belong to sibling slices.
//!   This slice calls them only through [`seams::TagReader`] and
//!   [`seams::IdentifyQueue`]; the null implementations here exist for
//!   tests, not production.
//! * The stream gateway keeps its own files. [`roots::StreamRootSeam`] is
//!   the root-resolution seam the integrator wires in; see its docs.
//! * Settings storage owns inclusion rules under excluded roots; the
//!   scheduler resolves [`scheduler::InclusionRule`] values carried as
//!   parameters until that slice lands.
//!
//! ## Purity
//!
//! Scan and identify never write music files. The walk stats, the tag
//! seam reads, and nothing in this slice opens a library file for
//! writing. `server/tests/it/library_scan.rs` pins zero file writes across
//! full runs.
//!
//! ## Quirk citations
//!
//! F-029 (`revision`), F-021 (`walk::text_safe_posix`), F-020/NFC twins
//! (`walk`), F-022 degraded walks (`walk`, `coordinator::reconcile`),
//! F-023 probes (`walk`), F-024 detach cap (`walk`), F-025 heartbeat
//! (`walk`), F-027 event throttle (`coordinator`), F-030 supersede
//! re-walk (`walk`), F-SCAN-02 cover rule (`store`), F-INDEXREC-01 union
//! (`store`), F-INDEXREC-02 fence release (`coordinator`),
//! F-INDEXREC-06 control exits (`walk`, `coordinator`), F-12 deferred
//! re-offer (`store`, `coordinator`), F-13 non-regular skip (`walk`),
//! F-15/4.12 legacy band (`store`), F-16 NFC classify (`store`), F-32
//! failure details (`walk`), GH-296 skip-and-report (`walk`),
//! GH-444 timeout retry (`walk`), S-01 Hooks A/B/C (`supervisor`,
//! `watcher`), S-05 tick dispositions (`scheduler`), R-02 settle bound
//! (`coordinator`), R-05 single-start (`watcher`, `supervisor` via
//! single-loop construction), E11 symlinks (`walk`).

pub mod coordinator;
pub mod fs;
pub mod models;
pub mod poller;
pub mod pool;
pub mod revision;
pub mod roots;
pub mod scheduler;
pub mod seams;
pub mod sqlite_store;
pub mod store;
pub mod supervisor;
pub mod walk;
pub mod watcher;

pub use coordinator::{
    IndexCounts, LibraryScanCoordinator, ResolverSource, ScanEvent, ScanEventPublisher,
    ScanRequestError, SharedResolver, StaticResolver,
};
pub use fs::{FsCoordinator, ReadGuard, WriteGuard, is_management_artifact};
pub use models::{
    Counters, Disposition, EffectivePolicy, RequestedControl, ScanControl, ScanFailureRecord,
    ScanInventoryItem, ScanKind, ScanPhase, ScanRequest, ScanRequestResult, ScanRun, ScanScope,
    ScanState, ScanTrigger, ScopeDiscoveryState, Verdict, counter_names, failure_codes,
    scope_covers_path,
};
pub use poller::{
    PollerState, RevisionPublisher, RevisionSource, poll_once as poll_revisions_once,
    revision_event_id,
};
pub use pool::BlockingPool;
pub use revision::{
    exact_stat_revision, legacy_mtime_eps_seconds, mtime_ns_from_metadata, revision_from_metadata,
};
pub use roots::{
    LibraryRoot, PolicyResolver, RootRegistry, RootSeamError, StreamRootSeam, fingerprint_roots,
};
pub use scheduler::{InclusionRule, ScheduleSettings, scheduled_scopes, seconds_until_due};
pub use seams::{
    AllowAll, ArmableDeferTagReader, Checkpoint, DeferOnceTagReader, FnCheckpoint, IdentifyQueue,
    NullIdentifyQueue, NullTagReader, ScannedTags, TagReadError, TagReader,
};
pub use sqlite_store::SqliteScanStore;
pub use store::{
    CatalogEntry, ClassifyInput, CommitIndexedItem, InventoryPage, MemoryScanStore, RevisionKind,
    ScanStore, ScanStoreError,
};
pub use supervisor::SupervisorInputs;
pub use walk::{InventoryScanner, inventory_key, is_audio_file, relativize, text_safe_posix};
pub use watcher::{
    DirtyScopes, Snapshot, WatcherAction, WatcherInputs, WatcherSettings, WatcherState,
    WorkWakeups, clear_pending as watcher_clear_pending, poll_once as watcher_poll_once,
    watcher_request,
};
