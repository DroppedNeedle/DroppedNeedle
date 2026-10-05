//! Test doubles for the Subsonic seams: a loaded store, an audio backend
//! and a verifier. The clock is fixed ([`NOW_UNIX`]) so time-derived
//! fields (`minutesAgo`) stay deterministic.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use super::Verifier;
use super::auth::{Credentials, Principal};
use super::error::SubsonicError;
use super::ids::IdKind;
use super::store::{
    AlbumSort, BookmarkRecord, ClientInfo, LyricsData, NowPlayingRow, PlaylistDetail,
    PlaylistEntry, PlaylistRecord, PlaylistSummary, QueueState, Store, StreamDetailsData,
    TranscodeDecisionData,
};
use super::stream::{AudioBackend, AudioFacts, BackendError, StreamPlan};
use super::views::{ViewAlbum, ViewArtist, ViewGenre, ViewTrack};

/// Fixed clock (2023-11-14T22:13:20Z): golden time fields pin to this.
pub const NOW_UNIX: f64 = 1_700_000_000.0;

/// Test user id.
pub const USER_ID: &str = "u1";
/// Test username.
pub const USERNAME: &str = "user";
/// Admin username.
pub const ADMIN_USERNAME: &str = "admin";

/// Fake principal.
#[derive(Debug, Clone)]
pub struct FakePrincipal {
    /// User id.
    pub user_id: String,
    /// Username.
    pub username: String,
    /// Display name.
    pub display_name: String,
    /// Admin flag.
    pub admin: bool,
}

impl Principal for FakePrincipal {
    fn user_id(&self) -> &str {
        &self.user_id
    }

    fn username(&self) -> &str {
        &self.username
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn is_admin(&self) -> bool {
        self.admin
    }
}

/// Fake verifier: `user`/`secret`, apiKey `key-1`, or any token pair
/// for `user`/`admin` authenticate; anything else is 40.
#[derive(Debug, Clone, Default)]
pub struct FakeVerifier;

impl Verifier for FakeVerifier {
    type Principal = FakePrincipal;

    async fn verify(&self, credentials: &Credentials) -> Result<FakePrincipal, SubsonicError> {
        let username = match credentials {
            Credentials::Password {
                username, password, ..
            } => {
                if password != "secret" {
                    return Err(SubsonicError::code_only(40));
                }
                username.clone()
            }
            Credentials::Token {
                username,
                token,
                salt,
                ..
            } => {
                if token.is_empty() || salt.is_empty() {
                    return Err(SubsonicError::code_only(40));
                }
                username.clone()
            }
            Credentials::ApiKey { key } => {
                if key != "key-1" {
                    return Err(SubsonicError::code_only(44));
                }
                USERNAME.to_owned()
            }
        };
        if username != USERNAME && username != ADMIN_USERNAME {
            return Err(SubsonicError::code_only(40));
        }
        Ok(FakePrincipal {
            user_id: USER_ID.to_owned(),
            username: username.clone(),
            display_name: format!("{username} Display"),
            admin: username == ADMIN_USERNAME,
        })
    }
}

/// Mutable fake state behind the store.
#[derive(Debug, Default)]
pub struct FakeInner {
    /// Starred targets.
    pub favorites: Vec<(IdKind, String)>,
    /// Saved queue.
    pub queue: QueueState,
    /// Bookmarks.
    pub bookmarks: Vec<BookmarkRecord>,
    /// Playlists.
    pub playlists: Vec<(PlaylistRecord, Vec<PlaylistEntry>, bool, String)>,
    /// Scrobble log (file id, played_at).
    pub scrobbles: Vec<(String, Option<f64>)>,
    /// Now-playing log.
    pub presence: Vec<String>,
    /// Playback reports.
    pub reports: Vec<String>,
    /// Scan running.
    pub scanning: bool,
    /// Next playlist id.
    pub next_playlist: i64,
    /// Next entry id.
    pub next_entry: i64,
}

/// In-memory store with a tiny fixed catalog.
#[derive(Debug, Clone, Default)]
pub struct FakeStore {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeStore {
    /// Poison-tolerant lock (`unwrap` is denied in this tree).
    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Store preloaded with the golden catalog.
    pub fn loaded() -> Self {
        let store = Self::default();
        let mut inner = store.lock();
        inner.favorites = vec![(IdKind::Track, "f1".to_owned())];
        inner.queue = QueueState {
            file_ids: vec!["f1".to_owned(), "f2".to_owned()],
            current_index: Some(0),
            position_ms: 1000,
            updated_at: NOW_UNIX - 60.0,
            changed_by_client: "golden".to_owned(),
        };
        inner.bookmarks = vec![BookmarkRecord {
            file_id: "f1".to_owned(),
            position_ms: 5000,
            comment: "chorus".to_owned(),
            created_at: NOW_UNIX as i64 - 3600,
            changed_at: NOW_UNIX as i64 - 60,
        }];
        inner.playlists = vec![(
            PlaylistRecord {
                id: "p1".to_owned(),
                name: "Mix".to_owned(),
                is_public: true,
                has_cover: true,
                created_at: Some("2023-11-01T00:00:00Z".to_owned()),
                changed_at: Some("2023-11-02T00:00:00Z".to_owned()),
            },
            vec![
                PlaylistEntry {
                    id: "e1".to_owned(),
                    library_file_id: Some("f1".to_owned()),
                },
                PlaylistEntry {
                    id: "e2".to_owned(),
                    library_file_id: None,
                },
                PlaylistEntry {
                    id: "e3".to_owned(),
                    library_file_id: Some("f3".to_owned()),
                },
            ],
            true,
            USERNAME.to_owned(),
        )];
        inner.next_playlist = 2;
        inner.next_entry = 4;
        drop(inner);
        store
    }

    /// Catalog artists.
    pub fn artists() -> Vec<ViewArtist> {
        vec![
            ViewArtist {
                artist_mbid: "artist-1".to_owned(),
                name: "The Testers".to_owned(),
                album_count: Some(2),
                starred_at: None,
                musicbrainz_artist_id: None,
                provider_identity_projected: false,
            },
            ViewArtist {
                artist_mbid: "artist-2".to_owned(),
                name: "Aardvark".to_owned(),
                album_count: Some(0),
                starred_at: None,
                musicbrainz_artist_id: None,
                provider_identity_projected: false,
            },
        ]
    }

    /// Catalog albums.
    pub fn albums() -> Vec<ViewAlbum> {
        vec![
            ViewAlbum {
                rg_mbid: "rg-1".to_owned(),
                title: "First Album".to_owned(),
                artist_name: Some("The Testers".to_owned()),
                artist_mbid: Some("artist-1".to_owned()),
                track_count: Some(2),
                total_duration_seconds: Some(400.0),
                date_added: Some(NOW_UNIX as i64 - 86_400),
                year: Some(2020),
                genre: Some("Rock".to_owned()),
                ..ViewAlbum::default()
            },
            ViewAlbum {
                rg_mbid: "rg-2".to_owned(),
                title: "Second Sounds".to_owned(),
                artist_name: Some("The Testers".to_owned()),
                artist_mbid: Some("artist-1".to_owned()),
                track_count: Some(1),
                total_duration_seconds: Some(180.0),
                date_added: Some(NOW_UNIX as i64 - 100),
                year: Some(2021),
                genre: Some("Rock".to_owned()),
                ..ViewAlbum::default()
            },
        ]
    }

    /// Catalog tracks.
    pub fn tracks() -> Vec<ViewTrack> {
        vec![
            ViewTrack {
                file_id: "f1".to_owned(),
                title: "Song One".to_owned(),
                rg_mbid: Some("rg-1".to_owned()),
                album_title: Some("First Album".to_owned()),
                artist_name: "The Testers".to_owned(),
                artist_mbid: Some("artist-1".to_owned()),
                album_artist_name: Some("The Testers".to_owned()),
                album_artist_mbid: Some("artist-1".to_owned()),
                track_number: 1,
                disc_number: 1,
                year: Some(2020),
                genre: Some("Rock".to_owned()),
                file_size_bytes: 100,
                file_format: Some("mp3".to_owned()),
                duration_seconds: 200.0,
                bitrate: Some(320),
                created_at: Some(NOW_UNIX as i64 - 86_400),
                bit_depth: Some(16),
                sample_rate: Some(44_100),
                ..ViewTrack::default()
            },
            ViewTrack {
                file_id: "f2".to_owned(),
                title: "Song Two".to_owned(),
                rg_mbid: Some("rg-1".to_owned()),
                album_title: Some("First Album".to_owned()),
                artist_name: "The Testers".to_owned(),
                artist_mbid: Some("artist-1".to_owned()),
                track_number: 2,
                year: Some(2020),
                genre: Some("Rock;Indie".to_owned()),
                file_size_bytes: 64,
                file_format: Some("flac".to_owned()),
                duration_seconds: 200.0,
                bitrate: Some(900),
                ..ViewTrack::default()
            },
            ViewTrack {
                file_id: "f3".to_owned(),
                title: "Ballad".to_owned(),
                rg_mbid: Some("rg-2".to_owned()),
                album_title: Some("Second Sounds".to_owned()),
                artist_name: "The Testers".to_owned(),
                artist_mbid: Some("artist-1".to_owned()),
                track_number: 1,
                year: Some(2021),
                genre: Some("Rock".to_owned()),
                file_size_bytes: 32,
                file_format: Some("mp3".to_owned()),
                duration_seconds: 180.0,
                bitrate: Some(192),
                ..ViewTrack::default()
            },
        ]
    }
}

/// Case-insensitive substring match (None query matches all).
fn matches(query: Option<&str>, haystacks: &[&str]) -> bool {
    match query {
        None => true,
        Some(query) => {
            let needle = query.to_lowercase();
            haystacks
                .iter()
                .any(|hay| hay.to_lowercase().contains(&needle))
        }
    }
}

impl Store for FakeStore {
    type Error = Infallible;

    async fn get_artists(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> Result<(Vec<ViewArtist>, usize), Infallible> {
        let all: Vec<_> = Self::artists()
            .into_iter()
            .filter(|a| matches(query, &[&a.name]))
            .collect();
        let total = all.len();
        Ok((all.into_iter().skip(offset).take(limit).collect(), total))
    }

    async fn get_library_revision(&self) -> Result<i64, Infallible> {
        Ok(42)
    }

    async fn get_artist_with_albums(
        &self,
        artist_mbid: &str,
    ) -> Result<Option<(ViewArtist, Vec<ViewAlbum>)>, Infallible> {
        Ok(Self::artists()
            .into_iter()
            .find(|a| a.artist_mbid == artist_mbid)
            .map(|artist| {
                let albums = Self::albums()
                    .into_iter()
                    .filter(|album| album.artist_mbid.as_deref() == Some(artist_mbid))
                    .collect();
                (artist, albums)
            }))
    }

    async fn get_album(&self, rg_mbid: &str) -> Result<Option<ViewAlbum>, Infallible> {
        Ok(Self::albums()
            .into_iter()
            .find(|album| album.rg_mbid == rg_mbid))
    }

    async fn get_album_tracks(&self, rg_mbid: &str) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .filter(|track| track.rg_mbid.as_deref() == Some(rg_mbid))
            .collect())
    }

    async fn get_track(&self, file_id: &str) -> Result<Option<ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .find(|track| track.file_id == file_id))
    }

    async fn get_tracks_by_file_ids(
        &self,
        file_ids: &[String],
    ) -> Result<HashMap<String, ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .filter(|track| file_ids.contains(&track.file_id))
            .map(|track| (track.file_id.clone(), track))
            .collect())
    }

    async fn get_tracks_page(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> Result<(Vec<ViewTrack>, usize), Infallible> {
        let all: Vec<_> = Self::tracks()
            .into_iter()
            .filter(|t| matches(query, &[&t.title, &t.artist_name]))
            .collect();
        let total = all.len();
        Ok((all.into_iter().skip(offset).take(limit).collect(), total))
    }

    async fn get_albums_offset(
        &self,
        limit: usize,
        offset: usize,
        sort: AlbumSort,
        from_year: Option<i64>,
        to_year: Option<i64>,
        genre: Option<&str>,
        query: Option<&str>,
    ) -> Result<(Vec<ViewAlbum>, usize), Infallible> {
        let mut all: Vec<_> = Self::albums()
            .into_iter()
            .filter(|album| {
                from_year.is_none_or(|from| album.year.is_some_and(|year| year >= from))
                    && to_year.is_none_or(|to| album.year.is_some_and(|year| year <= to))
                    && genre.is_none_or(|g| {
                        album
                            .genre
                            .as_deref()
                            .is_some_and(|ag| ag.eq_ignore_ascii_case(g))
                    })
                    && matches(query, &[&album.title])
            })
            .collect();
        match sort {
            AlbumSort::Title => all.sort_by(|a, b| a.title.cmp(&b.title)),
            AlbumSort::Artist => all.sort_by(|a, b| a.artist_name.cmp(&b.artist_name)),
            AlbumSort::YearAsc => all.sort_by_key(|a| a.year.unwrap_or(0)),
            AlbumSort::YearDesc => all.sort_by_key(|a| std::cmp::Reverse(a.year.unwrap_or(0))),
            AlbumSort::Recent => all.sort_by_key(|a| std::cmp::Reverse(a.date_added.unwrap_or(0))),
            AlbumSort::Random => {}
        }
        let total = all.len();
        Ok((all.into_iter().skip(offset).take(limit).collect(), total))
    }

    async fn get_random_songs(
        &self,
        count: usize,
        genre: Option<&str>,
        from_year: Option<i64>,
        to_year: Option<i64>,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .filter(|track| {
                genre.is_none_or(|g| {
                    track
                        .genre
                        .as_deref()
                        .is_some_and(|tg| tg.eq_ignore_ascii_case(g))
                }) && from_year.is_none_or(|from| track.year.is_some_and(|y| y >= from))
                    && to_year.is_none_or(|to| track.year.is_some_and(|y| y <= to))
            })
            .take(count)
            .collect())
    }

    async fn get_history_albums(
        &self,
        _user_id: &str,
        _frequent: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewAlbum>, Infallible> {
        Ok(Self::albums()
            .into_iter()
            .skip(offset)
            .take(limit)
            .collect())
    }

    async fn get_starred_albums(
        &self,
        _user_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewAlbum>, Infallible> {
        let starred: Vec<String> = self
            .lock()
            .favorites
            .iter()
            .filter(|(kind, _)| *kind == IdKind::Album)
            .map(|(_, id)| id.clone())
            .collect();
        Ok(Self::albums()
            .into_iter()
            .filter(|album| starred.contains(&album.rg_mbid))
            .skip(offset)
            .take(limit)
            .collect())
    }

    async fn get_genres(&self) -> Result<Vec<ViewGenre>, Infallible> {
        Ok(vec![
            ViewGenre {
                name: "Rock".to_owned(),
                song_count: 3,
                album_count: 2,
            },
            ViewGenre {
                name: "Indie".to_owned(),
                song_count: 1,
                album_count: 1,
            },
        ])
    }

    async fn get_songs_by_genre(
        &self,
        genre: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .filter(|track| {
                track.genre.as_deref().is_some_and(|g| {
                    g.split(';')
                        .any(|part| part.trim().eq_ignore_ascii_case(genre))
                })
            })
            .skip(offset)
            .take(limit)
            .collect())
    }

    async fn missing_targets(
        &self,
        targets: &[(IdKind, String)],
    ) -> Result<Vec<(IdKind, String)>, Infallible> {
        let artists = Self::artists();
        let albums = Self::albums();
        let tracks = Self::tracks();
        Ok(targets
            .iter()
            .filter(|(kind, id)| match kind {
                IdKind::Artist => !artists.iter().any(|a| &a.artist_mbid == id),
                IdKind::Album => !albums.iter().any(|a| &a.rg_mbid == id),
                IdKind::Track => !tracks.iter().any(|t| &t.file_id == id),
                IdKind::Playlist => !self
                    .lock()
                    .playlists
                    .iter()
                    .any(|(record, ..)| &record.id == id),
                IdKind::Genre => false,
            })
            .cloned()
            .collect())
    }

    async fn get_top_songs(
        &self,
        artist: &str,
        _user_id: &str,
        count: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Self::tracks()
            .into_iter()
            .filter(|track| track.artist_name.eq_ignore_ascii_case(artist))
            .take(count)
            .collect())
    }

    async fn get_similar_songs(
        &self,
        _artist_mbid: &str,
        _user_id: &str,
        count: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Self::tracks().into_iter().take(count).collect())
    }

    async fn get_all_playlists(&self, _user_id: &str) -> Result<Vec<PlaylistSummary>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .iter()
            .map(|(record, _, is_owner, owner_name)| PlaylistSummary {
                record: Some(record.clone()),
                is_owner: *is_owner,
                owner_name: owner_name.clone(),
            })
            .collect())
    }

    async fn get_streamable_counts(&self) -> Result<HashMap<String, (i64, i64)>, Infallible> {
        let inner = self.lock();
        let mut counts = HashMap::new();
        for (record, entries, _, _) in &inner.playlists {
            let mut count = 0i64;
            let mut duration = 0i64;
            for entry in entries {
                if let Some(fid) = entry.library_file_id.as_deref()
                    && let Some(track) = Self::tracks().into_iter().find(|t| t.file_id == fid)
                {
                    count += 1;
                    duration += track.duration_seconds.round() as i64;
                }
            }
            counts.insert(record.id.clone(), (count, duration));
        }
        Ok(counts)
    }

    async fn get_playlist_with_tracks(
        &self,
        playlist_id: &str,
        _user_id: &str,
    ) -> Result<Option<PlaylistDetail>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .iter()
            .find(|(record, ..)| record.id == playlist_id)
            .map(|(record, tracks, is_owner, owner_name)| PlaylistDetail {
                record: record.clone(),
                tracks: tracks.clone(),
                is_owner: *is_owner,
                owner_name: owner_name.clone(),
            }))
    }

    async fn get_playlist_tracks(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistEntry>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .iter()
            .find(|(record, ..)| record.id == playlist_id)
            .map(|(_, tracks, ..)| tracks.clone())
            .unwrap_or_default())
    }

    async fn create_playlist(
        &self,
        name: &str,
        _user_id: &str,
    ) -> Result<PlaylistRecord, Infallible> {
        let mut inner = self.lock();
        let id = format!("p{}", inner.next_playlist);
        inner.next_playlist += 1;
        let record = PlaylistRecord {
            id: id.clone(),
            name: name.to_owned(),
            is_public: false,
            has_cover: false,
            created_at: Some("2023-11-14T22:13:20Z".to_owned()),
            changed_at: Some("2023-11-14T22:13:20Z".to_owned()),
        };
        inner
            .playlists
            .push((record.clone(), Vec::new(), true, USERNAME.to_owned()));
        Ok(record)
    }

    async fn update_playlist(
        &self,
        playlist_id: &str,
        _user_id: &str,
        name: &str,
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        if let Some((record, ..)) = inner
            .playlists
            .iter_mut()
            .find(|(record, ..)| record.id == playlist_id)
        {
            record.name = name.to_owned();
        }
        Ok(())
    }

    async fn set_playlist_public(
        &self,
        playlist_id: &str,
        _user_id: &str,
        public: bool,
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        if let Some((record, ..)) = inner
            .playlists
            .iter_mut()
            .find(|(record, ..)| record.id == playlist_id)
        {
            record.is_public = public;
        }
        Ok(())
    }

    async fn remove_playlist_tracks(
        &self,
        playlist_id: &str,
        _user_id: &str,
        entry_ids: &[String],
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        if let Some((_, tracks, ..)) = inner
            .playlists
            .iter_mut()
            .find(|(record, ..)| record.id == playlist_id)
        {
            tracks.retain(|entry| !entry_ids.contains(&entry.id));
        }
        Ok(())
    }

    async fn add_playlist_file(
        &self,
        playlist_id: &str,
        file_id: &str,
        _user_id: &str,
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        let entry_id = format!("e{}", inner.next_entry);
        inner.next_entry += 1;
        if let Some((_, tracks, ..)) = inner
            .playlists
            .iter_mut()
            .find(|(record, ..)| record.id == playlist_id)
        {
            tracks.push(PlaylistEntry {
                id: entry_id,
                library_file_id: Some(file_id.to_owned()),
            });
        }
        Ok(())
    }

    async fn delete_playlist(&self, playlist_id: &str, _user_id: &str) -> Result<(), Infallible> {
        self.lock()
            .playlists
            .retain(|(record, ..)| record.id != playlist_id);
        Ok(())
    }

    async fn playlist_cover(
        &self,
        playlist_id: &str,
        _user_id: &str,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        let inner = self.lock();
        let has_cover = inner
            .playlists
            .iter()
            .find(|(record, ..)| record.id == playlist_id)
            .is_some_and(|(record, ..)| record.has_cover);
        Ok(has_cover.then(|| (b"PLAYLIST-COVER".to_vec(), "image/jpeg".to_owned())))
    }

    async fn apply_favorites(
        &self,
        _user_id: &str,
        targets: &[(IdKind, String)],
        add: bool,
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        for target in targets {
            if add {
                if !inner.favorites.contains(target) {
                    inner.favorites.push(target.clone());
                }
            } else {
                inner.favorites.retain(|fav| fav != target);
            }
        }
        Ok(())
    }

    async fn list_favorites(
        &self,
        _user_id: &str,
        kind: IdKind,
    ) -> Result<Vec<(String, i64)>, Infallible> {
        Ok(self
            .lock()
            .favorites
            .iter()
            .filter(|(fav_kind, _)| *fav_kind == kind)
            .map(|(_, id)| (id.clone(), NOW_UNIX as i64 - 100))
            .collect())
    }

    async fn scrobble(
        &self,
        file_id: &str,
        _user_id: &str,
        _client: Option<&str>,
        played_at: Option<f64>,
        _user_name: &str,
    ) -> Result<(), Infallible> {
        self.lock().scrobbles.push((file_id.to_owned(), played_at));
        Ok(())
    }

    async fn now_playing(
        &self,
        file_id: &str,
        _user_id: &str,
        _client: Option<&str>,
        _user_name: &str,
    ) -> Result<(), Infallible> {
        self.lock().presence.push(file_id.to_owned());
        Ok(())
    }

    async fn compat_now_playing(&self) -> Result<Vec<NowPlayingRow>, Infallible> {
        Ok(vec![NowPlayingRow {
            user_name: "user Display".to_owned(),
            file_id: "f2".to_owned(),
            updated_at: NOW_UNIX - 120.0,
            source: Some("Symfonium".to_owned()),
            device_name: Some("phone".to_owned()),
        }])
    }

    async fn report_playback(
        &self,
        file_id: &str,
        _user_id: &str,
        _user_name: &str,
        _client: &str,
        position_ms: i64,
        state: &str,
        _ignore_scrobble: bool,
    ) -> Result<(), Infallible> {
        self.lock()
            .reports
            .push(format!("{file_id}@{position_ms}:{state}"));
        Ok(())
    }

    async fn get_play_queue(&self, _user_id: &str) -> Result<QueueState, Infallible> {
        Ok(self.lock().queue.clone())
    }

    async fn replace_play_queue(
        &self,
        _user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
    ) -> Result<(), Infallible> {
        self.lock().queue = QueueState {
            file_ids: file_ids.to_vec(),
            current_index,
            position_ms,
            updated_at: NOW_UNIX,
            changed_by_client: changed_by_client.to_owned(),
        };
        Ok(())
    }

    async fn list_bookmarks(&self, _user_id: &str) -> Result<Vec<BookmarkRecord>, Infallible> {
        Ok(self.lock().bookmarks.clone())
    }

    async fn upsert_bookmark(
        &self,
        _user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
    ) -> Result<(), Infallible> {
        let mut inner = self.lock();
        if let Some(existing) = inner.bookmarks.iter_mut().find(|b| b.file_id == file_id) {
            existing.position_ms = position_ms;
            existing.comment = comment.to_owned();
            existing.changed_at = NOW_UNIX as i64;
        } else {
            inner.bookmarks.push(BookmarkRecord {
                file_id: file_id.to_owned(),
                position_ms,
                comment: comment.to_owned(),
                created_at: NOW_UNIX as i64,
                changed_at: NOW_UNIX as i64,
            });
        }
        Ok(())
    }

    async fn delete_bookmark(&self, _user_id: &str, file_id: &str) -> Result<(), Infallible> {
        self.lock().bookmarks.retain(|b| b.file_id != file_id);
        Ok(())
    }

    async fn get_lyrics(&self, file_id: &str) -> Result<Option<LyricsData>, Infallible> {
        if file_id != "f1" {
            return Ok(None);
        }
        Ok(Some(LyricsData {
            language: "eng".to_owned(),
            synced: true,
            lines: vec![
                super::store::LyricLine {
                    value: "First line".to_owned(),
                    start_ms: Some(0),
                },
                super::store::LyricLine {
                    value: "Second line".to_owned(),
                    start_ms: Some(2000),
                },
            ],
        }))
    }

    async fn resolve_avatar(&self, user_id: &str) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        if user_id == USER_ID {
            Ok(Some((b"AVATAR-BYTES".to_vec(), "image/png".to_owned())))
        } else {
            Ok(None)
        }
    }

    async fn scan_status(&self) -> Result<(bool, Option<i64>), Infallible> {
        Ok((self.lock().scanning, Some(3)))
    }

    async fn start_scan(&self) -> Result<(), Infallible> {
        self.lock().scanning = true;
        Ok(())
    }

    async fn release_group_cover(
        &self,
        rg_mbid: &str,
        bucket: &str,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        if rg_mbid == "rg-1" {
            Ok(Some((
                format!("COVER-{bucket}").into_bytes(),
                "image/jpeg".to_owned(),
            )))
        } else {
            Ok(None)
        }
    }

    async fn artist_image(
        &self,
        artist_mbid: &str,
        px: Option<i64>,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        if artist_mbid == "artist-1" {
            Ok(Some((
                format!("ARTIST-{}", px.unwrap_or(0)).into_bytes(),
                "image/jpeg".to_owned(),
            )))
        } else {
            Ok(None)
        }
    }

    async fn advanced_decide(
        &self,
        _track: &ViewTrack,
        _client: &ClientInfo,
        _user_id: &str,
    ) -> Result<TranscodeDecisionData, Infallible> {
        Ok(TranscodeDecisionData {
            can_direct_play: true,
            can_transcode: true,
            transcode_reason: vec!["codec".to_owned()],
            error_reason: None,
            transcode_params: Some("signed-params".to_owned()),
            source_stream: Some(StreamDetailsData {
                protocol: "file".to_owned(),
                container: "flac".to_owned(),
                codec: "flac".to_owned(),
                audio_channels: Some(2),
                audio_bitrate: Some(900),
                audio_samplerate: Some(44_100),
                audio_bitdepth: Some(16),
            }),
            transcode_stream: Some(StreamDetailsData {
                protocol: "http".to_owned(),
                container: "mp3".to_owned(),
                codec: "mp3".to_owned(),
                audio_channels: Some(2),
                audio_bitrate: Some(128),
                audio_samplerate: Some(44_100),
                audio_bitdepth: None,
            }),
        })
    }

    async fn decode_transcode_params(
        &self,
        params: &str,
        _user_id: &str,
        _file_id: &str,
    ) -> Result<(bool, Option<String>, Option<i64>), Infallible> {
        match params {
            "direct-params" => Ok((true, None, None)),
            "signed-params" => Ok((false, Some("mp3".to_owned()), Some(128))),
            _ => Ok((false, None, None)),
        }
    }
}

/// Fake audio backend: f1 = 100 mp3 bytes (0..100), f2 = 64 flac
/// bytes, f3 = 32 mp3 bytes. Transcodes answer canned bytes.
#[derive(Debug, Clone, Default)]
pub struct FakeAudio;

impl FakeAudio {
    /// File bytes for a file id.
    pub fn bytes(file_id: &str) -> Option<Vec<u8>> {
        match file_id {
            "f1" => Some((0..100u8).collect()),
            "f2" => Some(vec![7u8; 64]),
            "f3" => Some(vec![9u8; 32]),
            _ => None,
        }
    }
}

impl AudioBackend for FakeAudio {
    async fn audio_facts(&self, file_id: &str) -> Result<Option<AudioFacts>, BackendError> {
        Ok(Self::bytes(file_id).map(|bytes| {
            let (suffix, bitrate) = match file_id {
                "f2" => ("flac".to_owned(), Some(900)),
                _ => ("mp3".to_owned(), Some(320)),
            };
            AudioFacts {
                size: bytes.len() as u64,
                suffix,
                bitrate_kbps: bitrate,
                duration_seconds: Some(200.0),
            }
        }))
    }

    async fn read_range(
        &self,
        file_id: &str,
        start: u64,
        end: u64,
    ) -> Result<Vec<u8>, BackendError> {
        Self::bytes(file_id)
            .map(|bytes| bytes[start as usize..=end as usize].to_vec())
            .ok_or_else(|| BackendError::failed(format!("no audio for {file_id}")))
    }

    async fn transcode(
        &self,
        file_id: &str,
        plan: &StreamPlan,
    ) -> Result<(Vec<u8>, String), BackendError> {
        if Self::bytes(file_id).is_none() {
            return Err(BackendError::failed(format!("no audio for {file_id}")));
        }
        let content_type = match plan.out_format.as_deref() {
            Some("opus") => "audio/ogg",
            _ => "audio/mpeg",
        };
        Ok((b"TRANSCODED".to_vec(), content_type.to_owned()))
    }
}
