//! Honest-memory production store: user mutations round-trip in memory,
//! catalog reads are empty until the v3 catalog join lands.
//!
//! Playlists, favorites, queues, and bookmarks work within the process
//! (the collections in-memory precedent). Playback reports and presence
//! are REAL (stage-6 services); scan status/trigger and avatars are REAL
//! (library + users deps). Catalog reads (artists, albums, tracks, genres,
//! search, lyrics, covers) return empty: v3's scan-to-catalog publish path
//! has not landed, so there is nothing to serve yet, and serving fixture
//! data in production would be a lie. The catalog join is the recorded
//! stage-9 follow-up.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use crate::auth::users::UsersDeps;
use crate::compat::adapters::playback::CompatPlayback;
use crate::compat::adapters::queues::CompatQueues;
use crate::compat::subsonic::ids::IdKind;
use crate::compat::subsonic::store::{
    AlbumSort, BookmarkRecord, ClientInfo, LyricsData, NowPlayingRow, PlaylistDetail,
    PlaylistEntry, PlaylistRecord, PlaylistSummary, QueueState, Store, TranscodeDecisionData,
};
use crate::compat::subsonic::views::{ViewAlbum, ViewArtist, ViewGenre, ViewTrack};
use crate::library::scan::counter_names;
use crate::library::scan::models::{ScanKind, ScanRequest, ScanTrigger};
use crate::library::wiring::LibrarySetup;

#[derive(Debug, Default)]
struct MemoryInner {
    playlists: HashMap<String, (PlaylistRecord, Vec<PlaylistEntry>, String)>,
    favorites: HashMap<(String, String, String), i64>,
    next_playlist: u64,
    next_entry: u64,
}

/// Production store until the catalog join lands (see module docs).
#[derive(Clone)]
pub struct MemoryStore {
    inner: Arc<Mutex<MemoryInner>>,
    queues: CompatQueues,
    playback: CompatPlayback,
    users: UsersDeps,
    scan: LibrarySetup,
}

impl MemoryStore {
    /// Wrap the live deps; library state starts empty.
    pub fn new(
        queues: CompatQueues,
        playback: CompatPlayback,
        users: UsersDeps,
        scan: LibrarySetup,
    ) -> Self {
        Self {
            inner: Arc::new(Mutex::new(MemoryInner::default())),
            queues,
            playback,
            users,
            scan,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MemoryInner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn now_f64() -> f64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_secs_f64())
            .unwrap_or(0.0)
    }

    fn kind_name(kind: IdKind) -> &'static str {
        match kind {
            IdKind::Artist => "artist",
            IdKind::Album => "album",
            IdKind::Track => "track",
            IdKind::Playlist => "playlist",
            IdKind::Genre => "genre",
        }
    }
}

impl Store for MemoryStore {
    type Error = Infallible;

    async fn get_artists(
        &self,
        _limit: usize,
        _offset: usize,
        _query: Option<&str>,
    ) -> Result<(Vec<ViewArtist>, usize), Infallible> {
        Ok((Vec::new(), 0))
    }

    async fn get_library_revision(&self) -> Result<i64, Infallible> {
        Ok(0)
    }

    async fn get_artist_with_albums(
        &self,
        _artist_mbid: &str,
    ) -> Result<Option<(ViewArtist, Vec<ViewAlbum>)>, Infallible> {
        Ok(None)
    }

    async fn get_album(&self, _rg_mbid: &str) -> Result<Option<ViewAlbum>, Infallible> {
        Ok(None)
    }

    async fn get_album_tracks(&self, _rg_mbid: &str) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_track(&self, _file_id: &str) -> Result<Option<ViewTrack>, Infallible> {
        Ok(None)
    }

    async fn get_tracks_by_file_ids(
        &self,
        _file_ids: &[String],
    ) -> Result<HashMap<String, ViewTrack>, Infallible> {
        Ok(HashMap::new())
    }

    async fn get_tracks_page(
        &self,
        _limit: usize,
        _offset: usize,
        _query: Option<&str>,
    ) -> Result<(Vec<ViewTrack>, usize), Infallible> {
        Ok((Vec::new(), 0))
    }

    async fn get_albums_offset(
        &self,
        _limit: usize,
        _offset: usize,
        _sort: AlbumSort,
        _from_year: Option<i64>,
        _to_year: Option<i64>,
        _genre: Option<&str>,
        _query: Option<&str>,
    ) -> Result<(Vec<ViewAlbum>, usize), Infallible> {
        Ok((Vec::new(), 0))
    }

    async fn get_random_songs(
        &self,
        _count: usize,
        _genre: Option<&str>,
        _from_year: Option<i64>,
        _to_year: Option<i64>,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_history_albums(
        &self,
        _user_id: &str,
        _frequent: bool,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<ViewAlbum>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_starred_albums(
        &self,
        _user_id: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<ViewAlbum>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_genres(&self) -> Result<Vec<ViewGenre>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_songs_by_genre(
        &self,
        _genre: &str,
        _limit: usize,
        _offset: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Vec::new())
    }

    async fn missing_targets(
        &self,
        targets: &[(IdKind, String)],
    ) -> Result<Vec<(IdKind, String)>, Infallible> {
        // The catalog is empty, so every library target misses; memory
        // playlists resolve.
        let guard = self.lock();
        Ok(targets
            .iter()
            .filter(|(kind, id)| match kind {
                IdKind::Playlist => !guard.playlists.contains_key(id),
                _ => true,
            })
            .cloned()
            .collect())
    }

    async fn get_top_songs(
        &self,
        _artist: &str,
        _user_id: &str,
        _count: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_similar_songs(
        &self,
        _artist_mbid: &str,
        _user_id: &str,
        _count: usize,
    ) -> Result<Vec<ViewTrack>, Infallible> {
        Ok(Vec::new())
    }

    async fn get_all_playlists(&self, user_id: &str) -> Result<Vec<PlaylistSummary>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .values()
            .map(|(record, _, owner)| {
                let is_owner = owner == user_id;
                PlaylistSummary {
                    record: if is_owner || record.is_public {
                        Some(record.clone())
                    } else {
                        None
                    },
                    is_owner,
                    owner_name: owner.clone(),
                }
            })
            .collect())
    }

    async fn get_streamable_counts(&self) -> Result<HashMap<String, (i64, i64)>, Infallible> {
        // No catalog tracks resolve, so every count is zero.
        Ok(self
            .lock()
            .playlists
            .keys()
            .map(|id| (id.clone(), (0, 0)))
            .collect())
    }

    async fn get_playlist_with_tracks(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> Result<Option<PlaylistDetail>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .get(playlist_id)
            .and_then(|(record, tracks, owner)| {
                if owner != user_id && !record.is_public {
                    return None;
                }
                Some(PlaylistDetail {
                    record: record.clone(),
                    tracks: tracks.clone(),
                    is_owner: owner == user_id,
                    owner_name: owner.clone(),
                })
            }))
    }

    async fn get_playlist_tracks(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistEntry>, Infallible> {
        Ok(self
            .lock()
            .playlists
            .get(playlist_id)
            .map(|(_, tracks, _)| tracks.clone())
            .unwrap_or_default())
    }

    async fn create_playlist(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<PlaylistRecord, Infallible> {
        let mut guard = self.lock();
        guard.next_playlist += 1;
        let record = PlaylistRecord {
            id: format!("pl-{}", guard.next_playlist),
            name: name.to_owned(),
            is_public: false,
            has_cover: false,
            created_at: None,
            changed_at: None,
        };
        guard.playlists.insert(
            record.id.clone(),
            (record.clone(), Vec::new(), user_id.to_owned()),
        );
        Ok(record)
    }

    async fn update_playlist(
        &self,
        playlist_id: &str,
        _user_id: &str,
        name: &str,
    ) -> Result<(), Infallible> {
        if let Some((record, _, _)) = self.lock().playlists.get_mut(playlist_id) {
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
        if let Some((record, _, _)) = self.lock().playlists.get_mut(playlist_id) {
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
        if let Some((_, tracks, _)) = self.lock().playlists.get_mut(playlist_id) {
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
        let mut guard = self.lock();
        guard.next_entry += 1;
        let entry_id = format!("e{}", guard.next_entry);
        if let Some((_, tracks, _)) = guard.playlists.get_mut(playlist_id) {
            tracks.push(PlaylistEntry {
                id: entry_id,
                library_file_id: Some(file_id.to_owned()),
            });
        }
        Ok(())
    }

    async fn delete_playlist(&self, playlist_id: &str, _user_id: &str) -> Result<(), Infallible> {
        self.lock().playlists.remove(playlist_id);
        Ok(())
    }

    async fn playlist_cover(
        &self,
        _playlist_id: &str,
        _user_id: &str,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        Ok(None)
    }

    async fn apply_favorites(
        &self,
        user_id: &str,
        targets: &[(IdKind, String)],
        add: bool,
    ) -> Result<(), Infallible> {
        let mut guard = self.lock();
        let now = Self::now_f64() as i64;
        for (kind, id) in targets {
            let key = (
                user_id.to_owned(),
                Self::kind_name(*kind).to_owned(),
                id.clone(),
            );
            if add {
                guard.favorites.entry(key).or_insert(now);
            } else {
                guard.favorites.remove(&key);
            }
        }
        Ok(())
    }

    async fn list_favorites(
        &self,
        user_id: &str,
        kind: IdKind,
    ) -> Result<Vec<(String, i64)>, Infallible> {
        Ok(self
            .lock()
            .favorites
            .iter()
            .filter(|((user, fav_kind, _), _)| user == user_id && fav_kind == Self::kind_name(kind))
            .map(|((_, _, id), at)| (id.clone(), *at))
            .collect())
    }

    async fn scrobble(
        &self,
        file_id: &str,
        user_id: &str,
        _client: Option<&str>,
        played_at: Option<f64>,
        _user_name: &str,
    ) -> Result<(), Infallible> {
        let _ = self.playback.scrobble(file_id, user_id, played_at);
        Ok(())
    }

    async fn now_playing(
        &self,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        _user_name: &str,
    ) -> Result<(), Infallible> {
        self.playback.now_playing(file_id, user_id, client);
        Ok(())
    }

    async fn compat_now_playing(&self) -> Result<Vec<NowPlayingRow>, Infallible> {
        Ok(self.playback.compat_rows())
    }

    async fn report_playback(
        &self,
        file_id: &str,
        user_id: &str,
        _user_name: &str,
        client: &str,
        _position_ms: i64,
        state: &str,
        ignore_scrobble: bool,
    ) -> Result<(), Infallible> {
        let _ = self
            .playback
            .report_playback(file_id, user_id, client, state, ignore_scrobble);
        Ok(())
    }

    async fn get_play_queue(&self, user_id: &str) -> Result<QueueState, Infallible> {
        Ok(self.queues.get_queue(user_id))
    }

    async fn replace_play_queue(
        &self,
        user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
    ) -> Result<(), Infallible> {
        self.queues.replace_queue(
            user_id,
            file_ids,
            current_index,
            position_ms,
            changed_by_client,
            Self::now_f64(),
        );
        Ok(())
    }

    async fn list_bookmarks(&self, user_id: &str) -> Result<Vec<BookmarkRecord>, Infallible> {
        Ok(self.queues.list_bookmarks(user_id))
    }

    async fn upsert_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
    ) -> Result<(), Infallible> {
        self.queues.upsert_bookmark(
            user_id,
            file_id,
            position_ms,
            comment,
            Self::now_f64() as i64,
        );
        Ok(())
    }

    async fn delete_bookmark(&self, user_id: &str, file_id: &str) -> Result<(), Infallible> {
        self.queues.delete_bookmark(user_id, file_id);
        Ok(())
    }

    async fn get_lyrics(&self, _file_id: &str) -> Result<Option<LyricsData>, Infallible> {
        Ok(None)
    }

    async fn resolve_avatar(&self, user_id: &str) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        Ok(self.users.avatars.load(user_id).await.ok().flatten())
    }

    async fn scan_status(&self) -> Result<(bool, Option<i64>), Infallible> {
        let current = self.scan.coordinator.current();
        if current.is_empty() {
            return Ok((false, None));
        }
        let inspected = current
            .iter()
            .filter_map(|run| run.counters.get(counter_names::INSPECTED))
            .sum();
        Ok((true, Some(inspected)))
    }

    async fn start_scan(&self) -> Result<(), Infallible> {
        let registry = self.scan.live_registry();
        let _ = self.scan.coordinator.request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Subsonic,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: None,
            policy_revision: registry.policy_revision().to_owned(),
        });
        Ok(())
    }

    async fn release_group_cover(
        &self,
        _rg_mbid: &str,
        _bucket: &str,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        Ok(None)
    }

    async fn artist_image(
        &self,
        _artist_mbid: &str,
        _px: Option<i64>,
    ) -> Result<Option<(Vec<u8>, String)>, Infallible> {
        Ok(None)
    }

    async fn advanced_decide(
        &self,
        _track: &ViewTrack,
        _client: &ClientInfo,
        _user_id: &str,
    ) -> Result<TranscodeDecisionData, Infallible> {
        // Sealed advanced-transcode params need the catalog join (source
        // stream facts); the decide-based stream path still transcodes.
        Ok(TranscodeDecisionData {
            can_direct_play: true,
            can_transcode: false,
            ..TranscodeDecisionData::default()
        })
    }

    async fn decode_transcode_params(
        &self,
        _params: &str,
        _user_id: &str,
        _file_id: &str,
    ) -> Result<(bool, Option<String>, Option<i64>), Infallible> {
        Ok((false, None, None))
    }
}
