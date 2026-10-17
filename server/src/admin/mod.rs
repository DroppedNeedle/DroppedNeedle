//! Admin UX: backups, caches, queues, providers, quotas, plus the system
//! health read every user's header polls.
//!
//! Every `/admin` handler takes [`Admin`](droppedneedle_auth::Admin): 401
//! without a session, 403 for a signed-in non-admin, judged on the role
//! the session gate read for this request. `/system/health` and the
//! precache status take any signed-in user, as in v2; stopping a precache
//! takes a curator.
//!
//! Optional backends ([`AdminDb`], backups, checkpoint, precache) are
//! `None` on unwired test states; handlers answer 503 there instead of
//! guessing. Production always wires them.

pub mod backups;
pub mod cache;
pub mod error;
pub mod handlers;
pub mod models;
pub mod queues;
pub mod quota;
pub mod system;

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};

use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::db::{BackupService, CheckpointService, Db, DurableWorkWakeups};
use droppedneedle_providers::{InMemoryProviderCache, Providers};

pub use error::AdminError;
pub use models::{
    BackupListResponse, BackupRunResponse, BackupView, CacheClearBody, CacheClearResponse,
    CacheStatsResponse, CacheSyncCancelResponse, CheckpointView, JobView, PrecacheRunResponse,
    ProviderLimiterView, ProviderStatsResponse, QueueStatsResponse, QuotaOverrideBody,
    QuotaOverrideView, QuotaResponse, RestoreCheck, RestoreReport, SlotView, WakeupChannelView,
};
pub use system::{ServiceHealthItem, SystemHealthResponse};

/// Database handles the admin routes read and write through.
#[derive(Clone)]
pub struct AdminDb {
    /// The database: admin reads (wakeup rows, quota rows, task sizes)
    /// and quota override writes.
    pub db: Db,
    /// Durable fabric for the job-registry half of queue stats.
    pub wakeups: DurableWorkWakeups,
}

impl AdminDb {
    /// Bundle the database with the wakeup handle over its reader pool.
    pub fn new(db: Db) -> Self {
        Self {
            wakeups: DurableWorkWakeups::new(db.sqlx().clone()),
            db,
        }
    }
}

/// Everything the admin router needs, built once at boot.
#[derive(Clone)]
pub struct AdminSetup {
    /// Account rows for quota identity.
    pub users: UsersDeps,
    /// Live quota ledger (shared with acquisition enforcement).
    pub quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
    /// Provider byte cache (shared with the provider clients).
    pub cache: Arc<InMemoryProviderCache>,
    /// Provider limiters and slot lanes for provider stats.
    pub providers: Arc<Providers>,
    /// Database handles. `None` on unwired test states.
    pub db: Option<AdminDb>,
    /// Backup service. `None` on unwired test states.
    pub backups: Option<Arc<BackupService>>,
    /// Checkpoint service for health observability. `None` until wired.
    pub checkpoint: Option<CheckpointService>,
    /// Precache trigger over the shared jobs registry. `None` on unwired
    /// test states.
    pub precache: Option<crate::bootstrap::jobs::PrecacheTrigger>,
    /// Cover and artist image cache, for its stats and clears. `None` on
    /// states without one.
    pub covers: Option<droppedneedle_catalog::reads::platform::artwork::cache::ArtworkCache>,
}

impl AdminSetup {
    /// Bundle the always-present handles. Optional backends arrive via the
    /// `with_*` builders; production wires all three.
    pub fn new(
        users: UsersDeps,
        quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
        cache: Arc<InMemoryProviderCache>,
        providers: Arc<Providers>,
    ) -> Self {
        Self {
            users,
            quota,
            cache,
            providers,
            db: None,
            backups: None,
            checkpoint: None,
            precache: None,
            covers: None,
        }
    }

    /// Attach the database handles.
    #[must_use]
    pub fn with_db(mut self, db: AdminDb) -> Self {
        self.db = Some(db);
        self
    }

    /// Attach the backup service.
    #[must_use]
    pub fn with_backups(mut self, backups: BackupService) -> Self {
        self.backups = Some(Arc::new(backups));
        self
    }

    /// Attach the checkpoint service.
    #[must_use]
    pub fn with_checkpoint(mut self, checkpoint: CheckpointService) -> Self {
        self.checkpoint = Some(checkpoint);
        self
    }

    /// Attach the precache trigger.
    #[must_use]
    pub fn with_precache(mut self, precache: crate::bootstrap::jobs::PrecacheTrigger) -> Self {
        self.precache = Some(precache);
        self
    }

    /// Attach the cover and artist image cache.
    #[must_use]
    pub fn with_covers(
        mut self,
        covers: droppedneedle_catalog::reads::platform::artwork::cache::ArtworkCache,
    ) -> Self {
        self.covers = Some(covers);
        self
    }

    /// Test bundle: always-present handles only, backends unwired. Suites
    /// that need a database attach it with the `with_*` builders.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
        cache: Arc<InMemoryProviderCache>,
        providers: Arc<Providers>,
    ) -> Self {
        Self::new(users, quota, cache, providers)
    }

    /// Mount the admin routes and the system health read. Layers are
    /// applied by `create_app`; each handler takes the caller it admits.
    pub fn gated_router(&self) -> Router {
        Router::new()
            .route("/system/health", get(system::system_health))
            .route("/cache/sync/status", get(handlers::cache_sync_status))
            .route("/cache/sync/cancel", post(handlers::cancel_cache_sync))
            .route(
                "/admin/backups",
                get(handlers::list_backups).post(handlers::run_backup),
            )
            .route(
                "/admin/backups/{name}/restore-report",
                get(handlers::restore_report),
            )
            .route("/admin/cache/stats", get(handlers::cache_stats))
            .route("/admin/cache/clear", post(handlers::clear_cache))
            .route("/admin/queue-stats", get(handlers::queue_stats))
            .route("/admin/provider-stats", get(handlers::provider_stats))
            .route(
                "/admin/users/{id}/quota",
                get(handlers::get_quota).put(handlers::set_quota),
            )
            .route("/admin/precache/run", post(handlers::run_precache))
            .with_state(self.clone())
    }
}
