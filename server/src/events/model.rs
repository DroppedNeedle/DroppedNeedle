//! What travels on the live event stream: the event names the web UI
//! listens for and the JSON each one carries. Names and payloads match the
//! v2 stream (`api/v1/routes/events.py` and its publishers), so the web
//! UI's handlers read them unchanged.

use std::collections::BTreeMap;

use serde::Serialize;
use utoipa::ToSchema;

use crate::playback::models::NowPlayingSnapshot;

/// Event names on the wire.
pub mod names {
    /// Library activity revisions moved.
    pub const ACTIVITY_CHANGED: &str = "activity.changed";
    /// Live now-playing sessions, privacy-projected.
    pub const NOW_PLAYING: &str = "snapshot";
    /// The wanted watcher found new candidates (auto-download off).
    pub const WANTED_NEW_CANDIDATES: &str = "wanted_new_candidates";
    /// The wanted watcher started a download.
    pub const WANTED_AUTO_DISPATCHED: &str = "wanted_auto_dispatched";
    /// A wanted album reached the library.
    pub const WANTED_FULFILLED: &str = "wanted_fulfilled";
    /// A followed artist's new release was queued for download.
    pub const AUTO_DOWNLOAD_ENQUEUED: &str = "auto_download_enqueued";
    /// Someone else imported an album this user requested.
    pub const REQUEST_IMPORTED: &str = "request_imported";
    /// A playlist import finished filling its tracks.
    pub const PLAYLIST_IMPORTED: &str = "playlist_imported";
    /// The user's drop-import job moved.
    pub const DROP_IMPORT_UPDATED: &str = "drop_import_updated";
    /// The user's Free Music task moved.
    pub const FREE_MUSIC_UPDATED: &str = "free_music_updated";
    /// The user's weekly mix was rebuilt (or could not be).
    pub const PERSONAL_MIX_REFRESHED: &str = "personal_mix_refreshed";
    /// New concerts were found for an artist the user follows.
    pub const CONCERTS_NEW: &str = "concerts_new";
}

/// A fresh id the web UI uses to drop an event it has already shown (a
/// reconnect replays the latest one).
pub fn new_event_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// `activity.changed`: the library's activity revisions moved. Sent to
/// everyone; the web UI refetches library activity, or the catalog when
/// `catalog` moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ActivityChanged {
    /// Stable id of this revision set (`activity:` plus 16 hex digits). The
    /// stream also sends it as the SSE `id:` line.
    pub id: String,
    /// Revision per stream: `scan`, `identification`, `operation` and
    /// `catalog`.
    pub revisions: BTreeMap<String, u64>,
}

/// `wanted_new_candidates`, `wanted_auto_dispatched` and `wanted_fulfilled`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct WantedNotice {
    /// Id for de-duplication.
    pub event_id: String,
    /// The wanted album's release group.
    pub release_group_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
    /// New candidates found (`wanted_new_candidates` only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_candidates: Option<u32>,
    /// Download task started (`wanted_auto_dispatched` for whole albums).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Tracks started (`wanted_auto_dispatched` for a partial album).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tracks: Option<u32>,
}

impl WantedNotice {
    /// A notice about one wanted album, with no extras yet.
    pub fn new(release_group_mbid: &str, artist_name: &str, album_title: &str) -> Self {
        Self {
            event_id: new_event_id(),
            release_group_mbid: release_group_mbid.to_owned(),
            artist_name: artist_name.to_owned(),
            album_title: album_title.to_owned(),
            new_candidates: None,
            task_id: None,
            tracks: None,
        }
    }
}

/// `auto_download_enqueued`: a followed artist's new release was queued.
/// The web UI de-duplicates on `task_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct AutoDownloadEnqueued {
    /// Followed artist.
    pub artist_mbid: String,
    /// Artist name, empty when the poll did not carry one.
    pub artist_name: String,
    /// The new release group.
    pub release_group_mbid: String,
    /// Release title.
    pub title: String,
    /// Download task started.
    pub task_id: String,
}

/// `request_imported`: another user (a curator) imported an album this
/// user had requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct RequestImported {
    /// Id for de-duplication.
    pub event_id: String,
    /// The imported release group.
    pub release_group_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Album title.
    pub album_title: String,
}

/// `playlist_imported`: an imported playlist has its tracks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PlaylistImported {
    /// The filled playlist.
    pub playlist_id: String,
    /// Id for de-duplication.
    pub event_id: String,
}

/// `drop_import_updated`: one of the user's drop-import jobs moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct DropImportUpdated {
    /// Id for de-duplication.
    pub event_id: String,
    /// The job that moved.
    pub job_id: String,
}

/// `free_music_updated`: one of the user's Free Music tasks moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct FreeMusicUpdated {
    /// Id for de-duplication.
    pub event_id: String,
    /// The task that moved.
    pub task_id: String,
    /// Its status now (`completed` makes the UI refresh library views).
    pub status: String,
}

/// `personal_mix_refreshed`: the weekly mix build finished.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PersonalMixRefreshed {
    /// The mix playlist, when one was written.
    pub playlist_id: Option<String>,
    /// Tracks in the mix.
    pub track_count: u32,
    /// Albums requested to fill it.
    pub requested_albums: u32,
    /// True when no mix was built.
    pub skipped: bool,
    /// Why it was skipped (`no_tracks`, ...), empty otherwise.
    pub reason: String,
    /// Id for de-duplication.
    pub event_id: String,
}

/// `concerts_new`: a sweep stored new concerts for a followed artist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ConcertsNew {
    /// Followed artist.
    pub artist_mbid: String,
    /// Artist name.
    pub artist_name: String,
    /// Listings first seen in this sweep.
    pub new_events: usize,
}

/// One event for one user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UserNotice {
    /// `wanted_new_candidates`.
    WantedNewCandidates(WantedNotice),
    /// `wanted_auto_dispatched`.
    WantedAutoDispatched(WantedNotice),
    /// `wanted_fulfilled`.
    WantedFulfilled(WantedNotice),
    /// `auto_download_enqueued`.
    AutoDownloadEnqueued(AutoDownloadEnqueued),
    /// `request_imported`.
    RequestImported(RequestImported),
    /// `playlist_imported`.
    PlaylistImported(PlaylistImported),
    /// `drop_import_updated`.
    DropImportUpdated(DropImportUpdated),
    /// `free_music_updated`.
    FreeMusicUpdated(FreeMusicUpdated),
    /// `personal_mix_refreshed`.
    PersonalMixRefreshed(PersonalMixRefreshed),
    /// `concerts_new`.
    ConcertsNew(ConcertsNew),
}

impl UserNotice {
    /// The event name on the wire.
    pub fn name(&self) -> &'static str {
        match self {
            Self::WantedNewCandidates(_) => names::WANTED_NEW_CANDIDATES,
            Self::WantedAutoDispatched(_) => names::WANTED_AUTO_DISPATCHED,
            Self::WantedFulfilled(_) => names::WANTED_FULFILLED,
            Self::AutoDownloadEnqueued(_) => names::AUTO_DOWNLOAD_ENQUEUED,
            Self::RequestImported(_) => names::REQUEST_IMPORTED,
            Self::PlaylistImported(_) => names::PLAYLIST_IMPORTED,
            Self::DropImportUpdated(_) => names::DROP_IMPORT_UPDATED,
            Self::FreeMusicUpdated(_) => names::FREE_MUSIC_UPDATED,
            Self::PersonalMixRefreshed(_) => names::PERSONAL_MIX_REFRESHED,
            Self::ConcertsNew(_) => names::CONCERTS_NEW,
        }
    }

    fn to_json(&self) -> serde_json::Result<String> {
        match self {
            Self::WantedNewCandidates(payload)
            | Self::WantedAutoDispatched(payload)
            | Self::WantedFulfilled(payload) => serde_json::to_string(payload),
            Self::AutoDownloadEnqueued(payload) => serde_json::to_string(payload),
            Self::RequestImported(payload) => serde_json::to_string(payload),
            Self::PlaylistImported(payload) => serde_json::to_string(payload),
            Self::DropImportUpdated(payload) => serde_json::to_string(payload),
            Self::FreeMusicUpdated(payload) => serde_json::to_string(payload),
            Self::PersonalMixRefreshed(payload) => serde_json::to_string(payload),
            Self::ConcertsNew(payload) => serde_json::to_string(payload),
        }
    }
}

/// Everything the hub carries.
#[derive(Debug, Clone)]
pub enum Event {
    /// `activity.changed`, for everyone.
    ActivityChanged(ActivityChanged),
    /// `snapshot` of live listening sessions, for everyone. Sessions are
    /// already projected through each owner's privacy setting.
    NowPlaying(NowPlayingSnapshot),
    /// A notice for one user only.
    User {
        /// Who receives it.
        user_id: String,
        /// What happened.
        notice: UserNotice,
    },
}

/// Who receives an event.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum Audience {
    /// Every signed-in stream.
    Everyone,
    /// Streams of this user only.
    User(String),
}

/// How long a retained event replays to new streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Replay {
    /// Current state: always replays, so a fresh tab starts from it.
    State,
    /// A one-off notice: replays only for a short while, so a tab that
    /// was hidden when it happened still hears about it.
    Notice,
}

/// One event, encoded once for every stream that receives it.
#[derive(Debug)]
pub struct Frame {
    pub(super) audience: Audience,
    pub(super) replay: Replay,
    pub(super) name: &'static str,
    pub(super) id: Option<String>,
    pub(super) data: String,
    pub(super) at: std::time::Instant,
}

impl Frame {
    /// Encode one event.
    pub(super) fn encode(event: Event) -> serde_json::Result<Self> {
        let (audience, replay, name, id, data) = match event {
            Event::ActivityChanged(payload) => (
                Audience::Everyone,
                Replay::State,
                names::ACTIVITY_CHANGED,
                Some(payload.id.clone()),
                serde_json::to_string(&payload)?,
            ),
            Event::NowPlaying(payload) => (
                Audience::Everyone,
                Replay::State,
                names::NOW_PLAYING,
                None,
                serde_json::to_string(&payload)?,
            ),
            Event::User { user_id, notice } => (
                Audience::User(user_id),
                Replay::Notice,
                notice.name(),
                None,
                notice.to_json()?,
            ),
        };
        Ok(Self {
            audience,
            replay,
            name,
            id,
            data,
            at: std::time::Instant::now(),
        })
    }

    /// The SSE event name.
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The SSE `id:` line, when the event has one.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// The JSON payload.
    pub fn data(&self) -> &str {
        &self.data
    }

    /// True when `user_id`'s streams receive this frame.
    pub(super) fn visible_to(&self, user_id: &str) -> bool {
        match &self.audience {
            Audience::Everyone => true,
            Audience::User(owner) => owner == user_id,
        }
    }
}
