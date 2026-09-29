//! Plex source adapter.
//!
//! Live-version-cited quirks, kept with their citations:
//!
//! - Auth rides `X-Plex-Token` plus `X-Plex-Product`/`X-Plex-Version` and,
//!   when known, `X-Plex-Client-Identifier`, with `Accept: application/json`.
//! - An http base behind a TLS-terminating proxy answers with a 302 to
//!   https; when a redirect swapped only the scheme on the same host, port,
//!   and path, the adapter rewrites its stored base once so later calls go
//!   straight to https.
//! - Every payload unwraps the `MediaContainer` envelope; library browse
//!   pages through `X-Plex-Container-Start`/`X-Plex-Container-Size` and
//!   reads totals from `totalSize`. Music sections are `type == "artist"`.
//! - Decade filters spell `"2020s"`: the trailing `s` strips and the year
//!   expands to a comma-separated `2020..=2029` list.
//! - Playlist art prefers the playlist's own `composite` path and falls
//!   back to `/playlists/{id}/composite` when empty.
//! - Plex.tv account calls (OAuth pins, per-server `accessToken` resolution
//!   per the Plex API 1.2.2 resource contract verified 2026-07-17) belong
//!   to the login flow, not browse, and live outside this adapter.

use std::collections::HashSet;
use std::sync::RwLock;
use std::time::Duration;

use serde_json::Value;

use super::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemotePage, TrackBrowse};
use super::models::{
    AlbumView, ArtistIndexEntry, ArtistView, FavoritesView, HistoryEntry, HistoryPage, HubView,
    InfoView, LyricsView, MatchView, PlaylistDetail, PlaylistSummary, SearchResults, SessionView,
    SessionsView, SourceName, StatsView, TrackView,
};

/// Request timeout per upstream call, matching the v2 repository.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Plex browse client. Section ids pin the music libraries; when empty the
/// adapter resolves every `artist`-typed section instead of answering empty
/// (v2 answered empty; auto-resolution is the documented clean-slate delta,
/// and a pinned id behaves exactly like v2).
pub struct PlexAdapter {
    client: reqwest::Client,
    base_url: RwLock<String>,
    token: String,
    client_id: String,
    section_ids: Vec<String>,
}

impl std::fmt::Debug for PlexAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlexAdapter")
            .field("client_id", &self.client_id)
            .field("section_ids", &self.section_ids)
            .finish_non_exhaustive()
    }
}

impl PlexAdapter {
    /// Build a client for one server. Empty URL or token reads as
    /// unconfigured. `section_ids` pins music libraries; empty auto-resolves.
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        token: String,
        client_id: String,
        section_ids: Vec<String>,
    ) -> Self {
        Self {
            client,
            base_url: RwLock::new(base_url.trim_end_matches('/').to_owned()),
            token,
            client_id,
            section_ids,
        }
    }

    /// True when a base URL and token are present.
    pub fn is_configured(&self) -> bool {
        !self.base_url().is_empty() && !self.token.is_empty()
    }

    /// Current base URL (visible so tests can observe the https upgrade).
    pub fn base_url(&self) -> String {
        self.base_url
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Connectivity probe against `/`. Returns the friendly-name label.
    pub async fn validate_connection(&self) -> Result<String, AdapterError> {
        let container = self.request("/", &[]).await?;
        let name = str_field(&container, "friendlyName").unwrap_or("Unknown");
        let version = str_field(&container, "version").unwrap_or("unknown");
        Ok(format!("Connected to {name} (v{version})"))
    }

    /// Server machine identifier via `/identity`, for token scoping.
    pub async fn machine_identifier(&self) -> Result<Option<String>, AdapterError> {
        self.require_configured()?;
        let response = self
            .client
            .get(format!("{}/identity", self.base_url()))
            .headers(self.headers())
            .timeout(Duration::from_secs(10))
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        if !response.status().is_success() {
            return Ok(None);
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| {
            AdapterError::Api("Plex returned invalid JSON for /identity".to_owned())
        })?;
        let machine = data
            .get("MediaContainer")
            .and_then(|container| str_field(container, "machineIdentifier"))
            .or_else(|| str_field(&data, "machineIdentifier"))
            .map(str::to_owned);
        Ok(machine)
    }

    /// Music library sections (`type == "artist"`).
    pub async fn music_libraries(&self) -> Result<Vec<(String, String)>, AdapterError> {
        self.require_configured()?;
        let container = self.request("/library/sections", &[]).await?;
        Ok(container
            .get("Directory")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter(|section| str_field(section, "type").unwrap_or("") == "artist")
            .map(|section| {
                (
                    str_field(section, "key").unwrap_or("").to_owned(),
                    str_field(section, "title").unwrap_or("").to_owned(),
                )
            })
            .collect())
    }

    /// Hub highlights. Sections fail open to empty; an all-failed hub is an
    /// upstream error.
    pub async fn hub(&self) -> Result<HubView, AdapterError> {
        self.require_configured()?;
        let preview_browse = AlbumBrowse {
            limit: 12,
            ..AlbumBrowse::default()
        };
        let (stats, recent, added, preview, genres) = tokio::join!(
            self.stats(),
            self.recent(20),
            self.recently_added(20),
            self.albums(&preview_browse),
            self.genres(),
        );
        let failures = [
            stats.is_err(),
            recent.is_err(),
            added.is_err(),
            preview.is_err(),
            genres.is_err(),
        ]
        .into_iter()
        .filter(|failed| *failed)
        .count();
        if failures == 5 {
            return Err(AdapterError::Api(
                "All Plex hub data requests failed".to_owned(),
            ));
        }
        Ok(HubView {
            source: SourceName::Plex,
            stats: stats.ok(),
            recently_played: recent.unwrap_or_default(),
            recently_added: added.unwrap_or_default(),
            favorites: Vec::new(),
            favorite_artists: Vec::new(),
            most_played_artists: Vec::new(),
            all_albums_preview: preview.map(|page| page.items).unwrap_or_default(),
            genres: genres.unwrap_or_default(),
        })
    }

    /// Library totals summed over the resolved sections.
    pub async fn stats(&self) -> Result<StatsView, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut stats = StatsView {
            total_albums: 0,
            total_artists: 0,
            total_tracks: 0,
        };
        for section in &sections {
            stats.total_albums += self.count(section, 9).await?;
            stats.total_artists += self.count(section, 8).await?;
            stats.total_tracks += self.count(section, 10).await?;
        }
        Ok(stats)
    }

    /// One page of albums. A single section pages upstream; several merge
    /// client-side (fetch `offset + size` per section, dedupe, sort, slice).
    pub async fn albums(
        &self,
        browse: &AlbumBrowse,
    ) -> Result<RemotePage<AlbumView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        if sections.is_empty() {
            return Ok(empty_page());
        }
        let sort = plex_sort(&browse.sort_by, browse.descending);
        // An exact year filters exactly; the "2020s" decade spelling
        // expands to a year list only when no exact year is set.
        let (exact_year, decade) = match browse.year {
            Some(year) => (Some(year.to_string()), String::new()),
            None => (None, browse.decade.clone()),
        };
        let year_param = exact_year.as_deref().unwrap_or("");
        let filters = AlbumFilters {
            genre: &browse.genre,
            mood: "",
            decade: &decade,
            exact_year: year_param,
        };
        if sections.len() == 1 {
            let (items, total) = self
                .section_albums(&sections[0], browse.limit, browse.offset, &sort, filters)
                .await?;
            return Ok(RemotePage { items, total });
        }
        let fetch = browse.offset + browse.limit;
        let mut merged: Vec<Value> = Vec::new();
        let mut seen = HashSet::new();
        let mut total: i64 = 0;
        for section in &sections {
            let (albums, section_total) = self
                .section_albums_raw(section, fetch, 0, &sort, filters)
                .await?;
            total += section_total;
            for album in albums {
                let key = str_field(&album, "ratingKey").unwrap_or("").to_owned();
                if seen.insert(key) {
                    merged.push(album);
                }
            }
        }
        sort_raw_albums(&mut merged, &sort);
        if let Some(year) = browse.year {
            merged.retain(|album| album.get("year").and_then(value_to_i64) == Some(year as i64));
            total = merged.len() as i64;
        } else if let Some((start, end)) = parse_decade(&decade) {
            merged.retain(|album| {
                album
                    .get("year")
                    .and_then(value_to_i64)
                    .map(|year| year >= start && year <= end)
                    .unwrap_or(false)
            });
            total = merged.len() as i64;
        }
        let start = browse.offset.max(0) as usize;
        let page: Vec<AlbumView> = merged
            .into_iter()
            .skip(start)
            .take(browse.limit.max(0) as usize)
            .filter(known_title)
            .map(|album| self.album_view(&album))
            .collect();
        Ok(RemotePage { items: page, total })
    }

    /// One album by rating key. None is absence.
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(&format!("/library/metadata/{id}"), &[])
            .await?;
        let raw = container
            .get("Metadata")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.first().map(|album| self.album_view(album)))
    }

    /// Album tracks via the metadata children endpoint.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(&format!("/library/metadata/{id}/children"), &[])
            .await?;
        let raw = container
            .get("Metadata")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.iter().map(|track| self.track_view(track)).collect())
    }

    /// One page of artists, merged over the resolved sections.
    pub async fn artists(
        &self,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        if sections.is_empty() {
            return Ok(empty_page());
        }
        if sections.len() == 1 {
            return self.section_artists(&sections[0], browse).await;
        }
        let fetch = browse.offset + browse.limit;
        let mut merged: Vec<ArtistView> = Vec::new();
        let mut seen = HashSet::new();
        for section in &sections {
            let page = self
                .section_artists(
                    section,
                    &ArtistBrowse {
                        limit: fetch,
                        offset: 0,
                        search: browse.search.clone(),
                        ..ArtistBrowse::default()
                    },
                )
                .await?;
            for artist in page.items {
                if seen.insert(artist.id.clone()) {
                    merged.push(artist);
                }
            }
        }
        merged.sort_by(|left, right| left.name.cmp(&right.name));
        if browse.descending {
            merged.reverse();
        }
        let total = merged.len() as i64;
        let start = browse.offset.max(0) as usize;
        let items = merged
            .into_iter()
            .skip(start)
            .take(browse.limit.max(0) as usize)
            .collect();
        Ok(RemotePage { items, total })
    }

    /// Full alphabetic artist index, bucketed client-side.
    pub async fn artist_index(&self) -> Result<Vec<ArtistIndexEntry>, AdapterError> {
        let page = self
            .artists(&ArtistBrowse {
                limit: 10_000,
                ..ArtistBrowse::default()
            })
            .await?;
        Ok(bucket_index(page.items))
    }

    /// One artist by rating key. None is absence.
    pub async fn artist_detail(&self, id: &str) -> Result<Option<ArtistView>, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(&format!("/library/metadata/{id}"), &[])
            .await?;
        let raw = container
            .get("Metadata")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.first().map(|artist| self.artist_view(artist)))
    }

    /// One page of tracks, merged over the resolved sections.
    pub async fn tracks(
        &self,
        browse: &TrackBrowse,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        if sections.is_empty() {
            return Ok(empty_page());
        }
        let sort = plex_sort(&browse.sort_by, browse.descending);
        if sections.len() == 1 {
            return self.section_tracks(&sections[0], browse, &sort).await;
        }
        let fetch = browse.offset + browse.limit;
        let mut merged: Vec<TrackView> = Vec::new();
        let mut seen = HashSet::new();
        let mut total: i64 = 0;
        for section in &sections {
            let page = self
                .section_tracks(
                    section,
                    &TrackBrowse {
                        limit: fetch,
                        offset: 0,
                        search: browse.search.clone(),
                        genre: browse.genre.clone(),
                        ..TrackBrowse::default()
                    },
                    &sort,
                )
                .await?;
            total += page.total;
            for track in page.items {
                if seen.insert(track.id.clone()) {
                    merged.push(track);
                }
            }
        }
        merged.sort_by(|left, right| left.title.cmp(&right.title));
        if browse.descending {
            merged.reverse();
        }
        let start = browse.offset.max(0) as usize;
        let items = merged
            .into_iter()
            .skip(start)
            .take(browse.limit.max(0) as usize)
            .collect();
        Ok(RemotePage { items, total })
    }

    /// Unified search over the typed hub array.
    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResults, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut results = SearchResults {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        };
        let mut seen = HashSet::new();
        let targets: Vec<Option<String>> = if sections.is_empty() {
            vec![None]
        } else {
            sections.into_iter().map(Some).collect()
        };
        for section in targets {
            let mut params = vec![
                ("query".to_owned(), query.to_owned()),
                ("limit".to_owned(), limit.to_string()),
            ];
            if let Some(section) = &section {
                params.push(("sectionId".to_owned(), section.clone()));
            }
            let container = self.request("/hubs/search", &params).await?;
            let hubs = container
                .get("Hub")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for hub in &hubs {
                let hub_type = str_field(hub, "type").unwrap_or("");
                let items = hub
                    .get("Metadata")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for item in &items {
                    let key = format!("{hub_type}:{}", str_field(item, "ratingKey").unwrap_or(""));
                    if !seen.insert(key) {
                        continue;
                    }
                    match hub_type {
                        "album" => results.albums.push(self.album_view(item)),
                        "track" => results.tracks.push(self.track_view(item)),
                        "artist" => results.artists.push(self.artist_view(item)),
                        _ => {}
                    }
                }
            }
        }
        Ok(results)
    }

    /// Recently viewed albums, newest first, falling back to recently added
    /// when nothing was viewed yet.
    pub async fn recent(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut viewed: Vec<(i64, Value)> = Vec::new();
        for section in &sections {
            let container = self
                .request(
                    &format!("/library/sections/{section}/recentlyViewed"),
                    &[
                        ("type".to_owned(), "9".to_owned()),
                        ("X-Plex-Container-Size".to_owned(), limit.to_string()),
                    ],
                )
                .await?;
            for album in metadata(&container) {
                let at = album
                    .get("lastViewedAt")
                    .and_then(value_to_i64)
                    .unwrap_or(0);
                viewed.push((at, album));
            }
        }
        if !viewed.is_empty() {
            viewed.sort_by(|left, right| right.0.cmp(&left.0));
            return Ok(viewed
                .into_iter()
                .take(limit.max(0) as usize)
                .map(|(_, album)| album)
                .filter(known_title)
                .map(|album| self.album_view(&album))
                .collect());
        }
        self.recently_added(limit).await
    }

    /// Recently added albums via `recentlyAdded`, newest first.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut added: Vec<(i64, Value)> = Vec::new();
        for section in &sections {
            let container = self
                .request(
                    &format!("/library/sections/{section}/recentlyAdded"),
                    &[
                        ("type".to_owned(), "9".to_owned()),
                        ("X-Plex-Container-Size".to_owned(), limit.to_string()),
                    ],
                )
                .await?;
            for album in metadata(&container) {
                let at = album.get("addedAt").and_then(value_to_i64).unwrap_or(0);
                added.push((at, album));
            }
        }
        added.sort_by(|left, right| right.0.cmp(&left.0));
        Ok(added
            .into_iter()
            .take(limit.max(0) as usize)
            .map(|(_, album)| album)
            .filter(known_title)
            .map(|album| self.album_view(&album))
            .collect())
    }

    /// Plex exposes no favorites endpoint; the unified shape stays empty.
    pub async fn favorites(&self) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        Ok(FavoritesView {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        })
    }

    /// Genre labels unioned over the resolved sections.
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut genres = HashSet::new();
        for section in &sections {
            let container = self
                .request(&format!("/library/sections/{section}/genre"), &[])
                .await?;
            for directory in directories(&container) {
                if let Some(title) = str_field(&directory, "title")
                    && !title.is_empty()
                {
                    genres.insert(title.to_owned());
                }
            }
        }
        let mut sorted: Vec<String> = genres.into_iter().collect();
        sorted.sort();
        Ok(sorted)
    }

    /// Mood labels unioned over the resolved sections.
    pub async fn moods(&self) -> Result<Vec<String>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut moods = HashSet::new();
        for section in &sections {
            let container = self
                .request(&format!("/library/sections/{section}/mood"), &[])
                .await?;
            for directory in directories(&container) {
                if let Some(title) = str_field(&directory, "title")
                    && !title.is_empty()
                {
                    moods.insert(title.to_owned());
                }
            }
        }
        let mut sorted: Vec<String> = moods.into_iter().collect();
        sorted.sort();
        Ok(sorted)
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
        let container = self
            .request(
                "/playlists",
                &[("playlistType".to_owned(), "audio".to_owned())],
            )
            .await?;
        Ok(metadata(&container)
            .iter()
            .map(|item| self.playlist_summary(item))
            .collect())
    }

    /// One playlist with its tracks. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let playlists = self.playlists().await?;
        let summary = playlists.into_iter().find(|playlist| playlist.id == id);
        let Some(summary) = summary else {
            return Ok(None);
        };
        let container = self.request(&format!("/playlists/{id}/items"), &[]).await?;
        Ok(Some(PlaylistDetail {
            playlist: summary,
            tracks: metadata(&container)
                .iter()
                .map(|track| self.track_view(track))
                .collect(),
        }))
    }

    /// Plex exposes no artist-info endpoint.
    pub async fn artist_info(&self, _id: &str) -> Result<InfoView, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no artist-info endpoint".to_owned(),
        ))
    }

    /// Plex exposes no album-info endpoint.
    pub async fn album_info(&self, _id: &str) -> Result<InfoView, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no album-info endpoint".to_owned(),
        ))
    }

    /// Plex exposes no lyrics endpoint.
    pub async fn lyrics(&self, _id: &str) -> Result<Option<LyricsView>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no lyrics endpoint".to_owned(),
        ))
    }

    /// Plex exposes no top-songs endpoint.
    pub async fn top_songs(
        &self,
        _artist: &str,
        _limit: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no top-songs endpoint".to_owned(),
        ))
    }

    /// Plex exposes no similar-tracks endpoint.
    pub async fn similar(&self, _id: &str, _limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no similar-tracks endpoint".to_owned(),
        ))
    }

    /// Audio-only sessions from `/status/sessions`.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let container = self.request("/status/sessions", &[]).await?;
        let mut sessions = Vec::new();
        for track in metadata(&container) {
            if str_field(&track, "type").unwrap_or("") != "track" {
                continue;
            }
            let user = track.get("User").cloned().unwrap_or(Value::Null);
            let player = track.get("Player").cloned().unwrap_or(Value::Null);
            let session = track.get("Session").cloned().unwrap_or(Value::Null);
            sessions.push(SessionView {
                source: SourceName::Plex,
                session_id: str_field(&session, "id").unwrap_or("").to_owned(),
                user_name: str_field(&user, "title").unwrap_or("").to_owned(),
                device_name: str_field(&player, "title").unwrap_or("").to_owned(),
                track_title: str_field(&track, "title").unwrap_or("").to_owned(),
                artist_name: str_field(&track, "grandparentTitle")
                    .unwrap_or("")
                    .to_owned(),
                album_name: str_field(&track, "parentTitle").unwrap_or("").to_owned(),
                progress_ms: track.get("viewOffset").and_then(value_to_i64).unwrap_or(0),
                duration_ms: track.get("duration").and_then(value_to_i64).unwrap_or(0),
                is_paused: str_field(&player, "state").unwrap_or("playing") == "paused",
            });
        }
        Ok(SessionsView {
            source: SourceName::Plex,
            sessions,
        })
    }

    /// Audio-only listening history, newest first.
    pub async fn history(&self, limit: i64, offset: i64) -> Result<HistoryPage, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(
                "/status/sessions/history/all",
                &[
                    ("X-Plex-Container-Start".to_owned(), offset.to_string()),
                    ("X-Plex-Container-Size".to_owned(), limit.to_string()),
                    ("sort".to_owned(), "viewedAt:desc".to_owned()),
                ],
            )
            .await?;
        let items: Vec<HistoryEntry> = metadata(&container)
            .into_iter()
            .filter(|item| str_field(item, "type").unwrap_or("") == "track")
            .map(|item| HistoryEntry {
                id: str_field(&item, "ratingKey").unwrap_or("").to_owned(),
                track_title: str_field(&item, "title").unwrap_or("").to_owned(),
                artist_name: str_field(&item, "grandparentTitle")
                    .unwrap_or("")
                    .to_owned(),
                album_name: str_field(&item, "parentTitle").unwrap_or("").to_owned(),
                viewed_at: item.get("viewedAt").and_then(value_to_i64).unwrap_or(0),
                duration_ms: item.get("duration").and_then(value_to_i64).unwrap_or(0),
            })
            .collect();
        let total = container
            .get("totalSize")
            .and_then(value_to_i64)
            .unwrap_or(items.len() as i64);
        Ok(HistoryPage {
            source: SourceName::Plex,
            items,
            total,
        })
    }

    /// Thumbnail bytes for a rating key.
    pub async fn image_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("width".to_owned(), size.to_string()),
            ("height".to_owned(), size.to_string()),
        ];
        self.get_bytes(
            &format!("/library/metadata/{id}/thumb"),
            &params,
            false,
            "image/jpeg",
        )
        .await
    }

    /// Direct audio bytes for one part key (v2 `proxy_get_stream` target,
    /// fetched whole for the gateway seam). Keys are validated exactly as
    /// in v2: a leading slash is added, `..` segments and non-part paths
    /// are rejected without any request.
    pub async fn audio_bytes(&self, part_key: &str) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let key = validated_part_key(part_key)?;
        self.get_bytes(&key, &[], false, "audio/mpeg").await
    }

    /// Playback-state report via `/:/timeline` (v2 `now_playing`): `state`
    /// is `playing`, `paused`, or `stopped`.
    pub async fn timeline(&self, rating_key: &str, state: &str) -> Result<(), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("ratingKey".to_owned(), rating_key.to_owned()),
            ("state".to_owned(), state.to_owned()),
            ("key".to_owned(), format!("/library/metadata/{rating_key}")),
        ];
        self.request("/:/timeline", &params).await?;
        Ok(())
    }

    /// Scrobble via `/:/scrobble` (v2 `scrobble`).
    pub async fn scrobble(&self, rating_key: &str) -> Result<(), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("key".to_owned(), rating_key.to_owned()),
            (
                "identifier".to_owned(),
                "com.plexapp.plugins.library".to_owned(),
            ),
        ];
        self.request("/:/scrobble", &params).await?;
        Ok(())
    }

    /// Playlist composite bytes: the playlist's own `composite` path when
    /// present, else `/playlists/{id}/composite`.
    pub async fn playlist_cover_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let container = self
            .request(
                "/playlists",
                &[("playlistType".to_owned(), "audio".to_owned())],
            )
            .await?;
        let mut composite = format!("/playlists/{id}/composite");
        for playlist in metadata(&container) {
            if str_field(&playlist, "ratingKey").unwrap_or("") != id {
                continue;
            }
            if let Some(path) = str_field(&playlist, "composite")
                && !path.is_empty()
            {
                composite = path.to_owned();
            }
            break;
        }
        let params = vec![
            ("width".to_owned(), size.to_string()),
            ("height".to_owned(), size.to_string()),
        ];
        self.get_bytes(&composite, &params, true, "image/jpeg")
            .await
    }

    /// Resolve an MBID by searching for it per section and comparing
    /// `mbid://` guids.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        let results = self.search(mbid, 50).await?;
        for album in &results.albums {
            if album.release_mbid.as_deref() == Some(mbid)
                || album.release_group_mbid.as_deref() == Some(mbid)
            {
                let tracks = self.album_tracks(&album.id).await?;
                return Ok(MatchView {
                    source: SourceName::Plex,
                    found: true,
                    remote_album_id: Some(album.id.clone()),
                    tracks,
                });
            }
        }
        Ok(MatchView {
            source: SourceName::Plex,
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

    /// Pinned sections, or every music section when nothing is pinned.
    async fn resolve_sections(&self) -> Result<Vec<String>, AdapterError> {
        self.require_configured()?;
        if !self.section_ids.is_empty() {
            return Ok(self.section_ids.clone());
        }
        Ok(self
            .music_libraries()
            .await?
            .into_iter()
            .map(|(key, _)| key)
            .collect())
    }

    /// `totalSize` for one section and item type via a zero-size query.
    async fn count(&self, section: &str, item_type: u8) -> Result<i64, AdapterError> {
        let container = self
            .request(
                &format!("/library/sections/{section}/all"),
                &[
                    ("type".to_owned(), item_type.to_string()),
                    ("X-Plex-Container-Start".to_owned(), "0".to_owned()),
                    ("X-Plex-Container-Size".to_owned(), "0".to_owned()),
                ],
            )
            .await?;
        Ok(container
            .get("totalSize")
            .and_then(value_to_i64)
            .unwrap_or(0))
    }

    async fn section_albums(
        &self,
        section: &str,
        size: i64,
        offset: i64,
        sort: &str,
        filters: AlbumFilters<'_>,
    ) -> Result<(Vec<AlbumView>, i64), AdapterError> {
        let (raw, total) = self
            .section_albums_raw(section, size, offset, sort, filters)
            .await?;
        Ok((
            raw.into_iter()
                .filter(known_title)
                .map(|album| self.album_view(&album))
                .collect(),
            total,
        ))
    }

    async fn section_albums_raw(
        &self,
        section: &str,
        size: i64,
        offset: i64,
        sort: &str,
        filters: AlbumFilters<'_>,
    ) -> Result<(Vec<Value>, i64), AdapterError> {
        let mut params = vec![
            ("type".to_owned(), "9".to_owned()),
            ("X-Plex-Container-Start".to_owned(), offset.to_string()),
            ("X-Plex-Container-Size".to_owned(), size.to_string()),
            ("sort".to_owned(), sort.to_owned()),
        ];
        if !filters.genre.is_empty() {
            params.push(("genre".to_owned(), filters.genre.to_owned()));
        }
        if !filters.mood.is_empty() {
            params.push(("mood".to_owned(), filters.mood.to_owned()));
        }
        if !filters.exact_year.is_empty() {
            params.push(("year".to_owned(), filters.exact_year.to_owned()));
        } else if let Some(years) = decade_years(filters.decade) {
            params.push(("year".to_owned(), years));
        }
        let container = self
            .request(&format!("/library/sections/{section}/all"), &params)
            .await?;
        let raw = metadata(&container);
        let total = container
            .get("totalSize")
            .and_then(value_to_i64)
            .unwrap_or(raw.len() as i64);
        Ok((raw, total))
    }

    async fn section_artists(
        &self,
        section: &str,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        let mut params = vec![
            ("type".to_owned(), "8".to_owned()),
            (
                "X-Plex-Container-Start".to_owned(),
                browse.offset.to_string(),
            ),
            ("X-Plex-Container-Size".to_owned(), browse.limit.to_string()),
        ];
        if !browse.search.is_empty() {
            params.push(("title".to_owned(), browse.search.clone()));
        }
        let container = self
            .request(&format!("/library/sections/{section}/all"), &params)
            .await?;
        let raw = metadata(&container);
        let total = container
            .get("totalSize")
            .and_then(value_to_i64)
            .unwrap_or(raw.len() as i64);
        let mut items: Vec<ArtistView> =
            raw.iter().map(|artist| self.artist_view(artist)).collect();
        if browse.descending {
            items.reverse();
        }
        Ok(RemotePage { items, total })
    }

    async fn section_tracks(
        &self,
        section: &str,
        browse: &TrackBrowse,
        sort: &str,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        let mut params = vec![
            ("type".to_owned(), "10".to_owned()),
            ("sort".to_owned(), sort.to_owned()),
            (
                "X-Plex-Container-Start".to_owned(),
                browse.offset.to_string(),
            ),
            ("X-Plex-Container-Size".to_owned(), browse.limit.to_string()),
        ];
        if !browse.search.is_empty() {
            params.push(("title".to_owned(), browse.search.clone()));
        }
        if !browse.genre.is_empty() {
            params.push(("genre".to_owned(), browse.genre.clone()));
        }
        let container = self
            .request(&format!("/library/sections/{section}/all"), &params)
            .await?;
        let raw = metadata(&container);
        let total = container
            .get("totalSize")
            .and_then(value_to_i64)
            .unwrap_or(raw.len() as i64);
        Ok(RemotePage {
            items: raw.iter().map(|track| self.track_view(track)).collect(),
            total,
        })
    }

    fn headers(&self) -> reqwest::header::HeaderMap {
        let mut headers = reqwest::header::HeaderMap::new();
        if let Ok(value) = reqwest::header::HeaderValue::from_str(&self.token) {
            headers.insert("X-Plex-Token", value);
        }
        headers.insert(
            "X-Plex-Product",
            reqwest::header::HeaderValue::from_static("DroppedNeedle"),
        );
        headers.insert(
            "X-Plex-Version",
            reqwest::header::HeaderValue::from_static("1.0"),
        );
        headers.insert(
            reqwest::header::ACCEPT,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        if !self.client_id.is_empty()
            && let Ok(value) = reqwest::header::HeaderValue::from_str(&self.client_id)
        {
            headers.insert("X-Plex-Client-Identifier", value);
        }
        headers
    }

    /// GET a JSON endpoint and unwrap `MediaContainer`.
    async fn request(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<Value, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let url = format!("{}{endpoint}", self.base_url());
        let response = self
            .client
            .get(&url)
            .query(params)
            .headers(self.headers())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        self.maybe_upgrade_base(&url, response.url());
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
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let data: Value = serde_json::from_slice(&bytes)
            .map_err(|_| AdapterError::Api(format!("Plex returned invalid JSON for {endpoint}")))?;
        data.get("MediaContainer").cloned().ok_or_else(|| {
            AdapterError::Api("Missing MediaContainer envelope in Plex response".to_owned())
        })
    }

    /// GET raw bytes (thumbs, composites, audio). A missing content type
    /// falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        params: &[(String, String)],
        accept_image: bool,
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut headers = self.headers();
        if accept_image {
            headers.insert(
                reqwest::header::ACCEPT,
                reqwest::header::HeaderValue::from_static("image/*"),
            );
        }
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url()))
            .query(params)
            .headers(headers)
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

    /// Persist an http->https base upgrade when the redirect swapped only
    /// the scheme on the same host, port, and path.
    fn maybe_upgrade_base(&self, requested: &str, final_url: &reqwest::Url) {
        let Ok(before) = reqwest::Url::parse(requested) else {
            return;
        };
        if before.scheme() != "http"
            || final_url.scheme() != "https"
            || before.host_str() != final_url.host_str()
            || before.port() != final_url.port()
            || before.path() != final_url.path()
        {
            return;
        }
        if let Ok(mut guard) = self.base_url.write() {
            let upgraded = format!("https://{}", guard.trim_start_matches("http://"));
            if upgraded != *guard && guard.starts_with("http://") {
                *guard = upgraded;
            }
        }
    }

    fn album_view(&self, album: &Value) -> AlbumView {
        let id = str_field(album, "ratingKey").unwrap_or("").to_owned();
        let genres: Vec<String> = album
            .get("Genre")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|genre| str_field(genre, "tag").map(str::to_owned))
            .filter(|tag| !tag.is_empty())
            .collect();
        AlbumView {
            source: SourceName::Plex,
            image_url: (!id.is_empty() && has_thumb(album))
                .then(|| format!("/api/v3/remotes/plex/images/{id}")),
            id,
            title: str_field(album, "title").unwrap_or("Unknown").to_owned(),
            artist_name: str_field(album, "parentTitle").unwrap_or("").to_owned(),
            artist_id: non_empty(str_field(album, "parentRatingKey")),
            year: album
                .get("year")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            genre: genres.first().cloned(),
            track_count: album
                .get("leafCount")
                .and_then(value_to_i64)
                .map(|n| n as i32),
            release_mbid: guid_mbid(album),
            release_group_mbid: None,
            artist_mbid: None,
        }
    }

    fn artist_view(&self, artist: &Value) -> ArtistView {
        let id = str_field(artist, "ratingKey").unwrap_or("").to_owned();
        ArtistView {
            source: SourceName::Plex,
            image_url: (!id.is_empty() && has_thumb(artist))
                .then(|| format!("/api/v3/remotes/plex/images/{id}")),
            id,
            name: str_field(artist, "title").unwrap_or("Unknown").to_owned(),
            album_count: None,
            artist_mbid: guid_mbid(artist),
        }
    }

    fn track_view(&self, track: &Value) -> TrackView {
        let id = str_field(track, "ratingKey").unwrap_or("").to_owned();
        let parent_id = str_field(track, "parentRatingKey").unwrap_or("").to_owned();
        TrackView {
            source: SourceName::Plex,
            image_url: (!parent_id.is_empty())
                .then(|| format!("/api/v3/remotes/plex/images/{parent_id}")),
            id,
            title: str_field(track, "title").unwrap_or("Unknown").to_owned(),
            album_name: str_field(track, "parentTitle").unwrap_or("").to_owned(),
            album_id: non_empty(Some(parent_id.as_str())),
            artist_name: str_field(track, "grandparentTitle")
                .unwrap_or("")
                .to_owned(),
            artist_id: None,
            track_number: track.get("index").and_then(value_to_i64).map(|n| n as i32),
            disc_number: track
                .get("parentIndex")
                .and_then(value_to_i64)
                .map(|n| n as i32),
            duration_secs: track
                .get("duration")
                .and_then(value_to_i64)
                .map(|ms| ms / 1000),
            year: track
                .get("year")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            recording_mbid: guid_mbid(track),
        }
    }

    fn playlist_summary(&self, playlist: &Value) -> PlaylistSummary {
        let id = str_field(playlist, "ratingKey").unwrap_or("").to_owned();
        PlaylistSummary {
            source: SourceName::Plex,
            image_url: (!id.is_empty())
                .then(|| format!("/api/v3/remotes/plex/covers/playlists/{id}")),
            id,
            name: str_field(playlist, "title").unwrap_or("").to_owned(),
            track_count: playlist
                .get("leafCount")
                .and_then(value_to_i64)
                .unwrap_or(0),
            duration_secs: playlist
                .get("duration")
                .and_then(value_to_i64)
                .map(|ms| ms / 1000)
                .unwrap_or(0),
        }
    }
}

/// Album list filters for one section query.
#[derive(Debug, Clone, Copy)]
struct AlbumFilters<'a> {
    /// Genre label, empty for none.
    genre: &'a str,
    /// Mood label, empty for none.
    mood: &'a str,
    /// Decade in `"2020s"` spelling, empty for none.
    decade: &'a str,
    /// Exact year, empty for none. Wins over the decade.
    exact_year: &'a str,
}

/// UI sort names to Plex `field:direction`, ported from the v2 route map.
fn plex_sort(sort_by: &str, descending: bool) -> String {
    let field = match sort_by {
        "name" | "" => "titleSort",
        "date_added" => "addedAt",
        "year" => "year",
        "play_count" => "viewCount",
        "rating" => "userRating",
        "last_played" => "lastViewedAt",
        other => other,
    };
    let direction = if descending { "desc" } else { "asc" };
    format!("{field}:{direction}")
}

/// Parse a `"2020s"` decade to its inclusive year range.
fn parse_decade(decade: &str) -> Option<(i64, i64)> {
    if decade.is_empty() {
        return None;
    }
    let start: i64 = decade.trim_end_matches('s').parse().ok()?;
    Some((start, start + 9))
}

/// `"2020s"` to `"2020,...,2029"`. Unparseable input disables the filter.
fn decade_years(decade: &str) -> Option<String> {
    if decade.is_empty() {
        return None;
    }
    let start: i32 = decade.trim_end_matches('s').parse().ok()?;
    Some(
        (start..start + 10)
            .map(|year| year.to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Client-side album sort for merged multi-section pages.
fn sort_raw_albums(albums: &mut [Value], sort: &str) {
    let mut parts = sort.splitn(2, ':');
    let field = parts.next().unwrap_or("titleSort");
    let reverse = parts.next().unwrap_or("asc") == "desc";
    match field {
        "addedAt" => {
            albums.sort_by_key(|album| album.get("addedAt").and_then(value_to_i64).unwrap_or(0))
        }
        "year" => albums.sort_by_key(|album| album.get("year").and_then(value_to_i64).unwrap_or(0)),
        "viewCount" => {
            albums.sort_by_key(|album| album.get("viewCount").and_then(value_to_i64).unwrap_or(0))
        }
        "lastViewedAt" => albums.sort_by_key(|album| {
            album
                .get("lastViewedAt")
                .and_then(value_to_i64)
                .unwrap_or(0)
        }),
        "userRating" => albums.sort_by(|left, right| {
            rating(left)
                .partial_cmp(&rating(right))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => albums.sort_by_key(|album| str_field(album, "title").unwrap_or("").to_lowercase()),
    }
    if reverse {
        albums.reverse();
    }
}

fn rating(album: &Value) -> f64 {
    album
        .get("userRating")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn metadata(container: &Value) -> Vec<Value> {
    container
        .get("Metadata")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn directories(container: &Value) -> Vec<Value> {
    container
        .get("Directory")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// First `mbid://` guid on the record.
fn guid_mbid(record: &Value) -> Option<String> {
    record
        .get("Guid")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|guid| str_field(guid, "id"))
        .find_map(|id| id.strip_prefix("mbid://").map(str::to_owned))
}

fn has_thumb(record: &Value) -> bool {
    !str_field(record, "thumb").unwrap_or("").is_empty()
}

fn known_title(album: &Value) -> bool {
    let title = str_field(album, "title").unwrap_or("");
    !title.is_empty() && title != "Unknown"
}

fn empty_page<T>() -> RemotePage<T> {
    RemotePage {
        items: Vec::new(),
        total: 0,
    }
}

fn bucket_index(artists: Vec<ArtistView>) -> Vec<ArtistIndexEntry> {
    let mut sorted = artists;
    sorted.sort_by(|left, right| left.name.cmp(&right.name));
    let mut buckets: Vec<ArtistIndexEntry> = Vec::new();
    for artist in sorted {
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

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|text| !text.is_empty()).map(str::to_owned)
}

fn str_field<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Value::as_str)
}

fn value_to_i64(value: &Value) -> Option<i64> {
    if let Some(number) = value.as_i64() {
        return Some(number);
    }
    if let Some(number) = value.as_u64() {
        return i64::try_from(number).ok();
    }
    // Plex serializes some ids and counts as strings.
    value.as_str().and_then(|text| text.parse::<i64>().ok())
}

fn trim_cause(cause: &reqwest::Error) -> String {
    let text = cause.to_string();
    if text.len() > 200 {
        text[..200].to_owned()
    } else {
        text
    }
}

/// Validate a Plex part key exactly as v2 `proxy_get_stream` does: a
/// leading slash is added, `..` segments and anything outside
/// `/library/parts/` are rejected without any request.
fn validated_part_key(raw: &str) -> Result<String, AdapterError> {
    let key = if raw.starts_with('/') {
        raw.to_owned()
    } else {
        format!("/{raw}")
    };
    if key.split('/').any(|segment| segment == "..") || !key.starts_with("/library/parts/") {
        return Err(AdapterError::Unsupported(
            "Invalid Plex part key".to_owned(),
        ));
    }
    Ok(key)
}
