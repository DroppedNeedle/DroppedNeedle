//! The playlist pull: a playlist that follows its Spotify source checks the
//! source's snapshot every hour and queues the import job when it changed.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tracing::{info, warn};

use droppedneedle_kernel::jobs::{AlreadyRunning, FnTick, Next, Schedule, Supervisor, TickCtx};

use super::jobs::SpotifyImports;
use super::spotify::SpotifyImportService;

/// Every playlist that pulls is checked this often.
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
/// The first check after start waits this long.
const FIRST_CHECK: Duration = Duration::from_secs(5 * 60);
/// The loop's name on the supervisor.
const LOOP_NAME: &str = "playlist-pull";

/// The pull loop's state: the Spotify service that reads snapshots and the
/// import jobs it queues.
pub struct PlaylistPull {
    service: Arc<SpotifyImportService>,
    imports: SpotifyImports,
    wake: Arc<watch::Sender<u64>>,
}

impl PlaylistPull {
    /// Pull over the Spotify service and the import queue.
    pub fn new(service: Arc<SpotifyImportService>, imports: SpotifyImports) -> Self {
        Self {
            service,
            imports,
            wake: Arc::new(watch::Sender::new(0)),
        }
    }

    /// One pass: for every playlist that pulls, compare the source's
    /// snapshot with the stored one and queue the import when it changed.
    pub async fn pull_due(&self) {
        let due = match self.service.enabled_pulls().await {
            Ok(due) => due,
            Err(error) => {
                warn!(%error, "playlist pull: the list could not be read");
                return;
            }
        };
        for (playlist_id, user_id, source_ref, stored) in due {
            let Some(spotify_id) = source_ref.strip_prefix("spotify:") else {
                continue;
            };
            match self.service.snapshot_of(&user_id, spotify_id).await {
                Ok(Some(snapshot)) if Some(&snapshot) == stored.as_ref() => {
                    if let Err(error) = self.service.record_pull(&playlist_id, None, None).await {
                        warn!(%error, playlist = %playlist_id, "playlist pull: the check was not recorded");
                    }
                }
                Ok(Some(snapshot)) => {
                    match self.imports.start(&user_id, spotify_id, &playlist_id).await {
                        Ok(()) => {
                            info!(playlist = %playlist_id, "playlist pull: the source changed; the import is queued");
                            if let Err(error) = self
                                .service
                                .record_pull(&playlist_id, Some(&snapshot), None)
                                .await
                            {
                                warn!(%error, playlist = %playlist_id, "playlist pull: the snapshot was not recorded");
                            }
                        }
                        Err(error) => {
                            if let Err(write) = self
                                .service
                                .record_pull(&playlist_id, None, Some(&error.to_string()))
                                .await
                            {
                                warn!(%write, playlist = %playlist_id, "playlist pull: the error was not recorded");
                            }
                        }
                    }
                }
                Ok(None) => {
                    let _ = self
                        .service
                        .record_pull(&playlist_id, None, Some("spotify returned no snapshot"))
                        .await;
                }
                Err(error) => {
                    let _ = self
                        .service
                        .record_pull(&playlist_id, None, Some(&format!("{error:?}")))
                        .await;
                }
            }
        }
    }
}

/// Start the pull on the supervisor: the first check after five minutes,
/// then hourly.
pub(crate) fn spawn(
    supervisor: &Supervisor,
    pull: Arc<PlaylistPull>,
) -> Result<(), AlreadyRunning> {
    supervisor.on_wakeup(
        LOOP_NAME,
        pull.wake.subscribe(),
        Schedule::new(CHECK_EVERY).with_initial_delay(FIRST_CHECK),
        FnTick(move |ctx: TickCtx| {
            let pull = pull.clone();
            async move {
                tokio::select! {
                    () = ctx.token().cancelled() => {}
                    () = pull.pull_due() => {}
                }
                Ok(Next::Continue)
            }
        }),
    )
}
