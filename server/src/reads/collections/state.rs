//! Collections state: the SQLite stores plus the seams into acquisition.

use std::sync::Arc;

use super::db::CollectionsDb;
use super::store::Stores;

/// Auto-download state of one follow, as the routes show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoDownloadState {
    /// Auto-download off.
    Off,
    /// Asked for, awaiting an admin verdict.
    Pending,
    /// On: approved, or the follower's role approves itself.
    Active,
}

impl AutoDownloadState {
    /// Wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Pending => "pending",
            Self::Active => "active",
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
    /// Acquire-owned pending approvals, when wired.
    pub acquire_approvals: Option<Arc<dyn PendingApprovalsSource>>,
    /// Seed sink into the acquire approval store, when wired.
    pub approval_seeds: Option<Arc<dyn ApprovalSeedSink>>,
}

impl CollectionsState {
    /// State over one database, with the acquisition seams unwired.
    pub fn new(db: CollectionsDb) -> Self {
        Self {
            stores: Stores::new(&db),
            db,
            acquire_approvals: None,
            approval_seeds: None,
        }
    }

    /// State with no database: every route fails closed. Bundles that never
    /// serve collections use this.
    pub fn unwired() -> Self {
        Self::new(CollectionsDb::unwired())
    }
}
