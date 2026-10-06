//! Durable downloads: SQLite state machines with crash-safe recovery.
//!
//! Ports the v2 acquisition durability contract (the download
//! orchestrator, acquisition cleanup, recycle bin, download store journal,
//! and acquisition status) as small pieces wired to the real sources:
//!
//! - [`state`] task and attempt states, terminal sets, legal transitions.
//! - [`store`] the journal itself: tasks, attempts, idempotency keys,
//!   quarantine rows, held-import retry, all on plain SQLite.
//! - [`manifest`] the per-task staging manifest (`manifest.json`).
//! - [`watchdog`] stall/queued/deadline evaluation plus retry backoff.
//! - [`recovery`] startup classification and orchestrator failover.
//! - [`quarantine`] release blocklist with TTL and prune-on-write.
//! - [`orphans`] fail-closed orphan reconcile and recycle-bin prune.
//! - [`sources`] the per-source fetch seam (slskd/usenet live elsewhere).
//! - [`landing_rows`] held files and per-landing import decisions.
//! - [`queue`] the queue view and its actions, over [`queue_rows`].
//! - [`reasons`] why a task sits where it does, in plain words.
//! - [`http`] the queue routes.
//!
//! Quirk citations name the v2 behavior each port preserves, so a reader
//! can diff against the Python without guessing.

pub mod http;
pub mod landing_rows;
pub mod manifest;
pub mod orphans;
pub mod quarantine;
pub mod queue;
pub mod queue_rows;
pub mod reasons;
pub mod recovery;
pub mod sources;
pub mod state;
pub mod store;
pub mod watchdog;

#[cfg(any(test, feature = "test-support"))]
pub use http::downloads_router;
pub use http::{ReimportResponse, downloads_core_routes};
pub use manifest::{
    DownloadManifest, ExpectedFile, ExpectedTrack, ManifestCodec, RecordedReason, TaskHandle,
    TrackAlbumContext, TrackPosition,
};
pub use orphans::{
    OrphanDecision, OrphanEvidence, OrphanPolicy, RecycleBin, evaluate_orphan, job_name_parts,
};
pub use quarantine::{QUARANTINE_TTL_SECONDS, QuarantineDir, canonical_soulseek_identity};
pub use recovery::{RetrySpawn, StartupAction, classify_startup, plan_retry};
pub use sources::{
    DownloadSource, Materialization, OrphanOwnership, SourceError, SourceHandle, TransferProgress,
};
pub use state::{AttemptState, TaskStatus, can_transition, is_terminal};
#[cfg(any(test, feature = "test-support"))]
pub use store::apply_test_schema;
pub use store::{DownloadStore, StoreError, TaskDetails};
pub use watchdog::{PollSample, RetryPolicy, Watchdog, WatchdogConfig, WatchdogOutcome};
