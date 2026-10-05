//! The library both compat protocols read: the v3 catalog, the shared
//! collections (playlists, favorites), lyrics and cover art.
//!
//! Ids are local library ids. The view field names (`rg_mbid`,
//! `artist_mbid`, `file_id`) are the protocol layers' names for "album id",
//! "artist id" and "track id"; real MusicBrainz ids ride the
//! `musicbrainz_*` fields with `provider_identity_projected` set, so no
//! local id is ever advertised as an MBID.

use std::collections::HashMap;
use std::sync::Arc;

use crate::reads::collections::service::{CollectionsService, LOCAL_SOURCE, Visible};
use crate::reads::collections::store::playlists::NewEntry;
use crate::reads::collections::{CollectionsState, error::CollectionsError};
use crate::reads::library::player::{
    AlbumOrder, AlbumQuery, PlayStat, PlayerAlbum, PlayerCatalog, PlayerTrack, TrackOrder,
    TrackQuery,
};
use crate::reads::library::stores::{
    ArtistRecord, ArtistScope, ArtistSort, GenreRecord, LibraryCatalog, LyricDoc, LyricsPort,
    StoreError,
};
use crate::reads::platform::covers::CoverArt;

/// A compat read or write failed. The text goes to the log only; the
/// protocol layers answer with their fixed internal message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatError(pub String);

impl std::fmt::Display for CompatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<StoreError> for CompatError {
    fn from(error: StoreError) -> Self {
        Self(error.to_string())
    }
}

impl From<crate::reads::collections::db::StoreError> for CompatError {
    fn from(error: crate::reads::collections::db::StoreError) -> Self {
        Self(error.to_string())
    }
}

impl From<CollectionsError> for CompatError {
    fn from(error: CollectionsError) -> Self {
        Self(format!("collections refused: {error:?}"))
    }
}

/// Cover bytes plus content type.
pub type ImageBytes = (Vec<u8>, String);

/// Everything compat reads, behind one cheap clone.
#[derive(Clone)]
pub struct CompatLibrary {
    /// Native catalog reads (artists, genres, single albums and artists).
    pub catalog: Arc<dyn LibraryCatalog>,
    /// Player reads (rich tracks, filtered pages, history).
    pub player: Arc<dyn PlayerCatalog>,
    /// Shared playlists and favorites.
    pub collections: CollectionsState,
    /// Stored or fetched lyrics.
    pub lyrics: Arc<dyn LyricsPort>,
    /// Cover art by MBID.
    pub covers: Arc<dyn CoverArt>,
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
    pub fn from_reads(reads: &crate::reads::ReadsSetup) -> Self {
        use crate::reads::library::{player::SqlitePlayerCatalog, sqlite::LibraryDb};

        let player_db = reads
            .collections
            .db
            .pool()
            .map(LibraryDb::new)
            .unwrap_or_else(|_| LibraryDb::unwired());
        Self {
            catalog: reads.library.catalog.clone(),
            player: Arc::new(SqlitePlayerCatalog::new(&player_db)),
            collections: reads.collections.clone(),
            lyrics: reads.library.lyrics.clone(),
            covers: reads.platform.covers.covers.clone(),
        }
    }

    fn service(&self) -> CollectionsService<'_> {
        CollectionsService::new(&self.collections)
    }

    /// Artists leading at least one album, name order, plus the total.
    pub async fn album_artists(
        &self,
        query: Option<&str>,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<ArtistRecord>, u64), CompatError> {
        let (artists, total, _, _) = self
            .catalog
            .list_artists(
                ArtistScope::AlbumArtists,
                query,
                ArtistSort::Name,
                false,
                limit,
                offset,
            )
            .await?;
        Ok((artists, total))
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
            .player
            .albums_by_ids(&[id.to_owned()])
            .await?
            .into_iter()
            .next())
    }

    /// Albums by id, in the order asked; missing ids drop out.
    pub async fn albums_in_order(&self, ids: &[String]) -> Result<Vec<PlayerAlbum>, CompatError> {
        let mut by_id = self
            .player
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
        Ok(self.player.albums(query, order, limit, offset).await?)
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<PlayerTrack>, u64), CompatError> {
        Ok(self.player.tracks(query, order, limit, offset).await?)
    }

    /// An album's tracks in disc and track order.
    pub async fn album_tracks(&self, album_id: &str) -> Result<Vec<PlayerTrack>, CompatError> {
        let query = TrackQuery {
            album_id: Some(album_id.to_owned()),
            ..TrackQuery::default()
        };
        Ok(self
            .player
            .tracks(&query, TrackOrder::Album, u64::MAX >> 1, 0)
            .await?
            .0)
    }

    /// Tracks by id, in no particular order.
    pub async fn tracks_by_ids(&self, ids: &[String]) -> Result<Vec<PlayerTrack>, CompatError> {
        Ok(self.player.tracks_by_ids(ids).await?)
    }

    /// One track.
    pub async fn track(&self, id: &str) -> Result<Option<PlayerTrack>, CompatError> {
        Ok(self
            .player
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
        Ok(self.player.revision().await?)
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

    /// The user's play counts for some ids of one kind.
    pub async fn plays(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PlayStat>, CompatError> {
        Ok(self.player.play_stats(user_id, kind, ids).await?)
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
            .player
            .history_albums(user_id, frequent, limit, offset)
            .await?)
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

    /// Create a playlist owned by the user; returns its id.
    pub async fn create_playlist(&self, user_id: &str, name: &str) -> Result<String, CompatError> {
        Ok(self.service().create_named(user_id, name, None).await?)
    }

    /// Rename a playlist the user owns.
    pub async fn rename_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        name: &str,
    ) -> Result<(), CompatError> {
        let body = crate::reads::collections::models::UpdatePlaylistBody {
            name: Some(name.to_owned()),
        };
        self.service()
            .update_playlist(user_id, playlist_id, &body)
            .await?;
        Ok(())
    }

    /// Flip visibility of a playlist the user owns.
    pub async fn set_playlist_public(
        &self,
        user_id: &str,
        playlist_id: &str,
        public: bool,
    ) -> Result<(), CompatError> {
        self.service()
            .set_visibility(user_id, playlist_id, public)
            .await?;
        Ok(())
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

    /// Append library files to a playlist the user owns. Each entry keeps
    /// the file's names so the web UI shows it too (v2 `add_file_id_entry`).
    /// Unknown files are skipped.
    pub async fn add_files(
        &self,
        user_id: &str,
        playlist_id: &str,
        file_ids: &[String],
    ) -> Result<(), CompatError> {
        let mut by_id = self
            .tracks_by_ids(file_ids)
            .await?
            .into_iter()
            .map(|track| (track.id.clone(), track))
            .collect::<HashMap<_, _>>();
        let entries = file_ids
            .iter()
            .filter_map(|id| by_id.remove(id))
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
            .collect::<Vec<_>>();
        if entries.is_empty() {
            return Ok(());
        }
        self.service()
            .add_entries(user_id, playlist_id, None, entries)
            .await?;
        Ok(())
    }

    /// Uploaded cover of a readable playlist.
    pub async fn playlist_cover(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<ImageBytes>, CompatError> {
        match self.service().cover(user_id, playlist_id).await {
            Ok(cover) => Ok(Some((cover.bytes, cover.content_type))),
            Err(CollectionsError::NotFound) => Ok(None),
            Err(other) => Err(other.into()),
        }
    }

    /// Release art for a local album, through its release group.
    pub async fn album_cover(
        &self,
        album_id: &str,
        size: Option<&str>,
    ) -> Result<Option<ImageBytes>, CompatError> {
        let Some(album) = self.catalog.get_album(album_id).await? else {
            return Ok(None);
        };
        let Some(group) = album.release_group_mbid else {
            return Ok(None);
        };
        Ok(self
            .covers
            .release_group_cover(&group, size)
            .await
            .map(|cover| (cover.bytes, cover.content_type)))
    }

    /// Artist image for a local artist, through its MBID.
    pub async fn artist_cover(
        &self,
        artist_id: &str,
        size_px: Option<u32>,
    ) -> Result<Option<ImageBytes>, CompatError> {
        let Some(artist) = self.catalog.get_artist(artist_id).await? else {
            return Ok(None);
        };
        let Some(mbid) = artist.artist_mbid else {
            return Ok(None);
        };
        Ok(self
            .covers
            .artist_image(&mbid, size_px)
            .await
            .map(|cover| (cover.bytes, cover.content_type)))
    }
}
