//! Acquisition's side of an album removal (download cleanup plus the
//! wanted follow-up), run as the library's `library.album_removed`
//! job, which the removal queues in its own commit: the album's failed
//! downloads stop retrying, its held downloads are discarded with their
//! copies, its blocklist entries go, and its wanted watch stops or rearms.
//! Uploads waiting in the held list are not touched.
//!
//! Safe to run again: only rows made at or before the removal are
//! touched, so a request made since keeps its task, held files and watch.
//! Held copies are deleted before the one write that discards their rows
//! and finishes the job, so a run that stops in between finds them again.

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::jobs::queue::{
    JobCtx, JobError, JobHandler, KindSpec, Outcome, PAYLOAD_INVALID,
};
use droppedneedle_library::mutations::{ALBUM_REMOVED, AlbumRemovedJob};

use super::downloads::store::{DownloadStore, StoreError};
use super::held::store::HeldRows;
use super::requests::sqlite::{rearm_after_removal, stop_after_removal};

/// How the clean-up runs: the queue's defaults.
pub const SPEC: KindSpec = KindSpec::new(ALBUM_REMOVED);

/// A clean-up write that did not finish.
const CLEANUP_FAILED: &str = "CLEANUP_FAILED";

/// Cleans up download and wanted state for removed albums.
pub struct AcquireAlbumCleanup {
    db: Db,
    clock: Arc<dyn Clock>,
    held_dir: PathBuf,
}

impl AcquireAlbumCleanup {
    /// Cleanup over the download journal and the wanted watches the
    /// acquisition bundle is built on.
    pub fn new(stores: &super::wiring::AcquireStores) -> Self {
        Self {
            db: stores.db.db().clone(),
            clock: stores.db.clock().clone(),
            held_dir: stores.folders.held.clone(),
        }
    }
}

/// What the clean-up's write did.
struct Cleaned {
    retries_cancelled: usize,
    held_removed: usize,
    wanted_changed: bool,
}

#[derive(Debug, thiserror::Error)]
enum CleanupError {
    #[error(transparent)]
    Db(#[from] DbError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Job(#[from] JobError),
}

impl From<rusqlite::Error> for CleanupError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Db(DbError::from(error))
    }
}

impl CleanupError {
    fn is_busy(&self) -> bool {
        match self {
            Self::Db(error) => matches!(error, DbError::Busy { .. } | DbError::LaneClosed),
            Self::Job(error) => error.is_busy(),
            Self::Store(_) => false,
        }
    }
}

impl JobHandler for AcquireAlbumCleanup {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let db = self.db.clone();
        let clock = self.clock.clone();
        let held_dir = self.held_dir.clone();
        Box::pin(async move {
            let Ok(removed) = job.payload::<AlbumRemovedJob>() else {
                return Outcome::Fail {
                    code: PAYLOAD_INVALID,
                };
            };
            match clean_up(&db, clock, &held_dir, job, &removed).await {
                Ok(()) => Outcome::Done,
                Err(error) if error.is_busy() => Outcome::Busy,
                Err(error) => {
                    tracing::warn!(%error, "clean-up after an album removal did not finish");
                    Outcome::Retry {
                        code: CLEANUP_FAILED,
                        at: None,
                    }
                }
            }
        })
    }
}

async fn clean_up(
    db: &Db,
    clock: Arc<dyn Clock>,
    held_dir: &std::path::Path,
    job: JobCtx,
    removed: &AlbumRemovedJob,
) -> Result<(), CleanupError> {
    let group = removed.release_group_mbid.clone();
    let before = (removed.removed_at * 1000.0) as i64;
    let held = db
        .read("downloads.removed_album_held", move |conn| {
            Ok::<_, CleanupError>(HeldRows::new(conn).removed_album_files(&group, before)?)
        })
        .await?;
    for (_, _, path) in &held {
        super::held::remove_copy(held_dir, path).await;
    }
    let job_removed = removed.clone();
    let cleaned = db
        .write(Lane::Background, "downloads.removed_album", move |tx| {
            let removed = job_removed;
            let group = removed.release_group_mbid.as_str();
            let now = crate::db::unix_secs_f64(clock.as_ref());
            let purge = DownloadStore::new(tx).purge_album(group, removed.removed_at, now)?;
            let wanted_changed = if removed.stop_wanted {
                stop_after_removal(tx, group, removed.removed_at)?
            } else {
                rearm_after_removal(tx, group, removed.removed_at, now)?
            };
            job.complete(tx)?;
            Ok::<_, CleanupError>(Cleaned {
                retries_cancelled: purge.retries_cancelled,
                held_removed: purge.held_removed,
                wanted_changed,
            })
        })
        .await?;
    tracing::info!(
        release_group_mbid = removed.release_group_mbid,
        retries_cancelled = cleaned.retries_cancelled,
        held_files = cleaned.held_removed,
        wanted_changed = cleaned.wanted_changed,
        "download state of a removed album cleaned up"
    );
    Ok(())
}
