//! How each playback ended: a second record beside the counted plays.
//!
//! An event says what played, for how long, how it ended and what it was
//! started from. It is never read for a play count or a statistic: what
//! counts as a play stays with the scrobble rules in [`super::report`] and
//! [`super::rules`], and nothing here touches presence, sessions, dedup or
//! scrobble jobs. The web player reports its own playbacks ([`submit`]);
//! a player app's stop report writes one through the compat adapter. Events
//! are kept like plays, with no age limit; they go with the account, and
//! [`forget`] removes one person's.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::{
    PlaybackDeps,
    models::PlaybackEventRequest,
    report::{SCROBBLE_FUTURE_SKEW_SECS, SCROBBLE_TIMESTAMP_MAX_AGE_SECS},
    rules::ServiceError,
    sqlite::PlaybackStore,
};
use droppedneedle_kernel::db::DbError;

/// The longest track or artist name an event keeps, in characters.
const MAX_NAME_CHARS: usize = 1024;
/// The longest track or origin id an event keeps, in characters.
const MAX_ID_CHARS: usize = 256;

/// Where a playback's audio came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlaySource {
    /// A library track.
    Local,
    /// A YouTube video.
    Youtube,
    /// The admin's Jellyfin server.
    Jellyfin,
    /// The admin's Navidrome server.
    Navidrome,
    /// The admin's Plex server.
    Plex,
    /// An enabled streaming plugin. Not written yet: kept for an album
    /// heard through a plugin before the library holds it.
    Plugin,
}

droppedneedle_kernel::spelled!(PlaySource {
    Local => "local",
    Youtube => "youtube",
    Jellyfin => "jellyfin",
    Navidrome => "navidrome",
    Plex => "plex",
    Plugin => "plugin",
});

/// Which kind of player reported a playback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayClient {
    /// The web player.
    Web,
    /// A Subsonic app.
    Subsonic,
    /// A Jellyfin app.
    Jellyfin,
}

droppedneedle_kernel::spelled!(PlayClient {
    Web => "web",
    Subsonic => "subsonic",
    Jellyfin => "jellyfin",
});

/// How a playback ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayEnd {
    /// It played to its end, or into the next track.
    Finished,
    /// A person moved to another track before its end.
    Skipped,
    /// Playback stopped before its end: a stop, a cleared queue, a page
    /// left, or an app that stopped early.
    Stopped,
    /// The stream or its decoding failed.
    Failed,
    /// An app stopped without saying where.
    Unknown,
}

droppedneedle_kernel::spelled!(PlayEnd {
    Finished => "finished",
    Skipped => "skipped",
    Stopped => "stopped",
    Failed => "failed",
    Unknown => "unknown",
});

/// What a playback was started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlayOrigin {
    /// An album.
    Album,
    /// An artist's page.
    Artist,
    /// A playlist.
    Playlist,
    /// A radio station.
    Radio,
    /// A mix: a Daily Mix or an app's instant mix.
    Mix,
    /// A search result.
    Search,
    /// The library's own lists.
    Library,
    /// Nothing the player was told.
    Unknown,
    /// Not written yet: kept for plays started from Discover's weekly set.
    WeeklySet,
    /// Not written yet: kept for plays started from Discover's "dig
    /// further" shelves.
    DigFurther,
    /// Not written yet: kept for plays started from Discover's Later pile.
    LaterPile,
}

droppedneedle_kernel::spelled!(PlayOrigin {
    Album => "album",
    Artist => "artist",
    Playlist => "playlist",
    Radio => "radio",
    Mix => "mix",
    Search => "search",
    Library => "library",
    Unknown => "unknown",
    WeeklySet => "weekly_set",
    DigFurther => "dig_further",
    LaterPile => "later_pile",
});

/// One ended playback, as it is stored. Times are milliseconds since the
/// epoch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackEvent {
    /// The library track it was, when the server resolved one.
    pub local_track_id: Option<String>,
    /// Recording MBID, when known.
    pub recording_mbid: Option<String>,
    /// Release-group MBID, when known.
    pub release_group_mbid: Option<String>,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Where the audio came from.
    pub source: PlaySource,
    /// Which player reported it.
    pub client: PlayClient,
    /// When it started.
    pub started_at: i64,
    /// When it ended.
    pub ended_at: i64,
    /// Time heard in milliseconds; for a player app, the last position it
    /// reported.
    pub listened_ms: i64,
    /// Track length in milliseconds, when known.
    pub duration_ms: Option<i64>,
    /// How it ended.
    pub ended_by: PlayEnd,
    /// What it was started from.
    pub came_from: PlayOrigin,
    /// The album, playlist or station it was started from, when named.
    /// It is the id as the page that started the play named it: a library
    /// id, a MusicBrainz id or a media server's id.
    pub came_from_id: Option<String>,
    /// Whether this playback also counted as a play.
    pub counted: bool,
}

/// Store one ended playback for `user_id`. A write that fails is logged by
/// the store and never fails the caller: the event is a side record.
pub fn record(deps: &PlaybackDeps, user_id: &str, event: &PlaybackEvent) {
    deps.store.record_event(user_id, event);
}

/// Remove every playback event of `user_id`, in one write; the count
/// removed. Plays, counts and scrobbles are left as they are.
pub async fn forget(store: &PlaybackStore, user_id: &str) -> Result<usize, DbError> {
    store.forget_events(user_id).await
}

/// Record how one web-player playback ended. Names must be 1 to 1024
/// characters, ids at most 256, and a recording a MusicBrainz id. The start
/// is held to the scrobble's window (not ahead of now, no older than 14
/// days), a library
/// track is filed under the track its id names now with that track's
/// recording and release group (an id the library does not know keeps the
/// event by name), and the time heard is held to the time the playback
/// could have run. Answers once the write was tried: a failed write is
/// logged and not retried.
pub fn submit(
    deps: &PlaybackDeps,
    user_id: &str,
    request: &PlaybackEventRequest,
) -> Result<(), ServiceError> {
    let now_ms = deps.clock.now_ms();
    let now = now_ms.div_euclid(1000);
    if request.started_at > now + SCROBBLE_FUTURE_SKEW_SECS {
        return Err(ServiceError::InvalidInput(
            "started_at cannot be in the future".to_owned(),
        ));
    }
    if request.started_at < now - SCROBBLE_TIMESTAMP_MAX_AGE_SECS {
        return Err(ServiceError::InvalidInput(
            "started_at cannot be older than 14 days".to_owned(),
        ));
    }
    if request.listened_ms < 0 || request.duration_ms.is_some_and(i64::is_negative) {
        return Err(ServiceError::InvalidInput(
            "listened_ms and duration_ms must be >= 0".to_owned(),
        ));
    }
    let recording = checked_recording(request)?;
    let track_id = request.track_id.as_deref().filter(|id| !id.is_empty());
    if track_id.is_some() && request.source != PlaySource::Local {
        return Err(ServiceError::InvalidInput(
            "track_id is only for a play from the library".to_owned(),
        ));
    }
    let (local_track_id, recording_mbid, release_group_mbid) = match track_id {
        None => (None, recording, None),
        Some(track_id) => match deps.store.get_track(track_id) {
            Some(track) => (Some(track.track_id), track.recording_mbid, track.rg_mbid),
            None => {
                tracing::info!(
                    track_id,
                    "playback event names no library track; kept by name"
                );
                (None, None, None)
            }
        },
    };
    let started_ms = request.started_at.saturating_mul(1000);
    // A start the window lets through may sit up to the skew ahead of this
    // clock, and the bound allows the same, so a client clock that runs
    // fast does not cut a listen short.
    let longest_ms = (now_ms - started_ms + SCROBBLE_FUTURE_SKEW_SECS * 1000).max(0);
    record(
        deps,
        user_id,
        &PlaybackEvent {
            local_track_id,
            recording_mbid,
            release_group_mbid,
            track_name: request.track_name.clone(),
            artist_name: request.artist_name.clone(),
            source: request.source,
            client: PlayClient::Web,
            started_at: started_ms.min(now_ms),
            ended_at: now_ms,
            listened_ms: request.listened_ms.min(longest_ms),
            duration_ms: request.duration_ms.filter(|duration| *duration > 0),
            ended_by: request.ended_by,
            came_from: request.came_from,
            came_from_id: request.came_from_id.clone().filter(|id| !id.is_empty()),
            counted: request.counted,
        },
    );
    Ok(())
}

/// The request's names and ids within their bounds, and its recording as
/// a MusicBrainz id in the usual spelling, else the refusal.
fn checked_recording(request: &PlaybackEventRequest) -> Result<Option<String>, ServiceError> {
    for (field, name) in [
        ("track_name", &request.track_name),
        ("artist_name", &request.artist_name),
    ] {
        if name.trim().is_empty() || name.chars().count() > MAX_NAME_CHARS {
            return Err(ServiceError::InvalidInput(format!(
                "{field} must be 1 to {MAX_NAME_CHARS} characters"
            )));
        }
    }
    for (field, id) in [
        ("track_id", &request.track_id),
        ("came_from_id", &request.came_from_id),
    ] {
        if id
            .as_deref()
            .is_some_and(|id| id.chars().count() > MAX_ID_CHARS)
        {
            return Err(ServiceError::InvalidInput(format!(
                "{field} must be at most {MAX_ID_CHARS} characters"
            )));
        }
    }
    match request
        .recording_mbid
        .as_deref()
        .map(str::trim)
        .filter(|mbid| !mbid.is_empty())
    {
        None => Ok(None),
        Some(mbid) => uuid::Uuid::parse_str(mbid)
            .map(|id| Some(id.hyphenated().to_string()))
            .map_err(|_| {
                ServiceError::InvalidInput("recording_mbid must be a MusicBrainz id".to_owned())
            }),
    }
}
