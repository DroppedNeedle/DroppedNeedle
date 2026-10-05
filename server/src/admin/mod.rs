//! Admin UX: backups, caches, queues, providers, quotas.
//!
//! Every route here is admin-only. [`require_admin`] re-reads the caller's
//! user row on each request (role changes land immediately) and answers 401
//! for missing or stale sessions, 403 for signed-in non-admins: the same
//! posture as the users routes, minus their extractor (this router
//! carries [`AdminSetup`] state, not `UsersDeps`).
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

use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use sqlx::SqlitePool;

use crate::{
    auth::{
        session::middleware::CurrentSession,
        users::{UsersDeps, roles::Role},
    },
    db::{BackupService, CheckpointService, DurableWorkWakeups, WriteLane},
    providers::{InMemoryProviderCache, Providers},
};

pub use error::AdminError;
pub use models::{
    BackupListResponse, BackupRunResponse, BackupView, CacheClearBody, CacheClearResponse,
    CacheStatsResponse, CheckpointView, JobView, PrecacheRunResponse, ProviderLimiterView,
    ProviderStatsResponse, QueueStatsResponse, QuotaOverrideBody, QuotaOverrideView, QuotaResponse,
    RestoreCheck, RestoreReport, SlotView, WakeupChannelView,
};

/// Database handles the admin routes read and write through.
#[derive(Clone)]
pub struct AdminDb {
    /// Pool for admin reads (wakeup rows, quota rows, task sizes).
    pub pool: SqlitePool,
    /// Writer lane for quota override writes.
    pub lane: WriteLane,
    /// Durable fabric for the job-registry half of queue stats.
    pub wakeups: DurableWorkWakeups,
}

impl AdminDb {
    /// Bundle the pool with its lane; the wakeup handle derives from the pool.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self {
            wakeups: DurableWorkWakeups::new(pool.clone()),
            pool,
            lane,
        }
    }
}

/// Everything the admin router needs, built once at boot.
#[derive(Clone)]
pub struct AdminSetup {
    /// Account rows for gating and quota identity.
    pub users: UsersDeps,
    /// Live quota ledger (shared with acquisition enforcement).
    pub quota: Arc<crate::acquire::requests::quota::QuotaLedger>,
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
    pub precache: Option<crate::jobs::wiring::PrecacheTrigger>,
}

impl AdminSetup {
    /// Bundle the always-present handles. Optional backends arrive via the
    /// `with_*` builders; production wires all three.
    pub fn new(
        users: UsersDeps,
        quota: Arc<crate::acquire::requests::quota::QuotaLedger>,
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
    pub fn with_precache(mut self, precache: crate::jobs::wiring::PrecacheTrigger) -> Self {
        self.precache = Some(precache);
        self
    }

    /// Test bundle: always-present handles only, backends unwired. Suites
    /// that need a database attach it with the `with_*` builders.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_tests(
        users: UsersDeps,
        quota: Arc<crate::acquire::requests::quota::QuotaLedger>,
        cache: Arc<InMemoryProviderCache>,
        providers: Arc<Providers>,
    ) -> Self {
        Self::new(users, quota, cache, providers)
    }

    /// Mount the admin routes. Layers are applied by `create_app`, plus the
    /// [`require_admin`] gate below; handlers take `State<AdminSetup>`.
    pub fn gated_router(&self) -> Router {
        Router::new()
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
            .layer(axum::middleware::from_fn_with_state(
                self.clone(),
                require_admin,
            ))
            .with_state(self.clone())
    }
}

/// Admin gate: 401 when no session is stashed or the account is gone, 403
/// when the account is not an admin. The role rereads the user row every
/// request, so promotions and demotions land on the next call.
async fn require_admin(State(admin): State<AdminSetup>, request: Request, next: Next) -> Response {
    let missing = || AdminError::Unauthorized {
        message: "Authentication required".to_owned(),
    };
    let Some(session) = request.extensions().get::<CurrentSession>().cloned() else {
        return missing().into_response();
    };
    let user = match admin.users.users.get_by_id(&session.user_id).await {
        Ok(Some(user)) => user,
        Ok(None) => return missing().into_response(),
        Err(error) => {
            return AdminError::internal(&format_args!("admin gate lookup failed: {error}"))
                .into_response();
        }
    };
    if user.role != Role::Admin {
        return AdminError::Forbidden {
            message: "Admin role required".to_owned(),
        }
        .into_response();
    }
    next.run(request).await
}
