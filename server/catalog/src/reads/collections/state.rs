//! Collections state: the SQLite stores plus the seams into acquisition.

use std::sync::Arc;

use super::db::CollectionsDb;
use super::store::Stores;
use crate::reads::library::player::SqlitePlayerCatalog;
use crate::reads::library::sqlite::LibraryDb;

/// Auto-download state of one follow, as the routes show it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AutoDownloadState {
    /// Off, or on with no verdict on file.
    None,
    /// Asked for, awaiting an admin verdict.
    Pending,
    /// On: approved, or the follower's role approves itself.
    Approved,
    /// An admin turned the ask down.
    Rejected,
    /// An admin withdrew an earlier grant.
    Revoked,
}

impl AutoDownloadState {
    /// Parse a stored approval state; unknown text reads as no verdict.
    pub fn from_approval(state: &str) -> Self {
        match state {
            "pending" => Self::Pending,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "revoked" => Self::Revoked,
            _ => Self::None,
        }
    }
}

/// One pending auto-download approval, plain data for the acquire bridge.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Artist MBID.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// One pending bulk batch, plain data for the acquire bridge.
#[derive(Debug, Clone)]
pub struct PendingBatch {
    /// Batch id.
    pub batch_id: String,
    /// Requesting user id.
    pub user_id: String,
    /// Requesting display name.
    pub user_name: String,
    /// Covered `(artist MBID, artist name)` pairs.
    pub artists: Vec<(String, String)>,
    /// Epoch seconds when asked.
    pub requested_at: u64,
}

/// Pending approvals owned by `acquire::requests`. Wiring connects this so
/// the approval reads share one store with the approval mutations; without
/// it the reads list follows still waiting for a verdict.
pub trait PendingApprovalsSource: Send + Sync {
    /// Pending approvals, oldest first.
    fn pending_approvals(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<Vec<PendingApproval>, String>>;
    /// Pending batches, oldest first.
    fn pending_batches(
        &self,
    ) -> futures_util::future::BoxFuture<'_, Result<Vec<PendingBatch>, String>>;
}

/// Seed sink into the requests approval store. Wiring connects this so
/// follow toggles that land Pending also file the approval the admin
/// mutations decide.
pub trait ApprovalSeedSink: Send + Sync {
    /// File one pending approval.
    fn seed_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
        artist_name: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
    /// Withdraw a pending ask (the user toggled back off).
    fn withdraw_approval<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> futures_util::future::BoxFuture<'a, Result<(), String>>;
}

/// All collections state. Handlers receive this via the `State` extractor
/// and hand it to the service.
#[derive(Clone)]
pub struct CollectionsState {
    /// The database the stores use (also lent to the compat queues).
    pub db: CollectionsDb,
    /// The SQLite stores.
    pub stores: Stores,
    /// The library's live artists, albums and tracks, which a rating
    /// names: the same id resolution and existence check Subsonic's
    /// `setRating` goes through, over the same database.
    pub items: SqlitePlayerCatalog,
    /// Acquire-owned pending approvals, when wired.
    pub acquire_approvals: Option<Arc<dyn PendingApprovalsSource>>,
    /// Seed sink into the acquire approval store, when wired.
    pub approval_seeds: Option<Arc<dyn ApprovalSeedSink>>,
}

impl CollectionsState {
    /// State over one database, with the acquisition seams unwired.
    pub fn new(db: CollectionsDb) -> Self {
        let stores = Stores::new(&db);
        Self::over(db, stores, None, None)
    }

    /// The production state: the stores acquisition was built with, and
    /// its approval list and seed sink.
    pub fn wired(
        db: CollectionsDb,
        stores: Stores,
        approvals: Arc<dyn PendingApprovalsSource>,
        seeds: Arc<dyn ApprovalSeedSink>,
    ) -> Self {
        Self::over(db, stores, Some(approvals), Some(seeds))
    }

    fn over(
        db: CollectionsDb,
        stores: Stores,
        acquire_approvals: Option<Arc<dyn PendingApprovalsSource>>,
        approval_seeds: Option<Arc<dyn ApprovalSeedSink>>,
    ) -> Self {
        let library = db
            .pool()
            .map_or_else(|_| LibraryDb::unwired(), LibraryDb::new);
        Self {
            stores,
            items: SqlitePlayerCatalog::new(&library),
            db,
            acquire_approvals,
            approval_seeds,
        }
    }

    /// State with no database: every route fails closed. Bundles that never
    /// serve collections use this.
    pub fn unwired() -> Self {
        Self::new(CollectionsDb::unwired())
    }
}
