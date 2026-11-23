//! Collections state: the SQLite stores.

use droppedneedle_kernel::jobs::queue::JobQueue;

use super::db::CollectionsDb;
use super::store::Stores;
use crate::local::store::LocalCatalog;

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
    /// A stored approval state; `None` for text outside the table's CHECK.
    pub fn from_approval(state: &str) -> Option<Self> {
        match state {
            "pending" => Some(Self::Pending),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "revoked" => Some(Self::Revoked),
            _ => None,
        }
    }
}

/// All collections state. Handlers receive this via the `State` extractor
/// and hand it to the service.
#[derive(Clone)]
pub struct CollectionsState {
    /// The database the stores use (also lent to the compat queues).
    pub db: CollectionsDb,
    /// The SQLite stores. Other crates reach playlists only through the
    /// service, which names the caller on every store call.
    pub(crate) stores: Stores,
    /// The library's live artists, albums and tracks, which a rating
    /// names: the same id resolution and existence check Subsonic's
    /// `setRating` goes through, over the same database.
    pub items: LocalCatalog,
    /// The jobs table: a playlist file import queues the job that finds
    /// the albums of the songs it kept.
    pub(crate) jobs: JobQueue,
}

impl CollectionsState {
    /// State over one database, the local catalog and the jobs table.
    pub fn new(db: CollectionsDb, items: LocalCatalog, jobs: JobQueue) -> Self {
        let stores = Stores::new(&db);
        Self::over(db, stores, items, jobs)
    }

    /// The production state over the stores acquisition was built with,
    /// the one local catalog and the jobs table the composition root built.
    pub fn over(db: CollectionsDb, stores: Stores, items: LocalCatalog, jobs: JobQueue) -> Self {
        Self {
            db,
            stores,
            items,
            jobs,
        }
    }
}
