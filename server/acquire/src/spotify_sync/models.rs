//! The sync's stored states and its wire shapes.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Why the last pass did not reach Spotify or could not save: the values
/// of `acquire_playlist_syncs.failure`. The web UI words each one; the
/// cause goes to the log only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncFailure {
    /// No link, a link that no longer opens, or a refresh Spotify refused.
    NotLinked,
    /// Liked Songs without the permission to read them.
    NeedsReconnect,
    /// Spotify no longer has the playlist, or no longer gives its songs.
    PlaylistGone,
    /// Spotify could not be reached, failed, or answered something
    /// unreadable.
    Unavailable,
    /// Spotify asked to slow down.
    RateLimited,
    /// New songs could not be saved.
    SaveFailed,
}

droppedneedle_kernel::spelled!(SyncFailure {
    NotLinked => "not_linked",
    NeedsReconnect => "needs_reconnect",
    PlaylistGone => "playlist_gone",
    Unavailable => "unavailable",
    RateLimited => "rate_limited",
    SaveFailed => "save_failed",
});

/// Why a sync's requests wait: the values of `acquire_playlist_syncs.hold`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncHold {
    /// The owner's request limit is full.
    Quota,
    /// The owner's storage budget, or the library's, is full.
    Storage,
}

droppedneedle_kernel::spelled!(SyncHold {
    Quota => "quota",
    Storage => "storage",
});

/// Whether a synced playlist's songs are requested by themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncGrant {
    /// The owner is an admin or trusted, so no approval is needed.
    NotNeeded,
    /// Waiting for an admin.
    Waiting,
    /// An admin approved it.
    Approved,
    /// An admin turned it down.
    Rejected,
    /// An admin took an approval back.
    Revoked,
}

/// How far a sync has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncProgress {
    /// Sync is off.
    Off,
    /// On, and the first pass has not finished.
    Starting,
    /// On, with songs still to request.
    CatchingUp,
    /// On, with nothing left to request.
    CaughtUp,
}

/// A playlist's sync, as its owner or an editor sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSync {
    /// The playlist came from Spotify (a playlist or Liked Songs).
    pub syncable: bool,
    /// Sync is on.
    pub enabled: bool,
    /// New songs are asked for as whole albums.
    pub whole_albums: bool,
    /// Whether the songs are requested by themselves.
    pub grant: SyncGrant,
    /// How far the sync has got.
    pub progress: SyncProgress,
    /// Songs the sync still has to request or look an album up for.
    pub songs_left: u32,
    /// Why requests wait, when they do.
    pub hold: Option<SyncHold>,
    /// Why the last pass failed, when it did.
    pub failure: Option<SyncFailure>,
    /// Spotify is turned off on this server.
    pub spotify_off: bool,
    /// When the last pass finished, epoch seconds.
    pub checked_at: Option<i64>,
}

/// Turn a playlist's sync on or off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PlaylistSyncBody {
    /// Sync on.
    pub enabled: bool,
    /// Ask for new songs' whole albums.
    #[serde(default)]
    pub whole_albums: bool,
}
