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

use super::jellyfin_ids::{IdKind, IdsError, JellyfinIds};
use super::library::{CompatError, CompatLibrary, cover_version};
use crate::jellyfin::params::SortKey;
use crate::jellyfin::seams::{
    AlbumFilter, AlbumView, ArtistScope, ArtistView, CoverBytes, GenreView, ItemSort, ItemView,
    LyricsView, PlaylistDetail, PlaylistEntry, PlaylistView, ReadFailed, TrackFilter, TrackView,
    WriteRefusal,
};
use crate::subsonic::views::song_path;
use droppedneedle_catalog::reads::collections::store::play_marks::MarkScope;
use droppedneedle_catalog::reads::library::player::{
    AlbumOrder, AlbumQuery, OrderKey, PlayerAlbum, PlayerTrack, TrackOrder, TrackQuery,
};
use droppedneedle_catalog::reads::library::store::LocalCatalog;
use droppedneedle_catalog::reads::library::stores::{ArtistRecord, ArtistScope as CatalogScope};

fn to_u32(value: Option<i64>) -> Option<u32> {
    value.and_then(|value| u32::try_from(value).ok())
}

/// A track as the Jellyfin builders see it. `local_first` is the local-art
/// setting its album's picture tag is worked out under.
fn track_view(track: PlayerTrack, local_first: bool) -> TrackView {
    let album_image_tag =
        (track.cover_available || track.release_group_mbid.is_some()).then(|| {
            album_tag(
                &track.album_id,
                &cover_version(
                    track.art_version,
                    track.release_group_mbid.as_deref(),
                    local_first,
                ),
            )
        });
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

/// An album as the Jellyfin builders see it, its picture tag worked out
/// under the local-art setting `local_first`.
fn album_view(album: PlayerAlbum, local_first: bool) -> AlbumView {
    let record = album.record;
    let image_tag = (record.cover_available || record.release_group_mbid.is_some()).then(|| {
        album_tag(
            &record.id,
            &cover_version(
                record.art_version,
                record.release_group_mbid.as_deref(),
                local_first,
            ),
        )
    });
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

/// The picture tag of an album: it changes with the album's cover version,
/// so an app that sends the tag asks for the new picture. The item id
/// stays the same.
fn album_tag(id: &str, version: &str) -> String {
    tag_for(&format!("album:{id}:{version}"))
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
        let local_first = self.library.local_art_first();
        let views = found
            .into_iter()
            .map(|track| track_view(track, local_first))
            .collect();
        let views = self.overlay_tracks(user_id, views).await?;
        Ok(in_order(ids, views, |view| view.file_id.as_str()))
    }

    /// Albums with a streamable track, by id in the order asked.
    async fn read_albums(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<AlbumView>, CompatError> {
        let local_first = self.library.local_art_first();
        let found = self
            .library
            .albums_in_order(ids)
            .await?
            .into_iter()
            .filter(|album| album.record.track_count > 0)
            .map(|album| album_view(album, local_first))
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

impl JellyfinLibrary {
    /// One page of tracks; `limit` [`ALL`] means every match. The match
    /// total is counted only when `counted`.
    pub async fn track_page(
        &self,
        user_id: &str,
        filter: &TrackFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
        counted: bool,
    ) -> Result<(Vec<TrackView>, Option<usize>), ReadFailed> {
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
        let order = track_order(sort, || {
            self.library.track_shuffle(user_id, &query, to_u64(start))
        });
        let (limit, offset) = (to_u64(limit), to_u64(start));
        let (tracks, total) = if counted {
            let (tracks, total) = self.library.tracks(&query, order, limit, offset).await?;
            (tracks, Some(to_usize(total)))
        } else {
            let tracks = self
                .library
                .track_page(&query, order, limit, offset)
                .await?;
            (tracks, None)
        };
        let local_first = self.library.local_art_first();
        let views = tracks
            .into_iter()
            .map(|track| track_view(track, local_first))
            .collect();
        Ok((self.overlay_tracks(user_id, views).await?, total))
    }

    /// Tracks by id in the order asked; unknown ids drop out.
    pub async fn tracks_by_ids(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<TrackView>, ReadFailed> {
        Ok(self.read_tracks(user_id, ids).await?)
    }

    pub async fn track(
        &self,
        user_id: &str,
        file_id: &str,
    ) -> Result<Option<TrackView>, ReadFailed> {
        Ok(self
            .tracks_by_ids(user_id, &[file_id.to_owned()])
            .await?
            .pop())
    }

    /// One page of albums; `limit` [`ALL`] means every match. The match
    /// total is counted only when `counted`.
    pub async fn album_page(
        &self,
        user_id: &str,
        filter: &AlbumFilter,
        sort: ItemSort,
        start: usize,
        limit: usize,
        counted: bool,
    ) -> Result<(Vec<AlbumView>, Option<usize>), ReadFailed> {
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
        let order = album_order(sort, || {
            self.library.album_shuffle(user_id, &query, to_u64(start))
        });
        let (limit, offset) = (to_u64(limit), to_u64(start));
        let (albums, total) = if counted {
            let (albums, total) = self.library.albums(&query, order, limit, offset).await?;
            (albums, Some(to_usize(total)))
        } else {
            let albums = self
                .library
                .album_page(&query, order, limit, offset)
                .await?;
            (albums, None)
        };
        let local_first = self.library.local_art_first();
        let views = albums
            .into_iter()
            .map(|album| album_view(album, local_first))
            .collect();
        Ok((self.overlay_albums(user_id, views).await?, total))
    }

    /// Albums by id in the order asked; unknown ids drop out.
    pub async fn albums_by_ids(
        &self,
        user_id: &str,
        ids: &[String],
    ) -> Result<Vec<AlbumView>, ReadFailed> {
        Ok(self.read_albums(user_id, ids).await?)
    }

    pub async fn album(
        &self,
        user_id: &str,
        rg_mbid: &str,
    ) -> Result<Option<AlbumView>, ReadFailed> {
        Ok(self
            .albums_by_ids(user_id, &[rg_mbid.to_owned()])
            .await?
            .pop())
    }

    /// One page of artists in name order, optionally name-filtered.
    pub async fn artist_page(
        &self,
        user_id: &str,
        scope: ArtistScope,
        search: Option<&str>,
        start: usize,
        limit: usize,
    ) -> Result<(Vec<ArtistView>, usize), ReadFailed> {
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

    pub async fn artist(
        &self,
        user_id: &str,
        mbid: &str,
    ) -> Result<Option<ArtistView>, ReadFailed> {
        Ok(self.read_artist(user_id, mbid).await?)
    }

    /// One track, album or artist (`kind`) as the reads above give it,
    /// `Ok(None)` when there is none (any other kind has none).
    pub async fn item(
        &self,
        user_id: &str,
        kind: &str,
        internal: &str,
    ) -> Result<Option<ItemView>, ReadFailed> {
        let ids = [internal.to_owned()];
        Ok(match kind {
            "track" => self
                .read_tracks(user_id, &ids)
                .await?
                .pop()
                .map(|item| ItemView::Track(Box::new(item))),
            "album" => self
                .read_albums(user_id, &ids)
                .await?
                .pop()
                .map(|item| ItemView::Album(Box::new(item))),
            "artist" => self
                .read_artist(user_id, internal)
                .await?
                .map(|item| ItemView::Artist(Box::new(item))),
            _ => None,
        })
    }

    pub async fn genres(&self) -> Result<Vec<GenreView>, ReadFailed> {
        Ok(self
            .library
            .genres()
            .await?
            .into_iter()
            .map(|genre| GenreView {
                name: genre.name,
                song_count: genre.track_count as usize,
            })
            .collect())
    }

    /// Every year a playable album has, ascending.
    pub async fn years(&self) -> Result<Vec<i32>, ReadFailed> {
        let years = self
            .library
            .catalog
            .album_years()
            .await
            .map_err(CompatError::from)?;
        Ok(years
            .into_iter()
            .filter_map(|year| i32::try_from(year).ok())
            .collect())
    }

    pub async fn playlists(&self, user_id: &str) -> Result<Vec<PlaylistView>, ReadFailed> {
        Ok(self
            .library
            .playlists(user_id)
            .await?
            .into_iter()
            .filter(|visible| visible.full)
            .map(|visible| PlaylistView {
                id: visible.row.id,
                name: visible.row.name,
                track_count: visible.row.streamable_count,
                total_duration_seconds: Some(visible.row.streamable_duration),
            })
            .collect())
    }

    pub async fn playlist(
        &self,
        user_id: &str,
        id: &str,
    ) -> Result<Option<PlaylistDetail>, ReadFailed> {
        Ok(self.readable_entries(user_id, id).await?)
    }

    /// Create a playlist holding the library files `file_ids`, as one
    /// write; returns its id.
    pub async fn create_playlist(
        &self,
        user_id: &str,
        name: &str,
        file_ids: &[String],
    ) -> Result<String, WriteRefusal> {
        Ok(self
            .library
            .create_playlist(user_id, name, file_ids)
            .await?
            .id)
    }

    /// Append the library files `file_ids`, as one write.
    pub async fn add_playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_ids: &[String],
    ) -> Result<(), WriteRefusal> {
        Ok(self
            .library
            .add_files(user_id, playlist_id, file_ids)
            .await?)
    }

    pub async fn remove_playlist_entries(
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

    pub async fn move_playlist_entry(
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

    /// Favorited internal ids of one kind for one caller.
    pub async fn favorites(&self, user_id: &str, kind: &str) -> Result<Vec<String>, ReadFailed> {
        Ok(self
            .library
            .favorite_ids(user_id, kind)
            .await?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }

    pub async fn set_favorite(
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

    /// Mark a track played or unplayed for the caller; an album marks each
    /// of its tracks, an artist the tracks on the albums it leads. The play
    /// history is not touched.
    pub async fn set_played(
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

    /// Release art bytes (`size` is the 250/500/1200 bucket); `None` renders
    /// 404 with no placeholder, unlike Subsonic (v2 `_image`).
    pub async fn cover(&self, rg_mbid: &str, size: &str) -> Result<Option<CoverBytes>, ReadFailed> {
        let found = self.library.album_cover(rg_mbid, Some(size)).await?;
        Ok(found.map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        }))
    }

    /// An artist's picture. `size` (the 250/500/1200 bucket) sizes the
    /// picture drawn for an artist with no MusicBrainz link; a linked
    /// artist's own image is served as stored.
    pub async fn artist_image(
        &self,
        mbid: &str,
        size: &str,
    ) -> Result<Option<CoverBytes>, ReadFailed> {
        let found = self.library.artist_cover(mbid, None, size).await?;
        Ok(found.map(|(bytes, content_type)| CoverBytes {
            bytes,
            content_type,
        }))
    }

    /// The lyrics of one track: its file's first, else the online lookup
    /// while that is on. `Ok(None)` when it has none.
    pub async fn lyrics(&self, file_id: &str) -> Result<Option<LyricsView>, ReadFailed> {
        let doc = self.library.lyrics(file_id).await?;
        Ok(doc.map(|doc| LyricsView {
            lines: doc.lines,
            synced: doc.synced,
        }))
    }

    /// The album title or artist name of a catalog id, read for a caller who
    /// has not signed in (the picture address), so no user overlays. `kind`
    /// is `album` or `artist`; any other kind has no name here, and so does
    /// an album or artist that a merge retired or that is gone.
    pub async fn name_of(
        &self,
        kind: &str,
        catalog_id: &str,
    ) -> Result<Option<String>, ReadFailed> {
        if !matches!(kind, "album" | "artist") {
            return Ok(None);
        }
        // A merged-away album or artist keeps its old id, and the picture
        // address never opens one, so only the live holder of the id has a
        // name here.
        let live = self
            .library
            .catalog
            .current_id(kind, catalog_id)
            .await
            .map_err(CompatError::from)?;
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
    /// another becomes that track. Other kinds pass through.
    async fn current(
        &self,
        kind: IdKind,
        internal: String,
    ) -> Result<Option<(String, String)>, ReadFailed> {
        let name = kind.as_str().to_owned();
        if !matches!(kind, IdKind::Album | IdKind::Artist | IdKind::Track) {
            return Ok(Some((name, internal)));
        }
        let current = self
            .catalog
            .current_id(&name, &internal)
            .await
            .map_err(CompatError::from)?;
        Ok(current.map(|id| (name, id)))
    }
}

impl From<IdsError> for ReadFailed {
    fn from(error: IdsError) -> Self {
        match error {
            IdsError::Busy => Self::Busy("jellyfin ids: database busy".to_owned()),
            IdsError::Failed(cause) => Self::Failed(format!("jellyfin ids: {cause}")),
        }
    }
}

impl CatalogIds {
    /// Whether ids can be handed out and resolved yet. Until the stored
    /// ids are loaded at start, every route answers busy.
    pub fn ready(&self) -> bool {
        self.ids.ready()
    }

    /// Stable 32-hex id for a (kind, internal) pair. Reads nothing.
    pub async fn to_jf(&self, kind: &str, internal: &str) -> String {
        self.ids.handed_out(kind, internal)
    }

    /// Reverse lookup for a signed-in caller only: call it after `authed`.
    /// Answers to current ids, to the old ids albums and artists had
    /// before theirs were keyed, and to the ids of a merged album or
    /// artist; an album or artist comes back as the one that holds it now.
    /// Accepts dashed or undashed input, `Ok(None)` when unknown or when
    /// nothing live holds it, `Err` when a read failed. An id handed out
    /// since the last stored one is waited for, briefly. (`from_*` takes
    /// `self` by v2-naming parity, not by convention.)
    #[allow(clippy::wrong_self_convention)]
    pub async fn from_jf(&self, jf_id: &str) -> Result<Option<(String, String)>, ReadFailed> {
        Ok(self
            .from_jf_many(&[jf_id.to_owned()])
            .await?
            .pop()
            .flatten())
    }

    /// [`Self::from_jf`] for several ids, in order, waiting at most once
    /// for all of them.
    #[allow(clippy::wrong_self_convention)]
    pub async fn from_jf_many(
        &self,
        jf_ids: &[String],
    ) -> Result<Vec<Option<(String, String)>>, ReadFailed> {
        let found = self.ids.resolve(jf_ids, true).await?;
        let mut out = Vec::with_capacity(found.len());
        for item in found {
            out.push(match item {
                Some((kind, internal)) => self.current(kind, internal).await?,
                None => None,
            });
        }
        Ok(out)
    }

    /// Reverse lookup for a caller who has not signed in (the picture
    /// address): current ids only, never an old one, and a miss answers at
    /// once.
    #[allow(clippy::wrong_self_convention)]
    pub async fn from_jf_anonymous(
        &self,
        jf_id: &str,
    ) -> Result<Option<(String, String)>, ReadFailed> {
        let mut found = self.ids.resolve(&[jf_id.to_owned()], false).await?;
        Ok(found
            .pop()
            .flatten()
            .map(|(kind, internal)| (kind.as_str().to_owned(), internal)))
    }
}
