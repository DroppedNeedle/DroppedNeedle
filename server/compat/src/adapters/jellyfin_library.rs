//! The production Jellyfin library and id map.
//!
//! [`JellyfinLibrary`] serves the Jellyfin routes from the v3 catalog and
//! the shared collections. Every list is one SQL page (filter, order,
//! LIMIT) plus its count; the caller's favorites and play state (plays
//! weighed against the caller's played and unplayed marks) are read for
//! that page only, and image tags come from the rows themselves.
//!
//! [`CatalogIds`] answers Jellyfin ids from the stored table
//! ([`jellyfin_ids`](super::jellyfin_ids)), which a loop fills. Album and
//! artist ids are made with the data key, so nobody without an account can
//! work one out, and once stored an id never changes, not even with the
//! key. An id a client cached before a restart resolves with one read.

use std::collections::HashMap;

use super::jellyfin_ids::{IdKind, JellyfinIds};
use super::library::{CompatError, CompatLibrary};
use crate::jellyfin::params::SortKey;
use crate::jellyfin::seams::{
    AlbumFilter, AlbumView, ArtistScope, ArtistView, CoverBytes, GenreView, IdMap, ItemSort,
    ItemView, LibraryRead, LyricsView, PlaylistDetail, PlaylistEntry, PlaylistView, ReadFailed,
    TrackFilter, TrackView, WriteRefusal,
};
use crate::subsonic::views::song_path;
use droppedneedle_catalog::reads::collections::store::play_marks::MarkScope;
use droppedneedle_catalog::reads::library::player::{
    AlbumOrder, AlbumQuery, OrderKey, PlayerAlbum, PlayerTrack, TrackOrder, TrackQuery,
};
use droppedneedle_catalog::reads::library::store::LocalCatalog;
use droppedneedle_catalog::reads::library::stores::{ArtistRecord, ArtistScope as CatalogScope};

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
    let album_image_tag = (track.cover_available || track.release_group_mbid.is_some())
        .then(|| tag_for(&format!("album:{}", track.album_id)));
    TrackView {
        file_id: track.id,
        title: track.title,
        duration_seconds: track.duration_seconds,
        year: track.year.and_then(|year| i32::try_from(year).ok()),
        track_number: i32::try_from(track.track_number).ok().filter(|n| *n > 0),
        disc_number: i32::try_from(track.disc_number).ok().filter(|n| *n > 0),
        album_title: Some(track.album_title),
        album_id: Some(track.album_id),
        artist_name: Some(track.artist_name).filter(|name| !name.is_empty()),
        artist_id: track.artist_id,
        album_artist_name: track.album_artist_name,
        album_artist_id: Some(track.album_artist_id),
        genre: track.genres.first().cloned(),
        file_format: Some(track.format),
        bitrate: to_u32(track.bit_rate),
        channels: to_u32(track.channels),
        sample_rate: to_u32(track.sample_rate),
        bit_depth: to_u32(track.bit_depth),
        file_size_bytes: u64::try_from(track.file_size_bytes).ok(),
        created_at: track.date_added,
        recording_mbid: track.recording_mbid,
        artist_musicbrainz_id: track.artist_mbid,
        album_artist_musicbrainz_id: track.album_artist_mbid,
        release_group_mbid: track.release_group_mbid,
        release_mbid: None,
        path: song_path(&track.relative_path),
        starred: false,
        played: false,
        play_count: 0,
        last_played: None,
        album_image_tag,
        has_lyrics: track.has_lyrics,
    }
}

fn album_view(album: PlayerAlbum) -> AlbumView {
    let record = album.record;
    let image_tag = (record.cover_available || record.release_group_mbid.is_some())
        .then(|| tag_for(&format!("album:{}", record.id)));
    AlbumView {
        id: record.id,
        release_group_mbid: record.release_group_mbid,
        release_mbid: record.release_mbid,
        artist_musicbrainz_id: record.artist_mbid,
        title: record.title,
        artist_name: Some(record.artist_name),
        artist_id: Some(record.artist_id),
        year: record.year.and_then(|year| i32::try_from(year).ok()),
        release_date: album.original_release_date,
        genre: album.genre,
        track_count: record.track_count as usize,
        total_duration_seconds: Some(record.total_duration_seconds),
        date_added: record.date_added,
        starred: false,
        played: false,
        play_count: 0,
        last_played: None,
        image_tag,
    }
}

fn artist_view(artist: ArtistRecord) -> ArtistView {
    let image_tag = artist.artist_mbid.is_some().then(|| artist_tag(&artist.id));
    ArtistView {
        id: artist.id,
        artist_mbid: artist.artist_mbid,
        name: artist.name,
        album_count: artist.album_count as usize,
        appearance_album_count: artist.appearance_album_count as usize,
        track_count: 0,
        total_duration_seconds: 0.0,
        date_added: artist.date_added,
        starred: false,
        image_tag,
    }
}

/// The picture tag of an artist. An unlinked artist gets it only when it
/// has album art to draw its picture from (see `overlay_artists`).
fn artist_tag(id: &str) -> String {
    tag_for(&format!("artist:{id}"))
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
        let plays = self.library.played_values(user_id, "track", &ids).await?;
        let player_ids = self
            .library
            .player_ids(
                &distinct(views.iter().filter_map(|view| view.album_id.as_ref())),
                &distinct(views.iter().filter_map(|view| view.artist_id.as_ref())),
            )
            .await?;
        for view in &mut views {
            if let Some(album) = view
                .album_id
                .as_ref()
                .and_then(|id| player_ids.albums.get(id))
            {
                view.release_group_mbid = album.release_group_mbid.clone();
                view.release_mbid = album.release_mbid.clone();
            }
            if let Some(mbid) = view
                .artist_id
                .as_ref()
                .and_then(|id| player_ids.artists.get(id))
            {
                view.artist_musicbrainz_id = Some(mbid.clone());
            }
            view.starred = starred.contains_key(&view.file_id);
            if let Some(values) = plays.get(&view.file_id) {
                view.played = values.played;
                view.play_count = values.plays;
                view.last_played = values.last_played;
            }
        }
        Ok(views)
    }

    async fn overlay_albums(
        &self,
        user_id: &str,
        mut views: Vec<AlbumView>,
    ) -> Result<Vec<AlbumView>, CompatError> {
        let ids = views.iter().map(|view| view.id.clone()).collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "album", &ids).await?;
        let plays = self.library.played_values(user_id, "album", &ids).await?;
        let player_ids = self.library.player_ids(&ids, &[]).await?;
        for view in &mut views {
            if let Some(album) = player_ids.albums.get(&view.id) {
                view.release_group_mbid = album.release_group_mbid.clone();
                view.release_mbid = album.release_mbid.clone();
            }
            view.starred = starred.contains_key(&view.id);
            if let Some(values) = plays.get(&view.id) {
                view.played = values.played;
                view.play_count = values.plays;
                view.last_played = values.last_played;
            }
        }
        Ok(views)
    }

    async fn overlay_artists(
        &self,
        user_id: &str,
        mut views: Vec<ArtistView>,
    ) -> Result<Vec<ArtistView>, CompatError> {
        let ids = views.iter().map(|view| view.id.clone()).collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "artist", &ids).await?;
        let durations = self.library.catalog.artist_durations(&ids).await?;
        let unlinked = views
            .iter()
            .filter(|view| view.artist_mbid.is_none())
            .map(|view| view.id.clone())
            .collect::<Vec<_>>();
        let drawn = self.library.artists_with_local_art(&unlinked).await?;
        for view in &mut views {
            if drawn.contains(&view.id) {
                view.image_tag = Some(artist_tag(&view.id));
            }
            view.starred = starred.contains_key(&view.id);
            let (seconds, tracks) = durations.get(&view.id).copied().unwrap_or((0.0, 0));
            view.total_duration_seconds = seconds;
            view.track_count = usize::try_from(tracks).unwrap_or(usize::MAX);
        }
        Ok(views)
    }

    /// Tracks by id in the order asked; unknown ids drop out.
    async fn read_tracks(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<TrackView>, CompatError> {
        let mut unique = ids.to_vec();
        unique.sort();
        unique.dedup();
        let found = self.library.tracks_by_ids(&unique).await?;
        let views = found.into_iter().map(track_view).collect();
        let views = self.overlay_tracks(user_id, views).await?;
        Ok(in_order(ids, views, |view| view.file_id.as_str()))
    }

    /// Albums with a streamable track, by id in the order asked.
    async fn read_albums(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<AlbumView>, CompatError> {
        let found = self
            .library
            .albums_in_order(ids)
            .await?
            .into_iter()
            .filter(|album| album.record.track_count > 0)
            .map(album_view)
            .collect();
        self.overlay_albums(user_id, found).await
    }

    async fn read_artist(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<ArtistView>, CompatError> {
        let Some(artist) = self.library.artist(id).await? else {
            return Ok(None);
        };
        Ok(self
            .overlay_artists(user_id, vec![artist_view(artist)])
            .await?
            .pop())
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
            open_access: visible.row.is_public,
        }))
    }
}

/// The catalog order for a Jellyfin page order.
fn order_key(key: SortKey) -> Option<OrderKey> {
    match key {
        SortKey::Recent => Some(OrderKey::Added),
        SortKey::Title => Some(OrderKey::Title),
        SortKey::Year | SortKey::PremiereDate => Some(OrderKey::Year),
        SortKey::DatePlayed => Some(OrderKey::LastPlayed),
        SortKey::PlayCount => Some(OrderKey::PlayCount),
        SortKey::Random => None,
    }
}

/// Random sorts page through one shuffle per caller (`seed` is drawn at
/// start index 0), so `StartIndex` paging never repeats or skips an item.
fn track_order(sort: ItemSort, seed: impl FnOnce() -> u32) -> TrackOrder {
    match sort {
        ItemSort::Catalog => TrackOrder::Title,
        ItemSort::Disc => TrackOrder::Disc,
        ItemSort::By(key, descending) => match order_key(key) {
            Some(key) => TrackOrder::By(key, descending),
            None => TrackOrder::Shuffle(seed()),
        },
    }
}

fn album_order(sort: ItemSort, seed: impl FnOnce() -> u32) -> AlbumOrder {
    match sort {
        ItemSort::Catalog | ItemSort::Disc => AlbumOrder::Title,
        ItemSort::By(key, descending) => match order_key(key) {
            Some(key) => AlbumOrder::By(key, descending),
            None => AlbumOrder::Shuffle(seed()),
        },
    }
}

/// The caller, when the order reads their play history.
fn history_user(sort: ItemSort, user_id: &str) -> Option<String> {
    matches!(sort, ItemSort::By(key, _) if key.is_history()).then(|| user_id.to_owned())
}

fn to_u64(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

fn to_usize(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Each id once.
fn distinct<'a>(ids: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut ids = ids.cloned().collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

/// Keep `ids` order; unknown ids drop out, repeats stay.
fn in_order<T: Clone>(ids: &[String], found: Vec<T>, id_of: impl Fn(&T) -> &str) -> Vec<T> {
    let by_id = found
        .into_iter()
        .map(|item| (id_of(&item).to_owned(), item))
        .collect::<HashMap<_, _>>();
    ids.iter().filter_map(|id| by_id.get(id).cloned()).collect()
}

impl LibraryRead for JellyfinLibrary {
    async fn track_page(
        &self,
        user_id: &str,
        filter: &TrackFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
    ) -> (Vec<TrackView>, usize) {
        let outcome = async {
            let query = TrackQuery {
                q: filter.search.clone(),
                album_id: filter.album.clone(),
                artist_ids: filter.artists.clone(),
                album_artist_ids: filter.album_artists.clone(),
                listener: filter
                    .played
                    .map(|_| user_id.to_owned())
                    .or_else(|| history_user(sort, user_id)),
                played: filter.played,
                ..TrackQuery::default()
            };
            let (tracks, total) = self
                .library
                .tracks(
                    &query,
                    track_order(sort, || {
                        self.library.track_shuffle(user_id, &query, to_u64(start))
                    }),
                    to_u64(limit),
                    to_u64(start),
                )
                .await?;
            let views = tracks.into_iter().map(track_view).collect();
            Ok((self.overlay_tracks(user_id, views).await?, to_usize(total)))
        }
        .await;
        logged("track_page", outcome)
    }

    async fn tracks_by_ids(&self, user_id: &str, ids: &[String]) -> Vec<TrackView> {
        logged("tracks_by_ids", self.read_tracks(user_id, ids).await)
    }

    async fn track(&self, user_id: &str, file_id: &str) -> Option<TrackView> {
        self.tracks_by_ids(user_id, &[file_id.to_owned()])
            .await
            .pop()
    }

    async fn album_page(
        &self,
        user_id: &str,
        filter: &AlbumFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
    ) -> (Vec<AlbumView>, usize) {
        let outcome = async {
            let query = AlbumQuery {
                q: filter.search.clone(),
                album_artist_ids: filter.artists.clone(),
                appears_on: filter.appears_on.clone(),
                listener: filter
                    .played
                    .map(|_| user_id.to_owned())
                    .or_else(|| history_user(sort, user_id)),
                played: filter.played,
                ..AlbumQuery::default()
            };
            let (albums, total) = self
                .library
                .albums(
                    &query,
                    album_order(sort, || {
                        self.library.album_shuffle(user_id, &query, to_u64(start))
                    }),
                    to_u64(limit),
                    to_u64(start),
                )
                .await?;
            let views = albums.into_iter().map(album_view).collect();
            Ok((self.overlay_albums(user_id, views).await?, to_usize(total)))
        }
        .await;
        logged("album_page", outcome)
    }

    async fn albums_by_ids(&self, user_id: &str, ids: &[String]) -> Vec<AlbumView> {
        logged("albums_by_ids", self.read_albums(user_id, ids).await)
    }

    async fn album(&self, user_id: &str, rg_mbid: &str) -> Option<AlbumView> {
        self.albums_by_ids(user_id, &[rg_mbid.to_owned()])
            .await
            .pop()
    }

    async fn artist_page(
        &self,
        user_id: &str,
        scope: ArtistScope,
        search: Option<&str>,
        start: usize,
        limit: usize,
    ) -> (Vec<ArtistView>, usize) {
        let outcome = async {
            let scope = match scope {
                ArtistScope::All => CatalogScope::WithMusic,
                ArtistScope::Album => CatalogScope::AlbumArtists,
            };
            let (artists, total) = self
                .library
                .artist_page(scope, search, to_u64(limit), to_u64(start))
                .await?;
            let views = artists.into_iter().map(artist_view).collect();
            Ok((self.overlay_artists(user_id, views).await?, to_usize(total)))
        }
        .await;
        logged("artist_page", outcome)
    }

    async fn artist(&self, user_id: &str, mbid: &str) -> Option<ArtistView> {
        logged("artist", self.read_artist(user_id, mbid).await)
    }

    async fn item(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
    ) -> Result<Option<ItemView>, ReadFailed> {
        let ids = [internal.to_owned()];
        let outcome = match kind {
            "track" => self
                .read_tracks(user_id, &ids)
                .await
                .map(|mut found| found.pop().map(|item| ItemView::Track(Box::new(item)))),
            "album" => self
                .read_albums(user_id, &ids)
                .await
                .map(|mut found| found.pop().map(|item| ItemView::Album(Box::new(item)))),
            "artist" => self
                .read_artist(user_id, internal)
                .await
                .map(|found| found.map(|item| ItemView::Artist(Box::new(item)))),
            _ => Ok(None),
        };
        outcome.map_err(|error| {
            tracing::error!(%error, "jellyfin item read failed");
            ReadFailed
        })
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

    async fn years(&self) -> Vec<i32> {
        let outcome = async {
            let years = self.library.catalog.album_years().await?;
            Ok(years
                .into_iter()
                .filter_map(|year| i32::try_from(year).ok())
                .collect())
        }
        .await;
        logged("years", outcome)
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
                    total_duration_seconds: Some(visible.row.streamable_duration),
                })
                .collect()
        });
        logged("playlists", outcome)
    }

    async fn playlist(&self, user_id: &str, id: &str) -> Option<PlaylistDetail> {
        logged("playlist", self.readable_entries(user_id, id).await)
    }

    async fn create_playlist(&self, user_id: &str, name: &str) -> Result<String, WriteRefusal> {
        Ok(self.library.create_playlist(user_id, name).await?)
    }

    async fn add_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_id: &str,
    ) -> Result<(), WriteRefusal> {
        Ok(self
            .library
            .add_files(user_id, playlist_id, &[file_id.to_owned()])
            .await?)
    }

    async fn remove_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<(), WriteRefusal> {
        Ok(self
            .library
            .remove_entries(user_id, playlist_id, entry_ids)
            .await?)
    }

    async fn move_playlist_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) -> Result<(), WriteRefusal> {
        Ok(self
            .library
            .move_entry(user_id, playlist_id, entry_id, index)
            .await?)
    }

    async fn favorites(&self, user_id: &str, kind: &str) -> Vec<String> {
        let outcome = self
            .library
            .favorite_ids(user_id, kind)
            .await
            .map(|ids| ids.into_iter().map(|(id, _)| id).collect());
        logged("favorites", outcome)
    }

    async fn set_favorite(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
        add: bool,
    ) -> Result<(), WriteRefusal> {
        Ok(self
            .library
            .set_favorites(user_id, &[(kind.to_owned(), internal.to_owned())], add)
            .await?)
    }

    async fn set_played(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
        played: bool,
    ) -> Result<(), WriteRefusal> {
        // An artist marks the tracks Jellyfin 10.10.6 marked when asked:
        // those on the albums the artist leads (its `AlbumArtistIds`).
        let scope = match kind {
            "track" => MarkScope::Track(internal),
            "album" => MarkScope::Album(internal),
            "artist" => MarkScope::AlbumArtist(internal),
            _ => return Err(WriteRefusal::Invalid),
        };
        Ok(self.library.mark_played(user_id, scope, played).await?)
    }

    async fn cover(&self, rg_mbid: &str, size: &str) -> Option<CoverBytes> {
        let outcome = self.library.album_cover(rg_mbid, Some(size)).await;
        logged("cover", outcome).map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        })
    }

    async fn artist_image(&self, mbid: &str, size: &str) -> Option<CoverBytes> {
        let outcome = self.library.artist_cover(mbid, None, size).await;
        logged("artist_image", outcome).map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        })
    }

    async fn lyrics(&self, file_id: &str) -> Result<Option<LyricsView>, ReadFailed> {
        match self.library.lyrics(file_id).await {
            Ok(doc) => Ok(doc.map(|doc| LyricsView {
                lines: doc.lines,
                synced: doc.synced,
            })),
            Err(error) => {
                tracing::error!(%error, "jellyfin lyrics read failed");
                Err(ReadFailed)
            }
        }
    }

    async fn name_of(&self, kind: &str, catalog_id: &str) -> Option<String> {
        let outcome = async {
            if !matches!(kind, "album" | "artist") {
                return Ok(None);
            }
            // A merged-away album or artist keeps its old id, and the picture
            // address never opens one, so only the live holder of the id has
            // a name here.
            let live = self.library.catalog.current_id(kind, catalog_id).await?;
            if live.as_deref() != Some(catalog_id) {
                return Ok(None);
            }
            Ok(if kind == "album" {
                self.library
                    .album(catalog_id)
                    .await?
                    .map(|album| album.record.title)
            } else {
                self.library
                    .artist(catalog_id)
                    .await?
                    .map(|artist| artist.name)
            })
        }
        .await;
        logged("name_of", outcome)
    }
}

/// Jellyfin ids over the stored table (see
/// [`jellyfin_ids`](super::jellyfin_ids)): an id resolves with one read by
/// primary key, and an album, artist or track is then followed to the one
/// that holds it now.
#[derive(Clone)]
pub struct CatalogIds {
    ids: JellyfinIds,
    catalog: LocalCatalog,
}

impl CatalogIds {
    /// Ids over the stored table, followed through `catalog`.
    pub fn new(ids: JellyfinIds, catalog: LocalCatalog) -> Self {
        Self { ids, catalog }
    }

    /// The unkeyed id (see [`JellyfinIds::derive`]).
    pub fn derive(kind: &str, internal: &str) -> String {
        JellyfinIds::derive(kind, internal)
    }

    /// The id an item gets under today's data key (see
    /// [`JellyfinIds::computed`]).
    pub fn id_for(&self, kind: &str, internal: &str) -> String {
        self.ids.computed(kind, internal)
    }

    /// An album, artist or track as it is now: a merged album or artist
    /// becomes the one that holds it, and one with nothing live behind it
    /// is not found; the old id of a missing track that continued into
    /// another becomes that track. Other kinds pass through, and so does an
    /// id that could not be followed, so a live one still answers.
    async fn current(&self, kind: IdKind, internal: String) -> Option<(String, String)> {
        let name = kind.as_str().to_owned();
        if !matches!(kind, IdKind::Album | IdKind::Artist | IdKind::Track) {
            return Some((name, internal));
        }
        match self.catalog.current_id(&name, &internal).await {
            Ok(current) => current.map(|id| (name, id)),
            Err(error) => {
                tracing::warn!(%error, "jellyfin id could not be followed to its current item");
                Some((name, internal))
            }
        }
    }
}

impl IdMap for CatalogIds {
    fn ready(&self) -> bool {
        self.ids.ready()
    }

    async fn to_jf(&self, kind: &str, internal: &str) -> String {
        self.ids.handed_out(kind, internal)
    }

    async fn from_jf(&self, jf_id: &str) -> Option<(String, String)> {
        self.from_jf_many(&[jf_id.to_owned()]).await.pop().flatten()
    }

    async fn from_jf_many(&self, jf_ids: &[String]) -> Vec<Option<(String, String)>> {
        let found = match self.ids.resolve(jf_ids, true).await {
            Ok(found) => found,
            Err(error) => {
                tracing::error!(%error, "jellyfin ids could not be read");
                return vec![None; jf_ids.len()];
            }
        };
        let mut out = Vec::with_capacity(found.len());
        for item in found {
            out.push(match item {
                Some((kind, internal)) => self.current(kind, internal).await,
                None => None,
            });
        }
        out
    }

    async fn from_jf_anonymous(&self, jf_id: &str) -> Option<(String, String)> {
        match self.ids.resolve(&[jf_id.to_owned()], false).await {
            Ok(mut found) => found
                .pop()
                .flatten()
                .map(|(kind, internal)| (kind.as_str().to_owned(), internal)),
            Err(error) => {
                tracing::error!(%error, "jellyfin id could not be read");
                None
            }
        }
    }
}
