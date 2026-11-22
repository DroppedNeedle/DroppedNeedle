//! Playlist acquisition: the batch a press starts, run off the request so
//! the page can follow it instead of waiting on a long call.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_auth::Principal;
use droppedneedle_catalog::collections::store::playlists::PlaylistStore;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::imports::spotify_albums::AlbumResolver;
use crate::requests::error::RequestsError;
use crate::requests::ledger::ApprovalState;
use crate::requests::models::{AlbumIntake, RequestKind, TrackIntake};
use crate::requests::service::RequestsService;

/// How many items one run files, paced by the wait between them.
const PACE: Duration = Duration::from_millis(300);

/// One thing a press asks for.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AcquireItem {
    /// `album` for the whole release, `track` for one recording.
    pub kind: AcquireKind,
    /// Album title, for `album`.
    pub album: Option<String>,
    /// Artist credit, always.
    pub artist: String,
    /// Track title, for `track`.
    pub title: Option<String>,
}

/// What one item asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AcquireKind {
    /// The whole album.
    Album,
    /// One recording.
    Track,
}

/// A press's answer, and its progress while it runs.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AcquireState {
    /// `running` or `done`.
    pub state: String,
    /// Requests filed so far.
    pub requested: usize,
    /// Items passed over: a live request, or a cap.
    pub skipped: usize,
    /// Items the resolver or the intake could not place.
    pub failed: usize,
    /// Items the press asked for.
    pub total: usize,
}

/// Start a press, or read its progress. One run per playlist at a time.
pub struct PlaylistAcquire {
    requests: crate::requests::state::RequestsState,
    playlists: PlaylistStore,
    resolver: Arc<AlbumResolver>,
    runs: Mutex<HashMap<String, AcquireState>>,
}

impl AcquireItem {
    /// Whether two items name the same album, or the same song.
    fn same(&self, other: &Self) -> bool {
        self.kind == other.kind
            && self.artist.eq_ignore_ascii_case(&other.artist)
            && match self.kind {
                AcquireKind::Album => self
                    .album
                    .as_deref()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(other.album.as_deref().unwrap_or("")),
                AcquireKind::Track => self
                    .title
                    .as_deref()
                    .unwrap_or("")
                    .eq_ignore_ascii_case(other.title.as_deref().unwrap_or("")),
            }
    }
}

impl PlaylistAcquire {
    /// A batch runner over the request ledger and the resolver.
    pub fn new(
        requests: crate::requests::state::RequestsState,
        playlists: PlaylistStore,
        resolver: Arc<AlbumResolver>,
    ) -> Self {
        Self {
            requests,
            playlists,
            resolver,
            runs: Mutex::new(HashMap::new()),
        }
    }

    /// The owner's principal, for a pass that files on their behalf.
    pub async fn owner_principal(&self, user_id: &str) -> Option<Principal> {
        RequestsService::new(&self.requests)
            .owner_principal(user_id)
            .await
            .ok()
            .flatten()
    }

    /// Start a run for `playlist_id`, unless one is going. Items that do
    /// not match an unowned row of the playlist are dropped first, so a
    /// caller cannot ask for anything else through this route.
    pub async fn start(
        self: &Arc<Self>,
        principal: Principal,
        playlist_id: String,
        items: Vec<AcquireItem>,
    ) -> AcquireState {
        let allowed = self.items_from(principal.user_id(), &playlist_id).await;
        let items: Vec<AcquireItem> = items
            .into_iter()
            .filter(|item| allowed.iter().any(|row| row.same(item)))
            .collect();
        let state = AcquireState {
            state: "running".to_owned(),
            requested: 0,
            skipped: 0,
            failed: 0,
            total: items.len(),
        };
        {
            let mut runs = self.runs.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(running) = runs.get(&playlist_id)
                && running.state == "running"
            {
                return running.clone();
            }
            runs.insert(playlist_id.clone(), state.clone());
        }
        let this = Arc::clone(self);
        tokio::spawn(async move {
            this.run(principal, playlist_id, items).await;
        });
        state
    }

    /// The progress for one playlist, when anything ran.
    pub fn progress(&self, playlist_id: &str) -> Option<AcquireState> {
        self.runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get(playlist_id)
            .cloned()
    }

    /// Walk the items in order, filing each request, and record progress.
    async fn run(
        self: Arc<Self>,
        principal: Principal,
        playlist_id: String,
        items: Vec<AcquireItem>,
    ) {
        let service = RequestsService::new(&self.requests);
        let granted = principal.role().is_curator()
            || matches!(
                self.requests.mixes.state(principal.user_id()).await,
                Ok(Some(ApprovalState::Approved))
            );
        for (index, item) in items.iter().enumerate() {
            let outcome = self.file(&service, &principal, granted, item).await;
            {
                let mut runs = self.runs.lock().unwrap_or_else(|error| error.into_inner());
                let Some(state) = runs.get_mut(&playlist_id) else {
                    return;
                };
                match outcome {
                    Filed::Requested => state.requested += 1,
                    Filed::Skipped => state.skipped += 1,
                    Filed::Failed => state.failed += 1,
                }
                if index + 1 == items.len() {
                    state.state = "done".to_owned();
                }
            }
            tokio::time::sleep(PACE).await;
        }
        if let Some(state) = self
            .runs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .get_mut(&playlist_id)
        {
            state.state = "done".to_owned();
        }
    }

    async fn file(
        &self,
        service: &RequestsService,
        principal: &Principal,
        granted: bool,
        item: &AcquireItem,
    ) -> Filed {
        match item.kind {
            AcquireKind::Album => {
                let Some(album) = item
                    .album
                    .as_deref()
                    .map(str::trim)
                    .filter(|a| !a.is_empty())
                else {
                    return Filed::Skipped;
                };
                let artist = item.artist.trim();
                if artist.is_empty() {
                    return Filed::Skipped;
                }
                let group = match self.resolver.resolve_names(album, artist).await {
                    Ok(Some(group)) => group,
                    Ok(None) => return Filed::Failed,
                    Err(_) => return Filed::Failed,
                };
                if matches!(
                    self.requests.store.get(RequestKind::Album, &group).await,
                    Ok(Some(row)) if row.status.is_active()
                ) {
                    return Filed::Skipped;
                }
                let intake = AlbumIntake {
                    musicbrainz_id: group,
                    artist: Some(artist.to_owned()),
                    album: Some(album.to_owned()),
                    year: None,
                    artist_mbid: None,
                    release_mbid: None,
                    monitor_artist: false,
                    auto_download_artist: false,
                };
                let outcome = if granted {
                    service.request_album_granted(principal, &intake).await
                } else {
                    service.request_album(principal, &intake).await
                };
                match outcome {
                    Ok(_) => Filed::Requested,
                    Err(
                        RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. },
                    ) => Filed::Skipped,
                    Err(_) => Filed::Failed,
                }
            }
            AcquireKind::Track => {
                let Some(title) = item
                    .title
                    .as_deref()
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                else {
                    return Filed::Skipped;
                };
                let artist = item.artist.trim();
                if artist.is_empty() {
                    return Filed::Skipped;
                }
                let recording = match self.resolver.resolve_track(title, artist).await {
                    Ok(Some(recording)) => recording,
                    Ok(None) => return Filed::Failed,
                    Err(_) => return Filed::Failed,
                };
                if matches!(
                    self.requests.store.get(RequestKind::Track, &recording).await,
                    Ok(Some(row)) if row.status.is_active()
                ) {
                    return Filed::Skipped;
                }
                let intake = TrackIntake {
                    recording_mbid: recording,
                    artist_name: artist.to_owned(),
                    track_title: title.to_owned(),
                    album_title: None,
                    duration_seconds: None,
                    release_group_mbid: None,
                    artist_mbid: None,
                    release_mbid: None,
                };
                match service.request_track(principal, &intake).await {
                    Ok(_) => Filed::Requested,
                    Err(
                        RequestsError::QuotaExceeded { .. } | RequestsError::StorageFull { .. },
                    ) => Filed::Skipped,
                    Err(_) => Filed::Failed,
                }
            }
        }
    }

    /// The items a playlist's unowned rows ask for, all of them.
    pub async fn items_from(&self, user_id: &str, playlist_id: &str) -> Vec<AcquireItem> {
        let tracks = match self.playlists.visible_to(user_id).tracks(playlist_id).await {
            Ok(tracks) => tracks,
            Err(_) => return Vec::new(),
        };
        let mut items: Vec<AcquireItem> = Vec::new();
        for track in &tracks {
            if track.library_file_id.is_some() {
                continue;
            }
            if track
                .available_sources
                .as_ref()
                .is_some_and(|sources| sources.iter().any(|source| !source.trim().is_empty()))
            {
                continue;
            }
            let artist = track.artist_name.trim().to_owned();
            let title = track.track_name.trim().to_owned();
            let album = track.album_name.trim().to_owned();
            if artist.is_empty() || title.is_empty() {
                continue;
            }
            if album.is_empty() {
                if !items.iter().any(|seen| {
                    seen.kind == AcquireKind::Track
                        && seen.title.as_deref() == Some(title.as_str())
                        && seen.artist == artist
                }) {
                    items.push(AcquireItem {
                        kind: AcquireKind::Track,
                        album: None,
                        artist,
                        title: Some(title),
                    });
                }
            } else if !items.iter().any(|seen| {
                seen.kind == AcquireKind::Album
                    && seen.album.as_deref() == Some(album.as_str())
                    && seen.artist == artist
            }) {
                items.push(AcquireItem {
                    kind: AcquireKind::Album,
                    album: Some(album),
                    artist,
                    title: None,
                });
            }
        }
        items
    }
}

enum Filed {
    Requested,
    Skipped,
    Failed,
}

impl Default for AcquireState {
    fn default() -> Self {
        Self {
            state: "done".to_owned(),
            requested: 0,
            skipped: 0,
            failed: 0,
            total: 0,
        }
    }
}
