//! Jellyfin source adapter.
//!
//! Live-version-cited quirks, kept with their citations:
//!
//! - Auth rides `Authorization: MediaBrowser Token="<key>"`. Jellyfin 10.11
//!   dropped the legacy Emby auth: the same server API key gets 401 via
//!   `X-Emby-Token` / `X-MediaBrowser-Token` / `?api_key=` but 200 via this
//!   header (verified against Jellyfin 10.11.11 in issue #151, evidence
//!   table on `GET /System/Info`).
//! - `GET /Items/Latest` with `includeItemTypes=MusicAlbum` is the recently
//!   added feed; `GET /Search/Hints` (not `/Items`) backs free-text search
//!   and answers `SearchHints`.
//! - A 404 means absence and maps to `None`, never to an error.
//! - Playlist membership comes from `/Playlists/{id}/Items` filtered to
//!   `Type == "Audio"`; most-played lists sort by `PlayCount` and drop
//!   zero-play rows client-side.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::Value;

use super::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemotePage, TrackBrowse};
use super::models::{
    AlbumView, ArtistIndexEntry, ArtistView, FavoritesView, HistoryPage, HubView, InfoView,
    LyricLine, LyricsView, MatchView, PlaylistDetail, PlaylistSummary, SearchResults, SessionView,
    SessionsView, SourceName, StatsView, TrackView,
};

/// Request timeout per upstream call, matching the v2 repository.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// MBID index scan batch size, matching the v2 repository.
const MBID_BATCH: i64 = 500;

/// Jellyfin browse client. Built per request from the caller's stored
/// connection; holds no cache (response caching is a wiring concern).
pub struct JellyfinAdapter {
    client: reqwest::Client,
    base_url: String,
    api_key: String,
    user_id: String,
}

impl std::fmt::Debug for JellyfinAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinAdapter")
            .field("base_url", &self.base_url)
            .field("user_id", &self.user_id)
            .finish_non_exhaustive()
    }
}

impl JellyfinAdapter {
    /// Build a client for one server. Empty base URL or key reads as
    /// unconfigured and every call fails with [`AdapterError::NotConfigured`].
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        api_key: String,
        user_id: String,
    ) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            user_id,
        }
    }

    /// True when a base URL and key are present.
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.api_key.is_empty()
    }

    /// Connectivity probe against `/System/Info`. Returns the server version
    /// label on success.
    pub async fn validate_connection(&self) -> Result<String, AdapterError> {
        let value = self.get("/System/Info", &[]).await?.unwrap_or(Value::Null);
        let name = str_field(&value, "ServerName").unwrap_or("Unknown");
        let version = str_field(&value, "Version").unwrap_or("Unknown");
        Ok(format!("Connected to {name} (v{version})"))
    }

    /// Full MBID-to-item index over the album catalog, 500 rows per page.
    /// The integrator's warmup loop calls this; `match_album` prefers the
    /// cheap search fallback and only scans when search misses.
    pub async fn mbid_index(&self) -> Result<HashMap<String, String>, AdapterError> {
        let mut index = HashMap::new();
        let mut offset: i64 = 0;
        loop {
            let params = self.paged_params(MBID_BATCH, offset);
            let mut full = vec![
                ("includeItemTypes".to_owned(), "MusicAlbum".to_owned()),
                ("recursive".to_owned(), "true".to_owned()),
                ("Fields".to_owned(), "ProviderIds".to_owned()),
            ];
            full.extend(params);
            let value = self.get("/Items", &full).await?.unwrap_or(Value::Null);
            let items = value
                .get("Items")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            if items.is_empty() {
                break;
            }
            for item in &items {
                let Some(id) = str_field(item, "Id") else {
                    continue;
                };
                let providers = item.get("ProviderIds").cloned().unwrap_or(Value::Null);
                for key in ["MusicBrainzReleaseGroup", "MusicBrainzAlbum"] {
                    if let Some(mbid) = str_field(&providers, key) {
                        index.insert(mbid.to_owned(), id.to_owned());
                    }
                }
            }
            let total = value
                .get("TotalRecordCount")
                .and_then(Value::as_i64)
                .unwrap_or(0);
            offset += MBID_BATCH;
            if offset >= total {
                break;
            }
        }
        Ok(index)
    }

    /// Hub highlights. Sections fail open to empty; only an all-empty hub
    /// on a configured server still returns (Jellyfin sections are cheap and
    /// independent, so partial failure is normal).
    pub async fn hub(&self) -> Result<HubView, AdapterError> {
        self.require_configured()?;
        let preview_browse = AlbumBrowse {
            limit: 12,
            ..AlbumBrowse::default()
        };
        let (stats, recent, added, favorites, top_artists, preview, genres) = tokio::join!(
            self.stats(),
            self.recent(20),
            self.recently_added(20),
            self.favorites(),
            self.most_played_artists(10),
            self.albums(&preview_browse),
            self.genres(),
        );
        Ok(HubView {
            source: SourceName::Jellyfin,
            stats: stats.ok(),
            recently_played: recent.unwrap_or_default(),
            recently_added: added.unwrap_or_default(),
            favorites: favorites.map(|view| view.albums).unwrap_or_default(),
            favorite_artists: Vec::new(),
            most_played_artists: top_artists.unwrap_or_default(),
            all_albums_preview: preview.map(|page| page.items).unwrap_or_default(),
            genres: genres.unwrap_or_default(),
        })
    }

    /// Library totals via three `limit=0` count queries.
    pub async fn stats(&self) -> Result<StatsView, AdapterError> {
        self.require_configured()?;
        let mut stats = StatsView {
            total_albums: 0,
            total_artists: 0,
            total_tracks: 0,
        };
        for (item_type, slot) in [
            ("MusicAlbum", &mut stats.total_albums),
            ("MusicArtist", &mut stats.total_artists),
            ("Audio", &mut stats.total_tracks),
        ] {
            let params = vec![
                ("includeItemTypes".to_owned(), item_type.to_owned()),
                ("recursive".to_owned(), "true".to_owned()),
                ("limit".to_owned(), "0".to_owned()),
            ];
            let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
            *slot = value
                .get("TotalRecordCount")
                .and_then(Value::as_i64)
                .unwrap_or(0);
        }
        Ok(stats)
    }

    /// One page of albums with genre/year/tags/studio filters.
    pub async fn albums(
        &self,
        browse: &AlbumBrowse,
    ) -> Result<RemotePage<AlbumView>, AdapterError> {
        self.require_configured()?;
        let mut params = vec![
            ("includeItemTypes".to_owned(), "MusicAlbum".to_owned()),
            ("recursive".to_owned(), "true".to_owned()),
            (
                "sortBy".to_owned(),
                sort_or(browse.sort_by.clone(), "SortName"),
            ),
            (
                "sortOrder".to_owned(),
                order_word(browse.descending).to_owned(),
            ),
            ("limit".to_owned(), browse.limit.to_string()),
            ("startIndex".to_owned(), browse.offset.to_string()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds,ChildCount".to_owned()),
        ];
        if !browse.genre.is_empty() {
            params.push(("genres".to_owned(), browse.genre.clone()));
        }
        if let Some(year) = browse.year {
            params.push(("years".to_owned(), year.to_string()));
        }
        let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
        Ok(items_page(&value, |item| self.album_view(item)))
    }

    /// One album by id. None is absence (404).
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        let params = vec![("Fields".to_owned(), "ProviderIds,ChildCount".to_owned())];
        let value = self
            .get(&format!("/Items/{id}"), &params)
            .await?
            .unwrap_or(Value::Null);
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(self.album_view(&value)))
    }

    /// Album tracks in index order.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("albumIds".to_owned(), id.to_owned()),
            ("includeItemTypes".to_owned(), "Audio".to_owned()),
            ("sortBy".to_owned(), "IndexNumber".to_owned()),
            ("sortOrder".to_owned(), "Ascending".to_owned()),
            ("recursive".to_owned(), "true".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds,MediaStreams".to_owned()),
        ];
        let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
        Ok(items_list(&value, |item| self.track_view(item)))
    }

    /// One page of artists.
    pub async fn artists(
        &self,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        self.require_configured()?;
        let mut params = vec![
            ("limit".to_owned(), browse.limit.to_string()),
            ("startIndex".to_owned(), browse.offset.to_string()),
            (
                "sortBy".to_owned(),
                sort_or(browse.sort_by.clone(), "SortName"),
            ),
            (
                "sortOrder".to_owned(),
                order_word(browse.descending).to_owned(),
            ),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        if !browse.search.is_empty() {
            params.push(("searchTerm".to_owned(), browse.search.clone()));
        }
        let value = self.get("/Artists", &params).await?.unwrap_or(Value::Null);
        Ok(items_page(&value, |item| self.artist_view(item)))
    }

    /// Full alphabetic artist index, bucketed client-side.
    pub async fn artist_index(&self) -> Result<Vec<ArtistIndexEntry>, AdapterError> {
        self.require_configured()?;
        let mut entries: Vec<ArtistView> = Vec::new();
        let mut offset: i64 = 0;
        loop {
            let page = self
                .artists(&ArtistBrowse {
                    limit: 500,
                    offset,
                    ..ArtistBrowse::default()
                })
                .await?;
            if page.items.is_empty() {
                break;
            }
            entries.extend(page.items);
            offset += 500;
            if offset >= page.total {
                break;
            }
        }
        Ok(bucket_index(entries))
    }

    /// One artist by id. None is absence (404).
    pub async fn artist_detail(&self, id: &str) -> Result<Option<ArtistView>, AdapterError> {
        self.require_configured()?;
        let params = vec![("Fields".to_owned(), "ProviderIds".to_owned())];
        let value = self
            .get(&format!("/Items/{id}"), &params)
            .await?
            .unwrap_or(Value::Null);
        if value.is_null() {
            return Ok(None);
        }
        Ok(Some(self.artist_view(&value)))
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        browse: &TrackBrowse,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        self.require_configured()?;
        let mut params = vec![
            ("includeItemTypes".to_owned(), "Audio".to_owned()),
            ("recursive".to_owned(), "true".to_owned()),
            (
                "sortBy".to_owned(),
                sort_or(browse.sort_by.clone(), "SortName"),
            ),
            (
                "sortOrder".to_owned(),
                order_word(browse.descending).to_owned(),
            ),
            ("limit".to_owned(), browse.limit.to_string()),
            ("startIndex".to_owned(), browse.offset.to_string()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        if !browse.search.is_empty() {
            params.push(("searchTerm".to_owned(), browse.search.clone()));
        }
        if !browse.genre.is_empty() {
            params.push(("genres".to_owned(), browse.genre.clone()));
        }
        let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
        Ok(items_page(&value, |item| self.track_view(item)))
    }

    /// Free-text search via `/Search/Hints`, bucketed by item type.
    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResults, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("searchTerm".to_owned(), query.to_owned()),
            (
                "includeItemTypes".to_owned(),
                "MusicAlbum,Audio,MusicArtist".to_owned(),
            ),
            ("limit".to_owned(), limit.to_string()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        let value = self
            .get("/Search/Hints", &params)
            .await?
            .unwrap_or(Value::Null);
        let mut results = SearchResults {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        };
        let hints = value
            .get("SearchHints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for hint in &hints {
            match str_field(hint, "Type").unwrap_or("") {
                "MusicArtist" => results.artists.push(self.artist_view(hint)),
                "MusicAlbum" => results.albums.push(self.album_view(hint)),
                "Audio" => results.tracks.push(self.track_view(hint)),
                _ => {}
            }
        }
        Ok(results)
    }

    /// Recently played albums: played Audio items, album ids deduped in
    /// play order, details fetched per album.
    pub async fn recent(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(Vec::new());
        }
        let params = vec![
            ("includeItemTypes".to_owned(), "Audio".to_owned()),
            ("sortBy".to_owned(), "DatePlayed".to_owned()),
            ("sortOrder".to_owned(), "Descending".to_owned()),
            ("isPlayed".to_owned(), "true".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("limit".to_owned(), limit.to_string()),
            ("recursive".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
        let mut seen = std::collections::HashSet::new();
        let mut album_ids: Vec<String> = Vec::new();
        for item in items_array(&value) {
            let aid = str_field(item, "AlbumId")
                .or_else(|| str_field(item, "ParentId"))
                .unwrap_or("");
            if aid.is_empty() || !seen.insert(aid.to_owned()) {
                continue;
            }
            album_ids.push(aid.to_owned());
            if album_ids.len() as i64 >= limit {
                break;
            }
        }
        let mut albums = Vec::new();
        for aid in album_ids {
            if let Some(album) = self.album_detail(&aid).await? {
                albums.push(album);
            }
        }
        Ok(albums)
    }

    /// Recently added albums via `/Items/Latest`.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(Vec::new());
        }
        let params = vec![
            ("includeItemTypes".to_owned(), "MusicAlbum".to_owned()),
            ("limit".to_owned(), limit.to_string()),
            ("enableUserData".to_owned(), "true".to_owned()),
        ];
        let value = self
            .get("/Items/Latest", &params)
            .await?
            .unwrap_or(Value::Null);
        let raw = value.as_array().cloned().unwrap_or_default();
        Ok(raw.iter().map(|item| self.album_view(item)).collect())
    }

    /// Favorite artists, albums, and tracks.
    pub async fn favorites(&self) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(empty_favorites());
        }
        let artist_params = vec![
            ("isFavorite".to_owned(), "true".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("limit".to_owned(), "50".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        let artists_value = self
            .get("/Artists", &artist_params)
            .await?
            .unwrap_or(Value::Null);
        let album_params = vec![
            ("includeItemTypes".to_owned(), "MusicAlbum".to_owned()),
            ("isFavorite".to_owned(), "true".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("limit".to_owned(), "50".to_owned()),
            ("recursive".to_owned(), "true".to_owned()),
        ];
        let albums_value = self
            .get("/Items", &album_params)
            .await?
            .unwrap_or(Value::Null);
        let track_params = vec![
            ("includeItemTypes".to_owned(), "Audio".to_owned()),
            ("isFavorite".to_owned(), "true".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("limit".to_owned(), "50".to_owned()),
            ("recursive".to_owned(), "true".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        let tracks_value = self
            .get("/Items", &track_params)
            .await?
            .unwrap_or(Value::Null);
        Ok(FavoritesView {
            artists: items_list(&artists_value, |item| self.artist_view(item)),
            albums: items_list(&albums_value, |item| self.album_view(item)),
            tracks: items_list(&tracks_value, |item| self.track_view(item)),
        })
    }

    /// Most-played artists: `PlayCount` sort with the zero-play filter.
    pub async fn most_played_artists(&self, limit: i64) -> Result<Vec<ArtistView>, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(Vec::new());
        }
        let params = vec![
            ("sortBy".to_owned(), "PlayCount".to_owned()),
            ("sortOrder".to_owned(), "Descending".to_owned()),
            ("enableUserData".to_owned(), "true".to_owned()),
            ("limit".to_owned(), limit.to_string()),
        ];
        let value = self.get("/Artists", &params).await?.unwrap_or(Value::Null);
        Ok(items_array(&value)
            .into_iter()
            .filter(|item| play_count(item) > 0)
            .map(|item| self.artist_view(item))
            .collect())
    }

    /// Genre labels via `/MusicGenres`.
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        self.require_configured()?;
        let value = self.get("/MusicGenres", &[]).await?.unwrap_or(Value::Null);
        Ok(items_array(&value)
            .iter()
            .filter_map(|item| str_field(item, "Name").map(str::to_owned))
            .collect())
    }

    /// Tracks carrying one genre label.
    pub async fn genre_songs(
        &self,
        genre: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        let page = self
            .tracks(&TrackBrowse {
                limit,
                offset,
                genre: genre.to_owned(),
                ..TrackBrowse::default()
            })
            .await?;
        Ok(page.items)
    }

    /// Audio playlists.
    pub async fn playlists(&self) -> Result<Vec<PlaylistSummary>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("IncludeItemTypes".to_owned(), "Playlist".to_owned()),
            ("MediaTypes".to_owned(), "Audio".to_owned()),
            ("Recursive".to_owned(), "true".to_owned()),
            ("Limit".to_owned(), "50".to_owned()),
            ("SortBy".to_owned(), "SortName".to_owned()),
            ("SortOrder".to_owned(), "Ascending".to_owned()),
            ("Fields".to_owned(), "ChildCount,DateCreated".to_owned()),
        ];
        let value = self.get("/Items", &params).await?.unwrap_or(Value::Null);
        Ok(items_list(&value, |item| self.playlist_summary(item)))
    }

    /// One playlist with its Audio tracks. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let meta_params = vec![(
            "Fields".to_owned(),
            "ChildCount,DateCreated,ProviderIds".to_owned(),
        )];
        let meta = self
            .get(&format!("/Items/{id}"), &meta_params)
            .await?
            .unwrap_or(Value::Null);
        if meta.is_null() {
            return Ok(None);
        }
        let item_params = vec![
            ("Limit".to_owned(), "1000".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
            ("EnableUserData".to_owned(), "true".to_owned()),
        ];
        let value = self
            .get(&format!("/Playlists/{id}/Items"), &item_params)
            .await?
            .unwrap_or(Value::Null);
        let tracks: Vec<TrackView> = items_array(&value)
            .into_iter()
            .filter(|item| str_field(item, "Type").unwrap_or("") == "Audio")
            .map(|item| self.track_view(item))
            .collect();
        Ok(Some(PlaylistDetail {
            playlist: self.playlist_summary(&meta),
            tracks,
        }))
    }

    /// Jellyfin exposes no artist-info endpoint.
    pub async fn artist_info(&self, _id: &str) -> Result<InfoView, AdapterError> {
        Err(AdapterError::Unsupported(
            "Jellyfin has no artist-info endpoint".to_owned(),
        ))
    }

    /// Jellyfin exposes no album-info endpoint.
    pub async fn album_info(&self, _id: &str) -> Result<InfoView, AdapterError> {
        Err(AdapterError::Unsupported(
            "Jellyfin has no album-info endpoint".to_owned(),
        ))
    }

    /// Lyrics via `/Audio/{id}/Lyrics` (LyricDto). None when the server has
    /// no lyrics for the item.
    pub async fn lyrics(&self, id: &str) -> Result<Option<LyricsView>, AdapterError> {
        self.require_configured()?;
        let value = self
            .get(&format!("/Audio/{id}/Lyrics"), &[])
            .await?
            .unwrap_or(Value::Null);
        let raw = value
            .get("Lyrics")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if raw.is_empty() {
            return Ok(None);
        }
        let mut lines = Vec::new();
        for line in &raw {
            lines.push(LyricLine {
                text: str_field(line, "Text").unwrap_or("").to_owned(),
                start_ms: line.get("Start").and_then(value_to_i64),
            });
        }
        let text = lines
            .iter()
            .map(|line| line.text.clone())
            .collect::<Vec<_>>()
            .join("\n");
        // Ticks are 100ns; lyric starts arrive in ticks, so ms needs /10_000.
        for line in &mut lines {
            if let Some(ticks) = line.start_ms {
                line.start_ms = Some(ticks / 10_000);
            }
        }
        let is_synced = lines.iter().any(|line| line.start_ms.is_some());
        Ok(Some(LyricsView {
            source: SourceName::Jellyfin,
            text,
            is_synced,
            lines,
        }))
    }

    /// Jellyfin exposes no top-songs endpoint; most-played lists cover it.
    pub async fn top_songs(
        &self,
        _artist: &str,
        _limit: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Jellyfin has no top-songs endpoint".to_owned(),
        ))
    }

    /// Random tracks via the Audio listing with `sortBy=Random` (a
    /// documented `ItemSortBy` value) plus the optional genre filter.
    pub async fn random(&self, limit: i64, genre: &str) -> Result<Vec<TrackView>, AdapterError> {
        let page = self
            .tracks(&TrackBrowse {
                limit,
                sort_by: "Random".to_owned(),
                genre: genre.to_owned(),
                ..TrackBrowse::default()
            })
            .await?;
        Ok(page.items)
    }

    /// Similar items via `/Items/{id}/Similar`, audio rows mapped to tracks
    /// and album rows to their tracks is the caller's job; here similar
    /// albums resolve to their first-page tracks is wrong, so only Audio
    /// rows map directly and album rows resolve via album detail tracks.
    pub async fn similar(&self, id: &str, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("Limit".to_owned(), limit.to_string()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
            ("EnableUserData".to_owned(), "true".to_owned()),
        ];
        let value = self
            .get(&format!("/Items/{id}/Similar"), &params)
            .await?
            .unwrap_or(Value::Null);
        let mut tracks = Vec::new();
        for item in items_array(&value) {
            match str_field(item, "Type").unwrap_or("") {
                "Audio" => tracks.push(self.track_view(item)),
                "MusicAlbum" => {
                    if let Some(album_id) = str_field(item, "Id") {
                        let mut album_tracks = self.album_tracks(album_id).await?;
                        tracks.append(&mut album_tracks);
                    }
                }
                _ => {}
            }
            if tracks.len() as i64 >= limit {
                break;
            }
        }
        tracks.truncate(limit.max(0) as usize);
        Ok(tracks)
    }

    /// Instant mix for an item, artist, or genre. Preserved from v2 (three
    /// routes); genre names escape `/` as `%2F` before hitting the path.
    pub async fn mix(&self, seed: &MixSeed, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("Limit".to_owned(), limit.to_string()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
            ("EnableUserData".to_owned(), "true".to_owned()),
        ];
        let endpoint = match seed {
            MixSeed::Item(id) => format!("/Items/{id}/InstantMix"),
            MixSeed::Artist(id) => format!("/Artists/{id}/InstantMix"),
            MixSeed::Genre(name) => {
                format!("/MusicGenres/{}/InstantMix", name.replace('/', "%2F"))
            }
        };
        let value = self.get(&endpoint, &params).await?.unwrap_or(Value::Null);
        Ok(items_list(&value, |item| self.track_view(item)))
    }

    /// Audio-only sessions with an active NowPlayingItem.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let value = self.get("/Sessions", &[]).await?.unwrap_or(Value::Null);
        let raw = value.as_array().cloned().unwrap_or_default();
        let mut sessions = Vec::new();
        for entry in &raw {
            let Some(now_playing) = entry.get("NowPlayingItem") else {
                continue;
            };
            if str_field(now_playing, "Type").unwrap_or("") != "Audio" {
                continue;
            }
            let artists = now_playing
                .get("Artists")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut artist_names: Vec<String> = artists
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if artist_names.is_empty() {
                artist_names.push(
                    str_field(now_playing, "AlbumArtist")
                        .unwrap_or("")
                        .to_owned(),
                );
            }
            let play_state = entry.get("PlayState").cloned().unwrap_or(Value::Null);
            sessions.push(SessionView {
                source: SourceName::Jellyfin,
                session_id: str_field(entry, "Id").unwrap_or("").to_owned(),
                user_name: str_field(entry, "UserName").unwrap_or("").to_owned(),
                device_name: str_field(entry, "DeviceName").unwrap_or("").to_owned(),
                track_title: str_field(now_playing, "Name").unwrap_or("").to_owned(),
                artist_name: artist_names.join(", "),
                album_name: str_field(now_playing, "Album").unwrap_or("").to_owned(),
                progress_ms: play_state
                    .get("PositionTicks")
                    .and_then(value_to_i64)
                    .unwrap_or(0)
                    / 10_000,
                duration_ms: now_playing
                    .get("RunTimeTicks")
                    .and_then(value_to_i64)
                    .unwrap_or(0)
                    / 10_000,
                is_paused: play_state
                    .get("IsPaused")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            });
        }
        Ok(SessionsView {
            source: SourceName::Jellyfin,
            sessions,
        })
    }

    /// Jellyfin exposes no listening-history endpoint; `recent` covers recency.
    pub async fn history(&self, _limit: i64, _offset: i64) -> Result<HistoryPage, AdapterError> {
        Err(AdapterError::Unsupported(
            "Jellyfin has no listening-history endpoint".to_owned(),
        ))
    }

    /// Primary image bytes for any item.
    pub async fn image_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("maxWidth".to_owned(), size.to_string()),
            ("maxHeight".to_owned(), size.to_string()),
            ("quality".to_owned(), "90".to_owned()),
        ];
        self.get_bytes(
            &format!("/Items/{id}/Images/Primary"),
            &params,
            "image/jpeg",
        )
        .await
    }

    /// Direct audio bytes for one item (`/Audio/{id}/stream?static=true`,
    /// v2 `get_playback_url` direct landing without the playback-info round
    /// trip; server-side transcode stays a later-stage concern).
    pub async fn audio_bytes(&self, id: &str) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let mut params = vec![("static".to_owned(), "true".to_owned())];
        if !self.user_id.is_empty() {
            params.push(("userId".to_owned(), self.user_id.clone()));
        }
        self.get_bytes(&format!("/Audio/{id}/stream"), &params, "audio/mpeg")
            .await
    }

    /// Session report to `/Sessions/Playing`, `/Sessions/Playing/Progress`,
    /// or `/Sessions/Playing/Stopped` (v2 `report_playback_*`; the play
    /// session id is empty because stage 6 never opens a playback session).
    pub async fn report_session(
        &self,
        endpoint: &str,
        item_id: &str,
        position_ticks: Option<i64>,
        is_paused: bool,
    ) -> Result<(), AdapterError> {
        self.require_configured()?;
        let mut body = serde_json::json!({
            "ItemId": item_id,
            "PlaySessionId": "",
            "CanSeek": true,
        });
        if let Some(ticks) = position_ticks
            && let Some(map) = body.as_object_mut()
        {
            map.insert("PositionTicks".to_owned(), serde_json::Value::from(ticks));
        }
        if endpoint.ends_with("/Progress")
            && let Some(map) = body.as_object_mut()
        {
            map.insert("IsPaused".to_owned(), serde_json::Value::from(is_paused));
        }
        self.post_json(endpoint, &body).await
    }

    /// Playlist cover: the first Audio member's primary image, matching the
    /// v2 playlist-image route which proxies a member item's image.
    pub async fn playlist_cover_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        let detail = self
            .playlist_detail(id)
            .await?
            .ok_or(AdapterError::NotFound)?;
        let first = detail.tracks.first().ok_or(AdapterError::NotFound)?;
        self.image_bytes(&first.id, size).await
    }

    /// Resolve an MBID via the cheap `/Search/Hints` fallback first
    /// (comparing `MusicBrainzReleaseGroup`/`MusicBrainzAlbum`), then the
    /// full paged index scan when search misses.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("searchTerm".to_owned(), mbid.to_owned()),
            ("includeItemTypes".to_owned(), "MusicAlbum".to_owned()),
            ("limit".to_owned(), "50".to_owned()),
            ("Fields".to_owned(), "ProviderIds".to_owned()),
        ];
        let value = self
            .get("/Search/Hints", &params)
            .await?
            .unwrap_or(Value::Null);
        let hints = value
            .get("SearchHints")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for hint in &hints {
            let providers = hint.get("ProviderIds").cloned().unwrap_or(Value::Null);
            let release_group = str_field(&providers, "MusicBrainzReleaseGroup").unwrap_or("");
            let release = str_field(&providers, "MusicBrainzAlbum").unwrap_or("");
            if release_group == mbid || release == mbid {
                let id = str_field(hint, "Id")
                    .or_else(|| str_field(hint, "ItemId"))
                    .unwrap_or("")
                    .to_owned();
                let tracks = self.album_tracks(&id).await?;
                return Ok(MatchView {
                    source: SourceName::Jellyfin,
                    found: true,
                    remote_album_id: Some(id),
                    tracks,
                });
            }
        }
        let index = self.mbid_index().await?;
        if let Some(id) = index.get(mbid) {
            let tracks = self.album_tracks(id).await?;
            return Ok(MatchView {
                source: SourceName::Jellyfin,
                found: true,
                remote_album_id: Some(id.clone()),
                tracks,
            });
        }
        Ok(MatchView {
            source: SourceName::Jellyfin,
            found: false,
            remote_album_id: None,
            tracks: Vec::new(),
        })
    }

    fn require_configured(&self) -> Result<(), AdapterError> {
        if self.is_configured() {
            Ok(())
        } else {
            Err(AdapterError::NotConfigured)
        }
    }

    fn paged_params(&self, limit: i64, offset: i64) -> Vec<(String, String)> {
        let mut params = vec![
            ("limit".to_owned(), limit.to_string()),
            ("startIndex".to_owned(), offset.to_string()),
        ];
        if !self.user_id.is_empty() {
            params.push(("userId".to_owned(), self.user_id.clone()));
        }
        params
    }

    /// GET a JSON endpoint. 404 maps to `None`; 401/403 map to auth failure.
    async fn get(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<Option<Value>, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query: Vec<(String, String)> = params.to_vec();
        if !self.user_id.is_empty() && !query.iter().any(|(key, _)| key == "userId") {
            query.push(("userId".to_owned(), self.user_id.clone()));
        }
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .header("Accept", "application/json")
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_key),
            )
            .query(&query)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(AdapterError::Auth);
        }
        if response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !response.status().is_success() {
            return Err(AdapterError::Api(format!(
                "GET {endpoint} failed ({})",
                response.status()
            )));
        }
        if response.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if bytes.is_empty() {
            return Ok(None);
        }
        serde_json::from_slice::<Value>(&bytes)
            .map(Some)
            .map_err(|_| AdapterError::Api("Jellyfin returned an invalid response".to_owned()))
    }

    /// POST a JSON body, discarding the response. Used only for session
    /// reports; any non-2xx is an API error.
    async fn post_json(
        &self,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> Result<(), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let payload = serde_json::to_vec(body)
            .map_err(|_| AdapterError::Api("Jellyfin report body failed to render".to_owned()))?;
        let response = self
            .client
            .post(format!("{}{endpoint}", self.base_url))
            .header("Content-Type", "application/json")
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_key),
            )
            .body(payload)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(AdapterError::Auth);
        }
        if !response.status().is_success() {
            return Err(AdapterError::Api(format!(
                "POST {endpoint} failed ({})",
                response.status()
            )));
        }
        Ok(())
    }

    /// GET raw bytes (images, audio). Any non-200 is an API error; a
    /// missing content type falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        params: &[(String, String)],
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .header(
                "Authorization",
                format!("MediaBrowser Token=\"{}\"", self.api_key),
            )
            .query(params)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if response.status() == reqwest::StatusCode::UNAUTHORIZED
            || response.status() == reqwest::StatusCode::FORBIDDEN
        {
            return Err(AdapterError::Auth);
        }
        if !response.status().is_success() {
            return Err(AdapterError::Api(format!(
                "GET {endpoint} failed ({})",
                response.status()
            )));
        }
        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or(fallback_content_type)
            .to_owned();
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        Ok((bytes.to_vec(), content_type))
    }

    fn album_view(&self, item: &Value) -> AlbumView {
        let providers = item.get("ProviderIds").cloned().unwrap_or(Value::Null);
        let id = item_id(item);
        AlbumView {
            source: SourceName::Jellyfin,
            image_url: image_url_for(SourceName::Jellyfin, &id, image_tag(item)),
            id,
            title: str_field(item, "Name").unwrap_or("Unknown").to_owned(),
            artist_name: item_artist_name(item).unwrap_or_default(),
            artist_id: item_artist_id(item).map(str::to_owned),
            year: item
                .get("ProductionYear")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            genre: None,
            track_count: item
                .get("ChildCount")
                .and_then(value_to_i64)
                .map(|count| count as i32),
            release_mbid: str_field(&providers, "MusicBrainzAlbum").map(str::to_owned),
            release_group_mbid: str_field(&providers, "MusicBrainzReleaseGroup").map(str::to_owned),
            artist_mbid: str_field(&providers, "MusicBrainzArtist").map(str::to_owned),
        }
    }

    fn artist_view(&self, item: &Value) -> ArtistView {
        let providers = item.get("ProviderIds").cloned().unwrap_or(Value::Null);
        let id = item_id(item);
        ArtistView {
            source: SourceName::Jellyfin,
            image_url: image_url_for(SourceName::Jellyfin, &id, image_tag(item)),
            id,
            name: str_field(item, "Name").unwrap_or("Unknown").to_owned(),
            album_count: item
                .get("AlbumCount")
                .and_then(value_to_i64)
                .map(|count| count as i32),
            artist_mbid: str_field(&providers, "MusicBrainzArtist").map(str::to_owned),
        }
    }

    fn track_view(&self, item: &Value) -> TrackView {
        let providers = item.get("ProviderIds").cloned().unwrap_or(Value::Null);
        let id = item_id(item);
        TrackView {
            source: SourceName::Jellyfin,
            image_url: image_url_for(SourceName::Jellyfin, &id, image_tag(item)),
            id,
            title: str_field(item, "Name").unwrap_or("Unknown").to_owned(),
            album_name: str_field(item, "Album").unwrap_or("").to_owned(),
            album_id: str_field(item, "AlbumId").map(str::to_owned),
            artist_name: item_artist_name(item).unwrap_or_default(),
            artist_id: item_artist_id(item).map(str::to_owned),
            track_number: item
                .get("IndexNumber")
                .and_then(value_to_i64)
                .map(|number| number as i32),
            disc_number: item
                .get("ParentIndexNumber")
                .and_then(value_to_i64)
                .map(|number| number as i32),
            duration_secs: item
                .get("RunTimeTicks")
                .and_then(value_to_i64)
                .map(|ticks| ticks / 10_000_000),
            year: item
                .get("ProductionYear")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            recording_mbid: str_field(&providers, "MusicBrainzTrack").map(str::to_owned),
            part_key: None,
        }
    }

    fn playlist_summary(&self, item: &Value) -> PlaylistSummary {
        let id = item_id(item);
        PlaylistSummary {
            source: SourceName::Jellyfin,
            image_url: Some(format!("/api/v3/remotes/jellyfin/covers/playlists/{id}")),
            id,
            name: str_field(item, "Name").unwrap_or("").to_owned(),
            track_count: item.get("ChildCount").and_then(value_to_i64).unwrap_or(0),
            duration_secs: item
                .get("RunTimeTicks")
                .and_then(value_to_i64)
                .map(|ticks| ticks / 10_000_000)
                .unwrap_or(0),
        }
    }
}

/// What seeds a Jellyfin instant mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MixSeed {
    /// An item id (`/Items/{id}/InstantMix`).
    Item(String),
    /// An artist id (`/Artists/{id}/InstantMix`).
    Artist(String),
    /// A genre name (`/MusicGenres/{genre}/InstantMix`).
    Genre(String),
}

fn order_word(descending: bool) -> &'static str {
    if descending {
        "Descending"
    } else {
        "Ascending"
    }
}

fn sort_or(value: String, fallback: &str) -> String {
    if value.is_empty() {
        fallback.to_owned()
    } else {
        value
    }
}

fn item_id(item: &Value) -> String {
    str_field(item, "Id")
        .or_else(|| str_field(item, "ItemId"))
        .unwrap_or("")
        .to_owned()
}

fn image_tag(item: &Value) -> Option<&str> {
    item.get("ImageTags")
        .and_then(|tags| tags.get("Primary"))
        .and_then(Value::as_str)
}

fn image_url_for(source: SourceName, id: &str, tag: Option<&str>) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    let mut url = format!("/api/v3/remotes/{}/images/{id}", source.as_str());
    if let Some(tag) = tag {
        url.push_str("?tag=");
        url.push_str(tag);
    }
    Some(url)
}

fn item_artist_name(item: &Value) -> Option<String> {
    if let Some(artists) = item.get("ArtistItems").and_then(Value::as_array)
        && let Some(first) = artists.first()
        && let Some(name) = str_field(first, "Name")
    {
        return Some(name.to_owned());
    }
    str_field(item, "AlbumArtist").map(str::to_owned)
}

fn item_artist_id(item: &Value) -> Option<&str> {
    item.get("ArtistItems")
        .and_then(Value::as_array)
        .and_then(|artists| artists.first())
        .and_then(|first| str_field(first, "Id"))
}

fn play_count(item: &Value) -> i64 {
    item.get("UserData")
        .and_then(|data| data.get("PlayCount"))
        .and_then(value_to_i64)
        .unwrap_or(0)
}

fn items_array(value: &Value) -> Vec<&Value> {
    value
        .get("Items")
        .and_then(Value::as_array)
        .map(|items| items.iter().collect())
        .unwrap_or_default()
}

fn items_list<T>(value: &Value, map: impl Fn(&Value) -> T) -> Vec<T> {
    items_array(value).into_iter().map(map).collect()
}

fn items_page<T>(value: &Value, map: impl Fn(&Value) -> T) -> RemotePage<T> {
    let items = items_list(value, map);
    let total = value
        .get("TotalRecordCount")
        .and_then(value_to_i64)
        .unwrap_or(items.len() as i64);
    RemotePage { items, total }
}

fn empty_favorites() -> FavoritesView {
    FavoritesView {
        artists: Vec::new(),
        albums: Vec::new(),
        tracks: Vec::new(),
    }
}

fn bucket_index(mut artists: Vec<ArtistView>) -> Vec<ArtistIndexEntry> {
    artists.sort_by(|left, right| left.name.cmp(&right.name));
    let mut buckets: Vec<ArtistIndexEntry> = Vec::new();
    for artist in artists {
        let key = artist
            .name
            .chars()
            .next()
            .map(|letter| letter.to_uppercase().to_string())
            .unwrap_or_else(|| "#".to_owned());
        match buckets.last_mut() {
            Some(bucket) if bucket.name == key => bucket.artists.push(artist),
            _ => buckets.push(ArtistIndexEntry {
                name: key,
                artists: vec![artist],
            }),
        }
    }
    buckets
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn value_to_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    value.as_u64().and_then(|number| i64::try_from(number).ok())
}

fn trim_cause(cause: &reqwest::Error) -> String {
    let text = cause.to_string();
    if text.len() > 200 {
        text[..200].to_owned()
    } else {
        text
    }
}
