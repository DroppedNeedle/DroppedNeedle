//! The production Jellyfin library and id map.
//!
//! [`JellyfinLibrary`] serves the Jellyfin routes from the v3 catalog and
//! the shared collections. The routes filter, sort and page the snapshot
//! they get, as the seam requires; reads carry the caller's favorites and
//! play counts.
//!
//! [`CatalogIds`] keeps v2's deterministic `sha256("kind:internal")[:32]`
//! ids. Because an id is a pure function of what it names, nothing needs
//! storing: an id a client cached before a restart resolves again by
//! deriving the ids of everything the library holds.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::library::{CompatError, CompatLibrary};
use crate::compat::jellyfin::builders::LIBRARY_INTERNAL_ID;
use crate::compat::jellyfin::seams::{
    AlbumView, ArtistScope, ArtistView, CoverBytes, GenreView, IdMap, LibraryRead, PlaylistDetail,
    PlaylistEntry, PlaylistView, TrackView,
};
use crate::compat::subsonic::views::genre_slug;
use crate::reads::library::player::{
    AlbumOrder, AlbumQuery, PlayerAlbum, PlayerTrack, TrackOrder, TrackQuery,
};
use crate::reads::library::stores::{ArtistRecord, ArtistScope as CatalogScope};

/// Shortest gap between two full id rebuilds after a miss.
const REBUILD_COOLDOWN: Duration = Duration::from_secs(10);

/// Log a failed read and fall back to the empty answer the seam allows.
fn logged<T: Default>(operation: &str, outcome: Result<T, CompatError>) -> T {
    outcome.unwrap_or_else(|error| {
        tracing::error!(operation, %error, "jellyfin library read failed");
        T::default()
    })
}

fn to_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

fn track_view(track: PlayerTrack) -> TrackView {
    TrackView {
        file_id: track.id,
        title: track.title,
        duration_seconds: track.duration_seconds,
        year: track.year.and_then(|year| i32::try_from(year).ok()),
        track_number: i32::try_from(track.track_number).ok().filter(|n| *n > 0),
        disc_number: i32::try_from(track.disc_number).ok().filter(|n| *n > 0),
        album_title: Some(track.album_title),
        rg_mbid: Some(track.album_id),
        artist_name: Some(track.artist_name).filter(|name| !name.is_empty()),
        artist_mbid: track.artist_id,
        album_artist_name: track.album_artist_name,
        album_artist_mbid: Some(track.album_artist_id),
        genre: track.genres.first().cloned(),
        file_format: Some(track.format),
        bitrate: to_u32(track.bit_rate),
        channels: to_u32(track.channels),
        sample_rate: to_u32(track.sample_rate),
        bit_depth: to_u32(track.bit_depth),
        file_size_bytes: u64::try_from(track.file_size_bytes).ok(),
        created_at: track.date_added,
        recording_mbid: track.recording_mbid,
        starred: false,
        play_count: 0,
        last_played: None,
    }
}

fn album_view(album: PlayerAlbum) -> AlbumView {
    let record = album.record;
    AlbumView {
        rg_mbid: record.id,
        title: record.title,
        artist_name: Some(record.artist_name),
        artist_mbid: Some(record.artist_id),
        year: record.year.and_then(|year| i32::try_from(year).ok()),
        genre: album.genre,
        track_count: record.track_count as usize,
        total_duration_seconds: Some(record.total_duration_seconds),
        date_added: record.date_added,
        starred: false,
        play_count: 0,
        last_played: None,
    }
}

fn artist_view(artist: ArtistRecord) -> ArtistView {
    ArtistView {
        artist_mbid: artist.id,
        name: artist.name,
        album_count: artist.album_count as usize,
        date_added: artist.date_added,
        starred: false,
    }
}

/// Short stable etag for an image key.
fn tag_for(key: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(key.as_bytes()))[..16].to_owned()
}

/// Jellyfin library over the v3 catalog (see module docs).
#[derive(Clone)]
pub struct JellyfinLibrary {
    library: CompatLibrary,
}

impl JellyfinLibrary {
    /// Library over the shared compat reads.
    pub fn new(library: CompatLibrary) -> Self {
        Self { library }
    }

    async fn overlay_tracks(
        &self,
        user_id: &str,
        mut views: Vec<TrackView>,
    ) -> Result<Vec<TrackView>, CompatError> {
        let ids = views
            .iter()
            .map(|view| view.file_id.clone())
            .collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "track", &ids).await?;
        let plays = self.library.plays(user_id, "track", &ids).await?;
        for view in &mut views {
            view.starred = starred.contains_key(&view.file_id);
            if let Some((count, last)) = plays.get(&view.file_id) {
                view.play_count = *count;
                view.last_played = *last;
            }
        }
        Ok(views)
    }

    async fn overlay_albums(
        &self,
        user_id: &str,
        mut views: Vec<AlbumView>,
    ) -> Result<Vec<AlbumView>, CompatError> {
        let ids = views
            .iter()
            .map(|view| view.rg_mbid.clone())
            .collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "album", &ids).await?;
        let plays = self.library.plays(user_id, "album", &ids).await?;
        for view in &mut views {
            view.starred = starred.contains_key(&view.rg_mbid);
            if let Some((count, last)) = plays.get(&view.rg_mbid) {
                view.play_count = *count;
                view.last_played = *last;
            }
        }
        Ok(views)
    }

    async fn overlay_artists(
        &self,
        user_id: &str,
        mut views: Vec<ArtistView>,
    ) -> Result<Vec<ArtistView>, CompatError> {
        let ids = views
            .iter()
            .map(|view| view.artist_mbid.clone())
            .collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "artist", &ids).await?;
        for view in &mut views {
            view.starred = starred.contains_key(&view.artist_mbid);
        }
        Ok(views)
    }

    async fn all_tracks(&self, user_id: &str) -> Result<Vec<TrackView>, CompatError> {
        let (tracks, _) = self
            .library
            .tracks(&TrackQuery::default(), TrackOrder::Album, u64::MAX >> 1, 0)
            .await?;
        self.overlay_tracks(user_id, tracks.into_iter().map(track_view).collect())
            .await
    }

    async fn all_albums(&self, user_id: &str) -> Result<Vec<AlbumView>, CompatError> {
        let (albums, _) = self
            .library
            .albums(&AlbumQuery::default(), AlbumOrder::Title, u64::MAX >> 1, 0)
            .await?;
        self.overlay_albums(user_id, albums.into_iter().map(album_view).collect())
            .await
    }

    async fn readable_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<PlaylistDetail>, CompatError> {
        let Some(visible) = self.library.playlist(user_id, playlist_id).await? else {
            return Ok(None);
        };
        let entries = self
            .library
            .playlist_entries(playlist_id)
            .await?
            .into_iter()
            .map(|(id, file_id)| PlaylistEntry { id, file_id })
            .collect();
        Ok(Some(PlaylistDetail {
            id: visible.row.id,
            name: visible.row.name,
            entries,
        }))
    }

    async fn write(&self, operation: &str, outcome: impl Future<Output = Result<(), CompatError>>) {
        if let Err(error) = outcome.await {
            tracing::warn!(operation, %error, "jellyfin library write refused");
        }
    }
}

impl LibraryRead for JellyfinLibrary {
    async fn tracks(&self, user_id: &str) -> Vec<TrackView> {
        logged("tracks", self.all_tracks(user_id).await)
    }

    async fn track(&self, user_id: &str, file_id: &str) -> Option<TrackView> {
        let outcome = async {
            let Some(track) = self.library.track(file_id).await? else {
                return Ok(None);
            };
            Ok(self
                .overlay_tracks(user_id, vec![track_view(track)])
                .await?
                .pop())
        }
        .await;
        logged("track", outcome)
    }

    async fn albums(&self, user_id: &str) -> Vec<AlbumView> {
        logged("albums", self.all_albums(user_id).await)
    }

    async fn album(&self, user_id: &str, rg_mbid: &str) -> Option<AlbumView> {
        let outcome = async {
            let Some(album) = self.library.album(rg_mbid).await? else {
                return Ok(None);
            };
            if album.record.track_count == 0 {
                return Ok(None);
            }
            Ok(self
                .overlay_albums(user_id, vec![album_view(album)])
                .await?
                .pop())
        }
        .await;
        logged("album", outcome)
    }

    async fn artists(&self, user_id: &str, scope: ArtistScope) -> Vec<ArtistView> {
        let outcome = async {
            let scope = match scope {
                ArtistScope::All => CatalogScope::All,
                ArtistScope::Album => CatalogScope::AlbumArtists,
            };
            let artists = self.library.all_artists(scope).await?;
            self.overlay_artists(user_id, artists.into_iter().map(artist_view).collect())
                .await
        }
        .await;
        logged("artists", outcome)
    }

    async fn artist(&self, user_id: &str, mbid: &str) -> Option<ArtistView> {
        let outcome = async {
            let Some(artist) = self.library.artist(mbid).await? else {
                return Ok(None);
            };
            Ok(self
                .overlay_artists(user_id, vec![artist_view(artist)])
                .await?
                .pop())
        }
        .await;
        logged("artist", outcome)
    }

    async fn genres(&self) -> Vec<GenreView> {
        let outcome = self.library.genres().await.map(|genres| {
            genres
                .into_iter()
                .map(|genre| GenreView {
                    name: genre.name,
                    song_count: genre.track_count as usize,
                })
                .collect()
        });
        logged("genres", outcome)
    }

    async fn playlists(&self, user_id: &str) -> Vec<PlaylistView> {
        let outcome = self.library.playlists(user_id).await.map(|visible| {
            visible
                .into_iter()
                .filter(|visible| visible.full)
                .map(|visible| PlaylistView {
                    id: visible.row.id,
                    name: visible.row.name,
                    track_count: visible.row.streamable_count,
                    total_duration_seconds: (visible.row.streamable_duration > 0.0)
                        .then_some(visible.row.streamable_duration),
                })
                .collect()
        });
        logged("playlists", outcome)
    }

    async fn playlist(&self, user_id: &str, id: &str) -> Option<PlaylistDetail> {
        logged("playlist", self.readable_entries(user_id, id).await)
    }

    async fn create_playlist(&self, user_id: &str, name: &str) -> String {
        logged(
            "create_playlist",
            self.library.create_playlist(user_id, name).await,
        )
    }

    async fn add_playlist_entry(&self, user_id: &str, playlist_id: &str, file_id: &str) {
        self.write(
            "add_playlist_entry",
            self.library
                .add_files(user_id, playlist_id, &[file_id.to_owned()]),
        )
        .await;
    }

    async fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) {
        self.write(
            "remove_playlist_entries",
            self.library.remove_entries(user_id, playlist_id, entry_ids),
        )
        .await;
    }

    async fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) {
        self.write(
            "move_playlist_entry",
            self.library
                .move_entry(user_id, playlist_id, entry_id, index),
        )
        .await;
    }

    async fn favorites(&self, user_id: &str, kind: &str) -> Vec<String> {
        let outcome = self
            .library
            .favorite_ids(user_id, kind)
            .await
            .map(|ids| ids.into_iter().map(|(id, _)| id).collect());
        logged("favorites", outcome)
    }

    async fn set_favorite(&self, user_id: &str, kind: &str, internal: &str, add: bool) {
        self.write(
            "set_favorite",
            self.library
                .set_favorites(user_id, &[(kind.to_owned(), internal.to_owned())], add),
        )
        .await;
    }

    async fn cover(&self, rg_mbid: &str, size: &str) -> Option<CoverBytes> {
        let outcome = self.library.album_cover(rg_mbid, Some(size)).await;
        logged("cover", outcome).map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        })
    }

    async fn artist_image(&self, mbid: &str) -> Option<CoverBytes> {
        let outcome = self.library.artist_cover(mbid, None).await;
        logged("artist_image", outcome).map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        })
    }

    async fn cover_tag(&self, rg_mbid: &str) -> Option<String> {
        let outcome = self.library.album(rg_mbid).await;
        logged("cover_tag", outcome)
            .filter(|album| {
                album.record.cover_available || album.record.release_group_mbid.is_some()
            })
            .map(|album| tag_for(&format!("album:{}", album.record.id)))
    }

    async fn artist_tag(&self, mbid: &str) -> Option<String> {
        let outcome = self.library.artist(mbid).await;
        logged("artist_tag", outcome)
            .filter(|artist| artist.artist_mbid.is_some())
            .map(|artist| tag_for(&format!("artist:{}", artist.id)))
    }
}

#[derive(Default)]
struct IdTable {
    reverse: HashMap<String, (String, String)>,
    rebuilt_at: Option<Instant>,
}

/// Deterministic Jellyfin ids over the catalog (see module docs).
#[derive(Clone)]
pub struct CatalogIds {
    library: CompatLibrary,
    table: Arc<Mutex<IdTable>>,
}

impl CatalogIds {
    /// Ids over the shared compat reads.
    pub fn new(library: CompatLibrary) -> Self {
        Self {
            library,
            table: Arc::new(Mutex::new(IdTable::default())),
        }
    }

    /// The v2 derivation: first 32 hex chars of `sha256("kind:internal")`.
    pub fn derive(kind: &str, internal: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!("{kind}:{internal}").as_bytes())
        )[..32]
            .to_owned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, IdTable> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lookup(&self, jf_id: &str) -> Option<(String, String)> {
        self.lock().reverse.get(jf_id).cloned()
    }

    /// Derive the id of everything addressable and remember the reverse
    /// mapping. Rate-limited so a stream of unknown ids cannot turn into a
    /// stream of full catalog reads.
    async fn rebuild(&self) {
        {
            let table = self.lock();
            if table
                .rebuilt_at
                .is_some_and(|at| at.elapsed() < REBUILD_COOLDOWN)
            {
                return;
            }
        }
        let mut pairs: Vec<(String, String)> =
            vec![("library".to_owned(), LIBRARY_INTERNAL_ID.to_owned())];
        match self.library.player.every_id().await {
            Ok(ids) => {
                for (kind, id) in ids {
                    let internal = if kind == "genre" { genre_slug(&id) } else { id };
                    pairs.push((kind.to_owned(), internal));
                }
            }
            Err(error) => tracing::error!(%error, "jellyfin id rebuild could not read the catalog"),
        }
        match self.library.collections.stores.playlists.list().await {
            Ok(rows) => pairs.extend(rows.into_iter().map(|row| ("playlist".to_owned(), row.id))),
            Err(error) => tracing::error!(%error, "jellyfin id rebuild could not read playlists"),
        }
        let mut seen = HashSet::new();
        let mut table = self.lock();
        for (kind, internal) in pairs {
            if seen.insert((kind.clone(), internal.clone())) {
                table
                    .reverse
                    .insert(Self::derive(&kind, &internal), (kind, internal));
            }
        }
        table.rebuilt_at = Some(Instant::now());
    }
}

impl IdMap for CatalogIds {
    async fn to_jf(&self, kind: &str, internal: &str) -> String {
        let jf_id = Self::derive(kind, internal);
        self.lock()
            .reverse
            .entry(jf_id.clone())
            .or_insert_with(|| (kind.to_owned(), internal.to_owned()));
        jf_id
    }

    async fn from_jf(&self, jf_id: &str) -> Option<(String, String)> {
        let normalized = jf_id.replace('-', "").trim().to_lowercase();
        if let Some(found) = self.lookup(&normalized) {
            return Some(found);
        }
        self.rebuild().await;
        self.lookup(&normalized)
    }
}
