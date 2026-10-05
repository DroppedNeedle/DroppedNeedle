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
//! - Plex.tv account calls (OAuth pins) belong to the login flow and live
//!   outside this adapter. The one exception is the per-server
//!   `accessToken` lookup for links saved without one ([`PlexTokenProbe`]),
//!   per the Plex API 1.2.2 resource contract verified 2026-07-17: an
//!   account token authorizes `plex.tv/api/v2/resources`, and each server
//!   resource's `accessToken` is used for requests to that server.
//!
//! Payloads decode into the typed shapes in [`super::plex_models`]; a row
//! a view needs without its `ratingKey` is an upstream error.

use std::collections::HashSet;
use std::sync::RwLock;
use std::time::Duration;

use super::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemotePage, TrackBrowse};
use super::connections::{PlexServerTokens, ServerSettings};
use super::models::{
    AlbumView, ArtistIndexEntry, ArtistView, DiscoveryHubView, DiscoveryView, FavoritesView,
    HistoryEntry, HistoryPage, HubView, InfoView, LyricsView, MatchView, PlaylistDetail,
    PlaylistSummary, SearchResults, SessionView, SessionsView, SourceName, StatsView, TrackView,
};
use super::plex_models::{Container, Envelope, Metadata, Resource};

/// Request timeout per upstream call, matching the v2 repository.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// plex.tv account API root.
pub const PLEX_TV_BASE: &str = "https://plex.tv/api/v2";

/// Query pairs, owned.
type Params = Vec<(String, String)>;

fn params(pairs: &[(&str, &str)]) -> Params {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Plex browse client. Section ids pin the music libraries; when empty the
/// adapter resolves every `artist`-typed section instead of answering empty
/// (v2 answered empty; a pinned id behaves exactly like v2).
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

/// Album list filters for one section query.
#[derive(Debug, Clone, Copy)]
struct AlbumFilters<'a> {
    /// Genre label, empty for none.
    genre: &'a str,
    /// Decade in `"2020s"` spelling, empty for none.
    decade: &'a str,
    /// Exact year, empty for none. Wins over the decade.
    exact_year: &'a str,
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

    /// Current base URL; an https upgrade rewrites it in place.
    fn base_url(&self) -> String {
        self.base_url
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Connectivity probe against `/`. Returns the friendly-name label.
    pub async fn validate_connection(&self) -> Result<String, AdapterError> {
        let container = self.request("/", &[]).await?;
        Ok(format!(
            "Connected to {} (v{})",
            container.friendly_name.as_deref().unwrap_or("Unknown"),
            container.version.as_deref().unwrap_or("unknown")
        ))
    }

    /// The server's machine id (`/identity`); `None` when the server does
    /// not name one or answers with an error (v2 `get_machine_identifier`).
    pub async fn machine_identifier(&self) -> Result<Option<String>, AdapterError> {
        match self.request("/identity", &[]).await {
            Ok(container) => Ok(non_empty(container.machine_identifier.as_deref())),
            Err(AdapterError::Api(_)) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The account's token for the server `machine_id`, from plex.tv
    /// `/resources` under this adapter's token (an account token here).
    /// `None` when the account has no server device with that id.
    pub async fn server_access_token(
        &self,
        plex_tv: &str,
        machine_id: &str,
    ) -> Result<Option<String>, AdapterError> {
        let response = self
            .client
            .get(format!("{}/resources", plex_tv.trim_end_matches('/')))
            .query(&[("includeHttps", "1"), ("includeRelay", "1")])
            .headers(self.headers())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AdapterError::Auth);
        }
        if !status.is_success() {
            return Err(AdapterError::Api(format!(
                "GET /resources failed ({status})"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
        // Lenient per device: client devices without a token still list.
        let devices: Vec<serde_json::Value> = serde_json::from_slice(&bytes).map_err(|_| {
            AdapterError::Api("Plex returned an unreadable /resources payload".to_owned())
        })?;
        Ok(devices
            .into_iter()
            .filter_map(|device| serde_json::from_value::<Resource>(device).ok())
            .find(|device| {
                device.client_identifier.as_deref() == Some(machine_id)
                    && device
                        .provides
                        .as_deref()
                        .is_some_and(|provides| provides.contains("server"))
            })
            .and_then(|device| non_empty(device.access_token.as_deref())))
    }

    /// Music library sections (`type == "artist"`).
    pub async fn music_libraries(&self) -> Result<Vec<(String, String)>, AdapterError> {
        self.require_configured()?;
        let container = self.request("/library/sections", &[]).await?;
        Ok(container
            .directory
            .into_iter()
            .filter(|section| section.kind.as_deref() == Some("artist"))
            .filter_map(|section| Some((section.key?, section.title.unwrap_or_default())))
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
            Some(year) => (year.to_string(), String::new()),
            None => (String::new(), browse.decade.clone()),
        };
        let filters = AlbumFilters {
            genre: &browse.genre,
            decade: &decade,
            exact_year: &exact_year,
        };
        if let [section] = sections.as_slice() {
            let container = self
                .section_albums(section, browse.limit, browse.offset, &sort, filters)
                .await?;
            let total = container
                .total_size
                .unwrap_or(container.metadata.len() as i64);
            let items = album_views(container.metadata.iter().filter(|row| known_title(row)))?;
            return Ok(RemotePage { items, total });
        }
        let fetch = browse.offset + browse.limit;
        let mut merged: Vec<Metadata> = Vec::new();
        let mut seen = HashSet::new();
        let mut total: i64 = 0;
        for section in &sections {
            let container = self
                .section_albums(section, fetch, 0, &sort, filters)
                .await?;
            total += container
                .total_size
                .unwrap_or(container.metadata.len() as i64);
            for album in container.metadata {
                if seen.insert(album.rating_key().unwrap_or_default().to_owned()) {
                    merged.push(album);
                }
            }
        }
        sort_albums(&mut merged, &sort);
        if let Some(year) = browse.year {
            merged.retain(|album| album.year == Some(i64::from(year)));
            total = merged.len() as i64;
        } else if let Some((start, end)) = parse_decade(&decade) {
            merged.retain(|album| album.year.is_some_and(|year| year >= start && year <= end));
            total = merged.len() as i64;
        }
        let page = merged
            .iter()
            .skip(browse.offset.max(0) as usize)
            .take(browse.limit.max(0) as usize)
            .filter(|row| known_title(row));
        Ok(RemotePage {
            items: album_views(page)?,
            total,
        })
    }

    /// One album by rating key. None is absence.
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(&format!("/library/metadata/{id}"), &[])
            .await?;
        container.metadata.first().map(album_view).transpose()
    }

    /// Album tracks via the metadata children endpoint.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let container = self
            .request(&format!("/library/metadata/{id}/children"), &[])
            .await?;
        track_views(container.metadata.iter())
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
        if let [section] = sections.as_slice() {
            return self.section_artists(section, browse).await;
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
        let items = merged
            .into_iter()
            .skip(browse.offset.max(0) as usize)
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
        container.metadata.first().map(artist_view).transpose()
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
        if let [section] = sections.as_slice() {
            return self.section_tracks(section, browse, &sort).await;
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
        let items = merged
            .into_iter()
            .skip(browse.offset.max(0) as usize)
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
        let limit = limit.to_string();
        for section in targets {
            let mut pairs = params(&[("query", query), ("limit", &limit)]);
            if let Some(section) = &section {
                pairs.push(("sectionId".to_owned(), section.clone()));
            }
            let container = self.request("/hubs/search", &pairs).await?;
            for hub in &container.hub {
                let hub_type = hub.kind.as_deref().unwrap_or("");
                for item in &hub.metadata {
                    let key = format!("{hub_type}:{}", item.rating_key()?);
                    if !seen.insert(key) {
                        continue;
                    }
                    match hub_type {
                        "album" => results.albums.push(album_view(item)?),
                        "track" => results.tracks.push(track_view(item)?),
                        "artist" => results.artists.push(artist_view(item)?),
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
        let viewed = self
            .album_feed("recentlyViewed", limit, |album| album.last_viewed_at)
            .await?;
        if !viewed.is_empty() {
            return Ok(viewed);
        }
        self.recently_added(limit).await
    }

    /// Recently added albums via `recentlyAdded`, newest first.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.album_feed("recentlyAdded", limit, |album| album.added_at)
            .await
    }

    /// Plex exposes no favorites endpoint; the unified shape stays empty.
    pub async fn favorites(&self, _limit: i64) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        Ok(FavoritesView {
            artists: Vec::new(),
            albums: Vec::new(),
            tracks: Vec::new(),
        })
    }

    /// Genre labels unioned over the resolved sections.
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        self.taxonomy("genre").await
    }

    /// Mood labels unioned over the resolved sections.
    pub async fn moods(&self) -> Result<Vec<String>, AdapterError> {
        self.taxonomy("mood").await
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
        let container = self.audio_playlists().await?;
        container.metadata.iter().map(playlist_summary).collect()
    }

    /// One playlist with its tracks. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let Some(summary) = self
            .playlists()
            .await?
            .into_iter()
            .find(|playlist| playlist.id == id)
        else {
            return Ok(None);
        };
        let container = self.request(&format!("/playlists/{id}/items"), &[]).await?;
        Ok(Some(PlaylistDetail {
            playlist: summary,
            tracks: track_views(container.metadata.iter())?,
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

    /// Plex exposes no random/shuffle endpoint, so random-offset sampling
    /// would lie about pagination; explicit unsupported instead.
    pub async fn random(&self, _limit: i64, _genre: &str) -> Result<Vec<TrackView>, AdapterError> {
        Err(AdapterError::Unsupported(
            "Plex has no random-tracks endpoint".to_owned(),
        ))
    }

    /// Discovery shelves via `/hubs/sections/{section}?count=`, album-type
    /// hubs only (v2 `get_discovery_hubs` semantics: first configured
    /// section, `Metadata` rows as albums). Empty shelves when nothing
    /// resolves or the hubs call is declined.
    pub async fn discovery(&self, count: i64) -> Result<DiscoveryView, AdapterError> {
        self.require_configured()?;
        let empty = || DiscoveryView {
            source: SourceName::Plex,
            hubs: Vec::new(),
        };
        let sections = self.resolve_sections().await?;
        let Some(section) = sections.first() else {
            return Ok(empty());
        };
        let count = count.to_string();
        let container = match self
            .request(
                &format!("/hubs/sections/{section}"),
                &params(&[("count", &count)]),
            )
            .await
        {
            Ok(container) => container,
            Err(AdapterError::Api(detail)) => {
                tracing::debug!(%detail, "plex declined the discovery hubs");
                return Ok(empty());
            }
            Err(other) => return Err(other),
        };
        let mut hubs = Vec::new();
        for hub in &container.hub {
            if hub.kind.as_deref() != Some("album") {
                continue;
            }
            let albums = album_views(hub.metadata.iter())?;
            if albums.is_empty() {
                continue;
            }
            hubs.push(DiscoveryHubView {
                title: hub.title.clone().unwrap_or_default(),
                hub_type: "album".to_owned(),
                albums,
            });
        }
        Ok(DiscoveryView {
            source: SourceName::Plex,
            hubs,
        })
    }

    /// Audio-only sessions from `/status/sessions`.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let container = self.request("/status/sessions", &[]).await?;
        let mut sessions = Vec::new();
        for track in &container.metadata {
            if track.kind.as_deref() != Some("track") {
                continue;
            }
            let session_id = track
                .session
                .as_ref()
                .and_then(|session| session.id.clone())
                .filter(|id| !id.is_empty())
                .ok_or_else(|| {
                    AdapterError::Api("Plex returned a session without an id".to_owned())
                })?;
            let player = track.player.clone().unwrap_or_default();
            let art = track
                .parent_rating_key
                .clone()
                .filter(|key| !key.is_empty())
                .or_else(|| track.rating_key.clone())
                .unwrap_or_default();
            sessions.push(SessionView {
                source: SourceName::Plex,
                session_id,
                user_name: track
                    .user
                    .as_ref()
                    .and_then(|user| user.title.clone())
                    .unwrap_or_default(),
                device_name: player.title.unwrap_or_default(),
                track_title: track.title.clone().unwrap_or_default(),
                artist_name: track.grandparent_title.clone().unwrap_or_default(),
                album_name: track.parent_title.clone().unwrap_or_default(),
                progress_ms: track.view_offset.unwrap_or(0),
                duration_ms: track.duration.unwrap_or(0),
                is_paused: player.state.as_deref() == Some("paused"),
                image_url: (!art.is_empty()).then(|| format!("/api/v3/remotes/plex/images/{art}")),
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
        let (limit, offset) = (limit.to_string(), offset.to_string());
        let container = self
            .request(
                "/status/sessions/history/all",
                &params(&[
                    ("X-Plex-Container-Start", &offset),
                    ("X-Plex-Container-Size", &limit),
                    ("sort", "viewedAt:desc"),
                ]),
            )
            .await?;
        let items: Vec<HistoryEntry> = container
            .metadata
            .iter()
            .filter(|item| item.kind.as_deref() == Some("track"))
            .map(|item| HistoryEntry {
                // A history row outlives a deleted track, so its key may be
                // gone; the row still counts.
                id: item.rating_key.clone().unwrap_or_default(),
                track_title: item.title.clone().unwrap_or_default(),
                artist_name: item.grandparent_title.clone().unwrap_or_default(),
                album_name: item.parent_title.clone().unwrap_or_default(),
                viewed_at: item.viewed_at.unwrap_or(0),
                duration_ms: item.duration.unwrap_or(0),
            })
            .collect();
        let total = container.total_size.unwrap_or(items.len() as i64);
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
        let size = size.to_string();
        self.get_bytes(
            &format!("/library/metadata/{id}/thumb"),
            &params(&[("width", &size), ("height", &size)]),
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
        let key = format!("/library/metadata/{rating_key}");
        self.request(
            "/:/timeline",
            &params(&[("ratingKey", rating_key), ("state", state), ("key", &key)]),
        )
        .await?;
        Ok(())
    }

    /// Scrobble via `/:/scrobble` (v2 `scrobble`).
    pub async fn scrobble(&self, rating_key: &str) -> Result<(), AdapterError> {
        self.require_configured()?;
        self.request(
            "/:/scrobble",
            &params(&[
                ("key", rating_key),
                ("identifier", "com.plexapp.plugins.library"),
            ]),
        )
        .await?;
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
        let container = self.audio_playlists().await?;
        let composite = container
            .metadata
            .iter()
            .find(|playlist| playlist.rating_key.as_deref() == Some(id))
            .and_then(|playlist| playlist.composite.clone())
            .filter(|path| !path.is_empty())
            .unwrap_or_else(|| format!("/playlists/{id}/composite"));
        let size = size.to_string();
        self.get_bytes(
            &composite,
            &params(&[("width", &size), ("height", &size)]),
            true,
            "image/jpeg",
        )
        .await
    }

    /// Resolve an MBID by searching for it per section and comparing
    /// `mbid://` guids.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        let results = self.search(mbid, 50).await?;
        let Some(album) = results.albums.iter().find(|album| {
            album.release_mbid.as_deref() == Some(mbid)
                || album.release_group_mbid.as_deref() == Some(mbid)
        }) else {
            return Ok(MatchView {
                source: SourceName::Plex,
                found: false,
                remote_album_id: None,
                tracks: Vec::new(),
            });
        };
        let tracks = self.album_tracks(&album.id).await?;
        Ok(MatchView {
            source: SourceName::Plex,
            found: true,
            remote_album_id: Some(album.id.clone()),
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
        let item_type = item_type.to_string();
        let container = self
            .request(
                &format!("/library/sections/{section}/all"),
                &params(&[
                    ("type", &item_type),
                    ("X-Plex-Container-Start", "0"),
                    ("X-Plex-Container-Size", "0"),
                ]),
            )
            .await?;
        Ok(container.total_size.unwrap_or(0))
    }

    /// One album feed (`recentlyViewed`, `recentlyAdded`) merged over the
    /// sections, newest first by `stamp`, named albums only.
    async fn album_feed(
        &self,
        feed: &str,
        limit: i64,
        stamp: fn(&Metadata) -> Option<i64>,
    ) -> Result<Vec<AlbumView>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let size = limit.to_string();
        let mut rows: Vec<Metadata> = Vec::new();
        for section in &sections {
            let container = self
                .request(
                    &format!("/library/sections/{section}/{feed}"),
                    &params(&[("type", "9"), ("X-Plex-Container-Size", &size)]),
                )
                .await?;
            rows.extend(container.metadata);
        }
        rows.sort_by_key(|row| std::cmp::Reverse(stamp(row).unwrap_or(0)));
        album_views(
            rows.iter()
                .take(limit.max(0) as usize)
                .filter(|row| known_title(row)),
        )
    }

    /// Labels from one section taxonomy (`genre`, `mood`), unioned and
    /// sorted.
    async fn taxonomy(&self, kind: &str) -> Result<Vec<String>, AdapterError> {
        let sections = self.resolve_sections().await?;
        let mut labels = HashSet::new();
        for section in &sections {
            let container = self
                .request(&format!("/library/sections/{section}/{kind}"), &[])
                .await?;
            for directory in container.directory {
                if let Some(title) = directory.title.filter(|title| !title.is_empty()) {
                    labels.insert(title);
                }
            }
        }
        let mut sorted: Vec<String> = labels.into_iter().collect();
        sorted.sort();
        Ok(sorted)
    }

    async fn audio_playlists(&self) -> Result<Container, AdapterError> {
        self.request("/playlists", &params(&[("playlistType", "audio")]))
            .await
    }

    async fn section_albums(
        &self,
        section: &str,
        size: i64,
        offset: i64,
        sort: &str,
        filters: AlbumFilters<'_>,
    ) -> Result<Container, AdapterError> {
        let (offset, size) = (offset.to_string(), size.to_string());
        let mut pairs = params(&[
            ("type", "9"),
            ("X-Plex-Container-Start", &offset),
            ("X-Plex-Container-Size", &size),
            ("sort", sort),
        ]);
        if !filters.genre.is_empty() {
            pairs.push(("genre".to_owned(), filters.genre.to_owned()));
        }
        if !filters.exact_year.is_empty() {
            pairs.push(("year".to_owned(), filters.exact_year.to_owned()));
        } else if let Some(years) = decade_years(filters.decade) {
            pairs.push(("year".to_owned(), years));
        }
        self.request(&format!("/library/sections/{section}/all"), &pairs)
            .await
    }

    async fn section_artists(
        &self,
        section: &str,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        let (offset, limit) = (browse.offset.to_string(), browse.limit.to_string());
        let mut pairs = params(&[
            ("type", "8"),
            ("X-Plex-Container-Start", &offset),
            ("X-Plex-Container-Size", &limit),
        ]);
        if !browse.search.is_empty() {
            pairs.push(("title".to_owned(), browse.search.clone()));
        }
        let container = self
            .request(&format!("/library/sections/{section}/all"), &pairs)
            .await?;
        let total = container
            .total_size
            .unwrap_or(container.metadata.len() as i64);
        let mut items = container
            .metadata
            .iter()
            .map(artist_view)
            .collect::<Result<Vec<_>, _>>()?;
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
        let (offset, limit) = (browse.offset.to_string(), browse.limit.to_string());
        let mut pairs = params(&[
            ("type", "10"),
            ("sort", sort),
            ("X-Plex-Container-Start", &offset),
            ("X-Plex-Container-Size", &limit),
        ]);
        if !browse.search.is_empty() {
            pairs.push(("title".to_owned(), browse.search.clone()));
        }
        if !browse.genre.is_empty() {
            pairs.push(("genre".to_owned(), browse.genre.clone()));
        }
        let container = self
            .request(&format!("/library/sections/{section}/all"), &pairs)
            .await?;
        let total = container
            .total_size
            .unwrap_or(container.metadata.len() as i64);
        Ok(RemotePage {
            items: track_views(container.metadata.iter())?,
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
        pairs: &[(String, String)],
    ) -> Result<Container, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let url = format!("{}{endpoint}", self.base_url());
        let response = self
            .client
            .get(&url)
            .query(pairs)
            .headers(self.headers())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
        self.maybe_upgrade_base(&url, response.url());
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            return Err(AdapterError::Auth);
        }
        if !status.is_success() {
            return Err(AdapterError::Api(format!(
                "GET {endpoint} failed ({status})"
            )));
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|error| {
            AdapterError::Api(format!(
                "Plex returned an unreadable {endpoint} payload: {error}"
            ))
        })?;
        Ok(envelope.container)
    }

    /// GET raw bytes (thumbs, composites, audio). A missing content type
    /// falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        pairs: &[(String, String)],
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
            .query(pairs)
            .headers(headers)
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
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
            .map_err(|cause| AdapterError::Transport(trim_cause(cause)))?;
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
}

fn album_views<'a>(
    rows: impl Iterator<Item = &'a Metadata>,
) -> Result<Vec<AlbumView>, AdapterError> {
    rows.map(album_view).collect()
}

fn track_views<'a>(
    rows: impl Iterator<Item = &'a Metadata>,
) -> Result<Vec<TrackView>, AdapterError> {
    rows.map(track_view).collect()
}

fn album_view(album: &Metadata) -> Result<AlbumView, AdapterError> {
    let id = album.rating_key()?.to_owned();
    Ok(AlbumView {
        source: SourceName::Plex,
        image_url: album
            .has_thumb()
            .then(|| format!("/api/v3/remotes/plex/images/{id}")),
        title: album.title.clone().unwrap_or_else(|| "Unknown".to_owned()),
        artist_name: album.parent_title.clone().unwrap_or_default(),
        artist_id: non_empty(album.parent_rating_key.as_deref()),
        year: album.year.and_then(|year| i32::try_from(year).ok()),
        genre: album
            .genre
            .iter()
            .filter_map(|genre| genre.tag.clone())
            .find(|tag| !tag.is_empty()),
        track_count: album.leaf_count.and_then(|count| i32::try_from(count).ok()),
        release_mbid: album.mbid(),
        release_group_mbid: None,
        artist_mbid: None,
        id,
    })
}

fn artist_view(artist: &Metadata) -> Result<ArtistView, AdapterError> {
    let id = artist.rating_key()?.to_owned();
    Ok(ArtistView {
        source: SourceName::Plex,
        image_url: artist
            .has_thumb()
            .then(|| format!("/api/v3/remotes/plex/images/{id}")),
        name: artist.title.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_count: None,
        artist_mbid: artist.mbid(),
        id,
    })
}

fn track_view(track: &Metadata) -> Result<TrackView, AdapterError> {
    let id = track.rating_key()?.to_owned();
    let parent = non_empty(track.parent_rating_key.as_deref());
    Ok(TrackView {
        source: SourceName::Plex,
        image_url: parent
            .as_ref()
            .map(|parent| format!("/api/v3/remotes/plex/images/{parent}")),
        title: track.title.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_name: track.parent_title.clone().unwrap_or_default(),
        album_id: parent,
        artist_name: track.grandparent_title.clone().unwrap_or_default(),
        artist_id: None,
        track_number: track.index.and_then(|number| i32::try_from(number).ok()),
        disc_number: track
            .parent_index
            .and_then(|number| i32::try_from(number).ok()),
        duration_secs: track.duration.map(|ms| ms / 1000),
        year: track.year.and_then(|year| i32::try_from(year).ok()),
        recording_mbid: track.mbid(),
        part_key: track.part_key(),
        id,
    })
}

fn playlist_summary(playlist: &Metadata) -> Result<PlaylistSummary, AdapterError> {
    let id = playlist.rating_key()?.to_owned();
    Ok(PlaylistSummary {
        source: SourceName::Plex,
        image_url: Some(format!("/api/v3/remotes/plex/covers/playlists/{id}")),
        name: playlist.title.clone().unwrap_or_default(),
        track_count: playlist.leaf_count.unwrap_or(0),
        duration_secs: playlist.duration.map(|ms| ms / 1000).unwrap_or(0),
        id,
    })
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
    let (start, end) = parse_decade(decade)?;
    Some(
        (start..=end)
            .map(|year| year.to_string())
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Client-side album sort for merged multi-section pages.
fn sort_albums(albums: &mut [Metadata], sort: &str) {
    let mut parts = sort.splitn(2, ':');
    let field = parts.next().unwrap_or("titleSort");
    let reverse = parts.next().unwrap_or("asc") == "desc";
    match field {
        "addedAt" => albums.sort_by_key(|album| album.added_at.unwrap_or(0)),
        "year" => albums.sort_by_key(|album| album.year.unwrap_or(0)),
        "viewCount" => albums.sort_by_key(|album| album.view_count.unwrap_or(0)),
        "lastViewedAt" => albums.sort_by_key(|album| album.last_viewed_at.unwrap_or(0)),
        "userRating" => albums.sort_by(|left, right| {
            left.user_rating
                .unwrap_or(0.0)
                .partial_cmp(&right.user_rating.unwrap_or(0.0))
                .unwrap_or(std::cmp::Ordering::Equal)
        }),
        _ => albums.sort_by_key(|album| album.title.clone().unwrap_or_default().to_lowercase()),
    }
    if reverse {
        albums.reverse();
    }
}

fn known_title(album: &Metadata) -> bool {
    album
        .title
        .as_deref()
        .is_some_and(|title| !title.is_empty() && title != "Unknown")
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

/// Transport cause for logs and errors. The URL is stripped first: it can
/// carry credentials (Subsonic `u`/`t`/`s`, Plex and Jellyfin tokens).
fn trim_cause(cause: reqwest::Error) -> String {
    cause.without_url().to_string().chars().take(200).collect()
}

/// The production [`PlexServerTokens`]: probe the admin's server with the
/// account token, then read the server's token from plex.tv.
#[derive(Debug, Clone)]
pub struct PlexTokenProbe {
    http: reqwest::Client,
    plex_tv: String,
}

impl PlexTokenProbe {
    /// A probe against the real plex.tv.
    pub fn new(http: reqwest::Client) -> Self {
        Self::with_plex_tv(http, PLEX_TV_BASE.to_owned())
    }

    /// A probe against another plex.tv root (tests).
    pub fn with_plex_tv(http: reqwest::Client, plex_tv: String) -> Self {
        Self { http, plex_tv }
    }
}

impl PlexServerTokens for PlexTokenProbe {
    fn server_token<'a>(
        &'a self,
        server: &'a ServerSettings,
        auth_token: &'a str,
    ) -> super::adapter::BoxFuture<'a, Result<Option<String>, AdapterError>> {
        Box::pin(async move {
            let probe = PlexAdapter::new(
                self.http.clone(),
                server.base_url.clone(),
                auth_token.to_owned(),
                server.client_id.clone(),
                Vec::new(),
            );
            let Some(machine_id) = probe.machine_identifier().await? else {
                return Ok(None);
            };
            probe.server_access_token(&self.plex_tv, &machine_id).await
        })
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
