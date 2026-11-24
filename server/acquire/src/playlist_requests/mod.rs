//! Request missing for a playlist: its owner or an editor asks for the
//! albums of the songs the library lacks, in one press, through the
//! requests service's own batch filing, so approval, quotas and the
//! chosen edition apply as for any album ask. Each filed album records how
//! many of the playlist's songs it carries.
//!
//! Only a song with no library file and no source it plays from, whose
//! album was found (its `album_id`), is asked for; the album is never
//! guessed from names. Songs a Spotify sync is about to ask for are left
//! to it. Albums the library holds, or someone already asked
//! for, are skipped, and so are albums whose last request was rejected. A
//! cancelled one is asked for again: the cancel was a person's own. One
//! press files at most [`PRESS_MAX_ALBUMS`]; a plain user's press files
//! what their request quota has room for. Pressing again asks for the
//! rest, since the filed albums then read as requested. The batch id is
//! the press's idempotency key.
//!
//! A row can also ask for its own album, or for just its song: the
//! entry's recording, else the one track of that title on its album's
//! tracklist. The state read says what each missing song waits for.

pub mod http;
pub mod models;

use std::collections::{HashMap, HashSet};

use droppedneedle_auth::Principal;
use droppedneedle_catalog::collections::db::StoreError as PlaylistStoreError;
use droppedneedle_catalog::collections::identify_job::{IDENTIFY, identify_key};
use droppedneedle_catalog::collections::models::PlaylistTrack;
use droppedneedle_catalog::collections::store::PlaylistStore;
use droppedneedle_catalog::collections::store::playlists::PlaylistRow;
use droppedneedle_catalog::local::stores::StoreError as LibraryStoreError;
use droppedneedle_catalog::metadata::Catalog;
use droppedneedle_catalog::metadata::error::CatalogError;
use droppedneedle_catalog::metadata::library::LocalCatalog;
use droppedneedle_kernel::jobs::queue::{JobQueue, State};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

use self::models::{
    EntryRequestState, PlaylistEntryState, PlaylistImportState, PlaylistRequestBody,
    PlaylistRequestResult, PlaylistRequestState,
};
use crate::imports::jobs::{ImportState, SpotifyImports};
use crate::imports::spotify_albums::same_title;
use crate::requests::RequestsState;
use crate::requests::auth::auto_approves;
use crate::requests::batches::BatchFiling;
use crate::requests::batches::models::{BatchOutcome, BatchSource};
use crate::requests::batches::store::{BatchHead, BatchItem, BatchStore, NewBatch};
use crate::requests::error::RequestsError;
use crate::requests::ledger::{ApprovalState, RequestStatus};
use crate::requests::models::{RequestKind, RequestTaskState, TrackIntake, TrackIntakeResponse};
use crate::requests::service::RequestsService;
use crate::spotify_sync::store::SyncStore;

/// Most albums one press files.
pub const PRESS_MAX_ALBUMS: usize = 200;

/// What a playlist request can fail with. Only [`http`] maps it to a
/// status.
#[derive(Debug)]
pub enum PlaylistRequestError {
    /// No such playlist or entry, or the caller may not change its songs.
    NotFound,
    /// The song has no album found to ask for.
    NoAlbumYet,
    /// The song is not on its album's tracklist.
    SongNotOnAlbum,
    /// The song has a library file or a source it plays from.
    AlreadyPlayable,
    /// The requests service refused or failed.
    Requests(RequestsError),
    /// The album's tracklist could not be read.
    Catalog(CatalogError),
}

impl From<RequestsError> for PlaylistRequestError {
    fn from(error: RequestsError) -> Self {
        Self::Requests(error)
    }
}

impl From<PlaylistStoreError> for PlaylistRequestError {
    fn from(error: PlaylistStoreError) -> Self {
        Self::Requests(match error {
            PlaylistStoreError::Busy(operation) => RequestsError::Busy { operation },
            PlaylistStoreError::Internal(cause) => RequestsError::internal(&cause),
        })
    }
}

impl From<LibraryStoreError> for PlaylistRequestError {
    fn from(error: LibraryStoreError) -> Self {
        Self::Requests(match error {
            LibraryStoreError::Busy(operation) => RequestsError::Busy { operation },
            LibraryStoreError::Internal(cause) => RequestsError::internal(&cause),
        })
    }
}

/// The release group a song waits for: a song with no library file and no
/// source it plays from, whose album was found.
fn missing_album(track: &PlaylistTrack) -> Option<String> {
    if !is_missing(track) {
        return None;
    }
    let group = track.album_id.as_deref()?.trim().to_ascii_lowercase();
    is_valid_mbid(&group).then_some(group)
}

/// A song with no library file and no source it can be played from.
fn is_missing(track: &PlaylistTrack) -> bool {
    track.library_file_id.is_none()
        && track
            .available_sources
            .as_ref()
            .is_none_or(|sources| sources.is_empty())
}

/// The first credited artist of an entry's artist text, as a request
/// names it.
fn first_artist(artist: &str) -> String {
    artist
        .split([',', ';'])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// One album a press considers: its group, the entry that names it first
/// and how many of the playlist's songs it carries.
struct Wanted<'a> {
    group: String,
    first: &'a PlaylistTrack,
    songs: u32,
}

/// The missing albums of `tracks`, deduped by group in playlist order.
fn wanted_albums(tracks: &[PlaylistTrack]) -> Vec<Wanted<'_>> {
    let mut wanted: Vec<Wanted<'_>> = Vec::new();
    for track in tracks {
        let Some(group) = missing_album(track) else {
            continue;
        };
        match wanted.iter_mut().find(|album| album.group == group) {
            Some(album) => album.songs = album.songs.saturating_add(1),
            None => wanted.push(Wanted {
                group,
                first: track,
                songs: 1,
            }),
        }
    }
    wanted
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// The one recording titled `title` on the tracklist the album page shows
/// for `group`, with MusicBrainz asked at `priority`.
pub(crate) async fn recording_on(
    catalog: &Catalog,
    group: &str,
    title: &str,
    priority: RequestPriority,
) -> Result<String, PlaylistRequestError> {
    let tracklist = catalog
        .album_tracks_at(group, priority)
        .await
        .map_err(PlaylistRequestError::Catalog)?;
    let found: HashSet<String> = tracklist
        .tracks
        .iter()
        .filter(|track| same_title(&track.title, title))
        .filter_map(|track| track.recording_id.as_deref())
        .map(str::to_ascii_lowercase)
        .filter(|id| is_valid_mbid(id))
        .collect();
    let mut found = found.into_iter();
    match (found.next(), found.next()) {
        (Some(recording), None) => Ok(recording),
        _ => Err(PlaylistRequestError::SongNotOnAlbum),
    }
}

/// Playlist requests over the requests state, the playlist store and the
/// library.
#[derive(Clone)]
pub struct PlaylistRequests {
    requests: RequestsState,
    playlists: PlaylistStore,
    library: LocalCatalog,
    catalog: Catalog,
    imports: SpotifyImports,
    queue: JobQueue,
    syncs: SyncStore,
}

impl PlaylistRequests {
    /// Playlist requests filed through `requests`, reading playlists from
    /// `playlists`, album tracklists from `catalog`, Spotify imports from
    /// `imports`, identify jobs from `queue` and Spotify syncs from
    /// `syncs`.
    pub fn new(
        requests: RequestsState,
        playlists: PlaylistStore,
        catalog: Catalog,
        imports: SpotifyImports,
        queue: JobQueue,
        syncs: SyncStore,
    ) -> Self {
        Self {
            library: LocalCatalog::new(requests.library.db().clone()),
            requests,
            playlists,
            catalog,
            imports,
            queue,
            syncs,
        }
    }

    /// The entries a Spotify sync of the playlist asks for by itself at its
    /// next pass: its songs not followed up yet and not put off after the
    /// sync could not ask for them, while sync is on and its requests are
    /// granted (by the owner's role or an admin's approval). Empty
    /// otherwise.
    async fn sync_queue(&self, row: &PlaylistRow) -> Result<HashSet<String>, PlaylistRequestError> {
        let Some(owner) = row.owner_id.as_deref() else {
            return Ok(HashSet::new());
        };
        let failed = |error: droppedneedle_kernel::db::DbError| match error {
            droppedneedle_kernel::db::DbError::Busy { operation } => RequestsError::Busy {
                operation: operation.to_owned(),
            },
            other => RequestsError::internal(&other),
        };
        let Some(sync) = self.syncs.get(&row.id).await.map_err(failed)? else {
            return Ok(HashSet::new());
        };
        let curator = self
            .syncs
            .owner(owner)
            .await
            .map_err(failed)?
            .is_some_and(|(role, _)| role.is_curator());
        let granted = curator || sync.grant == Some(ApprovalState::Approved);
        if !sync.enabled || !granted {
            return Ok(HashSet::new());
        }
        Ok(self
            .playlists
            .open_source_entries(&row.id, self.syncs.now_ms())
            .await?)
    }

    /// The playlist and its entries, when the caller owns it or may edit
    /// its songs. A viewer, or anyone else, reads it as missing.
    async fn editable(
        &self,
        caller: &Principal,
        playlist_id: &str,
    ) -> Result<(PlaylistRow, Vec<PlaylistTrack>), PlaylistRequestError> {
        let visible = self.playlists.visible_to(caller.user_id());
        if visible.may_edit(playlist_id).await? != Some(true) {
            return Err(PlaylistRequestError::NotFound);
        }
        let row = visible
            .get(playlist_id)
            .await?
            .ok_or(PlaylistRequestError::NotFound)?;
        let tracks = visible.tracks(playlist_id).await?;
        Ok((row, tracks))
    }

    /// How many more albums the caller may ask for now, `None` when no
    /// quota limits them.
    async fn room(&self, caller: &Principal) -> Result<Option<u32>, RequestsError> {
        if caller.role().is_curator() {
            return Ok(None);
        }
        let quota = &self.requests.quota;
        let limits = quota.effective_quota(caller.user_id())?;
        if limits.request_count == 0 {
            return Ok(None);
        }
        let used = quota
            .count_in_window(
                caller.user_id(),
                limits.request_days,
                self.requests.library.now_epoch(),
            )
            .await?;
        Ok(Some(limits.request_count.saturating_sub(used)))
    }

    /// What an earlier press under the same key filed.
    async fn filed_before(
        &self,
        caller: &Principal,
        head: &BatchHead,
    ) -> Result<PlaylistRequestResult, PlaylistRequestError> {
        if head.user_id != caller.user_id() || head.source != BatchSource::Playlist {
            return Err(RequestsError::InvalidInput {
                message: "This idempotency key is already in use.".to_owned(),
            }
            .into());
        }
        let items = BatchStore::new(self.requests.library.clone())
            .items(&head.id)
            .await?;
        let requested = items
            .iter()
            .filter(|item| item.outcome == BatchOutcome::Requested)
            .count();
        Ok(PlaylistRequestResult {
            batch_id: Some(head.id.clone()),
            requested: count(requested),
            in_library: 0,
            already_requested: 0,
            rejected_before: 0,
            over_cap: 0,
            over_quota: 0,
            needs_approval: !auto_approves(caller.role()),
        })
    }

    /// Ask for the playlist's missing albums, leaving out the songs a
    /// Spotify sync will ask for by itself, or with `entry_id` for that
    /// entry's album only.
    pub async fn request_missing(
        &self,
        caller: &Principal,
        playlist_id: &str,
        body: &PlaylistRequestBody,
    ) -> Result<PlaylistRequestResult, PlaylistRequestError> {
        let key = body.idempotency_key.trim().to_ascii_lowercase();
        if uuid::Uuid::parse_str(&key).is_err() {
            return Err(RequestsError::InvalidInput {
                message: "idempotency_key must be a UUID".to_owned(),
            }
            .into());
        }
        let (row, mut tracks) = self.editable(caller, playlist_id).await?;
        let batches = BatchStore::new(self.requests.library.clone());
        if let Some(head) = batches.head(&key).await? {
            return self.filed_before(caller, &head).await;
        }
        if let Some(entry_id) = body.entry_id.as_deref() {
            let entry = tracks
                .into_iter()
                .find(|track| track.id == entry_id)
                .ok_or(PlaylistRequestError::NotFound)?;
            if !is_missing(&entry) {
                return Err(PlaylistRequestError::AlreadyPlayable);
            }
            if missing_album(&entry).is_none() {
                return Err(PlaylistRequestError::NoAlbumYet);
            }
            tracks = vec![entry];
        } else {
            // Songs a Spotify sync will ask for by itself are left to it.
            let queued = self.sync_queue(&row).await?;
            tracks.retain(|track| !queued.contains(&track.id));
        }
        let wanted = wanted_albums(&tracks);
        let groups: Vec<String> = wanted.iter().map(|album| album.group.clone()).collect();
        let owned = self.library.owned_albums(&groups).await?;
        let statuses = self
            .requests
            .store
            .states_of(RequestKind::Album, &groups)
            .await?;
        let mut result = PlaylistRequestResult {
            batch_id: None,
            requested: 0,
            in_library: 0,
            already_requested: 0,
            rejected_before: 0,
            over_cap: 0,
            over_quota: 0,
            needs_approval: !auto_approves(caller.role()),
        };
        let mut asks = Vec::new();
        for album in wanted {
            let status = statuses.get(&album.group).map(|state| state.0);
            if owned.contains(&album.group) {
                result.in_library += 1;
            } else if status
                .is_some_and(|status| status.is_active() || status == RequestStatus::Cancelling)
            {
                result.already_requested += 1;
            } else if status == Some(RequestStatus::Rejected) {
                result.rejected_before += 1;
            } else {
                asks.push(album);
            }
        }
        if asks.len() > PRESS_MAX_ALBUMS {
            result.over_cap = count(asks.len() - PRESS_MAX_ALBUMS);
            asks.truncate(PRESS_MAX_ALBUMS);
        }
        if let Some(room) = self.room(caller).await? {
            let room = usize::try_from(room).unwrap_or(usize::MAX);
            if asks.len() > room {
                result.over_quota = count(asks.len() - room);
                asks.truncate(room);
            }
        }
        if asks.is_empty() {
            return Ok(result);
        }
        let items = asks
            .iter()
            .map(|album| BatchItem {
                release_group_mbid: album.group.clone(),
                artist_mbid: String::new(),
                album_name: album.first.album_name.trim().to_owned(),
                artist_name: first_artist(&album.first.artist_name),
                outcome: BatchOutcome::Requested,
                request_status: None,
                song_count: Some(album.songs),
                request_generation: None,
            })
            .collect();
        let head = BatchHead {
            id: key,
            user_id: caller.user_id().to_owned(),
            name: row.name,
            source_section: String::new(),
            created_at: self.requests.library.clock().now_ms(),
            source: BatchSource::Playlist,
        };
        let batch = NewBatch {
            head: head.clone(),
            items,
            made_from: None,
        };
        match RequestsService::new(&self.requests)
            .file_discovery_batch(caller, batch)
            .await?
        {
            BatchFiling::Filed => {}
            BatchFiling::Taken { .. } => return self.filed_before(caller, &head).await,
        }
        let filed = batches.items(&head.id).await?;
        result.requested = count(
            filed
                .iter()
                .filter(|item| item.outcome == BatchOutcome::Requested)
                .count(),
        );
        result.batch_id = Some(head.id);
        Ok(result)
    }

    /// Ask for one entry's song alone: its recording, else the one track
    /// of its title on its album's tracklist.
    pub async fn request_song(
        &self,
        caller: &Principal,
        playlist_id: &str,
        entry_id: &str,
    ) -> Result<TrackIntakeResponse, PlaylistRequestError> {
        let (_, tracks) = self.editable(caller, playlist_id).await?;
        let entry = tracks
            .into_iter()
            .find(|track| track.id == entry_id)
            .ok_or(PlaylistRequestError::NotFound)?;
        if !is_missing(&entry) {
            return Err(PlaylistRequestError::AlreadyPlayable);
        }
        let group = missing_album(&entry).ok_or(PlaylistRequestError::NoAlbumYet)?;
        let recording = match entry
            .recording_mbid
            .as_deref()
            .map(str::to_ascii_lowercase)
            .filter(|id| is_valid_mbid(id))
        {
            Some(recording) => recording,
            None => self.recording_on(&group, &entry.track_name).await?,
        };
        let intake = TrackIntake {
            recording_mbid: recording,
            artist_name: first_artist(&entry.artist_name),
            track_title: entry.track_name.clone(),
            album_title: Some(entry.album_name.clone()).filter(|name| !name.trim().is_empty()),
            duration_seconds: entry.duration.map(|seconds| seconds.round() as i64),
            release_group_mbid: Some(group),
            artist_mbid: None,
            release_mbid: None,
        };
        Ok(RequestsService::new(&self.requests)
            .request_track(caller, &intake)
            .await?)
    }

    /// The one recording titled `title` on the tracklist the album page
    /// shows for `group`, for a person waiting.
    async fn recording_on(&self, group: &str, title: &str) -> Result<String, PlaylistRequestError> {
        recording_on(&self.catalog, group, title, RequestPriority::UserInitiated).await
    }

    /// What each of the playlist's missing songs waits for.
    pub async fn state(
        &self,
        caller: &Principal,
        playlist_id: &str,
    ) -> Result<PlaylistRequestState, PlaylistRequestError> {
        let (row, tracks) = self.editable(caller, playlist_id).await?;
        let import = self.import_state(&row).await?;
        let queued = self.sync_queue(&row).await?;
        let mut listed_on = self.playlists.listed_on(playlist_id).await?;
        let finding_albums = self
            .queue
            .latest(IDENTIFY, &identify_key(playlist_id))
            .await
            .map_err(|error| RequestsError::internal(&error))?
            .is_some_and(|job| matches!(job.state, State::Queued | State::Running));
        let missing: Vec<&PlaylistTrack> =
            tracks.iter().filter(|track| is_missing(track)).collect();
        let groups: Vec<String> = missing
            .iter()
            .filter_map(|track| missing_album(track))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let recordings: Vec<String> = missing
            .iter()
            .filter(|track| missing_album(track).is_some())
            .filter_map(|track| track.recording_mbid.as_deref())
            .map(str::to_ascii_lowercase)
            .filter(|id| is_valid_mbid(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let owned = self.library.owned_albums(&groups).await?;
        let albums = self.waits(RequestKind::Album, &groups).await?;
        let songs = self.waits(RequestKind::Track, &recordings).await?;
        let mut requestable = HashSet::new();
        let mut no_album_songs = 0_u32;
        let mut entries = Vec::with_capacity(missing.len());
        for track in missing {
            let state = match missing_album(track) {
                None if finding_albums && !track.album_name.trim().is_empty() => {
                    PlaylistEntryState::FindingAlbum
                }
                None => {
                    no_album_songs += 1;
                    PlaylistEntryState::NoAlbumYet
                }
                Some(group) => {
                    let song = track
                        .recording_mbid
                        .as_deref()
                        .map(str::to_ascii_lowercase)
                        .and_then(|recording| songs.get(&recording).copied());
                    match song
                        .filter(|wait| *wait != PlaylistEntryState::Rejected)
                        .or_else(|| albums.get(&group).copied())
                    {
                        Some(wait) => wait,
                        None if owned.contains(&group) => PlaylistEntryState::AlbumInLibrary,
                        // The sync asks for it by itself, so the press
                        // leaves it out and the count does too.
                        None if queued.contains(&track.id) => PlaylistEntryState::Queued,
                        None => {
                            requestable.insert(group);
                            PlaylistEntryState::Requestable
                        }
                    }
                }
            };
            entries.push(EntryRequestState {
                listed_on: listed_on.remove(&track.id),
                entry_id: track.id.clone(),
                state,
            });
        }
        Ok(PlaylistRequestState {
            import,
            finding_albums,
            requestable_albums: count(requestable.len()),
            no_album_songs,
            entries,
        })
    }

    /// The playlist's Spotify import, while it runs or after it failed,
    /// read for its owner.
    async fn import_state(
        &self,
        row: &PlaylistRow,
    ) -> Result<Option<PlaylistImportState>, PlaylistRequestError> {
        let (Some(owner), Some(spotify_id)) = (
            row.owner_id.as_deref(),
            row.source_ref
                .as_deref()
                .and_then(|source| source.strip_prefix("spotify:")),
        ) else {
            return Ok(None);
        };
        let state = self
            .imports
            .state(owner, spotify_id)
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        Ok(match state {
            Some(ImportState::Running) => Some(PlaylistImportState::Running),
            Some(ImportState::Error { reason }) => Some(PlaylistImportState::Failed { reason }),
            Some(ImportState::Done) | None => None,
        })
    }

    /// What the requests of `kind` for `keys` wait for, by key: approval,
    /// a download, a person's review, or nothing after a rejection. Keys
    /// with no live or rejected request are left out.
    async fn waits(
        &self,
        kind: RequestKind,
        keys: &[String],
    ) -> Result<HashMap<String, PlaylistEntryState>, RequestsError> {
        let states = self.requests.store.states_of(kind, keys).await?;
        let mut waits = HashMap::with_capacity(states.len());
        for (key, (status, task_id)) in states {
            let wait = match status {
                RequestStatus::AwaitingApproval => PlaylistEntryState::AwaitingApproval,
                RequestStatus::Rejected => PlaylistEntryState::Rejected,
                status if status.is_active() => match self.task_state(task_id.as_deref()).await {
                    Some(RequestTaskState::AwaitingReview) => PlaylistEntryState::AwaitingReview,
                    _ => PlaylistEntryState::Downloading,
                },
                _ => continue,
            };
            waits.insert(key, wait);
        }
        Ok(waits)
    }

    /// The state of a request's download, when it has one that reads.
    async fn task_state(&self, task_id: Option<&str>) -> Option<RequestTaskState> {
        match self.requests.dispatch.task(task_id?).await {
            Ok(task) => task.as_ref().and_then(RequestTaskState::of),
            Err(error) => {
                tracing::warn!(?error, "playlist request state: task read failed");
                None
            }
        }
    }
}
