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
//!
//! Payloads decode into the typed shapes in [`super::jellyfin_models`]; a
//! payload missing an item id is an upstream error, not an empty item.

use std::collections::HashMap;
use std::time::Duration;

use serde::de::DeserializeOwned;

use super::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemotePage, TrackBrowse};
use super::jellyfin_models::{
    AuthenticationResult, Item, ItemPage, Lyrics, NamedPage, QueryFilters, SearchHints, Session,
    SystemInfo,
};
use super::models::{
    AlbumView, ArtistIndexEntry, ArtistView, FavoritesView, FilterFacetsView, HistoryPage, HubView,
    InfoView, LyricLine, LyricsView, MatchView, PlaylistDetail, PlaylistSummary, SearchResults,
    SessionView, SessionsView, SourceName, StatsView, TrackView,
};

/// Request timeout per upstream call, matching the v2 repository.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// MBID index scan batch size, matching the v2 repository.
const MBID_BATCH: i64 = 500;

/// Jellyfin ticks per millisecond (ticks are 100ns).
const TICKS_PER_MS: i64 = 10_000;

/// Jellyfin browse client. Built per request from the caller's resolved
/// connection; holds no cache.
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

/// A Jellyfin user session from `AuthenticateByName`. The `Debug` impl
/// redacts the token.
pub struct JellyfinSession {
    /// Jellyfin-side user id.
    pub user_id: String,
    /// Jellyfin display name.
    pub user_name: String,
    /// User-scoped access token.
    pub access_token: String,
}

impl std::fmt::Debug for JellyfinSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JellyfinSession")
            .field("user_id", &self.user_id)
            .field("user_name", &self.user_name)
            .finish_non_exhaustive()
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

/// Query pairs, owned.
type Params = Vec<(String, String)>;

fn params(pairs: &[(&str, &str)]) -> Params {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
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
        let info: Option<SystemInfo> = self.get("/System/Info", &[]).await?;
        let info =
            info.ok_or_else(|| AdapterError::Api("Jellyfin has no system info".to_owned()))?;
        Ok(format!(
            "Connected to {} (v{})",
            info.server_name.as_deref().unwrap_or("Unknown"),
            info.version.as_deref().unwrap_or("Unknown")
        ))
    }

    /// Full MBID-to-item index over the album catalog, 500 rows per page.
    /// `match_album` prefers the cheap search fallback and only scans when
    /// search misses.
    pub async fn mbid_index(&self) -> Result<HashMap<String, String>, AdapterError> {
        let mut index = HashMap::new();
        let mut offset: i64 = 0;
        loop {
            let mut query = params(&[
                ("includeItemTypes", "MusicAlbum"),
                ("recursive", "true"),
                ("Fields", "ProviderIds"),
            ]);
            query.extend(self.paged_params(MBID_BATCH, offset));
            let page = self.items("/Items", &query).await?;
            if page.items.is_empty() {
                break;
            }
            for item in &page.items {
                for key in ["MusicBrainzReleaseGroup", "MusicBrainzAlbum"] {
                    if let Some(mbid) = item.provider(key) {
                        index.insert(mbid, item.id.clone());
                    }
                }
            }
            offset += MBID_BATCH;
            if offset >= page.total_record_count.unwrap_or(0) {
                break;
            }
        }
        Ok(index)
    }

    /// Hub highlights. Sections fail open to empty: Jellyfin sections are
    /// cheap and independent, so partial failure is normal.
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
            self.favorites(50),
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
        let mut totals = [0_i64; 3];
        for (item_type, slot) in ["MusicAlbum", "MusicArtist", "Audio"]
            .into_iter()
            .zip(totals.iter_mut())
        {
            let query = params(&[
                ("includeItemTypes", item_type),
                ("recursive", "true"),
                ("limit", "0"),
            ]);
            *slot = self
                .items("/Items", &query)
                .await?
                .total_record_count
                .unwrap_or(0);
        }
        Ok(StatsView {
            total_albums: totals[0],
            total_artists: totals[1],
            total_tracks: totals[2],
        })
    }

    /// One page of albums with genre and year filters.
    pub async fn albums(
        &self,
        browse: &AlbumBrowse,
    ) -> Result<RemotePage<AlbumView>, AdapterError> {
        self.require_configured()?;
        let mut query = params(&[
            ("includeItemTypes", "MusicAlbum"),
            ("recursive", "true"),
            ("sortBy", sort_or(&browse.sort_by, "SortName")),
            ("sortOrder", order_word(browse.descending)),
            ("enableUserData", "true"),
            ("Fields", "ProviderIds,ChildCount"),
        ]);
        query.push(("limit".to_owned(), browse.limit.to_string()));
        query.push(("startIndex".to_owned(), browse.offset.to_string()));
        if !browse.genre.is_empty() {
            query.push(("genres".to_owned(), browse.genre.clone()));
        }
        if let Some(year) = browse.year {
            query.push(("years".to_owned(), year.to_string()));
        }
        let page = self.items("/Items", &query).await?;
        Ok(page_of(page, album_view))
    }

    /// One album by id. None is absence (404).
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        let item: Option<Item> = self
            .get(
                &format!("/Items/{id}"),
                &params(&[("Fields", "ProviderIds,ChildCount")]),
            )
            .await?;
        Ok(item.as_ref().map(album_view))
    }

    /// Album tracks in index order.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let query = params(&[
            ("albumIds", id),
            ("includeItemTypes", "Audio"),
            ("sortBy", "IndexNumber"),
            ("sortOrder", "Ascending"),
            ("recursive", "true"),
            ("enableUserData", "true"),
            ("Fields", "ProviderIds,MediaStreams"),
        ]);
        let page = self.items("/Items", &query).await?;
        Ok(page.items.iter().map(track_view).collect())
    }

    /// One page of artists.
    pub async fn artists(
        &self,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        self.require_configured()?;
        let mut query = params(&[
            ("sortBy", sort_or(&browse.sort_by, "SortName")),
            ("sortOrder", order_word(browse.descending)),
            ("enableUserData", "true"),
            ("Fields", "ProviderIds"),
        ]);
        query.push(("limit".to_owned(), browse.limit.to_string()));
        query.push(("startIndex".to_owned(), browse.offset.to_string()));
        if !browse.search.is_empty() {
            query.push(("searchTerm".to_owned(), browse.search.clone()));
        }
        let page = self.items("/Artists", &query).await?;
        Ok(page_of(page, artist_view))
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
        let item: Option<Item> = self
            .get(
                &format!("/Items/{id}"),
                &params(&[("Fields", "ProviderIds")]),
            )
            .await?;
        Ok(item.as_ref().map(artist_view))
    }

    /// One page of tracks.
    pub async fn tracks(
        &self,
        browse: &TrackBrowse,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        self.require_configured()?;
        let mut query = params(&[
            ("includeItemTypes", "Audio"),
            ("recursive", "true"),
            ("sortBy", sort_or(&browse.sort_by, "SortName")),
            ("sortOrder", order_word(browse.descending)),
            ("enableUserData", "true"),
            ("Fields", "ProviderIds"),
        ]);
        query.push(("limit".to_owned(), browse.limit.to_string()));
        query.push(("startIndex".to_owned(), browse.offset.to_string()));
        if !browse.search.is_empty() {
            query.push(("searchTerm".to_owned(), browse.search.clone()));
        }
        if !browse.genre.is_empty() {
            query.push(("genres".to_owned(), browse.genre.clone()));
        }
        let page = self.items("/Items", &query).await?;
        Ok(page_of(page, track_view))
    }

    /// Free-text search via `/Search/Hints`, bucketed by item type.
    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResults, AdapterError> {
        self.require_configured()?;
        let mut pairs = params(&[
            ("searchTerm", query),
            ("includeItemTypes", "MusicAlbum,Audio,MusicArtist"),
            ("Fields", "ProviderIds"),
        ]);
        pairs.push(("limit".to_owned(), limit.to_string()));
        let hints = self.hints(&pairs).await?;
        let mut results = SearchResults {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        };
        for hint in &hints {
            match hint.kind.as_deref().unwrap_or("") {
                "MusicArtist" => results.artists.push(artist_view(hint)),
                "MusicAlbum" => results.albums.push(album_view(hint)),
                "Audio" => results.tracks.push(track_view(hint)),
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
        let mut query = params(&[
            ("includeItemTypes", "Audio"),
            ("sortBy", "DatePlayed"),
            ("sortOrder", "Descending"),
            ("isPlayed", "true"),
            ("enableUserData", "true"),
            ("recursive", "true"),
            ("Fields", "ProviderIds"),
        ]);
        query.push(("limit".to_owned(), limit.to_string()));
        let page = self.items("/Items", &query).await?;
        let mut seen = std::collections::HashSet::new();
        let mut album_ids: Vec<String> = Vec::new();
        for item in &page.items {
            let album_id = item
                .album_id
                .clone()
                .or_else(|| item.parent_id.clone())
                .unwrap_or_default();
            if album_id.is_empty() || !seen.insert(album_id.clone()) {
                continue;
            }
            album_ids.push(album_id);
            if album_ids.len() as i64 >= limit {
                break;
            }
        }
        let mut albums = Vec::new();
        for album_id in album_ids {
            if let Some(album) = self.album_detail(&album_id).await? {
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
        let mut query = params(&[
            ("includeItemTypes", "MusicAlbum"),
            ("enableUserData", "true"),
        ]);
        query.push(("limit".to_owned(), limit.to_string()));
        let items: Option<Vec<Item>> = self.get("/Items/Latest", &query).await?;
        Ok(items.unwrap_or_default().iter().map(album_view).collect())
    }

    /// Favorite artists, albums, and tracks, up to `limit` of each.
    pub async fn favorites(&self, limit: i64) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(FavoritesView {
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
            });
        }
        let limit = limit.to_string();
        let mut artist_query = params(&[
            ("isFavorite", "true"),
            ("enableUserData", "true"),
            ("Fields", "ProviderIds"),
        ]);
        artist_query.push(("limit".to_owned(), limit.clone()));
        let mut album_query = params(&[
            ("includeItemTypes", "MusicAlbum"),
            ("isFavorite", "true"),
            ("enableUserData", "true"),
            ("recursive", "true"),
        ]);
        album_query.push(("limit".to_owned(), limit.clone()));
        let mut track_query = params(&[
            ("includeItemTypes", "Audio"),
            ("isFavorite", "true"),
            ("enableUserData", "true"),
            ("recursive", "true"),
            ("Fields", "ProviderIds"),
        ]);
        track_query.push(("limit".to_owned(), limit));
        let artists = self.items("/Artists", &artist_query).await?;
        let albums = self.items("/Items", &album_query).await?;
        let tracks = self.items("/Items", &track_query).await?;
        Ok(FavoritesView {
            artists: artists.items.iter().map(artist_view).collect(),
            albums: albums.items.iter().map(album_view).collect(),
            tracks: tracks.items.iter().map(track_view).collect(),
        })
    }

    /// Most-played artists: `PlayCount` sort with the zero-play filter.
    pub async fn most_played_artists(&self, limit: i64) -> Result<Vec<ArtistView>, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = params(&[
            ("sortBy", "PlayCount"),
            ("sortOrder", "Descending"),
            ("enableUserData", "true"),
        ]);
        query.push(("limit".to_owned(), limit.to_string()));
        let page = self.items("/Artists", &query).await?;
        Ok(page
            .items
            .iter()
            .filter(|item| item.play_count() > 0)
            .map(artist_view)
            .collect())
    }

    /// Most-played albums: `PlayCount` sort with the zero-play filter
    /// (v2 `get_most_played_albums`).
    pub async fn most_played_albums(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.user_id.is_empty() {
            return Ok(Vec::new());
        }
        let mut query = params(&[
            ("includeItemTypes", "MusicAlbum"),
            ("sortBy", "PlayCount"),
            ("sortOrder", "Descending"),
            ("enableUserData", "true"),
            ("recursive", "true"),
        ]);
        query.push(("limit".to_owned(), limit.to_string()));
        let page = self.items("/Items", &query).await?;
        Ok(page
            .items
            .iter()
            .filter(|item| item.play_count() > 0)
            .map(album_view)
            .collect())
    }

    /// Album filter facets via `/Items/Filters` (v2 `get_filter_facets`):
    /// years newest first, tags and studios sorted, empty studios dropped.
    pub async fn filter_facets(&self) -> Result<FilterFacetsView, AdapterError> {
        self.require_configured()?;
        let filters: Option<QueryFilters> = self
            .get(
                "/Items/Filters",
                &params(&[("includeItemTypes", "MusicAlbum")]),
            )
            .await?;
        let filters = filters.unwrap_or_default();
        let mut years: Vec<i32> = filters
            .years
            .into_iter()
            .filter_map(|year| i32::try_from(year).ok())
            .collect();
        years.sort_unstable_by(|left, right| right.cmp(left));
        let mut tags: Vec<String> = filters
            .tags
            .into_iter()
            .filter(|tag| !tag.is_empty())
            .collect();
        tags.sort();
        let mut studios: Vec<String> = filters
            .studios
            .into_iter()
            .filter(|studio| !studio.is_empty())
            .collect();
        studios.sort();
        Ok(FilterFacetsView {
            years,
            tags,
            studios,
        })
    }

    /// Trade a Jellyfin username and password for a user session via
    /// `POST /Users/AuthenticateByName` (v2 `_authenticate_with_jellyfin`).
    /// 401 and 403 read as a rejected login; the password is not kept.
    pub async fn authenticate_by_name(
        client: &reqwest::Client,
        base_url: &str,
        device_id: &str,
        username: &str,
        password: &str,
    ) -> Result<JellyfinSession, AdapterError> {
        let base_url = base_url.trim_end_matches('/');
        if base_url.is_empty() {
            return Err(AdapterError::NotConfigured);
        }
        let header = format!(
            "MediaBrowser Client=\"DroppedNeedle\", Device=\"DroppedNeedle\", DeviceId=\"{device_id}\", Version=\"1.4.0\""
        );
        let response = client
            .post(format!("{base_url}/Users/AuthenticateByName"))
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .header("Authorization", header)
            .body(serde_json::json!({ "Username": username, "Pw": password }).to_string())
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
                "POST /Users/AuthenticateByName failed ({})",
                response.status()
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let result: AuthenticationResult = decode("/Users/AuthenticateByName", &bytes)?;
        if result.user.id.is_empty() || result.access_token.is_empty() {
            return Err(AdapterError::Api(
                "Jellyfin returned incomplete auth data".to_owned(),
            ));
        }
        Ok(JellyfinSession {
            user_name: result.user.name.unwrap_or_else(|| username.to_owned()),
            user_id: result.user.id,
            access_token: result.access_token,
        })
    }

    /// Genre labels via `/MusicGenres`.
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        self.require_configured()?;
        let page: Option<NamedPage> = self.get("/MusicGenres", &[]).await?;
        Ok(page
            .map(|page| page.items)
            .unwrap_or_default()
            .into_iter()
            .map(|genre| genre.name)
            .filter(|name| !name.is_empty())
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
        let query = params(&[
            ("IncludeItemTypes", "Playlist"),
            ("MediaTypes", "Audio"),
            ("Recursive", "true"),
            ("Limit", "50"),
            ("SortBy", "SortName"),
            ("SortOrder", "Ascending"),
            ("Fields", "ChildCount,DateCreated"),
        ]);
        let page = self.items("/Items", &query).await?;
        Ok(page.items.iter().map(playlist_summary).collect())
    }

    /// One playlist with its Audio tracks. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let meta: Option<Item> = self
            .get(
                &format!("/Items/{id}"),
                &params(&[("Fields", "ChildCount,DateCreated,ProviderIds")]),
            )
            .await?;
        let Some(meta) = meta else {
            return Ok(None);
        };
        let query = params(&[
            ("Limit", "1000"),
            ("Fields", "ProviderIds"),
            ("EnableUserData", "true"),
        ]);
        let page = self
            .items(&format!("/Playlists/{id}/Items"), &query)
            .await?;
        let tracks = page
            .items
            .iter()
            .filter(|item| item.kind.as_deref() == Some("Audio"))
            .map(track_view)
            .collect();
        Ok(Some(PlaylistDetail {
            playlist: playlist_summary(&meta),
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
        let lyrics: Option<Lyrics> = self.get(&format!("/Audio/{id}/Lyrics"), &[]).await?;
        let raw = lyrics.map(|lyrics| lyrics.lyrics).unwrap_or_default();
        if raw.is_empty() {
            return Ok(None);
        }
        let lines: Vec<LyricLine> = raw
            .into_iter()
            .map(|line| LyricLine {
                text: line.text.unwrap_or_default(),
                start_ms: line.start.map(|ticks| ticks / TICKS_PER_MS),
            })
            .collect();
        let text = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
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

    /// Similar items via `/Items/{id}/Similar`: Audio rows map directly,
    /// album rows contribute their tracks, up to `limit` tracks.
    pub async fn similar(&self, id: &str, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let mut query = params(&[("Fields", "ProviderIds"), ("EnableUserData", "true")]);
        query.push(("Limit".to_owned(), limit.to_string()));
        let page = self.items(&format!("/Items/{id}/Similar"), &query).await?;
        let mut tracks = Vec::new();
        for item in &page.items {
            match item.kind.as_deref().unwrap_or("") {
                "Audio" => tracks.push(track_view(item)),
                "MusicAlbum" => tracks.append(&mut self.album_tracks(&item.id).await?),
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
        let mut query = params(&[("Fields", "ProviderIds"), ("EnableUserData", "true")]);
        query.push(("Limit".to_owned(), limit.to_string()));
        let endpoint = match seed {
            MixSeed::Item(id) => format!("/Items/{id}/InstantMix"),
            MixSeed::Artist(id) => format!("/Artists/{id}/InstantMix"),
            MixSeed::Genre(name) => {
                format!("/MusicGenres/{}/InstantMix", name.replace('/', "%2F"))
            }
        };
        let page = self.items(&endpoint, &query).await?;
        Ok(page.items.iter().map(track_view).collect())
    }

    /// Audio-only sessions with an active `NowPlayingItem`.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let raw: Option<Vec<Session>> = self.get("/Sessions", &[]).await?;
        let sessions = raw
            .unwrap_or_default()
            .into_iter()
            .filter_map(|session| {
                let item = session.now_playing_item.as_ref()?;
                if item.kind.as_deref() != Some("Audio") {
                    return None;
                }
                let artist_name = if item.artists.is_empty() {
                    item.album_artist.clone().unwrap_or_default()
                } else {
                    item.artists.join(", ")
                };
                let art = item
                    .album_id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .unwrap_or(&item.id);
                let state = session.play_state.as_ref();
                Some(SessionView {
                    source: SourceName::Jellyfin,
                    session_id: session.id.clone(),
                    user_name: session.user_name.clone().unwrap_or_default(),
                    device_name: session
                        .device_name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .or_else(|| session.client.clone())
                        .unwrap_or_default(),
                    track_title: item.name.clone().unwrap_or_default(),
                    artist_name,
                    album_name: item.album.clone().unwrap_or_default(),
                    progress_ms: state.and_then(|state| state.position_ticks).unwrap_or(0)
                        / TICKS_PER_MS,
                    duration_ms: item.run_time_ticks.unwrap_or(0) / TICKS_PER_MS,
                    is_paused: state.and_then(|state| state.is_paused).unwrap_or(false),
                    image_url: image_url_for(art, None),
                })
            })
            .collect();
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
        let size = size.to_string();
        let query = params(&[("maxWidth", &size), ("maxHeight", &size), ("quality", "90")]);
        self.get_bytes(&format!("/Items/{id}/Images/Primary"), &query, "image/jpeg")
            .await
    }

    /// Direct audio bytes for one item (`/Audio/{id}/stream?static=true`,
    /// v2 `get_playback_url` direct landing without the playback-info round
    /// trip; server-side transcode is not supported here).
    pub async fn audio_bytes(&self, id: &str) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let mut query = params(&[("static", "true")]);
        if !self.user_id.is_empty() {
            query.push(("userId".to_owned(), self.user_id.clone()));
        }
        self.get_bytes(&format!("/Audio/{id}/stream"), &query, "audio/mpeg")
            .await
    }

    /// Session report to `/Sessions/Playing`, `/Sessions/Playing/Progress`,
    /// or `/Sessions/Playing/Stopped` (v2 `report_playback_*`; the play
    /// session id is empty because v3 never opens a playback session).
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
        let query = params(&[
            ("searchTerm", mbid),
            ("includeItemTypes", "MusicAlbum"),
            ("limit", "50"),
            ("Fields", "ProviderIds"),
        ]);
        let hints = self.hints(&query).await?;
        let hit = hints.iter().find(|hint| {
            hint.provider("MusicBrainzReleaseGroup").as_deref() == Some(mbid)
                || hint.provider("MusicBrainzAlbum").as_deref() == Some(mbid)
        });
        let album_id = match hit {
            Some(hint) => Some(hint.id.clone()),
            None => self.mbid_index().await?.get(mbid).cloned(),
        };
        let Some(album_id) = album_id else {
            return Ok(MatchView {
                source: SourceName::Jellyfin,
                found: false,
                remote_album_id: None,
                tracks: Vec::new(),
            });
        };
        let tracks = self.album_tracks(&album_id).await?;
        Ok(MatchView {
            source: SourceName::Jellyfin,
            found: true,
            remote_album_id: Some(album_id),
            tracks,
        })
    }

    fn require_configured(&self) -> Result<(), AdapterError> {
        if self.is_configured() {
            Ok(())
        } else {
            Err(AdapterError::NotConfigured)
        }
    }

    fn paged_params(&self, limit: i64, offset: i64) -> Params {
        vec![
            ("limit".to_owned(), limit.to_string()),
            ("startIndex".to_owned(), offset.to_string()),
        ]
    }

    /// An item list; a 404 or empty answer reads as an empty page.
    async fn items(
        &self,
        endpoint: &str,
        query: &[(String, String)],
    ) -> Result<ItemPage, AdapterError> {
        let page: Option<ItemPage> = self.get(endpoint, query).await?;
        Ok(page.unwrap_or(ItemPage {
            items: Vec::new(),
            total_record_count: Some(0),
        }))
    }

    /// Search hints; a 404 or empty answer reads as no hints.
    async fn hints(&self, query: &[(String, String)]) -> Result<Vec<Item>, AdapterError> {
        let hints: Option<SearchHints> = self.get("/Search/Hints", query).await?;
        Ok(hints.map(|hints| hints.search_hints).unwrap_or_default())
    }

    /// GET a JSON endpoint and decode it. 404, 204 and an empty body map to
    /// `None`; 401/403 map to auth failure; an undecodable body is an
    /// upstream error.
    async fn get<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        query: &[(String, String)],
    ) -> Result<Option<T>, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query: Params = query.to_vec();
        if !self.user_id.is_empty() && !query.iter().any(|(key, _)| key == "userId") {
            query.push(("userId".to_owned(), self.user_id.clone()));
        }
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .header("Accept", "application/json")
            .header("Authorization", self.auth_header())
            .query(&query)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AdapterError::Auth);
        }
        if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !status.is_success() {
            return Err(AdapterError::Api(format!(
                "GET {endpoint} failed ({status})"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if bytes.is_empty() {
            return Ok(None);
        }
        decode(endpoint, &bytes).map(Some)
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
        let response = self
            .client
            .post(format!("{}{endpoint}", self.base_url))
            .header("Content-Type", "application/json")
            .header("Authorization", self.auth_header())
            .body(body.to_string())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AdapterError::Auth);
        }
        if !status.is_success() {
            return Err(AdapterError::Api(format!(
                "POST {endpoint} failed ({status})"
            )));
        }
        Ok(())
    }

    /// GET raw bytes (images, audio). Any non-200 is an API error; a
    /// missing content type falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        query: &[(String, String)],
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .header("Authorization", self.auth_header())
            .query(query)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AdapterError::Auth);
        }
        if !status.is_success() {
            return Err(AdapterError::Api(format!(
                "GET {endpoint} failed ({status})"
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

    fn auth_header(&self) -> String {
        format!("MediaBrowser Token=\"{}\"", self.api_key)
    }
}

/// Decode one payload. A shape we cannot read (a missing id included) is
/// an upstream contract error; the detail stays in the log.
fn decode<T: DeserializeOwned>(endpoint: &str, bytes: &[u8]) -> Result<T, AdapterError> {
    serde_json::from_slice(bytes).map_err(|error| {
        AdapterError::Api(format!(
            "Jellyfin returned an unreadable {endpoint} payload: {error}"
        ))
    })
}

fn page_of<T>(page: ItemPage, map: impl Fn(&Item) -> T) -> RemotePage<T> {
    let items: Vec<T> = page.items.iter().map(map).collect();
    let total = page.total_record_count.unwrap_or(items.len() as i64);
    RemotePage { items, total }
}

fn album_view(item: &Item) -> AlbumView {
    AlbumView {
        source: SourceName::Jellyfin,
        image_url: image_url_for(&item.id, item.primary_tag()),
        id: item.id.clone(),
        title: item.name.clone().unwrap_or_else(|| "Unknown".to_owned()),
        artist_name: artist_name(item).unwrap_or_default(),
        artist_id: item.artist_items.first().map(|artist| artist.id.clone()),
        year: item
            .production_year
            .and_then(|year| i32::try_from(year).ok()),
        genre: None,
        track_count: item.child_count.and_then(|count| i32::try_from(count).ok()),
        release_mbid: item.provider("MusicBrainzAlbum"),
        release_group_mbid: item.provider("MusicBrainzReleaseGroup"),
        artist_mbid: item.provider("MusicBrainzArtist"),
    }
}

fn artist_view(item: &Item) -> ArtistView {
    ArtistView {
        source: SourceName::Jellyfin,
        image_url: image_url_for(&item.id, item.primary_tag()),
        id: item.id.clone(),
        name: item.name.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_count: item.album_count.and_then(|count| i32::try_from(count).ok()),
        artist_mbid: item.provider("MusicBrainzArtist"),
    }
}

fn track_view(item: &Item) -> TrackView {
    TrackView {
        source: SourceName::Jellyfin,
        image_url: image_url_for(&item.id, item.primary_tag()),
        id: item.id.clone(),
        title: item.name.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_name: item.album.clone().unwrap_or_default(),
        album_id: item.album_id.clone(),
        artist_name: artist_name(item).unwrap_or_default(),
        artist_id: item.artist_items.first().map(|artist| artist.id.clone()),
        track_number: item
            .index_number
            .and_then(|number| i32::try_from(number).ok()),
        disc_number: item
            .parent_index_number
            .and_then(|number| i32::try_from(number).ok()),
        duration_secs: item.run_time_ticks.map(|ticks| ticks / 10_000_000),
        year: item
            .production_year
            .and_then(|year| i32::try_from(year).ok()),
        recording_mbid: item.provider("MusicBrainzTrack"),
        part_key: None,
    }
}

fn playlist_summary(item: &Item) -> PlaylistSummary {
    PlaylistSummary {
        source: SourceName::Jellyfin,
        image_url: Some(format!(
            "/api/v3/remotes/jellyfin/covers/playlists/{}",
            item.id
        )),
        id: item.id.clone(),
        name: item.name.clone().unwrap_or_default(),
        track_count: item.child_count.unwrap_or(0),
        duration_secs: item
            .run_time_ticks
            .map(|ticks| ticks / 10_000_000)
            .unwrap_or(0),
    }
}

/// First linked artist name, else the album artist.
fn artist_name(item: &Item) -> Option<String> {
    item.artist_items
        .first()
        .and_then(|artist| artist.name.clone())
        .or_else(|| item.album_artist.clone())
}

fn order_word(descending: bool) -> &'static str {
    if descending {
        "Descending"
    } else {
        "Ascending"
    }
}

fn sort_or<'a>(value: &'a str, fallback: &'a str) -> &'a str {
    if value.is_empty() { fallback } else { value }
}

fn image_url_for(id: &str, tag: Option<&str>) -> Option<String> {
    if id.is_empty() {
        return None;
    }
    let mut url = format!("/api/v3/remotes/jellyfin/images/{id}");
    if let Some(tag) = tag {
        url.push_str("?tag=");
        url.push_str(tag);
    }
    Some(url)
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

fn trim_cause(cause: &reqwest::Error) -> String {
    let text = cause.to_string();
    text.chars().take(200).collect()
}
