//! "Forget my history": a person removes what the app keeps about their
//! listening and what Discover did for them, and everything built from it.
//!
//! One function runs every step, each deleting the person's rows by their
//! id and safe to run again, so a forget cut off part way is finished by
//! asking again. It leaves their plays and play counts, the library, their
//! requests and what they told Discover. Every table with a person's id is
//! named in `server/forget-coverage.txt` with what this does to it, and a
//! table listed there as forgotten has its step here.

use std::sync::Arc;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_media::playback::{events, sqlite::PlaybackStore};

use crate::discover::cache::{self, ForgetCounts};
use crate::discover::{record, scheduler::activity::ActivityDb, services::DiscoverDeps};

/// The stores a forget deletes from.
#[derive(Clone)]
pub struct HistoryStores {
    db: Db,
    activity: ActivityDb,
    playback: Arc<PlaybackStore>,
    forgets: ForgetCounts,
}

impl HistoryStores {
    /// The pick record, the Discover activity and saved pages in `db`, how
    /// plays ended in `playback`, and the forget counts every page cache
    /// shares in `forgets`.
    pub fn new(db: Db, playback: Arc<PlaybackStore>, forgets: ForgetCounts) -> Self {
        Self {
            activity: ActivityDb::new(db.clone()),
            db,
            playback,
            forgets,
        }
    }
}

/// How many rows each step removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forgotten {
    /// How the person's plays ended.
    pub playback_events: usize,
    /// The albums Discover offered them and what came of each.
    pub picks: usize,
    /// Which Discover pages and artist pages they opened lately.
    pub activity: usize,
    /// Their saved Discover, home and queue pages.
    pub pages: usize,
}

/// A step that failed; the steps before it are done.
#[derive(Debug, thiserror::Error)]
#[error("forget stopped at {step}: {error}")]
pub struct ForgetError {
    /// The step that failed.
    pub step: &'static str,
    /// Why.
    pub error: DbError,
}

fn at(step: &'static str) -> impl FnOnce(DbError) -> ForgetError {
    move |error| ForgetError { step, error }
}

/// Forget `user_id`'s history: how their plays ended, the pick record,
/// their Discover activity and their saved pages, in that order, then start
/// one rebuild of their Discover page. Their forget count moves on before
/// the first step and again inside the pages step's write, so a build of
/// theirs claimed before that commit keeps no page and records no pick. A
/// built deck not yet written is dropped too. While such a build runs no
/// rebuild starts here: their next visit builds the page, which is due
/// because its row is gone. Fails at the first step that fails; the
/// rebuild is asked for only once every step ran.
pub async fn forget(deps: &DiscoverDeps, user_id: &str) -> Result<Forgotten, ForgetError> {
    let stores = &deps.history;
    stores.forgets.bump(user_id);
    let playback_events = events::forget(&stores.playback, user_id)
        .await
        .map_err(at("playback events"))?;
    let picks = record::forget(&stores.db, user_id)
        .await
        .map_err(at("pick record"))?;
    let activity = stores
        .activity
        .forget(user_id)
        .await
        .map_err(at("discover activity"))?;
    let pages = cache::forget_user(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("saved pages"))?;
    deps.queues.forget_unsaved(user_id);
    if let Err(error) = deps.content.trigger_refresh(user_id).await {
        tracing::warn!(?error, "discover rebuild after a forget not started");
    }
    Ok(Forgotten {
        playback_events,
        picks,
        activity,
        pages,
    })
}
