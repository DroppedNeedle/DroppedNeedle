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
    acquire: Arc<crate::playlist_acquire::PlaylistAcquire>,
    wake: Arc<watch::Sender<u64>>,
}

impl PlaylistPull {
    /// Pull over the Spotify service and the import queue.
    pub fn new(
        service: Arc<SpotifyImportService>,
        imports: SpotifyImports,
        acquire: Arc<crate::playlist_acquire::PlaylistAcquire>,
    ) -> Self {
        Self {
            service,
            imports,
            acquire,
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
        for (playlist_id, user_id, source_ref, stored, acquire) in due {
            let _ = self
                .check_row(&playlist_id, &user_id, &source_ref, stored)
                .await;
            if acquire {
                self.acquire_missing(&user_id, &playlist_id).await;
            }
        }
    }

    /// Check one playlist now, for its owner. Answers whether the source
    /// changed and an import was queued.
    pub async fn check_now(&self, playlist_id: &str) -> Result<bool, String> {
        let Some((user_id, source_ref, stored, acquire)) = self
            .service
            .pull_row(playlist_id)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(false);
        };
        let changed = self
            .check_row(playlist_id, &user_id, &source_ref, stored)
            .await?;
        if acquire {
            self.acquire_missing(&user_id, playlist_id).await;
        }
        Ok(changed)
    }

    /// Ask for what the playlist still lacks, as its owner.
    async fn acquire_missing(&self, user_id: &str, playlist_id: &str) {
        let Some(principal) = self.acquire.owner_principal(user_id).await else {
            return;
        };
        let items = self.acquire.items_from(user_id, playlist_id).await;
        if !items.is_empty() {
            self.acquire.start(principal, playlist_id.to_owned(), items).await;
        }
    }

    /// One playlist's check, for the pass and for a person's press.
    async fn check_row(
        &self,
        playlist_id: &str,
        user_id: &str,
        source_ref: &str,
        stored: Option<String>,
    ) -> Result<bool, String> {
        let Some(spotify_id) = source_ref.strip_prefix("spotify:") else {
            return Ok(false);
        };
        match self.service.snapshot_of(user_id, spotify_id).await {
            Ok(Some(snapshot)) if Some(&snapshot) == stored.as_ref() => {
                if let Err(error) = self.service.record_pull(playlist_id, None, None).await {
                    warn!(%error, playlist = %playlist_id, "playlist pull: the check was not recorded");
                }
                Ok(false)
            }
            Ok(Some(snapshot)) => {
                match self.imports.start(user_id, spotify_id, playlist_id).await {
                    Ok(()) => {
                        info!(playlist = %playlist_id, "playlist pull: the source changed; the import is queued");
                        if let Err(error) = self
                            .service
                            .record_pull(playlist_id, Some(&snapshot), None)
                            .await
                        {
                            warn!(%error, playlist = %playlist_id, "playlist pull: the snapshot was not recorded");
                        }
                        Ok(true)
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if let Err(write) = self
                            .service
                            .record_pull(playlist_id, None, Some(&message))
                            .await
                        {
                            warn!(%write, playlist = %playlist_id, "playlist pull: the error was not recorded");
                        }
                        Err(message)
                    }
                }
            }
            Ok(None) => {
                let message = "spotify returned no snapshot".to_owned();
                let _ = self
                    .service
                    .record_pull(playlist_id, None, Some(&message))
                    .await;
                Err(message)
            }
            Err(error) => {
                let message = format!("{error:?}");
                let _ = self
                    .service
                    .record_pull(playlist_id, None, Some(&message))
                    .await;
                Err(message)
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
