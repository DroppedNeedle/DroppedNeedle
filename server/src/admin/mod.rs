//! Admin UX: backups, caches, queues, providers, quotas, plus the system
//! health read every user's header polls.
//!
//! Every `/admin` handler takes [`Admin`](droppedneedle_auth::Admin): 401
//! without a session, 403 for a signed-in non-admin, judged on the role
//! the session gate read for this request. `/system/health` and the
//! precache status take any signed-in user, as in v2; stopping a precache
//! takes a curator.

pub mod backups;
pub mod cache;
pub mod error;
pub mod handlers;
pub mod models;
pub mod queues;
pub mod quota;
pub(crate) mod sql;
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
    /// Database handles.
    pub db: AdminDb,
    /// Backup service: the runtime's, which boot also uses.
    pub backups: Arc<BackupService>,
    /// Checkpoint service for health observability.
    pub checkpoint: CheckpointService,
    /// Precache trigger on the loop supervisor.
    pub precache: crate::bootstrap::jobs::PrecacheTrigger,
    /// Cover and artist image cache, for its stats and clears.
    pub covers: droppedneedle_catalog::artwork::cache::ArtworkCache,
}

impl AdminSetup {
    /// Bundle every handle the admin routes use. The provider answer
    /// cache is the one `providers` holds.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        users: UsersDeps,
        quota: Arc<droppedneedle_acquire::requests::quota::QuotaLedger>,
        providers: Arc<Providers>,
        db: Db,
        backups: Arc<BackupService>,
        checkpoint: CheckpointService,
        precache: crate::bootstrap::jobs::PrecacheTrigger,
        covers: droppedneedle_catalog::artwork::cache::ArtworkCache,
    ) -> Self {
        Self {
            users,
            quota,
            cache: providers.cache.clone(),
            providers,
            db: AdminDb::new(db),
            backups,
            checkpoint,
            precache,
            covers,
        }
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
