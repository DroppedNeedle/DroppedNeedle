//! The library both compat protocols read: the v3 catalog, the shared
//! collections (playlists, favorites), lyrics and cover art.
//!
//! Ids are local library ids. The Subsonic view field names (`rg_mbid`,
//! `artist_mbid`, `file_id`) are that layer's names for "album id",
//! "artist id" and "track id"; real MusicBrainz ids ride the
//! `musicbrainz_*` fields with `provider_identity_projected` set, so no
//! local id is ever advertised as an MBID. Those ids come from
//! [`CompatLibrary::player_ids`]: the accepted match's, else the files'
//! tags'.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use droppedneedle_catalog::reads::collections::service::{
    CollectionsService, LOCAL_SOURCE, SavedPlaylist, Visible,
};
use droppedneedle_catalog::reads::collections::store::play_marks::MarkScope;
use droppedneedle_catalog::reads::collections::store::playlists::NewEntry;
use droppedneedle_catalog::reads::collections::store::ratings::{ItemRating, RatedKind};
use droppedneedle_catalog::reads::collections::{CollectionsState, error::CollectionsError};
use droppedneedle_catalog::reads::library::player::{
    AlbumOrder, AlbumQuery, PlayStat, PlayedValues, PlayerAlbum, PlayerIds, PlayerTrack,
    ShuffleSeeds, TrackOrder, TrackQuery,
};
use droppedneedle_catalog::reads::library::store::LocalCatalog;
use droppedneedle_catalog::reads::library::stores::{
    ArtistRecord, ArtistScope, ArtistSort, GenreRecord, LyricDoc, LyricsPort, StoreError,
};
use droppedneedle_catalog::reads::platform::artwork::ArtworkService;
use droppedneedle_catalog::reads::platform::covers::{CoverArt, CoverLookup};

/// A compat read or write failed. Client faults keep their kind so each
/// protocol answers its own code (Subsonic 70/50/10/0, Jellyfin
/// 404/403/400/409); server faults go to the log only and answer the
/// fixed internal message, a busy database as 503 with `Retry-After` and
/// anything else as 500.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompatError {
    /// The item does not exist (or is another user's private playlist).
    NotFound,
    /// The item is not the caller's to change.
    Forbidden,
    /// Bad input; the message is user-safe.
    Invalid(String),
    /// Valid input against the wrong state; the message is user-safe.
    Conflict(String),
    /// The database was too busy to answer in time; the text is for the
    /// log only.
    Busy(String),
    /// Server fault; the text is for the log only.
    Internal(String),
}

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Forbidden => f.write_str("not allowed"),
            Self::Invalid(message)
            | Self::Conflict(message)
            | Self::Busy(message)
            | Self::Internal(message) => f.write_str(message),
        }
    }
}

impl From<StoreError> for CompatError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Busy(_) => Self::Busy(error.to_string()),
            StoreError::Internal(_) => Self::Internal(error.to_string()),
        }
    }
}

impl From<droppedneedle_catalog::reads::collections::db::StoreError> for CompatError {
    fn from(error: droppedneedle_catalog::reads::collections::db::StoreError) -> Self {
        use droppedneedle_catalog::reads::collections::db::StoreError as CollectionsStoreError;
        match error {
            CollectionsStoreError::Busy(_) => Self::Busy(error.to_string()),
            CollectionsStoreError::Internal(_) => Self::Internal(error.to_string()),
        }
    }
}

impl From<CollectionsError> for CompatError {
    fn from(error: CollectionsError) -> Self {
        match error {
            CollectionsError::NotFound => Self::NotFound,
            CollectionsError::Forbidden { .. } => Self::Forbidden,
            CollectionsError::InvalidInput { message } => Self::Invalid(message),
            CollectionsError::Conflict { message } => Self::Conflict(message),
            CollectionsError::Busy { operation } => {
                Self::Busy(format!("collections database busy during {operation}"))
            }
            CollectionsError::Internal { cause } => {
                Self::Internal(format!("collections failed: {cause}"))
            }
        }
    }
}

impl CompatError {
    /// The Subsonic code and user-safe message for a client fault (not
    /// found, not allowed, bad input); `None` for a server fault, which is
    /// logged and answered with code 0 and the fixed message.
    pub fn client_error(&self) -> Option<(u8, String)> {
        use crate::subsonic::error::{GENERIC, NOT_AUTHORIZED, NOT_FOUND, PARAM_MISSING};
        match self {
            Self::NotFound => Some((NOT_FOUND, "The requested data was not found.".to_owned())),
            Self::Forbidden => Some((
                NOT_AUTHORIZED,
                "User is not authorized for the given operation.".to_owned(),
            )),
            Self::Invalid(message) => Some((PARAM_MISSING, message.clone())),
            Self::Conflict(message) => Some((GENERIC, message.clone())),
            Self::Busy(_) | Self::Internal(_) => None,
        }
    }

    /// The error of a step taken after the request already wrote. A busy
    /// answer tells an app nothing was done and to ask again, which would
    /// repeat the write, so busy here answers as a failure.
    pub fn after_write(self) -> Self {
        match self {
            Self::Busy(cause) => Self::Internal(format!("{cause}, after the request wrote")),
            other => other,
        }
    }
}

impl From<CompatError> for crate::jellyfin::seams::WriteRefusal {
    fn from(error: CompatError) -> Self {
        use crate::jellyfin::seams::WriteRefusal;
        match error {
            CompatError::NotFound => WriteRefusal::NotFound,
            CompatError::Forbidden => WriteRefusal::Forbidden,
            CompatError::Invalid(_) => WriteRefusal::Invalid,
            CompatError::Conflict(_) => WriteRefusal::Conflict,
            CompatError::Busy(cause) => WriteRefusal::Busy(cause),
            CompatError::Internal(cause) => WriteRefusal::Failed(cause),
        }
    }
}

impl From<CompatError> for crate::jellyfin::seams::ReadFailed {
    fn from(error: CompatError) -> Self {
        match error {
            CompatError::Busy(cause) => Self::Busy(cause),
            other => Self::Failed(other.to_string()),
        }
    }
}

/// Cover bytes plus content type.
pub type ImageBytes = (Vec<u8>, String);

/// Everything compat reads, behind one cheap clone.
#[derive(Clone)]
pub struct CompatLibrary {
    /// The local catalog: native reads (artists, genres, single albums and
    /// artists) and player reads (rich tracks, filtered pages, history).
    pub catalog: LocalCatalog,
    /// Shared playlists and favorites.
    pub collections: CollectionsState,
    /// Stored or fetched lyrics.
    pub lyrics: Arc<dyn LyricsPort>,
    /// Cover art by MBID.
    pub covers: Arc<dyn CoverArt>,
    /// The service behind `covers`, read for the local-art setting.
    pub artwork: Option<ArtworkService>,
    /// Per-caller seeds for paged random lists, shared by both protocols.
    pub shuffles: Arc<ShuffleSeeds>,
}

/// Covers in a made playlist picture: a two by two grid.
const TILE_COUNT: usize = 4;
/// Side of one tile, pixels.
const TILE_SIDE: u32 = 250;
/// The cover size asked of each album, the smallest that fills a tile well.
const TILE_SOURCE_SIZE: &str = "500";
/// Entries read from the top of a playlist when looking for albums.
const TILE_ENTRIES: usize = 100;
/// Albums asked for a cover before giving up on a full grid.
const TILE_ALBUMS: usize = 8;
/// Largest cover decoded for a tile, a decompression-bomb guard.
const TILE_MAX_PIXELS: u64 = 40_000_000;

/// Four covers as one square JPEG, two by two in order, each cropped to
/// fill its tile. None when a cover is too large or does not decode.
fn tile_covers(covers: &[ImageBytes]) -> Option<ImageBytes> {
    use image::{ImageReader, RgbImage, codecs::jpeg::JpegEncoder, imageops};
    use std::io::Cursor;

    let mut canvas = RgbImage::new(TILE_SIDE * 2, TILE_SIDE * 2);
    for (index, (bytes, _)) in (0u32..).zip(covers.iter().take(TILE_COUNT)) {
        let format = image::guess_format(bytes).ok()?;
        let (width, height) = ImageReader::with_format(Cursor::new(bytes), format)
            .into_dimensions()
            .ok()?;
        if u64::from(width) * u64::from(height) > TILE_MAX_PIXELS {
            return None;
        }
        let tile = ImageReader::with_format(Cursor::new(bytes), format)
            .decode()
            .ok()?
            .resize_to_fill(TILE_SIDE, TILE_SIDE, imageops::FilterType::Triangle)
            .to_rgb8();
        imageops::replace(
            &mut canvas,
            &tile,
            i64::from(index % 2 * TILE_SIDE),
            i64::from(index / 2 * TILE_SIDE),
        );
    }
    let mut out = Vec::new();
    canvas
        .write_with_encoder(JpegEncoder::new_with_quality(&mut out, 85))
        .ok()?;
    Some((out, "image/jpeg".to_owned()))
}

/// The version an album's cover id carries: eight hex digits of what
/// chooses the picture the cover read serves, so a change to any of them
/// gives the album a new cover id and apps fetch the picture again. The
/// inputs are the version of the album's own art (it goes up when a scan
/// finds new art), the release group the Cover Art Archive is asked for,
/// and whether local art comes first. Art replaced at the Cover Art
/// Archive for the same release group is not seen.
pub fn cover_version(
    art_version: Option<i64>,
    release_group: Option<&str>,
    local_first: bool,
) -> String {
    use sha2::{Digest, Sha256};
    let art = art_version.map_or_else(|| "-".to_owned(), |version| version.to_string());
    let group = release_group.unwrap_or("-");
    let order = if local_first { "local" } else { "archive" };
    let digest = Sha256::digest(format!("{art}\0{group}\0{order}").as_bytes());
    format!("{digest:x}")[..8].to_owned()
}

/// Unix seconds as a whole number.
pub fn whole(seconds: Option<f64>) -> Option<i64> {
    seconds
        .filter(|value| value.is_finite())
        .map(|value| value as i64)
}

impl CompatLibrary {
    /// Everything compat reads, from the native reads bundle: the same
    /// catalog, collections, lyrics and cover art the web UI uses.
    pub fn from_reads(reads: &droppedneedle_catalog::reads::ReadsSetup) -> Self {
        Self {
            catalog: reads.library.catalog.clone(),
            collections: reads.collections.clone(),
            lyrics: reads.library.lyrics.clone(),
            covers: reads.platform.covers.covers.clone(),
            artwork: reads.artwork.clone(),
            shuffles: Arc::new(ShuffleSeeds::default()),
        }
    }

    fn service(&self) -> CollectionsService<'_> {
        CollectionsService::new(&self.collections)
    }

    /// One page of artists in name order, plus the total.
    pub async fn artist_page(
        &self,
        scope: ArtistScope,
        query: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<ArtistRecord>, u64), CompatError> {
        let (artists, total, _, _) = self
            .catalog
            .list_artists(scope, query, ArtistSort::Name, false, limit, offset)
            .await?;
        Ok((artists, total))
    }

    /// One artist.
    pub async fn artist(&self, id: &str) -> Result<Option<ArtistRecord>, CompatError> {
        Ok(self.catalog.get_artist(id).await?)
    }

    /// One album.
    pub async fn album(&self, id: &str) -> Result<Option<PlayerAlbum>, CompatError> {
        Ok(self
            .catalog
            .albums_by_ids(&[id.to_owned()])
            .await?
            .into_iter()
            .next())
    }

    /// Albums by id, in the order asked; missing ids drop out.
    pub async fn albums_in_order(&self, ids: &[String]) -> Result<Vec<PlayerAlbum>, CompatError> {
        let mut by_id = self
            .catalog
            .albums_by_ids(ids)
            .await?
            .into_iter()
            .map(|album| (album.record.id.clone(), album))
            .collect::<HashMap<_, _>>();
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// One page of albums.
    pub async fn albums(
        &self,
        query: &AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<PlayerAlbum>, u64), CompatError> {
        Ok(self.catalog.albums(query, order, limit, offset).await?)
    }

    /// One page of albums without the total.
    pub async fn album_page(
        &self,
        query: &AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerAlbum>, CompatError> {
        Ok(self.catalog.album_page(query, order, limit, offset).await?)
    }

    /// One page of tracks without the total.
    pub async fn track_page(
        &self,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerTrack>, CompatError> {
        Ok(self.catalog.track_page(query, order, limit, offset).await?)
    }

    /// The caller's shuffle seed for a random track page at `offset`.
    pub fn track_shuffle(&self, caller: &str, query: &TrackQuery, offset: u64) -> u32 {
        self.shuffles.tracks(caller, query, offset)
    }

    /// The caller's shuffle seed for a random album page at `offset`.
    pub fn album_shuffle(&self, caller: &str, query: &AlbumQuery, offset: u64) -> u32 {
        self.shuffles.albums(caller, query, offset)
    }

    /// One page of album artists without the total: name order, or
    /// insertion order when `natural`.
    pub async fn album_artist_page(
        &self,
        query: Option<&str>,
        natural: bool,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<ArtistRecord>, CompatError> {
        Ok(self
            .catalog
            .album_artist_page(query, natural, limit, offset)
            .await?)
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<PlayerTrack>, u64), CompatError> {
        Ok(self.catalog.tracks(query, order, limit, offset).await?)
    }

    /// An album's tracks in disc and track order.
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<PlayerTrack>, CompatError> {
        let query = TrackQuery {
            album_id: Some(album_id.to_owned()),
            ..TrackQuery::default()
        };
        Ok(self
            .catalog
            .track_page(&query, TrackOrder::Album, u64::MAX >> 1, 0)
            .await?)
    }

    /// Tracks by id, in no particular order.
    pub async fn tracks_by_ids(&self, ids: &[String]) -> Result<Vec<PlayerTrack>, CompatError> {
        Ok(self.catalog.tracks_by_ids(ids).await?)
    }

    /// One track.
    pub async fn track(&self, id: &str) -> Result<Option<PlayerTrack>, CompatError> {
        Ok(self
            .catalog
            .tracks_by_ids(&[id.to_owned()])
            .await?
            .into_iter()
            .next())
    }

    /// Genres by track count.
    pub async fn genres(&self) -> Result<Vec<GenreRecord>, CompatError> {
        Ok(self.catalog.genres().await?)
    }

    /// Stored lyrics for a track.
    pub async fn lyrics(&self, track_id: &str) -> Result<Option<LyricDoc>, CompatError> {
        Ok(self.lyrics.get(track_id).await?)
    }

    /// Catalog revision.
    pub async fn revision(&self) -> Result<i64, CompatError> {
        Ok(self.catalog.revision().await?)
    }

    /// When the user favorited each id of one kind.
    pub async fn starred(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, i64>, CompatError> {
        Ok(self.service().favorited_at(user_id, kind, ids).await?)
    }

    /// The user's favorites of one kind, newest first.
    pub async fn favorite_ids(
        &self,
        user_id: &str,
        kind: &str,
    ) -> Result<Vec<(String, i64)>, CompatError> {
        Ok(self.service().favorite_ids(user_id, kind).await?)
    }

    /// Favorite or unfavorite `(kind, id)` items.
    pub async fn set_favorites(
        &self,
        user_id: &str,
        targets: &[(String, String)],
        add: bool,
    ) -> Result<(), CompatError> {
        let targets = targets
            .iter()
            .map(|(kind, id)| (kind.clone(), id.clone(), None))
            .collect::<Vec<_>>();
        Ok(self.service().set_favorites(user_id, &targets, add).await?)
    }

    /// The user's and everyone's ratings of some ids of one kind.
    pub async fn ratings(
        &self,
        user_id: &str,
        kind: RatedKind,
        ids: &[String],
    ) -> Result<HashMap<String, ItemRating>, CompatError> {
        Ok(self.service().ratings(user_id, kind, ids).await?)
    }

    /// Set the user's rating of one item, or clear it with 0.
    pub async fn set_rating(
        &self,
        user_id: &str,
        kind: RatedKind,
        id: &str,
        rating: u8,
    ) -> Result<(), CompatError> {
        Ok(self.service().set_rating(user_id, kind, id, rating).await?)
    }

    /// The ids of one kind the user rated, highest first.
    pub async fn rated_ids(
        &self,
        user_id: &str,
        kind: RatedKind,
    ) -> Result<Vec<String>, CompatError> {
        Ok(self.service().rated_ids(user_id, kind).await?)
    }

    /// The user's play counts for some ids of one kind.
    pub async fn plays(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PlayStat>, CompatError> {
        Ok(self.catalog.play_stats(user_id, kind, ids).await?)
    }

    /// The user's values for some albums or tracks as Jellyfin apps see
    /// them, with the user's played and unplayed marks weighed in.
    pub async fn played_values(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PlayedValues>, CompatError> {
        Ok(self.catalog.played_values(user_id, kind, ids).await?)
    }

    /// Mark the tracks `scope` covers played or unplayed for the user.
    pub async fn mark_played(
        &self,
        user_id: &str,
        scope: MarkScope<'_>,
        played: bool,
    ) -> Result<(), CompatError> {
        Ok(self.service().mark_played(user_id, scope, played).await?)
    }

    /// The MusicBrainz ids players are sent for some albums and artists.
    pub async fn player_ids(
        &self,
        album_ids: &[String],
        artist_ids: &[String],
    ) -> Result<PlayerIds, CompatError> {
        Ok(self.catalog.player_ids(album_ids, artist_ids).await?)
    }

    /// Album ids from the user's play history.
    pub async fn history_albums(
        &self,
        user_id: &str,
        frequent: bool,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<String>, CompatError> {
        Ok(self
            .catalog
            .history_albums(user_id, frequent, limit, offset)
            .await?)
    }

    /// Playable tracks and the folders holding at least one, across every
    /// library folder. A track is playable while its file is indexed.
    pub async fn playable_counts(&self) -> Result<(i64, i64), CompatError> {
        Ok(self.catalog.playable_counts().await?)
    }

    /// Every playlist with what the user may see of it.
    pub async fn playlists(&self, user_id: &str) -> Result<Vec<Visible>, CompatError> {
        Ok(self.service().visible_playlists(user_id).await?)
    }

    /// A readable playlist with its entries, or None.
    pub async fn playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<Visible>, CompatError> {
        match self.service().readable_playlist(user_id, playlist_id).await {
            Ok(row) => Ok(Some(Visible {
                is_owner: row.owner_id.as_deref() == Some(user_id),
                full: true,
                row,
            })),
            Err(CollectionsError::NotFound) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    /// Entries of a playlist as `(entry id, library file id)`, in order.
    /// Callers check readability first.
    pub async fn playlist_entries(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<(String, Option<String>)>, CompatError> {
        Ok(self
            .collections
            .stores
            .playlists
            .tracks(playlist_id)
            .await?
            .into_iter()
            .map(|track| (track.id, track.library_file_id))
            .collect())
    }

    /// Create a playlist owned by the user holding the library files
    /// `file_ids`, in order, as one write. Unknown files are skipped.
    pub async fn create_playlist(
        &self,
        user_id: &str,
        name: &str,
        file_ids: &[String],
    ) -> Result<SavedPlaylist, CompatError> {
        let entries = self.file_entries(file_ids).await?;
        Ok(self
            .service()
            .create_with_entries(user_id, name, entries)
            .await?)
    }

    /// Replace the entries of a playlist the user owns with the library
    /// files `file_ids` and, when `name` is given, rename it, as one write.
    pub async fn replace_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        name: Option<&str>,
        file_ids: &[String],
    ) -> Result<SavedPlaylist, CompatError> {
        let entries = self.file_entries(file_ids).await?;
        Ok(self
            .service()
            .replace_with_entries(user_id, playlist_id, name, entries)
            .await?)
    }

    /// Rename a playlist the user owns.
    pub async fn rename_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        name: &str,
    ) -> Result<(), CompatError> {
        Ok(self.service().rename(user_id, playlist_id, name).await?)
    }

    /// Flip visibility of a playlist the user owns.
    pub async fn set_playlist_public(
        &self,
        user_id: &str,
        playlist_id: &str,
        public: bool,
    ) -> Result<(), CompatError> {
        Ok(self
            .service()
            .set_public(user_id, playlist_id, public)
            .await?)
    }

    /// Delete a playlist the user owns.
    pub async fn delete_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<(), CompatError> {
        Ok(self.service().delete_playlist(user_id, playlist_id).await?)
    }

    /// Remove entries by id from a playlist the user owns.
    pub async fn remove_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<(), CompatError> {
        self.service()
            .remove_entries(user_id, playlist_id, entry_ids)
            .await?;
        Ok(())
    }

    /// Move one entry of a playlist the user owns.
    pub async fn move_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        index: usize,
    ) -> Result<(), CompatError> {
        self.service()
            .move_entry(user_id, playlist_id, entry_id, index)
            .await?;
        Ok(())
    }

    /// Append library files to a playlist the user owns, as one write.
    pub async fn add_files(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_ids: &[String],
    ) -> Result<(), CompatError> {
        let entries = self.file_entries(file_ids).await?;
        if entries.is_empty() {
            return Ok(());
        }
        self.service()
            .add_entries(user_id, playlist_id, None, entries)
            .await?;
        Ok(())
    }

    /// Playlist entries for library files, in order. Each entry keeps the
    /// file's names so the web UI shows it too (v2 `add_file_id_entry`). A
    /// file asked twice is added twice, as v2 appends each; unknown files
    /// are skipped.
    async fn file_entries(&self, file_ids: &[String]) -> Result<Vec<NewEntry>, CompatError> {
        let by_id = self
            .tracks_by_ids(file_ids)
            .await?
            .into_iter()
            .map(|track| (track.id.clone(), track))
            .collect::<HashMap<_, _>>();
        Ok(file_ids
            .iter()
            .filter_map(|id| by_id.get(id).cloned())
            .map(|track| NewEntry {
                track_name: track.title,
                artist_name: track.artist_name,
                album_name: track.album_title,
                album_id: Some(track.album_id),
                artist_id: track.artist_id,
                track_source_id: Some(track.id.clone()),
                cover_url: None,
                source_type: LOCAL_SOURCE.to_owned(),
                available_sources: Some(vec![LOCAL_SOURCE.to_owned()]),
                format: Some(track.format),
                track_number: i32::try_from(track.track_number).ok(),
                disc_number: i32::try_from(track.disc_number).ok(),
                duration: track.duration_seconds.map(f64::round),
                plex_rating_key: None,
                library_file_id: Some(track.id),
            })
            .collect())
    }

    /// Cover of a readable playlist: the uploaded one, else a picture made
    /// from its tracks' albums as the web tile is (four covers tiled when
    /// four albums have one, else the first cover), or None when no track
    /// has a cover. Made on each request, never stored.
    pub async fn playlist_cover(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<ImageBytes>, CompatError> {
        let Some(visible) = self.playlist(user_id, playlist_id).await? else {
            return Ok(None);
        };
        if visible.row.has_cover {
            return match self.service().cover(user_id, playlist_id).await {
                Ok(cover) => Ok(Some((cover.bytes, cover.content_type))),
                Err(CollectionsError::NotFound) => Ok(None),
                Err(other) => Err(other.into()),
            };
        }
        let file_ids = self
            .playlist_entries(playlist_id)
            .await?
            .into_iter()
            .filter_map(|(_, file_id)| file_id)
            .take(TILE_ENTRIES)
            .collect::<Vec<_>>();
        let album_of = self
            .tracks_by_ids(&file_ids)
            .await?
            .into_iter()
            .map(|track| (track.id, track.album_id))
            .collect::<HashMap<_, _>>();
        let mut albums: Vec<&String> = Vec::new();
        for album_id in file_ids.iter().filter_map(|id| album_of.get(id)) {
            if !albums.contains(&album_id) {
                albums.push(album_id);
            }
        }
        let mut covers = Vec::new();
        for album_id in albums.into_iter().take(TILE_ALBUMS) {
            if let Some(cover) = self.album_cover(album_id, Some(TILE_SOURCE_SIZE)).await? {
                covers.push(cover);
                if covers.len() == TILE_COUNT {
                    break;
                }
            }
        }
        if covers.len() < TILE_COUNT {
            return Ok(covers.into_iter().next());
        }
        let first = covers.first().cloned();
        let tiled = tokio::task::spawn_blocking(move || tile_covers(&covers))
            .await
            .map_err(|error| CompatError::Internal(format!("playlist cover task: {error}")))?;
        Ok(tiled.or(first))
    }

    /// Whether an album's own art comes before the Cover Art Archive,
    /// as the cover read decides it. Local art wins without the service,
    /// as it does when the setting cannot be read.
    pub fn local_art_first(&self) -> bool {
        self.artwork
            .as_ref()
            .is_none_or(ArtworkService::prefers_local_art)
    }

    /// Art for a local album: its own folder or embedded art, or the
    /// release group's Cover Art Archive front, per the local-art setting.
    pub async fn album_cover(
        &self,
        album_id: &str,
        size: Option<&str>,
    ) -> Result<Option<ImageBytes>, CompatError> {
        Ok(self
            .covers
            .album_cover(album_id, size)
            .await
            .map(|cover| (cover.bytes, cover.content_type)))
    }

    /// An artist's picture: a linked artist's own image through its MBID
    /// (`size_px` wide), else the picture the web UI shows for an unlinked
    /// one, a mosaic of their albums' art, at the `size` bucket
    /// (250/500/1200). None when there is no picture.
    pub async fn artist_cover(
        &self,
        artist_id: &str,
        size_px: Option<u32>,
        size: &str,
    ) -> Result<Option<ImageBytes>, CompatError> {
        let Some(artist) = self.catalog.get_artist(artist_id).await? else {
            return Ok(None);
        };
        let Some(mbid) = artist.artist_mbid else {
            return Ok(self
                .covers
                .local_artist_art(&artist.id, Some(size))
                .await
                .map(|cover| (cover.bytes, cover.content_type)));
        };
        Ok(match self.covers.artist_image(&mbid, size_px).await {
            CoverLookup::Found(cover) => Some((cover.bytes, cover.content_type)),
            CoverLookup::Warming | CoverLookup::Missing => None,
        })
    }

    /// Which of `artist_ids` have an album with its own art (folder or
    /// embedded) to draw their picture from: the albums the catalog's
    /// artist mosaic reads, in one query. Lets a list say which unlinked
    /// artists have a picture without drawing one.
    pub async fn artists_with_local_art(
        &self,
        artist_ids: &[String],
    ) -> Result<HashSet<String>, CompatError> {
        if artist_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let ids = serde_json::to_string(artist_ids)
            .map_err(|error| CompatError::Internal(format!("artist ids: {error}")))?;
        let found: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT credit.artist_id FROM ( \
               SELECT album_artist_id AS artist_id, id AS album_id FROM local_albums \
               WHERE album_artist_id IN (SELECT value FROM json_each(?1)) \
               UNION SELECT local_artist_id, local_album_id FROM local_album_artists \
               WHERE local_artist_id IN (SELECT value FROM json_each(?1))) credit \
             JOIN local_albums a ON a.id = credit.album_id \
               AND a.retired_into_album_id IS NULL AND a.has_indexed_track = 1 \
             JOIN local_album_artwork w ON w.local_album_id = a.id \
             LEFT JOIN local_tracks t ON t.id = w.source_locator AND w.source = 'embedded' \
               AND t.availability = 'indexed' \
             WHERE (w.source = 'folder' AND w.source_locator IS NOT NULL) \
               OR (w.source = 'embedded' AND t.file_path IS NOT NULL)",
        )
        .bind(ids)
        .fetch_all(self.collections.db.pool()?)
        .await
        .map_err(|error| {
            droppedneedle_catalog::reads::collections::db::StoreError::read(
                "compat.artist_art",
                error,
            )
        })?;
        Ok(found.into_iter().collect())
    }
}
