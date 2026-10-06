//! Library scan: roots, discovery, scheduling, and supervision.
//!
//! Owns the library root registry, the filesystem walk, scan runs, the
//! rolling scheduler, the filesystem watcher, worker supervision, and the
//! library-revision poller. Ported from v2's native library services
//! (inventory scanner, scan coordinator, scheduler, supervisor, filesystem
//! watcher and coordinator, revision poller, indexer, reconciler, file
//! revision, scan events) plus the scan half of its library store and work
//! models.
//!
//! ## Boundary
//!
//! * Tag parsing and provider identification live in `library::tags` and
//!   `library::identify`. Scan calls them only through [`seams::TagReader`]
//!   and [`seams::IdentifyQueue`]; the null implementations exist for
//!   tests only.
//! * The stream gateway resolves bare local keys under the primary root of
//!   [`roots::StreamRootSeam`] (via `Gateway::with_library_roots`). Per-root
//!   `resolve_key` is unused because playback keys carry no root id; see
//!   its docs.
//! * Settings storage owns inclusion rules under excluded roots; the
//!   scheduler resolves [`scheduler::InclusionRule`] values carried as
//!   parameters.
//!
//! ## Purity
//!
//! Scan and identify never write music files. The walk stats, the tag
//! seam reads, and nothing in scan opens a library file for writing.
//! `server/tests/it/library_scan.rs` pins zero file writes across full
//! runs.
//!
//! v2 quirks and bug fixes are cited where the code handles them: degraded
//! and superseded walks, wedged probes and the detach cap, heartbeats,
//! NFC twins, non-UTF-8 names, non-regular files, deferred tag reads,
//! legacy float mtimes, skip-and-report on unreachable roots (GH-296),
//! timeout retries (GH-444), and symlinks, which are never followed.

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

#[cfg(any(test, feature = "test-support"))]
pub use coordinator::StaticResolver;
pub use coordinator::{
    IndexCounts, LibraryScanCoordinator, ResolverSource, ScanEvent, ScanEventPublisher,
    ScanRequestError, SharedResolver,
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
pub use revision::{exact_stat_revision, legacy_mtime_eps_seconds, mtime_ns_from_metadata};
pub use roots::{
    LibraryRoot, PolicyResolver, RootRegistry, RootSeamError, StreamRootSeam, fingerprint_roots,
};
pub use scheduler::{InclusionRule, ScheduleSettings, scheduled_scopes, seconds_until_due};
#[cfg(any(test, feature = "test-support"))]
pub use seams::{ArmableDeferTagReader, NullIdentifyQueue, NullTagReader};
pub use seams::{Checkpoint, IdentifyQueue, ScannedTags, TagReadError, TagReader};
pub use sqlite_store::SqliteScanStore;
pub use store::{
    CatalogEntry, CatalogStore, ClassifyInput, CommitIndexedItem, InventoryPage, InventoryStore,
    RevisionKind, RunStore, ScanStore, ScanStoreError,
};
pub use supervisor::SupervisorInputs;
pub use walk::{InventoryScanner, inventory_key, is_audio_file, relativize, text_safe_posix};
pub use watcher::{
    DirtyScopes, Snapshot, WatcherAction, WatcherInputs, WatcherSettings, WatcherState,
    WorkWakeups, clear_pending as watcher_clear_pending, poll_once as watcher_poll_once,
    watcher_request,
};
