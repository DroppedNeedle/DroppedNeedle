//! "Forget my history": a person removes what the app keeps about their
//! listening and what Discover did for them, and everything built from it:
//! the copy of their ListenBrainz history, their taste and candidate pool
//! among it, their weekly sets and their Daily Mixes. Their next taste
//! build makes the taste and pool again from what is left, and the next
//! day's mixes are built from what is left; their sets are gone for good.
//!
//! One function runs every step, each deleting the person's rows by their
//! id and safe to run again, so a forget cut off part way is finished by
//! asking again. It leaves their plays and play counts, the library, their
//! requests, what they told Discover and their set day. Every table with a person's id is
//! named in `server/forget-coverage.txt` with what this does to it, and a
//! table listed there as forgotten has its step here.

use std::sync::Arc;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_media::history::{self, HistoryStore};
use droppedneedle_media::playback::{events, sqlite::PlaybackStore};

use crate::discover::cache::{self, ForgetCounts};
use crate::discover::engine::{pool_store, taste_store, weekly_store};
use crate::discover::{mixes, record, scheduler::activity::ActivityDb, services::DiscoverDeps};

/// The stores a forget deletes from.
#[derive(Clone)]
pub struct HistoryStores {
    db: Db,
    activity: ActivityDb,
    playback: Arc<PlaybackStore>,
    history: Arc<HistoryStore>,
    forgets: ForgetCounts,
}

impl HistoryStores {
    /// The pick record, the Discover activity and saved pages in `db`, how
    /// plays ended in `playback`, the imported ListenBrainz history in
    /// `history`, and the forget counts every page cache shares in
    /// `forgets`.
    pub fn new(
        db: Db,
        playback: Arc<PlaybackStore>,
        history: Arc<HistoryStore>,
        forgets: ForgetCounts,
    ) -> Self {
        Self {
            activity: ActivityDb::new(db.clone()),
            db,
            playback,
            history,
            forgets,
        }
    }
}

/// How many rows each step removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Forgotten {
    /// How the person's plays ended.
    pub playback_events: usize,
    /// The listens imported from their ListenBrainz history.
    pub imported_listens: usize,
    /// The albums Discover offered them and what came of each.
    pub picks: usize,
    /// Which Discover pages and artist pages they opened lately.
    pub activity: usize,
    /// Their taste: the build, its groups, the artists and albums known.
    pub taste: usize,
    /// Their candidate pool: the build, the candidates and their paths.
    pub pool: usize,
    /// Their weekly sets and the sets' picks.
    pub weekly_sets: usize,
    /// Their Daily Mixes and the mixes' tracks.
    pub mixes: usize,
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
    pub error: StepError,
}

/// Why a step failed.
#[derive(Debug, thiserror::Error)]
pub enum StepError {
    /// The database refused it.
    #[error(transparent)]
    Db(#[from] DbError),
    /// The imported history was not forgotten.
    #[error(transparent)]
    History(#[from] history::ForgetError),
}

fn at<E: Into<StepError>>(step: &'static str) -> impl FnOnce(E) -> ForgetError {
    move |error| ForgetError {
        step,
        error: error.into(),
    }
}

/// Forget `user_id`'s history: how their plays ended, their imported
/// ListenBrainz history (its import is marked forgotten and its jobs
/// cancelled in the same write, so an import running across the forget
/// keeps nothing), the pick record, their Discover activity, their taste,
/// their candidate pool, their weekly sets, their Daily Mixes and their
/// saved pages, in that order, then start one rebuild of their Discover
/// page. Their forget count moves on before the first step and again inside
/// the taste, the pool, the sets', the mixes' and the pages steps' writes,
/// so a build of theirs claimed before those commits keeps no taste, no
/// pool, no set, no mix, no page and records no pick. While such a build runs no rebuild starts here: their next visit
/// builds the page, which is due because its row is gone. Fails at the first step that fails; the
/// rebuild is asked for only once every step ran.
pub async fn forget(deps: &DiscoverDeps, user_id: &str) -> Result<Forgotten, ForgetError> {
    let stores = &deps.history;
    stores.forgets.bump(user_id);
    let playback_events = events::forget(&stores.playback, user_id)
        .await
        .map_err(at("playback events"))?;
    let imported_listens = history::forget(&stores.history, user_id)
        .await
        .map_err(at("listening history"))?;
    let picks = record::forget(&stores.db, user_id)
        .await
        .map_err(at("pick record"))?;
    let activity = stores
        .activity
        .forget(user_id)
        .await
        .map_err(at("discover activity"))?;
    let taste = taste_store::forget(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("taste"))?;
    let pool = pool_store::forget(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("pool"))?;
    let weekly_sets = weekly_store::forget(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("weekly sets"))?;
    let mixes = mixes::store::forget(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("daily mixes"))?;
    let pages = cache::forget_user(&stores.db, &stores.forgets, user_id)
        .await
        .map_err(at("saved pages"))?;
    if let Err(error) = deps.content.trigger_refresh(user_id).await {
        tracing::warn!(?error, "discover rebuild after a forget not started");
    }
    Ok(Forgotten {
        playback_events,
        imported_listens,
        picks,
        activity,
        taste,
        pool,
        weekly_sets,
        mixes,
        pages,
    })
}
