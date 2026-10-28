//! Durable downloads: SQLite state machines with crash-safe recovery.
//!
//! The durability contract (orchestrator, cleanup, the download journal
//! and task status), as small pieces wired to the real sources:
//!
//! - [`state`] task and attempt states, terminal sets, legal transitions.
//! - [`store`] the journal itself: tasks, attempts, idempotency keys and
//!   quarantine rows, all on plain SQLite, with its statements in `sql.rs`.
//! - [`journal`] the one way in: reads on the read pool, writes on the
//!   writer lane.
//! - [`plan`] what a task fetches beyond its row's columns (`plan_json`).
//! - [`watchdog`] stall/queued/deadline evaluation plus retry backoff.
//! - [`recovery`] startup classification and orchestrator failover.
//! - [`quarantine`] release blocklist with TTL and prune-on-write.
//! - [`orphans`] fail-closed orphan reconcile.
//! - [`sources`] the per-source fetch seam (slskd/usenet live elsewhere),
//!   with [`source_tag`] naming each source.
//! - [`landing_rows`] per-landing import decisions.
//! - [`queue`] the queue view and its actions, over [`queue_rows`].
//! - [`reasons`] why a task sits where it does, in plain words.
//! - [`upgrades`] the quality-upgrade worklist and asks.
//! - [`worker`] the loop that drives every task: startup recovery, then
//!   enqueue, poll, retry, cleanup and the orphan sweep on one cadence.
//! - [`http`] the queue routes and the upgrade routes. Held files are in
//!   [`crate::held`].

pub mod http;
pub mod journal;
pub mod landing_rows;
pub mod orphans;
pub mod plan;
pub mod quarantine;
pub mod queue;
pub mod queue_rows;
pub mod reasons;
pub mod recovery;
pub mod source_tag;
pub mod sources;
pub(crate) mod sql;
pub mod state;
pub mod store;
pub mod upgrades;
pub mod watchdog;
pub mod worker;

#[cfg(any(test, feature = "test-support"))]
pub use http::downloads_router;
pub use http::{DownloadsRoutes, ReimportResponse, routes, search_routes};
pub use orphans::{OrphanDecision, OrphanEvidence, OrphanPolicy, evaluate_orphan, job_name_parts};
pub use plan::{ExpectedTrack, PlannedAlbum, ReplaceVersion, TaskPlan, TrackAlbum, TrackPosition};
pub use quarantine::{QUARANTINE_TTL_SECONDS, QuarantineDir, canonical_soulseek_identity};
pub use recovery::{RetrySpawn, StartupAction, classify_startup, plan_retry};
pub use source_tag::{SourceTag, UnknownSource};
pub use sources::{
    DownloadSource, Materialization, OrphanOwnership, SourceError, SourceHandle, TransferProgress,
};
pub use state::{AttemptState, TaskStatus, can_transition, is_terminal};
pub use store::{DownloadStore, StoreError, TaskDetails};
pub use watchdog::{PollSample, RetryPolicy, Watchdog, WatchdogConfig, WatchdogOutcome};
