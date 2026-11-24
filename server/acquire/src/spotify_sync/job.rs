//! One sync pass, run as a [`KIND`](super::KIND) job: read what changed on
//! Spotify, add the new songs, and record how the pass went.

use std::collections::HashSet;
use std::time::Duration;

use droppedneedle_catalog::collections::db::StoreError;
use droppedneedle_catalog::collections::store::playlists::{SourcePlace, Written};
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::events::{PlaylistSyncChanged, UserNotice, new_event_id};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, Outcome, PAYLOAD_INVALID};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::spotify::models::EntryTrack;

use super::models::SyncFailure;
use super::store::SyncRow;
use super::walk::WalkStop;
use super::{SpotifySync, SyncJob, jitter};
use crate::imports::spotify::ImportedTrack;
use crate::imports::spotify_client::{SpotifyError, TokenRefresher};
use crate::imports::spotify_store::source_entry;
use crate::imports::{LIKED_SONGS_REF, spotify_oauth::SpotifyConnection};
use droppedneedle_auth::Principal;

/// How long after a pass the next one is due, before its spread.
const PASS_INTERVAL: Duration = Duration::from_secs(3600);
/// The spread added to each next pass, at most.
const PASS_SPREAD: Duration = Duration::from_secs(600);
/// The songs are read in full at least this often, whatever Spotify's
/// version says, in case the version lags a change.
const FULL_READ: Duration = Duration::from_secs(24 * 3600);
/// The wait after a failure that only a person can fix: a link, a
/// permission, a playlist taken away. A reconnect queues a pass at once.
const PERSON_WAIT: Duration = Duration::from_secs(24 * 3600);
/// The longest wait after a failure that may clear by itself.
const BACKOFF_CAP: Duration = Duration::from_secs(24 * 3600);
/// Liked Songs read per page (Spotify's maximum).
const LIKED_PAGE: i64 = 50;
/// The longest a pass waits for the Spotify app settings to be saved
/// before it reads them again. A save wakes it at once.
const SETTINGS_RECHECK: Duration = Duration::from_secs(300);

/// What a pass ended with.
enum Ended {
    /// Finished: songs added and requested this pass.
    Done { added: usize, requested: usize },
    /// Spotify or the store failed; the row says why.
    Failed {
        failure: SyncFailure,
        retry_after: Option<u64>,
    },
    /// Nothing to do: the sync is off, the playlist is gone, or Spotify is
    /// turned off on this server.
    Idle,
    /// The database was busy, or the pass was stopped: put the job back.
    Busy,
}

fn ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// The failure a Spotify error is, for a playlist or for Liked Songs.
fn failure_of(error: SpotifyError, liked: bool) -> Ended {
    let failure = match error {
        SpotifyError::Busy => return Ended::Busy,
        SpotifyError::NotLinked => SyncFailure::NotLinked,
        SpotifyError::Forbidden if liked => SyncFailure::NeedsReconnect,
        SpotifyError::Forbidden | SpotifyError::NotFound => SyncFailure::PlaylistGone,
        SpotifyError::RateLimited { retry_after } => {
            return Ended::Failed {
                failure: SyncFailure::RateLimited,
                retry_after,
            };
        }
        SpotifyError::Unavailable(cause) => {
            tracing::warn!(%cause, "spotify sync could not read spotify");
            SyncFailure::Unavailable
        }
        SpotifyError::Store(cause) => {
            tracing::error!(%cause, "spotify sync link store failed");
            SyncFailure::SaveFailed
        }
    };
    Ended::Failed {
        failure,
        retry_after: None,
    }
}

/// Each track once, at its first place.
fn each_once(tracks: Vec<EntryTrack>) -> Vec<EntryTrack> {
    let mut seen = HashSet::new();
    tracks
        .into_iter()
        .filter(|track| track.id.as_ref().is_some_and(|id| seen.insert(id.clone())))
        .collect()
}

impl SpotifySync {
    /// Run one pass of `playlist_id`'s sync.
    pub(super) async fn pass(&self, job: &JobCtx, playlist_id: &str) -> Outcome {
        let ended = self.try_pass(job, playlist_id).await;
        let now = self.store.now_ms();
        match ended {
            Ended::Idle => Outcome::Done,
            Ended::Busy => Outcome::Busy,
            Ended::Done { added, requested } => {
                if added > 0 || requested > 0 {
                    self.announce(playlist_id, added, requested).await;
                }
                Outcome::Done
            }
            Ended::Failed {
                failure,
                retry_after,
            } => {
                let failures = match self.store.get(playlist_id).await {
                    Ok(row) => row.map_or(0, |row| row.failures),
                    Err(DbError::Busy { .. }) => return Outcome::Busy,
                    Err(error) => {
                        tracing::error!(%error, "spotify sync row read failed");
                        0
                    }
                };
                let wait = match failure {
                    SyncFailure::NotLinked
                    | SyncFailure::NeedsReconnect
                    | SyncFailure::PlaylistGone => PERSON_WAIT,
                    SyncFailure::Unavailable
                    | SyncFailure::SaveFailed
                    | SyncFailure::RateLimited => {
                        let doubled = PASS_INTERVAL
                            .saturating_mul(1 << failures.clamp(0, 5) as u32)
                            .min(BACKOFF_CAP);
                        doubled.max(Duration::from_secs(retry_after.unwrap_or(0)))
                    }
                };
                let next = now
                    .saturating_add(ms(wait))
                    .saturating_add(jitter(PASS_SPREAD));
                match self.store.fail(job, playlist_id, failure, next).await {
                    Ok(()) | Err(JobError::LeaseLost) => Outcome::Done,
                    Err(error) if error.is_busy() => Outcome::Busy,
                    Err(error) => {
                        tracing::error!(%error, "spotify sync failure not recorded");
                        Outcome::Done
                    }
                }
            }
        }
    }

    async fn try_pass(&self, job: &JobCtx, playlist_id: &str) -> Ended {
        let row = match self.store.get(playlist_id).await {
            Ok(Some(row)) if row.enabled => row,
            Ok(_) => return Ended::Idle,
            Err(DbError::Busy { .. }) => return Ended::Busy,
            Err(error) => {
                tracing::error!(%error, "spotify sync row read failed");
                return Ended::Idle;
            }
        };
        let next = self
            .store
            .now_ms()
            .saturating_add(ms(PASS_INTERVAL))
            .saturating_add(jitter(PASS_SPREAD));
        match self.store.set_next_check(playlist_id, next).await {
            Ok(()) => {}
            Err(DbError::Busy { .. }) => return Ended::Busy,
            Err(error) => {
                tracing::error!(%error, "spotify sync schedule write failed");
                return Ended::Failed {
                    failure: SyncFailure::SaveFailed,
                    retry_after: None,
                };
            }
        }
        let mut changes = self.settings.subscribe();
        let raw = match self.settings.get_raw() {
            Ok(raw) => raw,
            // The settings read logs its own failure, once per revision.
            Err(_) => {
                tokio::select! {
                    _ = changes.changed() => {}
                    () = job.token().cancelled() => {}
                    () = tokio::time::sleep(SETTINGS_RECHECK) => {}
                }
                return Ended::Busy;
            }
        };
        if !super::app_ready(&raw) {
            return Ended::Idle;
        }
        let (owner, source) = match self.playlists.source_of(playlist_id).await {
            Ok(Some(found)) => found,
            Ok(None) => return Ended::Idle,
            Err(StoreError::Busy(_)) => return Ended::Busy,
            Err(StoreError::Internal(cause)) => {
                tracing::error!(%cause, "spotify sync playlist read failed");
                return Ended::Idle;
            }
        };
        let Some(spotify_id) = source
            .as_deref()
            .and_then(|source| source.strip_prefix("spotify:"))
        else {
            return Ended::Idle;
        };
        let liked = spotify_id == LIKED_SONGS_REF;
        let link: SpotifyConnection = match self.links.get(&owner).await {
            Ok(Some(link)) => link,
            Ok(None) => return failure_of(SpotifyError::NotLinked, liked),
            Err(error) => return failure_of(error, liked),
        };
        if liked && !link.reads_liked_songs() {
            return Ended::Failed {
                failure: SyncFailure::NeedsReconnect,
                retry_after: None,
            };
        }
        let refresher = TokenRefresher::new(
            &self.client,
            &self.links,
            &owner,
            (&raw.client_id, raw.client_secret.expose()),
            link,
            RequestPriority::BackgroundSync,
        );
        let read = if liked {
            self.read_liked(&refresher, playlist_id).await
        } else {
            self.read_playlist(&refresher, spotify_id, &row).await
        };
        let added = match read {
            Err(error) => return failure_of(error, liked),
            Ok(None) => 0,
            Ok(Some((tracks, snapshot))) => {
                let songs = tracks
                    .iter()
                    .map(|track| source_entry(&ImportedTrack::from_spotify(track, None)))
                    .collect();
                let place = if liked {
                    SourcePlace::Top
                } else {
                    SourcePlace::End
                };
                let taken = match self
                    .playlists
                    .owned_by(&owner)
                    .append_from_source(playlist_id, songs, place)
                    .await
                {
                    Ok(Written::Done(taken)) => taken,
                    Ok(Written::Missing) => return Ended::Idle,
                    Err(StoreError::Busy(_)) => return Ended::Busy,
                    Err(StoreError::Internal(cause)) => {
                        tracing::error!(%cause, "spotify sync songs not saved");
                        return Ended::Failed {
                            failure: SyncFailure::SaveFailed,
                            retry_after: None,
                        };
                    }
                };
                // Spotify's version is kept only once its songs are in, so
                // a write that failed is read again on the next pass.
                match self.store.set_read(playlist_id, snapshot).await {
                    Ok(()) => {}
                    Err(DbError::Busy { .. }) => return Ended::Busy,
                    Err(error) => {
                        tracing::error!(%error, "spotify sync read mark not saved");
                    }
                }
                if !taken.added.is_empty()
                    && let Err(error) = self
                        .playlists
                        .owned_by(&owner)
                        .link_library_copies(playlist_id)
                        .await
                {
                    tracing::warn!(%error, "synced songs not linked to the library");
                }
                taken.added.len()
            }
        };
        let (role, name) = match self.store.owner(&owner).await {
            Ok(Some(found)) => found,
            Ok(None) => return Ended::Idle,
            Err(DbError::Busy { .. }) => return Ended::Busy,
            Err(error) => {
                tracing::error!(%error, "spotify sync owner read failed");
                return Ended::Idle;
            }
        };
        // Albums are looked for either way; only a grant files requests.
        let granted = self.holds_grant(playlist_id, role, row.grant).await;
        let principal = Principal::acting_for(owner.clone(), role, name);
        let walked = match self
            .walk(job, playlist_id, &principal, row.whole_albums, granted)
            .await
        {
            Ok(walked) => walked,
            Err(WalkStop::Busy) => return Ended::Busy,
            Err(WalkStop::Failed(cause)) => {
                tracing::error!(%cause, "spotify sync walk failed");
                return Ended::Failed {
                    failure: SyncFailure::SaveFailed,
                    retry_after: None,
                };
            }
        };
        if job.token().is_cancelled() {
            return Ended::Busy;
        }
        let requested = walked.requested;
        match self.store.finish(job, playlist_id, walked.hold).await {
            Ok(()) | Err(JobError::LeaseLost) => Ended::Done { added, requested },
            Err(error) if error.is_busy() => Ended::Busy,
            Err(error) => {
                tracing::error!(%error, "spotify sync pass not recorded");
                Ended::Done { added, requested }
            }
        }
    }

    /// A playlist's songs when they may have changed since the last full
    /// read: Spotify's version differs from the one kept, either is
    /// missing, or the last full read is a day old. `None` when nothing
    /// changed.
    async fn read_playlist(
        &self,
        refresher: &TokenRefresher<'_>,
        spotify_id: &str,
        row: &SyncRow,
    ) -> Result<Option<(Vec<EntryTrack>, Option<String>)>, SpotifyError> {
        let snapshot = self.client.playlist_snapshot(refresher, spotify_id).await?;
        let now = self.store.now_ms();
        let stale = row
            .read_at
            .is_none_or(|read_at| now.saturating_sub(read_at) >= ms(FULL_READ));
        if !stale && snapshot.is_some() && snapshot == row.snapshot_id {
            return Ok(None);
        }
        let tracks = self.client.playlist_tracks(refresher, spotify_id).await?;
        Ok(Some((each_once(tracks), snapshot)))
    }

    /// Liked Songs from the newest, page by page, up to the first page
    /// that holds a song the playlist took before. `None` when the newest
    /// song is one it took.
    async fn read_liked(
        &self,
        refresher: &TokenRefresher<'_>,
        playlist_id: &str,
    ) -> Result<Option<(Vec<EntryTrack>, Option<String>)>, SpotifyError> {
        let seen =
            self.playlists
                .source_song_ids(playlist_id)
                .await
                .map_err(|error| match error {
                    StoreError::Busy(_) => SpotifyError::Busy,
                    StoreError::Internal(cause) => SpotifyError::Store(cause),
                })?;
        let mut tracks = Vec::new();
        let mut offset = 0;
        loop {
            let page = self
                .client
                .saved_tracks_page(refresher, offset, LIKED_PAGE)
                .await?;
            let reached_seen = page
                .tracks
                .iter()
                .any(|track| track.id.as_ref().is_some_and(|id| seen.contains(id)));
            tracks.extend(page.tracks);
            if reached_seen || !page.more {
                break;
            }
            offset += LIKED_PAGE;
        }
        let tracks = each_once(tracks);
        if tracks
            .iter()
            .all(|track| track.id.as_ref().is_some_and(|id| seen.contains(id)))
        {
            return Ok(None);
        }
        Ok(Some((tracks, None)))
    }

    /// Tell the owner's open tabs the playlist changed.
    pub(super) async fn announce(&self, playlist_id: &str, added: usize, requested: usize) {
        let owner = match self.playlists.source_of(playlist_id).await {
            Ok(Some((owner, _))) => owner,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "spotify sync notice not sent");
                return;
            }
        };
        self.events.notify(
            &owner,
            UserNotice::PlaylistSyncChanged(PlaylistSyncChanged {
                playlist_id: playlist_id.to_owned(),
                added: u32::try_from(added).unwrap_or(u32::MAX),
                requested: u32::try_from(requested).unwrap_or(u32::MAX),
                event_id: new_event_id(),
            }),
        );
    }
}

/// Runs [`KIND`](super::KIND) jobs.
pub struct SpotifySyncJobs {
    sync: std::sync::Arc<SpotifySync>,
}

impl SpotifySyncJobs {
    /// Passes run by `sync`.
    pub fn new(sync: std::sync::Arc<SpotifySync>) -> Self {
        Self { sync }
    }
}

impl JobHandler for SpotifySyncJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let sync = self.sync.clone();
        Box::pin(async move {
            let Ok(payload) = job.payload::<SyncJob>() else {
                return Outcome::Fail {
                    code: PAYLOAD_INVALID,
                };
            };
            sync.pass(&job, &payload.playlist_id).await
        })
    }
}
