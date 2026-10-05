//! The production Subsonic store: the v3 catalog, shared playlists and
//! favorites, durable queues and bookmarks, real playback reports, scans
//! and avatars.
//!
//! Catalog reads carry the caller's starred dates and play counts once the
//! dispatcher scopes the store to the authenticated user
//! ([`Store::for_caller`]).

use std::collections::HashMap;

use super::library::{CompatError, CompatLibrary, ImageBytes, whole};
use super::playback::CompatPlayback;
use super::queues::CompatQueues;
use crate::auth::users::UsersDeps;
use crate::compat::subsonic::ids::IdKind;
use crate::compat::subsonic::store::{
    AlbumSort, BookmarkRecord, ClientInfo, LyricLine, LyricsData, NowPlayingRow, PlaylistDetail,
    PlaylistEntry, PlaylistRecord, PlaylistSummary, QueueState, Store, TranscodeDecisionData,
};
use crate::compat::subsonic::views::{ViewAlbum, ViewArtist, ViewGenre, ViewTrack};
use crate::library::scan::counter_names;
use crate::library::scan::models::{ScanKind, ScanRequest, ScanTrigger};
use crate::library::wiring::LibrarySetup;
use crate::reads::collections::service::Visible;
use crate::reads::library::player::{
    AlbumOrder, AlbumQuery, PlayerAlbum, PlayerTrack, TrackOrder, TrackQuery,
};
use crate::reads::library::stores::ArtistRecord;

/// Unix seconds to the `YYYY-MM-DDTHH:MM:SSZ` form Subsonic shows.
fn iso_text(unix: u64) -> Option<String> {
    crate::compat::subsonic::views::iso(Some(unix as i64))
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

fn view_track(track: PlayerTrack) -> ViewTrack {
    ViewTrack {
        file_id: track.id,
        title: track.title,
        rg_mbid: Some(track.album_id),
        album_title: Some(track.album_title),
        artist_name: track.artist_name,
        artist_mbid: track.artist_id,
        album_artist_name: track.album_artist_name,
        album_artist_mbid: Some(track.album_artist_id),
        track_number: track.track_number,
        disc_number: track.disc_number,
        year: track.year,
        genre: (!track.genres.is_empty()).then(|| track.genres.join(";")),
        file_size_bytes: track.file_size_bytes,
        file_format: Some(track.format),
        duration_seconds: track.duration_seconds.unwrap_or(0.0),
        bitrate: track.bit_rate,
        created_at: whole(track.date_added),
        starred_at: None,
        play_count: None,
        bit_depth: track.bit_depth,
        sample_rate: track.sample_rate,
        channels: track.channels,
        musicbrainz_recording_id: track.recording_mbid.clone(),
        musicbrainz_artist_id: track.artist_mbid,
        musicbrainz_album_artist_id: track.album_artist_mbid,
        recording_mbid: track.recording_mbid,
        provider_identity_projected: true,
        played_at: None,
        sort_name: track.sort_name,
        replaygain_track_gain: track.replaygain_track_gain,
        replaygain_album_gain: track.replaygain_album_gain,
        replaygain_track_peak: track.replaygain_track_peak,
        replaygain_album_peak: track.replaygain_album_peak,
    }
}

fn view_album(album: PlayerAlbum) -> ViewAlbum {
    let record = album.record;
    ViewAlbum {
        rg_mbid: record.id,
        title: record.title,
        artist_name: Some(record.artist_name),
        artist_mbid: Some(record.artist_id),
        track_count: Some(record.track_count as i64),
        total_duration_seconds: Some(record.total_duration_seconds),
        play_count: None,
        date_added: whole(record.date_added),
        starred_at: None,
        year: record.year,
        genre: album.genre,
        is_compilation: Some(record.is_compilation),
        musicbrainz_release_group_id: record.release_group_mbid,
        musicbrainz_artist_id: record.artist_mbid,
        provider_identity_projected: true,
        played_at: None,
        sort_name: album.sort_name,
        original_release_date: album.original_release_date,
        release_types: None,
        disc_titles: Vec::new(),
    }
}

fn view_artist(artist: ArtistRecord) -> ViewArtist {
    ViewArtist {
        artist_mbid: artist.id,
        name: artist.name,
        album_count: Some(artist.album_count as i64),
        starred_at: None,
        musicbrainz_artist_id: artist.artist_mbid,
        provider_identity_projected: true,
    }
}

fn record(visible: &Visible) -> PlaylistRecord {
    PlaylistRecord {
        id: visible.row.id.clone(),
        name: visible.row.name.clone(),
        is_public: visible.row.is_public,
        has_cover: visible.row.has_cover,
        created_at: iso_text(visible.row.created_at),
        changed_at: iso_text(visible.row.updated_at),
    }
}

/// Production Subsonic store (see module docs).
#[derive(Clone)]
pub struct SubsonicStore {
    library: CompatLibrary,
    queues: CompatQueues,
    playback: CompatPlayback,
    users: UsersDeps,
    scan: LibrarySetup,
    caller: Option<String>,
}

impl SubsonicStore {
    /// Bind the live deps; unscoped until [`Store::for_caller`].
    pub fn new(
        library: CompatLibrary,
        queues: CompatQueues,
        playback: CompatPlayback,
        users: UsersDeps,
        scan: LibrarySetup,
    ) -> Self {
        Self {
            library,
            queues,
            playback,
            users,
            scan,
            caller: None,
        }
    }

    fn now() -> f64 {
        crate::reads::collections::db::now_real()
    }

    async fn tracks_for_caller(
        &self,
        tracks: Vec<PlayerTrack>,
    ) -> Result<Vec<ViewTrack>, CompatError> {
        let mut views = tracks.into_iter().map(view_track).collect::<Vec<_>>();
        if let Some(user_id) = &self.caller {
            let ids = views
                .iter()
                .map(|view| view.file_id.clone())
                .collect::<Vec<_>>();
            let starred = self.library.starred(user_id, "track", &ids).await?;
            let plays = self.library.plays(user_id, "track", &ids).await?;
            for view in &mut views {
                view.starred_at = starred.get(&view.file_id).copied();
                if let Some((count, last)) = plays.get(&view.file_id) {
                    view.play_count = Some(*count as i64);
                    view.played_at = whole(*last).and_then(|at| iso_text(at.max(0) as u64));
                }
            }
        }
        Ok(views)
    }

    async fn albums_for_caller(
        &self,
        albums: Vec<PlayerAlbum>,
    ) -> Result<Vec<ViewAlbum>, CompatError> {
        let mut views = albums.into_iter().map(view_album).collect::<Vec<_>>();
        if let Some(user_id) = &self.caller {
            let ids = views
                .iter()
                .map(|view| view.rg_mbid.clone())
                .collect::<Vec<_>>();
            let starred = self.library.starred(user_id, "album", &ids).await?;
            let plays = self.library.plays(user_id, "album", &ids).await?;
            for view in &mut views {
                view.starred_at = starred.get(&view.rg_mbid).copied();
                if let Some((count, last)) = plays.get(&view.rg_mbid) {
                    view.play_count = Some(*count as i64);
                    view.played_at = whole(*last).and_then(|at| iso_text(at.max(0) as u64));
                }
            }
        }
        Ok(views)
    }

    async fn artists_for_caller(
        &self,
        artists: Vec<ArtistRecord>,
    ) -> Result<Vec<ViewArtist>, CompatError> {
        let mut views = artists.into_iter().map(view_artist).collect::<Vec<_>>();
        if let Some(user_id) = &self.caller {
            let ids = views
                .iter()
                .map(|view| view.artist_mbid.clone())
                .collect::<Vec<_>>();
            let starred = self.library.starred(user_id, "artist", &ids).await?;
            for view in &mut views {
                view.starred_at = starred.get(&view.artist_mbid).copied();
            }
        }
        Ok(views)
    }
}

impl Store for SubsonicStore {
    type Error = CompatError;

    fn for_caller(&self, user_id: &str) -> Self {
        Self {
            caller: Some(user_id.to_owned()),
            ..self.clone()
        }
    }

    async fn get_artists(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> Result<(Vec<ViewArtist>, usize), CompatError> {
        let (artists, total) = self
            .library
            .album_artists(query, limit as u64, offset as u64)
            .await?;
        Ok((self.artists_for_caller(artists).await?, total as usize))
    }

    async fn get_library_revision(&self) -> Result<i64, CompatError> {
        self.library.revision().await
    }

    async fn get_artist_with_albums(
        &self,
        artist_mbid: &str,
    ) -> Result<Option<(ViewArtist, Vec<ViewAlbum>)>, CompatError> {
        let Some(artist) = self.library.artist(artist_mbid).await? else {
            return Ok(None);
        };
        let query = AlbumQuery {
            artist_id: Some(artist_mbid.to_owned()),
            ..AlbumQuery::default()
        };
        let (albums, _) = self
            .library
            .albums(&query, AlbumOrder::Title, u64::MAX >> 1, 0)
            .await?;
        let Some(artist) = self.artists_for_caller(vec![artist]).await?.pop() else {
            return Ok(None);
        };
        Ok(Some((artist, self.albums_for_caller(albums).await?)))
    }

    async fn get_album(&self, rg_mbid: &str) -> Result<Option<ViewAlbum>, CompatError> {
        let Some(album) = self.library.album(rg_mbid).await? else {
            return Ok(None);
        };
        if album.record.track_count == 0 {
            return Ok(None);
        }
        let tracks = self.library.album_tracks(rg_mbid).await?;
        let mut disc_titles: Vec<(i64, String)> = Vec::new();
        for track in &tracks {
            if let Some(title) = &track.disc_subtitle
                && !disc_titles
                    .iter()
                    .any(|(disc, _)| *disc == track.disc_number)
            {
                disc_titles.push((track.disc_number, title.clone()));
            }
        }
        let release_type = {
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for track in &tracks {
                if let Some(kind) = track.release_type.as_deref().map(str::trim)
                    && !kind.is_empty()
                {
                    *counts.entry(kind).or_default() += 1;
                }
            }
            counts
                .into_iter()
                .max_by(|a, b| a.1.cmp(&b.1).then(b.0.cmp(a.0)))
                .map(|(kind, _)| kind.to_lowercase())
        };
        let mut views = self.albums_for_caller(vec![album]).await?;
        Ok(views.pop().map(|mut view| {
            view.disc_titles = disc_titles;
            if let Some(kind) = release_type {
                let mut types = vec![kind];
                if view.is_compilation == Some(true) && !types.iter().any(|t| t == "compilation") {
                    types.push("compilation".to_owned());
                }
                view.release_types = Some(types);
            }
            view
        }))
    }

    async fn get_album_tracks(&self, rg_mbid: &str) -> Result<Vec<ViewTrack>, CompatError> {
        let tracks = self.library.album_tracks(rg_mbid).await?;
        self.tracks_for_caller(tracks).await
    }

    async fn get_track(&self, file_id: &str) -> Result<Option<ViewTrack>, CompatError> {
        let Some(track) = self.library.track(file_id).await? else {
            return Ok(None);
        };
        Ok(self.tracks_for_caller(vec![track]).await?.pop())
    }

    async fn get_tracks_by_file_ids(
        &self,
        file_ids: &[String],
    ) -> Result<HashMap<String, ViewTrack>, CompatError> {
        let tracks = self.library.tracks_by_ids(file_ids).await?;
        Ok(self
            .tracks_for_caller(tracks)
            .await?
            .into_iter()
            .map(|view| (view.file_id.clone(), view))
            .collect())
    }

    async fn get_tracks_page(
        &self,
        limit: usize,
        offset: usize,
        query: Option<&str>,
    ) -> Result<(Vec<ViewTrack>, usize), CompatError> {
        let query = TrackQuery {
            q: query.map(str::to_owned),
            ..TrackQuery::default()
        };
        let (tracks, total) = self
            .library
            .tracks(&query, TrackOrder::Album, limit as u64, offset as u64)
            .await?;
        Ok((self.tracks_for_caller(tracks).await?, total as usize))
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
    ) -> Result<(Vec<ViewAlbum>, usize), CompatError> {
        let (low, high) = match (from_year, to_year) {
            (Some(from), Some(to)) => (Some(from.min(to)), Some(from.max(to))),
            other => other,
        };
        let order = match sort {
            AlbumSort::Recent => AlbumOrder::Newest,
            AlbumSort::Title => AlbumOrder::Title,
            AlbumSort::Artist => AlbumOrder::Artist,
            AlbumSort::Random => AlbumOrder::Random,
            AlbumSort::YearAsc => AlbumOrder::YearAsc,
            AlbumSort::YearDesc => AlbumOrder::YearDesc,
        };
        let query = AlbumQuery {
            q: query.map(str::to_owned),
            genre: genre.map(str::to_owned),
            year_from: low,
            year_to: high,
            ..AlbumQuery::default()
        };
        let (albums, total) = self
            .library
            .albums(&query, order, limit as u64, offset as u64)
            .await?;
        Ok((self.albums_for_caller(albums).await?, total as usize))
    }

    async fn get_random_songs(
        &self,
        count: usize,
        genre: Option<&str>,
        from_year: Option<i64>,
        to_year: Option<i64>,
    ) -> Result<Vec<ViewTrack>, CompatError> {
        let query = TrackQuery {
            genre: genre.map(str::to_owned),
            year_from: from_year,
            year_to: to_year,
            ..TrackQuery::default()
        };
        let (tracks, _) = self
            .library
            .tracks(&query, TrackOrder::Random, count as u64, 0)
            .await?;
        self.tracks_for_caller(tracks).await
    }

    async fn get_history_albums(
        &self,
        user_id: &str,
        frequent: bool,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewAlbum>, CompatError> {
        let ids = self
            .library
            .history_albums(user_id, frequent, limit as u64, offset as u64)
            .await?;
        let albums = self.library.albums_in_order(&ids).await?;
        self.albums_for_caller(albums).await
    }

    async fn get_starred_albums(
        &self,
        user_id: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewAlbum>, CompatError> {
        let ids = self
            .library
            .favorite_ids(user_id, "album")
            .await?
            .into_iter()
            .skip(offset)
            .take(limit)
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        let albums = self.library.albums_in_order(&ids).await?;
        self.albums_for_caller(albums).await
    }

    async fn get_genres(&self) -> Result<Vec<ViewGenre>, CompatError> {
        Ok(self
            .library
            .genres()
            .await?
            .into_iter()
            .map(|genre| ViewGenre {
                name: genre.name,
                song_count: genre.track_count as i64,
                album_count: genre.album_count as i64,
            })
            .collect())
    }

    async fn get_songs_by_genre(
        &self,
        genre: &str,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ViewTrack>, CompatError> {
        let query = TrackQuery {
            genre: Some(genre.to_owned()),
            ..TrackQuery::default()
        };
        let (tracks, _) = self
            .library
            .tracks(&query, TrackOrder::Album, limit as u64, offset as u64)
            .await?;
        self.tracks_for_caller(tracks).await
    }

    async fn missing_targets(
        &self,
        targets: &[(IdKind, String)],
    ) -> Result<Vec<(IdKind, String)>, CompatError> {
        let mut existing: HashMap<&'static str, std::collections::HashSet<String>> = HashMap::new();
        for kind in ["artist", "album", "track"] {
            let ids = targets
                .iter()
                .filter(|(target, _)| kind_name(*target) == kind)
                .map(|(_, id)| id.clone())
                .collect::<Vec<_>>();
            if !ids.is_empty() {
                existing.insert(kind, self.library.player.existing(kind, &ids).await?);
            }
        }
        let mut missing = Vec::new();
        for (kind, id) in targets {
            let present = match kind {
                IdKind::Playlist => self
                    .library
                    .collections
                    .stores
                    .playlists
                    .get(id)
                    .await?
                    .is_some(),
                IdKind::Genre => true,
                other => existing
                    .get(kind_name(*other))
                    .is_some_and(|ids| ids.contains(id)),
            };
            if !present {
                missing.push((*kind, id.clone()));
            }
        }
        Ok(missing)
    }

    async fn get_top_songs(
        &self,
        artist: &str,
        user_id: &str,
        count: usize,
    ) -> Result<Vec<ViewTrack>, CompatError> {
        let query = TrackQuery {
            artist_name: Some(artist.to_owned()),
            ..TrackQuery::default()
        };
        let pool = (count.max(1) * 4) as u64;
        let (tracks, _) = self
            .library
            .tracks(&query, TrackOrder::Newest, pool, 0)
            .await?;
        let ids = tracks
            .iter()
            .map(|track| track.id.clone())
            .collect::<Vec<_>>();
        let plays = self.library.plays(user_id, "track", &ids).await?;
        let mut ranked = tracks;
        // Stable: most played first, newest import breaks ties (v2).
        ranked.sort_by_key(|track| {
            std::cmp::Reverse(plays.get(&track.id).map(|(count, _)| *count).unwrap_or(0))
        });
        ranked.truncate(count);
        self.tracks_for_caller(ranked).await
    }

    async fn get_similar_songs(
        &self,
        artist_mbid: &str,
        _user_id: &str,
        count: usize,
    ) -> Result<Vec<ViewTrack>, CompatError> {
        let query = TrackQuery {
            artist_ids: vec![artist_mbid.to_owned()],
            ..TrackQuery::default()
        };
        let (tracks, _) = self
            .library
            .tracks(&query, TrackOrder::Random, count as u64, 0)
            .await?;
        self.tracks_for_caller(tracks).await
    }

    async fn get_all_playlists(&self, user_id: &str) -> Result<Vec<PlaylistSummary>, CompatError> {
        Ok(self
            .library
            .playlists(user_id)
            .await?
            .iter()
            .map(|visible| PlaylistSummary {
                record: visible.full.then(|| record(visible)),
                is_owner: visible.is_owner,
                owner_name: visible.row.owner_name.clone().unwrap_or_default(),
            })
            .collect())
    }

    async fn get_streamable_counts(&self) -> Result<HashMap<String, (i64, i64)>, CompatError> {
        Ok(self
            .library
            .collections
            .stores
            .playlists
            .list()
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.id,
                    (
                        row.streamable_count as i64,
                        row.streamable_duration.round() as i64,
                    ),
                )
            })
            .collect())
    }

    async fn get_playlist_with_tracks(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> Result<Option<PlaylistDetail>, CompatError> {
        let Some(visible) = self.library.playlist(user_id, playlist_id).await? else {
            return Ok(None);
        };
        let tracks = self.get_playlist_tracks(playlist_id).await?;
        Ok(Some(PlaylistDetail {
            record: record(&visible),
            tracks,
            is_owner: visible.is_owner,
            owner_name: visible.row.owner_name.clone().unwrap_or_default(),
        }))
    }

    async fn get_playlist_tracks(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistEntry>, CompatError> {
        Ok(self
            .library
            .playlist_entries(playlist_id)
            .await?
            .into_iter()
            .map(|(id, library_file_id)| PlaylistEntry {
                id,
                library_file_id,
            })
            .collect())
    }

    async fn create_playlist(
        &self,
        name: &str,
        user_id: &str,
    ) -> Result<PlaylistRecord, CompatError> {
        let id = self.library.create_playlist(user_id, name).await?;
        let visible = self
            .library
            .playlist(user_id, &id)
            .await?
            .ok_or_else(|| CompatError("created playlist vanished".to_owned()))?;
        Ok(record(&visible))
    }

    async fn update_playlist(
        &self,
        playlist_id: &str,
        user_id: &str,
        name: &str,
    ) -> Result<(), CompatError> {
        self.library
            .rename_playlist(user_id, playlist_id, name)
            .await
    }

    async fn set_playlist_public(
        &self,
        playlist_id: &str,
        user_id: &str,
        public: bool,
    ) -> Result<(), CompatError> {
        self.library
            .set_playlist_public(user_id, playlist_id, public)
            .await
    }

    async fn remove_playlist_tracks(
        &self,
        playlist_id: &str,
        user_id: &str,
        entry_ids: &[String],
    ) -> Result<(), CompatError> {
        self.library
            .remove_entries(user_id, playlist_id, entry_ids)
            .await
    }

    async fn add_playlist_file(
        &self,
        playlist_id: &str,
        file_id: &str,
        user_id: &str,
    ) -> Result<(), CompatError> {
        self.library
            .add_files(user_id, playlist_id, &[file_id.to_owned()])
            .await
    }

    async fn delete_playlist(&self, playlist_id: &str, user_id: &str) -> Result<(), CompatError> {
        self.library.delete_playlist(user_id, playlist_id).await
    }

    async fn playlist_cover(
        &self,
        playlist_id: &str,
        user_id: &str,
    ) -> Result<Option<ImageBytes>, CompatError> {
        self.library.playlist_cover(user_id, playlist_id).await
    }

    async fn apply_favorites(
        &self,
        user_id: &str,
        targets: &[(IdKind, String)],
        add: bool,
    ) -> Result<(), CompatError> {
        let targets = targets
            .iter()
            .map(|(kind, id)| (kind_name(*kind).to_owned(), id.clone()))
            .collect::<Vec<_>>();
        self.library.set_favorites(user_id, &targets, add).await
    }

    async fn list_favorites(
        &self,
        user_id: &str,
        kind: IdKind,
    ) -> Result<Vec<(String, i64)>, CompatError> {
        self.library.favorite_ids(user_id, kind_name(kind)).await
    }

    async fn scrobble(
        &self,
        file_id: &str,
        user_id: &str,
        _client: Option<&str>,
        played_at: Option<f64>,
        _user_name: &str,
    ) -> Result<(), CompatError> {
        if !self.playback.scrobble(file_id, user_id, played_at) {
            tracing::warn!(
                file_id,
                "compat scrobble was refused by the playback service"
            );
        }
        Ok(())
    }

    async fn now_playing(
        &self,
        file_id: &str,
        user_id: &str,
        client: Option<&str>,
        _user_name: &str,
    ) -> Result<(), CompatError> {
        self.playback.now_playing(file_id, user_id, client);
        Ok(())
    }

    async fn compat_now_playing(&self) -> Result<Vec<NowPlayingRow>, CompatError> {
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
    ) -> Result<(), CompatError> {
        if !self
            .playback
            .report_playback(file_id, user_id, client, state, ignore_scrobble)
        {
            tracing::warn!(file_id, state, "compat playback report was refused");
        }
        Ok(())
    }

    async fn get_play_queue(&self, user_id: &str) -> Result<QueueState, CompatError> {
        Ok(self.queues.get_queue(user_id).await?)
    }

    async fn replace_play_queue(
        &self,
        user_id: &str,
        file_ids: &[String],
        current_index: Option<usize>,
        position_ms: i64,
        changed_by_client: &str,
    ) -> Result<(), CompatError> {
        Ok(self
            .queues
            .replace_queue(
                user_id,
                file_ids,
                current_index,
                position_ms,
                changed_by_client,
                Self::now(),
            )
            .await?)
    }

    async fn list_bookmarks(&self, user_id: &str) -> Result<Vec<BookmarkRecord>, CompatError> {
        Ok(self.queues.list_bookmarks(user_id).await?)
    }

    async fn upsert_bookmark(
        &self,
        user_id: &str,
        file_id: &str,
        position_ms: i64,
        comment: &str,
    ) -> Result<(), CompatError> {
        Ok(self
            .queues
            .upsert_bookmark(user_id, file_id, position_ms, comment, Self::now())
            .await?)
    }

    async fn delete_bookmark(&self, user_id: &str, file_id: &str) -> Result<(), CompatError> {
        Ok(self.queues.delete_bookmark(user_id, file_id).await?)
    }

    async fn get_lyrics(&self, file_id: &str) -> Result<Option<LyricsData>, CompatError> {
        Ok(self.library.lyrics(file_id).await?.map(|doc| LyricsData {
            language: "und".to_owned(),
            synced: doc.synced,
            lines: doc
                .lines
                .into_iter()
                .map(|(value, start_ms)| LyricLine { value, start_ms })
                .collect(),
        }))
    }

    async fn resolve_avatar(&self, user_id: &str) -> Result<Option<ImageBytes>, CompatError> {
        self.users
            .avatars
            .load(user_id)
            .await
            .map_err(|error| CompatError(format!("avatar load failed: {error:?}")))
    }

    async fn scan_status(&self) -> Result<(bool, Option<i64>), CompatError> {
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

    async fn start_scan(&self) -> Result<(), CompatError> {
        let registry = self.scan.live_registry();
        let outcome = self.scan.coordinator.request_run(&ScanRequest {
            kind: ScanKind::Incremental,
            trigger: ScanTrigger::Subsonic,
            scopes: registry.scheduled_root_scopes(),
            requested_by_user_id: self.caller.clone(),
            policy_revision: registry.policy_revision().to_owned(),
        });
        if let Err(error) = outcome {
            tracing::warn!(?error, "subsonic scan request was not admitted");
        }
        Ok(())
    }

    async fn release_group_cover(
        &self,
        rg_mbid: &str,
        bucket: &str,
    ) -> Result<Option<ImageBytes>, CompatError> {
        self.library.album_cover(rg_mbid, Some(bucket)).await
    }

    async fn artist_image(
        &self,
        artist_mbid: &str,
        px: Option<i64>,
    ) -> Result<Option<ImageBytes>, CompatError> {
        let px = px.and_then(|value| u32::try_from(value).ok());
        self.library.artist_cover(artist_mbid, px).await
    }

    async fn advanced_decide(
        &self,
        _track: &ViewTrack,
        _client: &ClientInfo,
        _user_id: &str,
    ) -> Result<TranscodeDecisionData, CompatError> {
        // Signed advanced-transcode params are not issued yet; the
        // decide-based stream path still transcodes on its own.
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
    ) -> Result<(bool, Option<String>, Option<i64>), CompatError> {
        Ok((false, None, None))
    }
}
