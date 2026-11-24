//! The due tick: the one scheduler of sync passes. Every few minutes it
//! queues a pass for each synced playlist whose next check has come. It
//! re-reads the Spotify app settings each turn and queues nothing while
//! they cannot be read or Spotify is turned off.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::schedule::Schedule;
use droppedneedle_kernel::jobs::supervisor::{
    AlreadyRunning, FnTick, Next, Supervisor, TickCtx, TickError,
};

use super::{SpotifySync, app_ready};

/// The tick's name in the jobs view.
pub const DUE_JOB: &str = "spotify-sync-due";
/// How often the tick looks for due passes.
const INTERVAL: Duration = Duration::from_secs(300);
/// Spread on that cadence.
const JITTER: Duration = Duration::from_secs(30);

impl SpotifySync {
    /// Queue a pass of every synced playlist due now. A busy database
    /// waits for the next turn.
    pub async fn queue_due(&self) -> Result<usize, TickError> {
        // The settings read logs a section that does not read, once per
        // revision.
        let Ok(settings) = self.settings.get_raw() else {
            return Ok(0);
        };
        if !app_ready(&settings) {
            return Ok(0);
        }
        let due = match self.store.due(self.store.now_ms()).await {
            Ok(due) => due,
            Err(DbError::Busy { .. }) => return Ok(0),
            Err(error) => return Err(TickError::new("SPOTIFY_SYNC_DUE_READ", error)),
        };
        if due.is_empty() {
            return Ok(0);
        }
        let queued = due.len();
        match self.store.enqueue_all(due).await {
            Ok(()) => Ok(queued),
            Err(error) if error.is_busy() => Ok(0),
            Err(error) => Err(TickError::new("SPOTIFY_SYNC_DUE_QUEUE", error)),
        }
    }
}

/// Run the due tick on the supervisor.
pub fn spawn_due(supervisor: &Supervisor, sync: Arc<SpotifySync>) -> Result<(), AlreadyRunning> {
    supervisor.periodic(
        DUE_JOB,
        Schedule::new(INTERVAL)
            .with_jitter(JITTER)
            .with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
        FnTick(move |ctx: TickCtx| {
            let sync = sync.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => Ok(Next::Continue),
                    queued = sync.queue_due() => queued.map(|_| Next::Continue),
                }
            }
        }),
    )
}
