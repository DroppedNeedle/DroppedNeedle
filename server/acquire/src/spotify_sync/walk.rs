//! The walk: a synced playlist's songs the library lacks, requested by
//! themselves under the owner's grant, a few a pass.
//!
//! It visits the songs the sync has not followed up, in playlist order,
//! whose next look for an album is due. A song the library holds (a file,
//! a source, or a track with its ISRC or recording) is followed up with
//! nothing asked. A song gets its album only by proof, through the cached
//! [`AlbumResolver`]; an album the playlist already placed for another of
//! its Spotify album's songs is reused. A song nothing proves waits and is
//! looked for again 1, 3, 7, 14 and 30 days after each look, then every 30
//! days; it is never asked for by name. A song whose album or recording was
//! asked for before, however that request ended, is followed up and not
//! asked again: the sync keeps its own record of what it followed up, so a
//! rejected request the ledger later prunes is never filed again.
//!
//! A pass files requests covering at most [`PASS_SONGS`] songs (an album
//! request counts every open song of that album on the list), looks at
//! most [`PASS_LOOKS`] albums up and reads MusicBrainz to file at most
//! [`PASS_CHECKS`] songs; a song whose album was asked for before is
//! followed up from the database alone. Albums are looked for whether or
//! not the owner holds the grant; without it nothing is filed and no song
//! moves. A full quota or storage ends the walk and is kept as the sync's
//! hold; the song waits for the next pass. A request refused for a reason
//! that would hold next time has the song looked at again on the look
//! schedule.

use std::collections::HashSet;
use std::time::Duration;

use droppedneedle_auth::Principal;
use droppedneedle_auth::users::roles::Role;
use droppedneedle_catalog::collections::db::StoreError;
use droppedneedle_catalog::collections::models::{AlbumPick, FiledKind};
use droppedneedle_catalog::collections::store::playlists::OpenSourceSong;
use droppedneedle_catalog::local::stores::StoreError as LibraryStoreError;
use droppedneedle_kernel::jobs::queue::JobCtx;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use droppedneedle_providers::{ProviderError, RequestPriority};

use super::SpotifySync;
use super::models::SyncHold;
use crate::imports::spotify_albums::{SpotifyAlbum, filed_as_compilation};
use crate::requests::error::RequestsError;
use crate::requests::ledger::RequestStatus;
use crate::requests::models::{AlbumIntake, RequestKind, TrackIntake};
use crate::requests::service::RequestsService;

/// Songs one pass files requests for, at most.
pub const PASS_SONGS: usize = 25;
/// Albums one pass looks up, at most.
pub const PASS_LOOKS: usize = 25;
/// Songs one pass reads MusicBrainz for to file them, at most: the
/// compilation rule's ISRC read and the song's recording.
pub const PASS_CHECKS: usize = 25;
/// Songs whose library copies are checked in one read.
const OWNED_CHUNK: usize = 50;
/// Days after each look before the next: 1, 3, 7, 14, 30, then every 30.
const LOOK_DAYS: [u64; 5] = [1, 3, 7, 14, 30];

/// What a walk did.
#[derive(Debug, Default)]
pub(super) struct Walked {
    /// Songs requested, counting each song an album request carries.
    pub requested: usize,
    /// Why requests wait, when the walk stopped on a full quota or storage.
    pub hold: Option<SyncHold>,
}

/// Why a walk stopped before it was done.
pub(super) enum WalkStop {
    /// The database was busy: the pass goes back.
    Busy,
    /// A store failed; the cause is for the log only.
    Failed(String),
}

impl From<StoreError> for WalkStop {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) => Self::Busy,
            StoreError::Internal(cause) => Self::Failed(cause),
        }
    }
}

/// When the next look for an album is due, after `looks` looks so far
/// (counting the one just made), from `now` (unix ms).
pub(super) fn next_look(now: i64, looks: i64) -> i64 {
    let at = usize::try_from(looks.max(1) - 1).unwrap_or(usize::MAX);
    let days = LOOK_DAYS.get(at).copied().unwrap_or(30);
    let wait = Duration::from_secs(days * 24 * 3600);
    now.saturating_add(i64::try_from(wait.as_millis()).unwrap_or(i64::MAX))
}

/// The release group a song is on, when it has a valid one.
fn group_of(song: &OpenSourceSong) -> Option<String> {
    song.album_id
        .as_deref()
        .map(|group| group.trim().to_ascii_lowercase())
        .filter(|group| is_valid_mbid(group))
}

/// Spotify's filing of `song`'s album, as the compilation rule reads it.
fn spotify_filing(song: &OpenSourceSong) -> SpotifyAlbum {
    SpotifyAlbum {
        title: song.album.title.clone(),
        artist: song.album.artist.clone(),
        kind: song.album.kind.map(|kind| kind.as_str().to_owned()),
        total_tracks: song.album.tracks,
        songs: Default::default(),
        isrcs: Default::default(),
    }
}

/// Why a database read stopped the walk.
fn stop_of(error: RequestsError) -> WalkStop {
    match error {
        RequestsError::Busy { .. } => WalkStop::Busy,
        error => WalkStop::Failed(format!("{error:?}")),
    }
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

impl SpotifySync {
    /// Whether the owner's songs are requested by themselves: by role for
    /// an admin or a trusted user, else by an approved grant. A plain
    /// user's sync with no grant at all (a trusted user since demoted)
    /// asks an admin for one.
    pub(super) async fn holds_grant(
        &self,
        playlist_id: &str,
        role: Role,
        grant: Option<crate::requests::ledger::ApprovalState>,
    ) -> bool {
        use crate::requests::ledger::ApprovalState;
        if role.is_curator() {
            return true;
        }
        if grant.is_none()
            && let Err(error) = self.store.ask_grant_if_none(playlist_id).await
        {
            tracing::warn!(%error, "spotify sync grant ask not filed");
        }
        grant == Some(ApprovalState::Approved)
    }

    /// Give the playlist's open songs their albums, and while the owner
    /// holds the grant (`granted`) request them for `owner`, as the sync
    /// row says (`whole_albums`).
    pub(super) async fn walk(
        &self,
        job: &JobCtx,
        playlist_id: &str,
        owner: &Principal,
        whole_albums: bool,
        granted: bool,
    ) -> Result<Walked, WalkStop> {
        let now = self.store.now_ms();
        // Without the grant only a look for an album is due, so only songs
        // with none are read.
        let open = self
            .playlists
            .open_source_songs(playlist_id, now, !granted)
            .await?;
        let mut walked = Walked::default();
        if open.is_empty() {
            return Ok(walked);
        }
        let mut placed = self.playlists.placed_albums(playlist_id).await?;
        placed.retain(|_, group| is_valid_mbid(group));
        let mut followed: HashSet<String> = HashSet::new();
        let (mut looks, mut checks) = (0, 0);
        let mut musicbrainz_down = false;
        for chunk in open.chunks(OWNED_CHUNK) {
            // What the library holds matters only to songs that may be
            // requested now.
            let owned = if granted {
                self.owned_songs(chunk).await?
            } else {
                HashSet::new()
            };
            // Songs done with this chunk, kept before the next one starts.
            let mut done: Vec<String> = Vec::new();
            let mut stop = false;
            for song in chunk {
                // Every song left needs a look or a check this pass can no
                // longer make, or waits for the next pass.
                let spent = looks >= PASS_LOOKS && (!granted || checks >= PASS_CHECKS);
                if walked.requested >= PASS_SONGS || spent || job.token().is_cancelled() {
                    stop = true;
                    break;
                }
                if followed.contains(&song.song_id) {
                    continue;
                }
                let in_library = song.playable
                    || song.isrc.as_ref().is_some_and(|isrc| owned.contains(isrc))
                    || song
                        .recording_mbid
                        .as_ref()
                        .is_some_and(|id| owned.contains(&id.to_ascii_lowercase()));
                if in_library {
                    // Followed up under the grant; without it nothing is
                    // looked for either.
                    if granted {
                        followed.insert(song.song_id.clone());
                        done.push(song.song_id.clone());
                    }
                    continue;
                }
                let group = match group_of(song) {
                    Some(group) => group,
                    None => {
                        let reused = song
                            .album
                            .source_id
                            .as_ref()
                            .and_then(|id| placed.get(id))
                            .cloned();
                        let found = match reused {
                            Some(group) => Some(group),
                            None if musicbrainz_down || looks >= PASS_LOOKS => continue,
                            None => {
                                looks += 1;
                                match self.look(playlist_id, song).await? {
                                    Look::Found(group) => Some(group),
                                    Look::Nothing => None,
                                    Look::Down => {
                                        musicbrainz_down = true;
                                        continue;
                                    }
                                }
                            }
                        };
                        let Some(group) = found else {
                            self.playlists
                                .look_again(
                                    playlist_id,
                                    &song.song_id,
                                    next_look(now, song.looks + 1),
                                )
                                .await?;
                            continue;
                        };
                        if let Some(source) = &song.album.source_id {
                            placed.insert(source.clone(), group.clone());
                        }
                        self.playlists
                            .set_entries_album(
                                playlist_id,
                                &group,
                                vec![(song.entry_id.clone(), None)],
                            )
                            .await?;
                        group
                    }
                };
                if !granted {
                    continue;
                }
                // Asked for before under a key MusicBrainz cannot change:
                // followed up with nothing asked of it.
                if self
                    .covered_before(song, &group, whole_albums)
                    .await
                    .map_err(stop_of)?
                {
                    followed.insert(song.song_id.clone());
                    done.push(song.song_id.clone());
                    continue;
                }
                if checks >= PASS_CHECKS
                    || (musicbrainz_down && song.isrc.is_some() && song.pick.is_none())
                {
                    continue;
                }
                checks += 1;
                let group = match self.own_album(playlist_id, song, group).await? {
                    Owned::Group(group) => group,
                    Owned::Down => {
                        musicbrainz_down = true;
                        continue;
                    }
                    Owned::Unreadable => {
                        self.playlists
                            .look_again(playlist_id, &song.song_id, next_look(now, song.looks + 1))
                            .await?;
                        continue;
                    }
                };
                let filed = if whole_albums {
                    self.file_album(owner, &group, song).await
                } else {
                    self.file_song(owner, &group, song).await
                };
                match filed {
                    Ok(Filed::Before) => {
                        followed.insert(song.song_id.clone());
                        done.push(song.song_id.clone());
                    }
                    Ok(Filed::NoRecording) => {
                        self.playlists
                            .look_again(playlist_id, &song.song_id, next_look(now, song.looks + 1))
                            .await?;
                    }
                    Ok(Filed::Now(kind, key)) => {
                        // The song itself, moved or not, and with whole
                        // albums every other open song on that album.
                        let mut covered = vec![song.song_id.clone()];
                        if whole_albums {
                            covered.extend(
                                open.iter()
                                    .filter(|other| {
                                        other.song_id != song.song_id
                                            && !followed.contains(&other.song_id)
                                    })
                                    .filter(|other| {
                                        group_of(other).as_deref() == Some(group.as_str())
                                            || other
                                                .album
                                                .source_id
                                                .as_ref()
                                                .and_then(|id| placed.get(id))
                                                .is_some_and(|placed| *placed == group)
                                    })
                                    .map(|other| other.song_id.clone()),
                            );
                        }
                        // Kept at once, with what was filed, so a restart never
                        // files these again and a kept album knows what to
                        // take back.
                        self.playlists
                            .mark_filed(playlist_id, covered.clone(), (kind, &key), now)
                            .await?;
                        walked.requested += covered.len();
                        followed.extend(covered);
                    }
                    Err(RequestsError::QuotaExceeded { .. }) => {
                        walked.hold = Some(SyncHold::Quota);
                        stop = true;
                        break;
                    }
                    Err(RequestsError::StorageFull { .. }) => {
                        walked.hold = Some(SyncHold::Storage);
                        stop = true;
                        break;
                    }
                    Err(RequestsError::Busy { .. }) => return Err(WalkStop::Busy),
                    Err(RequestsError::Internal { cause }) => {
                        tracing::warn!(%cause, "spotify sync request not filed; tried next pass");
                    }
                    // Refused for a reason the next pass would meet again.
                    Err(error) => {
                        tracing::warn!(?error, "spotify sync request refused; looked at later");
                        self.playlists
                            .look_again(playlist_id, &song.song_id, next_look(now, song.looks + 1))
                            .await?;
                    }
                }
            }
            self.playlists
                .mark_followed_up(playlist_id, done, now)
                .await?;
            // With MusicBrainz down the rest of the songs wait for the
            // next pass.
            if stop || musicbrainz_down {
                break;
            }
        }
        Ok(walked)
    }

    /// Whether `song`, on Spotify's album `group`, was asked for before
    /// under a key the compilation rule cannot change: its album was asked
    /// for, or, with whole albums on and Spotify not filing it as a
    /// compilation, the library owns its album. Reads only the database.
    async fn covered_before(
        &self,
        song: &OpenSourceSong,
        group: &str,
        whole_albums: bool,
    ) -> Result<bool, RequestsError> {
        if self.asked_before(RequestKind::Album, group, song).await? {
            return Ok(true);
        }
        let fixed = song.pick.is_some()
            || song.isrc.is_none()
            || !filed_as_compilation(&spotify_filing(song));
        if !(whole_albums && fixed) {
            return Ok(false);
        }
        let groups = vec![group.to_owned()];
        let owned = self
            .library
            .owned_albums(&groups)
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        Ok(owned.contains(group))
    }

    /// The album to ask for `song` on: the artist's own album when Spotify
    /// files it on a compilation and its ISRC's recording is there
    /// ([`AlbumResolver::own_group`](crate::imports::spotify_albums::AlbumResolver::own_group)),
    /// moved on the entry, else `group`. A song a person kept on Spotify's
    /// album, or one moved before, stays. MusicBrainz out of reach stops
    /// the pass's MusicBrainz reads; an answer for this song alone that
    /// could not be read has it looked at again later.
    async fn own_album(
        &self,
        playlist_id: &str,
        song: &OpenSourceSong,
        group: String,
    ) -> Result<Owned, WalkStop> {
        let Some(isrc) = song.isrc.as_deref().filter(|_| song.pick.is_none()) else {
            return Ok(Owned::Group(group));
        };
        let filing = spotify_filing(song);
        if !filed_as_compilation(&filing) && self.resolver.is_known_studio_group(&group).await {
            return Ok(Owned::Group(group));
        }
        let own = match self.resolver.own_group(&filing, isrc).await {
            Ok(Some(own)) if own.group != group => own,
            Ok(_) => return Ok(Owned::Group(group)),
            Err(error)
                if error.is_retriable() || matches!(error, ProviderError::NotConfigured { .. }) =>
            {
                tracing::warn!(%error, "spotify sync stopped checking compilations this pass");
                return Ok(Owned::Down);
            }
            Err(error) => {
                tracing::warn!(%error, "spotify sync could not check a compilation's song");
                return Ok(Owned::Unreadable);
            }
        };
        let moved = self
            .playlists
            .move_to_own_album(
                playlist_id,
                &song.song_id,
                &song.entry_id,
                (&group, &own.group),
                &own.title,
                own.recording.as_deref(),
            )
            .await?;
        Ok(Owned::Group(if moved { own.group } else { group }))
    }

    /// The ISRCs and recordings among `songs` a library track carries.
    async fn owned_songs(&self, songs: &[OpenSourceSong]) -> Result<HashSet<String>, WalkStop> {
        let isrcs: Vec<String> = songs.iter().filter_map(|song| song.isrc.clone()).collect();
        let recordings: Vec<String> = songs
            .iter()
            .filter_map(|song| song.recording_mbid.as_deref())
            .map(str::to_ascii_lowercase)
            .filter(|id| is_valid_mbid(id))
            .collect();
        match self.library.owned_songs(&isrcs, &recordings).await {
            Ok(owned) => Ok(owned),
            Err(LibraryStoreError::Busy(_)) => Err(WalkStop::Busy),
            Err(LibraryStoreError::Internal(cause)) => Err(WalkStop::Failed(cause)),
        }
    }

    /// Look for the album of `song`, by proof only: the Spotify album
    /// rebuilt from what the playlist kept about its songs on it.
    async fn look(&self, playlist_id: &str, song: &OpenSourceSong) -> Result<Look, WalkStop> {
        let siblings = match &song.album.source_id {
            Some(source) => {
                self.playlists
                    .source_album_songs(playlist_id, source)
                    .await?
            }
            None => vec![(song.track_name.clone(), song.isrc.clone())],
        };
        let mut album = SpotifyAlbum {
            title: song.album.title.clone(),
            artist: if song.album.artist.is_empty() {
                first_artist(&song.artist_name)
            } else {
                song.album.artist.clone()
            },
            kind: song.album.kind.map(|kind| kind.as_str().to_owned()),
            total_tracks: song.album.tracks,
            songs: Default::default(),
            isrcs: Default::default(),
        };
        for (title, isrc) in siblings {
            if !title.trim().is_empty() {
                album.songs.insert(title);
            }
            if let Some(isrc) = isrc {
                album.add_isrc(&isrc);
            }
        }
        Ok(
            match self
                .resolver
                .resolve_for(
                    song.album.source_id.as_deref(),
                    &album,
                    RequestPriority::BackgroundSync,
                )
                .await
            {
                Ok(Some(group)) => Look::Found(group.to_ascii_lowercase()),
                Ok(None) => Look::Nothing,
                Err(error) => {
                    tracing::warn!(%error, "spotify sync stopped looking albums up this pass");
                    Look::Down
                }
            },
        )
    }

    /// Whether `key` was asked for before, however that request ended: the
    /// sync never asks twice. A song a person kept on Spotify's album may be
    /// asked for again where their choice cancelled the sync's request.
    async fn asked_before(
        &self,
        kind: RequestKind,
        key: &str,
        song: &OpenSourceSong,
    ) -> Result<bool, RequestsError> {
        let states = self
            .requests
            .store
            .states_of(kind, std::slice::from_ref(&key.to_owned()))
            .await?;
        let kept = song.pick == Some(AlbumPick::KeptSpotify);
        Ok(states
            .get(key)
            .is_some_and(|(status, _)| !(kept && *status == RequestStatus::Cancelled)))
    }

    /// Ask for the album `group` of `song`, unless it was asked for
    /// before or the library owns it.
    async fn file_album(
        &self,
        owner: &Principal,
        group: &str,
        song: &OpenSourceSong,
    ) -> Result<Filed, RequestsError> {
        if self.asked_before(RequestKind::Album, group, song).await? {
            return Ok(Filed::Before);
        }
        let groups = vec![group.to_owned()];
        let owned = self
            .library
            .owned_albums(&groups)
            .await
            .map_err(|error| RequestsError::internal(&error))?;
        if owned.contains(group) {
            return Ok(Filed::Before);
        }
        let intake = AlbumIntake {
            musicbrainz_id: group.to_owned(),
            artist: Some(first_artist(&song.artist_name)),
            album: Some(song.album_name.clone()).filter(|name| !name.trim().is_empty()),
            year: None,
            artist_mbid: None,
            release_mbid: None,
            monitor_artist: false,
            auto_download_artist: false,
            made_from: None,
        };
        RequestsService::new(&self.requests)
            .request_album_granted(owner, &intake)
            .await?;
        Ok(Filed::Now(FiledKind::Album, group.to_owned()))
    }

    /// Ask for `song` alone on album `group`, unless it or its album was
    /// asked for before.
    async fn file_song(
        &self,
        owner: &Principal,
        group: &str,
        song: &OpenSourceSong,
    ) -> Result<Filed, RequestsError> {
        if self.asked_before(RequestKind::Album, group, song).await? {
            return Ok(Filed::Before);
        }
        let Some(recording) = self.recording_of(group, song).await else {
            return Ok(Filed::NoRecording);
        };
        if self
            .asked_before(RequestKind::Track, &recording, song)
            .await?
        {
            return Ok(Filed::Before);
        }
        let intake = TrackIntake {
            recording_mbid: recording.clone(),
            artist_name: first_artist(&song.artist_name),
            track_title: song.track_name.clone(),
            album_title: Some(song.album_name.clone()).filter(|name| !name.trim().is_empty()),
            duration_seconds: song.duration_ms.map(|ms| (ms + 500) / 1000),
            release_group_mbid: Some(group.to_owned()),
            artist_mbid: None,
            release_mbid: None,
        };
        match RequestsService::new(&self.requests)
            .request_track_granted(owner, &intake)
            .await
        {
            Ok(_) => Ok(Filed::Now(FiledKind::Track, recording)),
            Err(RequestsError::AlreadyInLibrary) => Ok(Filed::Before),
            Err(error) => Err(error),
        }
    }

    /// The song's recording on `group`: the one its ISRC's recordings put
    /// there, else the entry's own, else the one track of its title on the
    /// group's tracklist.
    async fn recording_of(&self, group: &str, song: &OpenSourceSong) -> Option<String> {
        if let Some(isrc) = &song.isrc {
            match self
                .resolver
                .recording_on_group(isrc, group, RequestPriority::BackgroundSync)
                .await
            {
                Ok(Some(recording)) => return Some(recording),
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, "spotify sync isrc read failed"),
            }
        }
        if let Some(recording) = song
            .recording_mbid
            .as_deref()
            .map(str::to_ascii_lowercase)
            .filter(|id| is_valid_mbid(id))
        {
            return Some(recording);
        }
        match crate::playlist_requests::recording_on(
            &self.catalog,
            group,
            &song.track_name,
            RequestPriority::BackgroundSync,
        )
        .await
        {
            Ok(recording) => Some(recording),
            Err(error) => {
                tracing::debug!(?error, "spotify sync song not on its album's tracklist");
                None
            }
        }
    }
}

/// The album a song is asked for on, or why it waits.
enum Owned {
    Group(String),
    /// MusicBrainz is out of reach: no more of its reads this pass.
    Down,
    /// MusicBrainz's answer for this song could not be read.
    Unreadable,
}

/// What a look for an album found.
enum Look {
    Found(String),
    Nothing,
    Down,
}

/// What filing a song did.
enum Filed {
    /// Asked for before, or held by the library: nothing filed.
    Before,
    /// Filed now: an album or a song, by MusicBrainz id.
    Now(FiledKind, String),
    /// No recording to ask for: look again later.
    NoRecording,
}
