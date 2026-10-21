//! Collections state: the SQLite stores.

use super::db::CollectionsDb;
use super::store::Stores;
use crate::reads::library::player::SqlitePlayerCatalog;
use crate::reads::library::pool_bridge::LibraryDb;

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
}

impl CollectionsState {
    /// State over one database.
    pub fn new(db: CollectionsDb) -> Self {
        let stores = Stores::new(&db);
        Self::over(db, stores)
    }

    /// The production state over the stores acquisition was built with.
    pub fn over(db: CollectionsDb, stores: Stores) -> Self {
        let library = db
            .pool()
            .map_or_else(|_| LibraryDb::unwired(), LibraryDb::new);
        Self {
            stores,
            items: SqlitePlayerCatalog::new(&library),
            db,
        }
    }

    /// State with no database: every route fails closed. Bundles that never
    /// serve collections use this.
    pub fn unwired() -> Self {
        Self::new(CollectionsDb::unwired())
    }
}
