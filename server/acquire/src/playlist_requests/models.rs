//! The playlist request routes' bodies and replies.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::imports::jobs::ImportFailureReason;

/// Body asking for a playlist's missing albums.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PlaylistRequestBody {
    /// A UUID naming this press. Sending the same key again answers what
    /// the first press filed instead of filing again.
    pub idempotency_key: String,
    /// Ask only for this entry's album, from the entry's row.
    #[serde(default)]
    pub entry_id: Option<String>,
}

/// What a press filed and why the rest were left out. Counts are albums.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlaylistRequestResult {
    /// The batch the press filed, when it filed any album.
    pub batch_id: Option<String>,
    /// Albums asked for.
    pub requested: u32,
    /// Albums the library holds already.
    pub in_library: u32,
    /// Albums someone has already asked for.
    pub already_requested: u32,
    /// Albums whose last request was rejected; they are not asked for
    /// again by a press.
    pub rejected_before: u32,
    /// Albums past the most one press files; pressing again asks for them.
    pub over_cap: u32,
    /// Albums left out because the caller's request quota is spent;
    /// pressing again once it has room asks for them.
    pub over_quota: u32,
    /// True when the asks wait for an admin's approval.
    pub needs_approval: bool,
}

/// Where the Spotify import of a playlist stands, while it is not done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PlaylistImportState {
    /// Queued or running.
    Running,
    /// The last import failed.
    Failed {
        /// Why.
        reason: ImportFailureReason,
    },
}

/// What a song the library lacks waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistEntryState {
    /// Its album can be asked for.
    Requestable,
    /// No album was found for it; it cannot be asked for.
    NoAlbumYet,
    /// Its album is still being looked up.
    FindingAlbum,
    /// Its request waits for an admin.
    AwaitingApproval,
    /// Its album or the song itself is being fetched.
    Downloading,
    /// A person picks the copy, or decides on held files.
    AwaitingReview,
    /// Its album is in the library but this song is not; only the song can
    /// be asked for.
    AlbumInLibrary,
    /// Its album's last request was rejected.
    Rejected,
    /// The playlist's Spotify sync asks for it by itself, a few songs a
    /// pass, and has not reached it yet.
    Queued,
}

/// One song's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct EntryRequestState {
    /// The playlist entry.
    pub entry_id: String,
    /// What it waits for.
    pub state: PlaylistEntryState,
}

/// What a playlist's missing songs wait for. Songs with a file or a
/// source of their own are not listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlaylistRequestState {
    /// The playlist's Spotify import, while it runs or after it failed.
    pub import: Option<PlaylistImportState>,
    /// True while the albums of kept songs are being looked up.
    pub finding_albums: bool,
    /// Albums a press would ask for.
    pub requestable_albums: u32,
    /// Songs with no album found.
    pub no_album_songs: u32,
    /// Each missing song, in playlist order.
    pub entries: Vec<EntryRequestState>,
}
