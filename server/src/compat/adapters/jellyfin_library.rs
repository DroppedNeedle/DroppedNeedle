//! The production Jellyfin library and id map.
//!
//! [`JellyfinLibrary`] serves the Jellyfin routes from the v3 catalog and
//! the shared collections. Every list is one SQL page (filter, order,
//! LIMIT) plus its count; the caller's favorites and play counts are read
//! for that page only, and image tags come from the rows themselves.
//!
//! [`CatalogIds`] makes album and artist ids with the data key, so nobody
//! without an account can work one out, and every other id as
//! `sha256("kind:internal")[:32]`. Because an id is a pure function of
//! what it names and the key, nothing needs storing: an id a client cached
//! before a restart resolves again by deriving the ids of everything the
//! library holds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::library::{CompatError, CompatLibrary};
use crate::compat::jellyfin::builders::LIBRARY_INTERNAL_ID;
use crate::compat::jellyfin::params::SortKey;
use crate::compat::jellyfin::seams::{
    AlbumFilter, AlbumView, ArtistScope, ArtistView, CoverBytes, GenreView, IdMap, ItemSort,
    LibraryRead, PlaylistDetail, PlaylistEntry, PlaylistView, TrackFilter, TrackView, WriteRefusal,
};
use crate::compat::subsonic::views::{genre_slug, song_path};
use crate::reads::library::player::{
    AlbumOrder, AlbumQuery, OrderKey, PlayerAlbum, PlayerTrack, TrackOrder, TrackQuery,
};
use crate::reads::library::stores::{ArtistRecord, ArtistScope as CatalogScope};
use droppedneedle_kernel::runtime_config::Crypto;

/// Shortest gap between two catalog reads for the id map.
pub const REBUILD_COOLDOWN: Duration = Duration::from_secs(10);

/// The data key's purpose for album and artist ids: nothing else the key
/// signs is a valid id, and an id signs nothing else.
const ITEM_ID_PURPOSE: &str = "jellyfin-item-id";

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
        artist_musicbrainz_id: track.artist_mbid,
        album_artist_musicbrainz_id: track.album_artist_mbid,
        release_group_mbid: track.release_group_mbid,
        path: song_path(&track.relative_path),
        starred: false,
        play_count: 0,
        last_played: None,
        album_image_tag,
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
        play_count: 0,
        last_played: None,
        image_tag,
    }
}

fn artist_view(artist: ArtistRecord) -> ArtistView {
    let image_tag = artist
        .artist_mbid
        .is_some()
        .then(|| tag_for(&format!("artist:{}", artist.id)));
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
        let ids = views.iter().map(|view| view.id.clone()).collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "album", &ids).await?;
        let plays = self.library.plays(user_id, "album", &ids).await?;
        for view in &mut views {
            view.starred = starred.contains_key(&view.id);
            if let Some((count, last)) = plays.get(&view.id) {
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
        let ids = views.iter().map(|view| view.id.clone()).collect::<Vec<_>>();
        let starred = self.library.starred(user_id, "artist", &ids).await?;
        let durations = self.library.player.artist_durations(&ids).await?;
        for view in &mut views {
            view.starred = starred.contains_key(&view.id);
            let (seconds, tracks) = durations.get(&view.id).copied().unwrap_or((0.0, 0));
            view.total_duration_seconds = seconds;
            view.track_count = usize::try_from(tracks).unwrap_or(usize::MAX);
        }
        Ok(views)
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
        let outcome = async {
            let mut unique = ids.to_vec();
            unique.sort();
            unique.dedup();
            let found = self.library.tracks_by_ids(&unique).await?;
            let views = found.into_iter().map(track_view).collect();
            let views = self.overlay_tracks(user_id, views).await?;
            Ok(in_order(ids, views, |view| view.file_id.as_str()))
        }
        .await;
        logged("tracks_by_ids", outcome)
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
                listener: history_user(sort, user_id),
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
        let outcome = async {
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
        .await;
        logged("albums_by_ids", outcome)
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
                ArtistScope::All => CatalogScope::All,
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

    async fn years(&self) -> Vec<i32> {
        let outcome = async {
            let years = self.library.player.album_years().await?;
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
}

#[derive(Default)]
struct IdTable {
    /// The ids this server hands out today, every kind.
    current: HashMap<String, (String, String)>,
    /// The unkeyed ids albums and artists had before theirs were keyed, and
    /// both ids of every album and artist a merge retired.
    old: HashMap<String, (String, String)>,
    /// Whether a whole catalog read has succeeded in this process.
    filled: bool,
    /// When the catalog was last read, whether or not the read succeeded.
    read_at: Option<Instant>,
}

/// Jellyfin ids over the catalog (see module docs).
///
/// The catalog is read for the first lookup of the process, by anyone.
/// After that only a miss from a signed-in caller ([`IdMap::from_jf`]) reads
/// it again, at most once per `cooldown`; a read that failed is retried no
/// sooner than that either.
#[derive(Clone)]
pub struct CatalogIds {
    library: CompatLibrary,
    crypto: Arc<Crypto>,
    cooldown: Duration,
    table: Arc<Mutex<IdTable>>,
    /// One catalog read at a time: concurrent misses wait for it instead of
    /// each reading the whole catalog.
    reading: Arc<tokio::sync::Mutex<()>>,
}

impl CatalogIds {
    /// Ids over the shared compat reads, keyed with the data key.
    /// Production passes [`REBUILD_COOLDOWN`] as `cooldown`.
    pub fn new(library: CompatLibrary, crypto: Arc<Crypto>, cooldown: Duration) -> Self {
        Self {
            library,
            crypto,
            cooldown,
            table: Arc::new(Mutex::new(IdTable::default())),
            reading: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    /// The unkeyed id: the first 32 hex characters of
    /// `sha256("kind:internal")`. Tracks, playlists, genres and the library
    /// keep it; for an album or artist it is the old id.
    pub fn derive(kind: &str, internal: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!("{kind}:{internal}").as_bytes())
        )[..32]
            .to_owned()
    }

    /// The id handed out for a (kind, internal) pair, without remembering
    /// it. Album and artist ids are the first 32 hex characters of a MAC
    /// under the data key over `kind\0internal`.
    pub fn id_for(&self, kind: &str, internal: &str) -> String {
        if kind != "album" && kind != "artist" {
            return Self::derive(kind, internal);
        }
        let message = [kind.as_bytes(), b"\0", internal.as_bytes()].concat();
        self.crypto.mac(ITEM_ID_PURPOSE, &message)[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    fn normalize(jf_id: &str) -> String {
        jf_id.replace('-', "").trim().to_lowercase()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, IdTable> {
        self.table
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lookup(&self, jf_id: &str, with_old: bool) -> Option<(String, String)> {
        let table = self.lock();
        table
            .current
            .get(jf_id)
            .or_else(|| with_old.then(|| table.old.get(jf_id)).flatten())
            .cloned()
    }

    /// Whether a lookup may read the catalog now: never within `cooldown`
    /// of the last read, and once a read has succeeded only for a
    /// signed-in caller.
    fn may_read(&self, signed_in: bool) -> bool {
        let table = self.lock();
        let rested = table.read_at.is_none_or(|at| at.elapsed() >= self.cooldown);
        rested && (signed_in || !table.filled)
    }

    /// Read every id the catalog holds and remember the current id of each,
    /// plus the old id of each album and artist, when [`Self::may_read`]
    /// allows it.
    async fn read_catalog(&self, signed_in: bool) {
        // Waiters see the read that ran while they queued and return; the
        // caller then looks the id up again.
        let _single = self.reading.lock().await;
        if !self.may_read(signed_in) {
            return;
        }
        let mut complete = true;
        let mut pairs: Vec<(String, String)> =
            vec![("library".to_owned(), LIBRARY_INTERNAL_ID.to_owned())];
        match self.library.player.every_id().await {
            Ok(ids) => {
                for (kind, id) in ids {
                    let internal = if kind == "genre" { genre_slug(&id) } else { id };
                    pairs.push((kind.to_owned(), internal));
                }
            }
            Err(error) => {
                complete = false;
                tracing::error!(%error, "jellyfin id rebuild could not read the catalog");
            }
        }
        match self.library.collections.stores.playlists.list().await {
            Ok(rows) => pairs.extend(rows.into_iter().map(|row| ("playlist".to_owned(), row.id))),
            Err(error) => {
                complete = false;
                tracing::error!(%error, "jellyfin id rebuild could not read playlists");
            }
        }
        let merged = match self.library.player.merged_ids().await {
            Ok(merged) => merged,
            Err(error) => {
                complete = false;
                tracing::error!(%error, "jellyfin id rebuild could not read merged ids");
                Vec::new()
            }
        };
        let mut current = Vec::with_capacity(pairs.len());
        let mut old = Vec::new();
        for (kind, internal) in pairs {
            if kind == "album" || kind == "artist" {
                old.push((
                    Self::derive(&kind, &internal),
                    (kind.clone(), internal.clone()),
                ));
            }
            current.push((self.id_for(&kind, &internal), (kind, internal)));
        }
        // A merged album or artist answers to both ids it was handed out
        // under, and a track alias to its own, for signed-in callers only;
        // `from_jf` follows it to the one that holds it now.
        for (kind, internal) in merged {
            let found = (kind.to_owned(), internal.clone());
            old.push((Self::derive(kind, &internal), found.clone()));
            old.push((self.id_for(kind, &internal), found));
        }
        let mut table = self.lock();
        table.current.extend(current);
        table.old.extend(old);
        table.filled |= complete;
        table.read_at = Some(Instant::now());
    }

    /// An album, artist or track as it is now: a merged album or artist
    /// becomes the one that holds it, and one with nothing live behind it
    /// is not found; the old id of a missing track that continued into
    /// another becomes that track. Other kinds pass through, and so does an
    /// id that could not be followed, so a live one still answers.
    async fn current(&self, (kind, internal): (String, String)) -> Option<(String, String)> {
        if kind != "album" && kind != "artist" && kind != "track" {
            return Some((kind, internal));
        }
        match self.library.player.current_id(&kind, &internal).await {
            Ok(current) => current.map(|id| (kind, id)),
            Err(error) => {
                tracing::warn!(%error, "jellyfin id could not be followed to its current item");
                Some((kind, internal))
            }
        }
    }
}

impl IdMap for CatalogIds {
    async fn to_jf(&self, kind: &str, internal: &str) -> String {
        let jf_id = self.id_for(kind, internal);
        self.lock()
            .current
            .entry(jf_id.clone())
            .or_insert_with(|| (kind.to_owned(), internal.to_owned()));
        jf_id
    }

    async fn from_jf(&self, jf_id: &str) -> Option<(String, String)> {
        let normalized = Self::normalize(jf_id);
        let found = match self.lookup(&normalized, true) {
            Some(found) => found,
            None => {
                self.read_catalog(true).await;
                self.lookup(&normalized, true)?
            }
        };
        self.current(found).await
    }

    async fn from_jf_anonymous(&self, jf_id: &str) -> Option<(String, String)> {
        let normalized = Self::normalize(jf_id);
        if let Some(found) = self.lookup(&normalized, false) {
            return Some(found);
        }
        self.read_catalog(false).await;
        self.lookup(&normalized, false)
    }
}
