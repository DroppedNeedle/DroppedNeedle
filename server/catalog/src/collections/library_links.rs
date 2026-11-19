//! The loop that links playlist entries to the library as albums land, so
//! a song plays in every app once its album is in, without its playlist
//! being opened in the web UI first.

use std::time::Duration;

use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};

use super::db::StoreError;
use super::error::CollectionsError;
use super::service::CollectionsService;
use super::state::CollectionsState;

/// The loop's name in the jobs view.
pub const JOB_NAME: &str = "catalog-playlist-links";

/// Shortest gap between two looks, so a burst of landings is one pass.
const MIN_GAP: Duration = Duration::from_secs(5);

/// Longest wait between two looks, should a commit go unheard.
const CEILING: Duration = Duration::from_secs(600);

/// Row code of a pass that failed.
const FAILED: &str = "PLAYLIST_LINKS_FAILED";

/// Start the loop on `supervisor`. It looks at once, then after every
/// commit (paced), and passes only when the library's catalog or a
/// playlist moved.
pub fn start(state: &CollectionsState, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
    let mut wake = state.db.commits();
    wake.mark_changed();
    supervisor.on_wakeup(
        JOB_NAME,
        wake,
        Schedule::new(CEILING)
            .with_min_delay(MIN_GAP)
            .with_jitter(Duration::from_secs(1))
            .with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
        LinkTick {
            state: state.clone(),
            done: None,
        },
    )
}

struct LinkTick {
    state: CollectionsState,
    /// The catalog revision and playlist change the last finished pass saw.
    done: Option<(i64, i64)>,
}

impl Tick for LinkTick {
    /// A busy database is not a failure: the next commit looks again.
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let mark = match self.state.stores.playlists.link_mark().await {
            Ok(mark) => mark,
            Err(StoreError::Busy(_)) => return Ok(busy(ctx)),
            Err(error) => return Err(TickError::new(FAILED, error)),
        };
        if self.done == Some(mark) {
            return Ok(Next::Continue);
        }
        match CollectionsService::new(&self.state)
            .link_library_everywhere()
            .await
        {
            Ok(linked) => {
                if linked > 0 {
                    tracing::info!(
                        loop_name = ctx.name(),
                        linked,
                        "playlist entries linked to the library"
                    );
                }
                self.done = Some(mark);
                Ok(Next::Continue)
            }
            Err(CollectionsError::Busy { .. }) => Ok(busy(ctx)),
            Err(error) => Err(TickError::new(FAILED, format!("{error:?}"))),
        }
    }
}

/// A pass the database was too busy for waits for the next commit.
fn busy(ctx: &TickCtx) -> Next {
    tracing::info!(
        loop_name = ctx.name(),
        "playlist link pass found the database busy; trying again"
    );
    Next::Continue
}
