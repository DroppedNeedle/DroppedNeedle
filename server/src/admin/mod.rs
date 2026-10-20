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

use droppedneedle_auth::users::UsersDeps;
use droppedneedle_kernel::db::{BackupService, CheckpointService, Db};
use droppedneedle_kernel::mount::{Routes, routes};
use droppedneedle_providers::{ProviderCacheStore, Providers};

pub use error::AdminError;
pub use models::{
    BackupListResponse, BackupRunResponse, BackupView, CacheClearBody, CacheClearResponse,
    CacheStatsResponse, CacheSyncCancelResponse, CheckpointView, JobView, PrecacheRunResponse,
    ProviderCallsView, ProviderLimiterView, ProviderStatsResponse, QueueDepthView,
    QueueStatsResponse, QuotaOverrideBody, QuotaOverrideView, QuotaResponse, RestoreCheck,
    RestoreReport,
};
pub use system::{ServiceHealthItem, SystemHealthResponse};

/// Database handles the admin routes read and write through.
#[derive(Clone)]
pub struct AdminDb {
    /// The database: admin reads (quota rows, task sizes) and quota
    /// override writes.
    pub db: Db,
}

impl AdminDb {
    /// The database the admin routes use.
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

/// Everything the admin router needs, built once at boot.
#[derive(Clone)]
pub struct AdminSetup {
    /// Account rows for quota identity.
    pub users: UsersDeps,
    /// Live quota ledger (shared with acquisition enforcement).
    pub quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
    /// Provider answer cache (shared with the provider clients).
    pub cache: ProviderCacheStore,
    /// Provider limiters and slot lanes for provider stats.
    pub providers: Arc<Providers>,
    /// Database handles. `None` on unwired test states.
    pub db: Option<AdminDb>,
    /// Backup service. `None` on unwired test states.
    pub backups: Option<Arc<BackupService>>,
    /// Checkpoint service for health observability. `None` until wired.
    pub checkpoint: Option<CheckpointService>,
    /// Precache trigger on the loop supervisor. `None` on unwired
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
        cache: ProviderCacheStore,
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
}

/// The admin routes and the system health read. Each handler also takes
/// the caller it admits.
pub fn routes() -> Routes<AdminSetup> {
    Routes::new()
        .user(routes!(system::system_health))
        .user(routes!(handlers::cache_sync_status))
        .curator(routes!(handlers::cancel_cache_sync))
        .admin(routes!(handlers::list_backups, handlers::run_backup))
        .admin(routes!(handlers::restore_report))
        .admin(routes!(handlers::cache_stats))
        .admin(routes!(handlers::clear_cache))
        .admin(routes!(handlers::queue_stats))
        .admin(routes!(handlers::provider_stats))
        .admin(routes!(handlers::get_quota, handlers::set_quota))
        .admin(routes!(handlers::run_precache))
}
