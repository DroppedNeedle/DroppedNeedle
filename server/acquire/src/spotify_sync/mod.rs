//! Spotify sync: a playlist imported from Spotify, or Liked Songs, kept in
//! step with it.
//!
//! Each synced playlist has one row in `acquire_playlist_syncs` and one job
//! key, `spotify:sync:{playlist_id}`, of kind [`KIND`]. A pass is queued only
//! by the due tick ([`due`]), the playlist's switch, a grant decision, a
//! Spotify reconnect and the import route; a pass never queues the next, so
//! a pass that fails for good cannot end a sync. Its first write moves the
//! next check an hour on, so the tick does not queue it again while it runs.
//!
//! A pass asks Spotify one cheap question first: the playlist's version
//! (`snapshot_id`), or the newest page of Liked Songs. It reads the songs
//! only when the version changed, the kept one is missing, or the last full
//! read is a day old, and Liked Songs only to the first page holding a song
//! already taken. New songs are added in one write with the record that the
//! playlist took them (`catalog_playlist_source_songs`), at the end, or for
//! Liked Songs at the top, newest first. Nothing is removed, moved or
//! replaced, and a song taken before is never added again, even after a
//! person removed it. Spotify's version is kept only after that write, so a
//! write that failed is read again next time.
//!
//! Then the walk ([`walk`]) requests the songs the library lacks, a few a
//! pass, while the owner holds the grant ([`grant`]): by role for an admin
//! or a trusted user, else by an admin's approval of that playlist. Without
//! it songs are still added and nothing is requested.
//!
//! A pass that cannot reach Spotify, or cannot save, records one closed
//! reason ([`models::SyncFailure`]) and when to try again, and finishes; a
//! busy database puts it back without spending anything. Every step is safe
//! to repeat after a restart.

pub mod due;
pub mod grant;
pub mod http;
pub mod job;
pub mod models;
pub(crate) mod sql;
pub mod store;
pub mod walk;

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use droppedneedle_auth::Principal;
use droppedneedle_catalog::collections::db::StoreError;
use droppedneedle_catalog::collections::store::PlaylistStore;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::events::EventHub;
use droppedneedle_kernel::jobs::queue::{JobError, JobKind, KindSpec};
use droppedneedle_kernel::runtime_config::secret_sections::SpotifySettings;

use self::models::{PlaylistSync, PlaylistSyncBody, SyncGrant, SyncProgress};
use self::store::SyncStore;
use crate::imports::LIKED_SONGS_REF;
use crate::imports::spotify_albums::AlbumResolver;
use crate::imports::spotify_client::SpotifyClient;
use crate::imports::spotify_store::SqliteSpotifyLinks;
use crate::requests::RequestsState;
use crate::requests::ledger::ApprovalState;
use crate::settings::ConfigSpotifySettings;
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::library::LocalCatalog;

/// The sync job kind.
pub const KIND: JobKind = JobKind::new("spotify.sync");

/// How passes run: a first read of a large Liked Songs waits on Spotify's
/// pace for a while, and the runner keeps the lease alive.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(5)
    .run_timeout(Duration::from_secs(30 * 60));

/// The job key of one playlist's sync.
pub fn sync_key(playlist_id: &str) -> String {
    format!("spotify:sync:{playlist_id}")
}

/// What one pass carries: a function of its key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncJob {
    /// The synced playlist.
    pub playlist_id: String,
}

/// Whether the admin's Spotify app is turned on and complete.
pub(crate) fn app_ready(settings: &SpotifySettings) -> bool {
    settings.enabled
        && !settings.client_id.is_empty()
        && !settings.client_secret.expose().is_empty()
}

/// A random spread of up to `most`, in milliseconds.
pub(crate) fn jitter(most: Duration) -> i64 {
    let mut bytes = [0u8; 8];
    if getrandom::fill(&mut bytes).is_err() {
        return 0;
    }
    let most = u64::try_from(most.as_millis()).unwrap_or(u64::MAX).max(1);
    i64::try_from(u64::from_le_bytes(bytes) % most).unwrap_or(0)
}

/// What the switch can be refused with. Only [`http`] maps it to a status.
#[derive(Debug)]
pub enum PlaylistSyncError {
    /// No such playlist, or the caller may not see it (or, to change it,
    /// does not own it).
    NotFound,
    /// The playlist did not come from Spotify.
    NotSyncable,
    /// The owner has not connected Spotify.
    NotLinked,
    /// Liked Songs, and the owner's link cannot read them.
    ReconnectNeeded,
    /// Spotify is turned off on this server.
    SpotifyOff,
    /// The Spotify app settings cannot be read.
    Settings(droppedneedle_kernel::runtime_config::ConfigError),
    /// The database was busy.
    Busy,
    /// A store failed; the cause is for the log only.
    Internal(String),
}

impl From<DbError> for PlaylistSyncError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { .. } => Self::Busy,
            other => Self::Internal(other.to_string()),
        }
    }
}

impl From<JobError> for PlaylistSyncError {
    fn from(error: JobError) -> Self {
        if error.is_busy() {
            Self::Busy
        } else {
            Self::Internal(error.to_string())
        }
    }
}

impl From<StoreError> for PlaylistSyncError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) => Self::Busy,
            StoreError::Internal(cause) => Self::Internal(cause),
        }
    }
}

/// What the walk files requests through and looks albums up with.
pub struct WalkPorts {
    /// The requests service's state.
    pub requests: RequestsState,
    /// The library's songs and albums.
    pub library: LocalCatalog,
    /// Albums by proof, and the ISRC reads.
    pub resolver: Arc<AlbumResolver>,
    /// Album tracklists.
    pub catalog: Catalog,
}

/// Spotify sync: the pass, the switch and the state read.
pub struct SpotifySync {
    client: SpotifyClient,
    settings: Arc<ConfigSpotifySettings>,
    links: Arc<SqliteSpotifyLinks>,
    playlists: PlaylistStore,
    store: SyncStore,
    events: EventHub,
    requests: RequestsState,
    library: LocalCatalog,
    resolver: Arc<AlbumResolver>,
    catalog: Catalog,
}

impl SpotifySync {
    /// Sync over the Spotify client and the owner's links, writing songs to
    /// `playlists` and sync rows to `store`, and telling owners on
    /// `events`; the walk files through `walk`.
    pub fn new(
        client: SpotifyClient,
        settings: Arc<ConfigSpotifySettings>,
        links: Arc<SqliteSpotifyLinks>,
        playlists: PlaylistStore,
        store: SyncStore,
        events: EventHub,
        walk: WalkPorts,
    ) -> Self {
        Self {
            client,
            settings,
            links,
            playlists,
            store,
            events,
            requests: walk.requests,
            library: walk.library,
            resolver: walk.resolver,
            catalog: walk.catalog,
        }
    }

    /// The sync rows.
    pub fn store(&self) -> &SyncStore {
        &self.store
    }

    /// The playlist's owner and Spotify source, when the caller may see
    /// it: its owner always, others when `editors` and they may change
    /// its songs. Anyone else reads it as missing.
    async fn playlist_for(
        &self,
        caller: &Principal,
        playlist_id: &str,
        editors: bool,
    ) -> Result<(String, Option<String>), PlaylistSyncError> {
        let visible = self.playlists.visible_to(caller.user_id());
        let allowed = if editors {
            visible.may_edit(playlist_id).await?
        } else {
            visible.is_owner(playlist_id).await?
        };
        if allowed != Some(true) {
            return Err(PlaylistSyncError::NotFound);
        }
        let (owner, source) = self
            .playlists
            .source_of(playlist_id)
            .await?
            .ok_or(PlaylistSyncError::NotFound)?;
        let spotify = source
            .as_deref()
            .and_then(|source| source.strip_prefix("spotify:"))
            .filter(|id| !id.is_empty())
            .map(str::to_owned);
        Ok((owner, spotify))
    }

    /// A playlist's sync, for its owner or an editor.
    pub async fn state(
        &self,
        caller: &Principal,
        playlist_id: &str,
    ) -> Result<PlaylistSync, PlaylistSyncError> {
        let (owner, spotify) = self.playlist_for(caller, playlist_id, true).await?;
        let spotify_off = self
            .settings
            .get_raw()
            .is_ok_and(|settings| !app_ready(&settings));
        let row = self.store.get(playlist_id).await?;
        let enabled = row.as_ref().is_some_and(|row| row.enabled);
        let curator = self
            .store
            .owner(&owner)
            .await?
            .is_some_and(|(role, _)| role.is_curator());
        let grant = if curator {
            SyncGrant::NotNeeded
        } else {
            match row.as_ref().and_then(|row| row.grant) {
                Some(ApprovalState::Approved) => SyncGrant::Approved,
                Some(ApprovalState::Rejected) => SyncGrant::Rejected,
                Some(ApprovalState::Revoked) => SyncGrant::Revoked,
                Some(ApprovalState::Pending) | None => SyncGrant::Waiting,
            }
        };
        let songs_left = if enabled {
            self.playlists
                .open_source_song_count(playlist_id, self.store.now_ms())
                .await?
        } else {
            0
        };
        let progress = match &row {
            Some(row) if row.enabled && row.checked_at.is_none() => SyncProgress::Starting,
            Some(row) if row.enabled && songs_left > 0 => SyncProgress::CatchingUp,
            Some(row) if row.enabled => SyncProgress::CaughtUp,
            _ => SyncProgress::Off,
        };
        Ok(PlaylistSync {
            syncable: spotify.is_some(),
            enabled,
            whole_albums: row.as_ref().is_some_and(|row| row.whole_albums),
            grant,
            progress,
            songs_left,
            hold: row
                .as_ref()
                .filter(|row| row.enabled)
                .and_then(|row| row.hold),
            failure: row
                .as_ref()
                .filter(|row| row.enabled)
                .and_then(|row| row.failure),
            spotify_off,
            checked_at: row
                .as_ref()
                .and_then(|row| row.checked_at)
                .map(|at| at / 1000),
        })
    }

    /// Turn a playlist's sync on or off, for its owner. Turning it on
    /// needs a Spotify playlist, Spotify on and the owner's link (one that
    /// reads Liked Songs, for Liked Songs); a plain user's playlist asks an
    /// admin for its grant. Turning it off always succeeds.
    pub async fn set(
        &self,
        caller: &Principal,
        playlist_id: &str,
        body: &PlaylistSyncBody,
    ) -> Result<PlaylistSync, PlaylistSyncError> {
        let (owner, spotify) = self.playlist_for(caller, playlist_id, false).await?;
        if !body.enabled {
            self.store.turn_off(playlist_id).await?;
            return self.state(caller, playlist_id).await;
        }
        let Some(spotify) = spotify else {
            return Err(PlaylistSyncError::NotSyncable);
        };
        let settings = self
            .settings
            .get_raw()
            .map_err(PlaylistSyncError::Settings)?;
        if !app_ready(&settings) {
            return Err(PlaylistSyncError::SpotifyOff);
        }
        let link = match self.links.get(&owner).await {
            Ok(Some(link)) => link,
            Ok(None) => return Err(PlaylistSyncError::NotLinked),
            Err(crate::imports::spotify_client::SpotifyError::Busy) => {
                return Err(PlaylistSyncError::Busy);
            }
            Err(error) => return Err(PlaylistSyncError::Internal(format!("{error:?}"))),
        };
        if spotify == LIKED_SONGS_REF && !link.reads_liked_songs() {
            return Err(PlaylistSyncError::ReconnectNeeded);
        }
        let ask_grant = !caller.role().is_curator();
        self.store
            .turn_on(playlist_id, body.whole_albums, ask_grant)
            .await?;
        self.state(caller, playlist_id).await
    }
}
