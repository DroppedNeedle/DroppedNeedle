//! Spotify playlist imports as [`KIND`] jobs on the jobs table.
//!
//! Starting an import queues one job keyed by user and Spotify playlist,
//! unless that key already has one queued or running, so one import per
//! user and playlist runs at a time. The route answers at once; the job
//! fetches the playlist, resolves its albums and replaces the playlist's
//! rows in one transaction.
//!
//! A run is safe to repeat: it reads the playlist from Spotify again and
//! replaces the rows wholesale, so a run cut short by a restart is simply
//! run again by the next process, and a run that already wrote leaves the
//! same rows. A busy database puts the job back without spending an
//! attempt. Any other failure ends the job failed, with a code the status
//! route turns into its message.
//!
//! The jobs module runs one handler per kind, so imports run one at a time
//! across users.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::db::{Db, DbError, Lane};
use droppedneedle_kernel::events::{EventHub, PlaylistImported, UserNotice, new_event_id};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{
    JobCtx, JobError, JobHandler, JobKind, JobQueue, KindSpec, Outcome, PAYLOAD_INVALID, State,
};
use serde::{Deserialize, Serialize};

use super::spotify::SpotifyImportService;
use super::spotify_client::SpotifyError;

/// The import job kind.
pub const KIND: JobKind = JobKind::new("spotify.import");

/// How imports run: a long playlist with many albums to resolve may take
/// a while at background priority, and the runner keeps the lease alive.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(2 * 3600));

/// Not linked to Spotify, or the link no longer refreshes.
pub const NOT_LINKED: &str = "SPOTIFY_NOT_LINKED";
/// Spotify did not answer, or answered something the import cannot read.
pub const UNAVAILABLE: &str = "SPOTIFY_UNAVAILABLE";
/// The playlist could not be saved.
pub const SAVE_FAILED: &str = "SPOTIFY_SAVE_FAILED";

/// The job key for one user's import of one Spotify playlist.
pub fn import_job_key(user_id: &str, spotify_playlist_id: &str) -> String {
    format!("spotify:import:{user_id}:{spotify_playlist_id}")
}

/// What one import job carries.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ImportJob {
    user_id: String,
    spotify_playlist_id: String,
    playlist_id: String,
}

/// Where one import stands.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportState {
    /// Queued or running.
    Running,
    /// Finished.
    Done,
    /// Failed. The message is user-safe.
    Error {
        /// What went wrong.
        message: String,
    },
}

/// The user-safe line for a failed import's code.
pub fn failure_message(code: Option<&str>) -> &'static str {
    match code {
        Some(NOT_LINKED) => "Spotify account not linked",
        Some(UNAVAILABLE) => "Failed to fetch playlist from Spotify",
        Some(SAVE_FAILED) => "Failed to save the imported playlist",
        _ => "The import did not finish",
    }
}

/// Queues imports and reads where they stand.
#[derive(Clone, Debug)]
pub struct SpotifyImports {
    db: Db,
    queue: JobQueue,
}

impl SpotifyImports {
    /// Imports on `queue`, written through `db`.
    pub fn new(db: Db, queue: JobQueue) -> Self {
        Self { db, queue }
    }

    /// Queue the import of `spotify_playlist_id` into `playlist_id` for
    /// `user_id`, unless one for that pair is already queued or running.
    pub async fn start(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
        playlist_id: &str,
    ) -> Result<(), JobError> {
        let key = import_job_key(user_id, spotify_playlist_id);
        if let Some(status) = self.queue.latest(KIND, &key).await?
            && matches!(status.state, State::Queued | State::Running)
        {
            return Ok(());
        }
        let job = ImportJob {
            user_id: user_id.to_owned(),
            spotify_playlist_id: spotify_playlist_id.to_owned(),
            playlist_id: playlist_id.to_owned(),
        };
        let queue = self.queue.clone();
        self.db
            .write(Lane::Foreground, "spotify-import-enqueue", move |tx| {
                queue.enqueue(tx, KIND, &key, &job).map(|_| ())
            })
            .await
    }

    /// Where the newest import of `spotify_playlist_id` for `user_id`
    /// stands, or `None` when there is none.
    pub async fn state(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
    ) -> Result<Option<ImportState>, DbError> {
        let key = import_job_key(user_id, spotify_playlist_id);
        Ok(self
            .queue
            .latest(KIND, &key)
            .await?
            .map(|status| match status.state {
                State::Queued | State::Running => ImportState::Running,
                State::Done => ImportState::Done,
                State::Failed => ImportState::Error {
                    message: failure_message(status.last_error.as_deref()).to_owned(),
                },
            }))
    }

    /// Record a finished import for a test that needs one without running
    /// it.
    #[cfg(any(test, feature = "test-support"))]
    pub async fn seed_done(
        &self,
        user_id: &str,
        spotify_playlist_id: &str,
        playlist_id: &str,
    ) -> Result<(), JobError> {
        let key = import_job_key(user_id, spotify_playlist_id);
        let job = ImportJob {
            user_id: user_id.to_owned(),
            spotify_playlist_id: spotify_playlist_id.to_owned(),
            playlist_id: playlist_id.to_owned(),
        };
        let queue = self.queue.clone();
        self.db
            .write(Lane::Background, "spotify-import-seed", move |tx| {
                queue.enqueue(tx, KIND, &key, &job)?;
                let claimed = queue.claim_in(tx, &SPEC)?.ok_or(JobError::LeaseLost)?;
                queue.complete(tx, &claimed.lease)
            })
            .await
    }
}

/// Runs [`KIND`] jobs through the import service.
pub struct SpotifyImportJobs {
    service: Arc<SpotifyImportService>,
    events: EventHub,
}

impl SpotifyImportJobs {
    /// Imports run by `service`; each finished one is announced on
    /// `events`.
    pub fn new(service: Arc<SpotifyImportService>, events: EventHub) -> Self {
        Self { service, events }
    }
}

impl JobHandler for SpotifyImportJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let (service, events) = (self.service.clone(), self.events.clone());
        Box::pin(async move {
            let Ok(import) = job.payload::<ImportJob>() else {
                return Outcome::Fail {
                    code: PAYLOAD_INVALID,
                };
            };
            let result = service
                .populate_playlist(
                    &import.user_id,
                    &import.spotify_playlist_id,
                    &import.playlist_id,
                )
                .await;
            let code = match result {
                Ok(_) => {
                    announce_playlist_imported(&events, &import);
                    return Outcome::Done;
                }
                Err(SpotifyError::Busy) => return Outcome::Busy,
                Err(SpotifyError::NotLinked) => NOT_LINKED,
                Err(SpotifyError::Unavailable(cause)) => {
                    tracing::warn!(%cause, "spotify import fetch failed");
                    UNAVAILABLE
                }
                Err(SpotifyError::Store(cause)) => {
                    tracing::error!(%cause, "spotify import write failed");
                    SAVE_FAILED
                }
            };
            Outcome::Fail { code }
        })
    }
}

/// Tell the importing user's open tabs the playlist has its tracks, so the
/// list and detail views refresh.
fn announce_playlist_imported(events: &EventHub, import: &ImportJob) {
    events.notify(
        &import.user_id,
        UserNotice::PlaylistImported(PlaylistImported {
            playlist_id: import.playlist_id.clone(),
            event_id: new_event_id(),
        }),
    );
}
