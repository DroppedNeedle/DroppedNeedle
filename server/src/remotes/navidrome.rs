//! Navidrome source adapter (Subsonic/OpenSubsonic API).
//!
//! Live-version-cited quirks, kept with their citations:
//!
//! - Catalog scoping rides repeated `musicFolderId` query params; "all"
//!   omits the param and an empty selection fails closed without any
//!   request. Verified against the Navidrome 0.62.0 live probe (2026-07-13):
//!   the probed single-folder server accepted the same `musicFolderId`
//!   twice and ignored an unknown folder for catalog endpoints. Two-folder
//!   behavior was not observable and the mock does not model it.
//! - Auth is the Subsonic token scheme: `t=md5(password+salt)` with a fresh
//!   3-byte hex salt per request, `v=1.16.1`, `c=droppedneedle`, `f=json`.
//!   HTTP 401/403 and Subsonic error codes 40/41 both mean auth failure.
//! - `getAlbumList2` answers no total; page totals come from stats when the
//!   page is full (`offset + len` otherwise), and stats sum `songCount`
//!   over a 500-row album scan. Track browse is `search3` with an empty
//!   query spelled `""`.
//! - Album sort names map `name`/`date_added`/`year` to Subsonic list types
//!   with client-side reversal for descending name and ascending recency.

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

/// Subsonic API version pinned by the v2 repository.
const SUBSONIC_VERSION: &str = "1.16.1";

/// Client name sent on every Navidrome call.
const CLIENT_NAME: &str = "droppedneedle";

/// Stats album scan batch size, matching the v2 service.
const STATS_BATCH: i64 = 500;

/// Navidrome browse client. Built per request from the caller's stored
/// connection plus their resolved folder scope; holds no cache.
pub struct NavidromeAdapter {
    client: reqwest::Client,
    base_url: String,
    username: String,
    password: String,
    folder_ids: Option<Vec<String>>,
}

impl std::fmt::Debug for NavidromeAdapter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NavidromeAdapter")
            .field("base_url", &self.base_url)
            .field("username", &self.username)
            .field("folder_ids", &self.folder_ids)
            .finish_non_exhaustive()
    }
}

impl NavidromeAdapter {
    /// Build a client for one server. Empty URL, username, or password reads
    /// as unconfigured. The folder scope defaults to all folders.
    pub fn new(
        client: reqwest::Client,
        base_url: String,
        username: String,
        password: String,
    ) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            username,
            password,
            folder_ids: None,
        }
    }

    /// Pin the catalog scope: `None` is all folders, `Some` lists the
    /// selected ids. An empty selection fails every scoped call closed.
    pub fn with_folders(mut self, folder_ids: Option<Vec<String>>) -> Self {
        self.folder_ids = folder_ids;
        self
    }

    /// True when URL, username, and password are present.
    pub fn is_configured(&self) -> bool {
        !self.base_url.is_empty() && !self.username.is_empty() && !self.password.is_empty()
    }

    /// Connectivity probe against `ping`. Returns the API version label.
    pub async fn validate_connection(&self) -> Result<String, AdapterError> {
        let value = self.request("/rest/ping", &[]).await?;
        let version = str_field(&value, "version").unwrap_or("unknown");
        Ok(format!("Connected to Navidrome (API v{version})"))
    }

    /// Music folders exposed by the server. Unscoped by design: the folder
    /// preference UI lists everything before the user picks.
    pub async fn music_folders(&self) -> Result<Vec<(String, String)>, AdapterError> {
        self.require_configured()?;
        let value = self.unscoped_request("/rest/getMusicFolders", &[]).await?;
        let raw = value
            .get("musicFolders")
            .and_then(|folders| folders.get("musicFolder"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw
            .iter()
            .map(|folder| {
                (
                    str_field(folder, "id").unwrap_or("").to_owned(),
                    str_field(folder, "name").unwrap_or("").to_owned(),
                )
            })
            .collect())
    }

    /// Server identity for folder-preference staleness: sha256 of the
    /// casefolded base URL, matching the v2 repository.
    pub fn server_identity(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.base_url.to_lowercase().as_bytes());
        hex_bytes(&hasher.finalize())
    }

    /// Hub highlights. Sections fail open to empty; an all-failed hub is an
    /// upstream error.
    pub async fn hub(&self) -> Result<HubView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_hub());
        }
        let preview_browse = AlbumBrowse {
            limit: 12,
            ..AlbumBrowse::default()
        };
        let (stats, recent, favorites, preview, genres) = tokio::join!(
            self.stats(),
            self.recent(20),
            self.favorites(),
            self.albums(&preview_browse),
            self.genres(),
        );
        let failures = [
            stats.is_err(),
            recent.is_err(),
            favorites.is_err(),
            preview.is_err(),
            genres.is_err(),
        ]
        .into_iter()
        .filter(|failed| *failed)
        .count();
        if failures == 5 {
            return Err(AdapterError::Api(
                "All Navidrome hub data requests failed".to_owned(),
            ));
        }
        let favorites = favorites.unwrap_or_else(|_| empty_favorites());
        Ok(HubView {
            source: SourceName::Navidrome,
            stats: stats.ok(),
            recently_played: recent.unwrap_or_default(),
            recently_added: Vec::new(),
            favorites: favorites.albums,
            favorite_artists: favorites.artists,
            most_played_artists: Vec::new(),
            all_albums_preview: preview.map(|page| page.items).unwrap_or_default(),
            genres: genres.unwrap_or_default(),
        })
    }

    /// Library totals: artist count plus a 500-row album scan summing
    /// `songCount` for the track total.
    pub async fn stats(&self) -> Result<StatsView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(zero_stats());
        }
        let artists = self.artist_list().await?;
        let mut total_albums: i64 = 0;
        let mut total_tracks: i64 = 0;
        let mut offset: i64 = 0;
        loop {
            let batch = self
                .album_list("alphabeticalByName", STATS_BATCH, offset, None, None, None)
                .await?;
            if batch.is_empty() {
                break;
            }
            total_albums += batch.len() as i64;
            total_tracks += batch
                .iter()
                .map(|album| album.get("songCount").and_then(value_to_i64).unwrap_or(0))
                .sum::<i64>();
            if (batch.len() as i64) < STATS_BATCH {
                break;
            }
            offset += STATS_BATCH;
        }
        Ok(StatsView {
            total_albums,
            total_artists: artists.len() as i64,
            total_tracks,
        })
    }

    /// One page of albums. Totals follow the v2 heuristic: the stats total
    /// when the page is full, `offset + len` otherwise.
    pub async fn albums(
        &self,
        browse: &AlbumBrowse,
    ) -> Result<RemotePage<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_page());
        }
        let list_type = if !browse.genre.is_empty() {
            "byGenre"
        } else {
            match browse.sort_by.as_str() {
                "date_added" => "newest",
                "year" => "byYear",
                _ => "alphabeticalByName",
            }
        };
        let (from_year, to_year) = if list_type == "byYear" {
            if browse.descending {
                (Some(9999), Some(0))
            } else {
                (Some(0), Some(9999))
            }
        } else {
            (None, None)
        };
        let genre = if browse.genre.is_empty() {
            None
        } else {
            Some(browse.genre.as_str())
        };
        let mut raw = self
            .album_list(
                list_type,
                browse.limit,
                browse.offset,
                genre,
                from_year,
                to_year,
            )
            .await?;
        if needs_reverse(&browse.sort_by, browse.descending, genre.is_some()) {
            raw.reverse();
        }
        let items: Vec<AlbumView> = raw
            .iter()
            .filter(|album| known_name(album_name(album)))
            .map(|album| self.album_view(album))
            .collect();
        let total = if items.len() as i64 >= browse.limit {
            self.stats()
                .await
                .map(|stats| stats.total_albums)
                .unwrap_or_else(|_| browse.offset + items.len() as i64 + 1)
        } else {
            browse.offset + items.len() as i64
        };
        Ok(RemotePage { items, total })
    }

    /// One album by id, with tracks. Out-of-scope ids read as absent, and
    /// an empty scope reads every detail as absent.
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(None);
        }
        if self.folder_ids.is_some() && !self.album_in_scope(id).await? {
            return Ok(None);
        }
        let value = self
            .unscoped_request("/rest/getAlbum", &[("id".to_owned(), id.to_owned())])
            .await?;
        let album = value.get("album").cloned().unwrap_or(Value::Null);
        if album.is_null() || str_field(&album, "id").unwrap_or("").is_empty() {
            return Ok(None);
        }
        Ok(Some(self.album_view(&album)))
    }

    /// Album tracks in track order.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let value = self
            .unscoped_request("/rest/getAlbum", &[("id".to_owned(), id.to_owned())])
            .await?;
        let songs = value
            .get("album")
            .and_then(|album| album.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs.iter().map(|song| self.track_view(song)).collect())
    }

    /// One page of artists. Navidrome answers the whole artist list, so
    /// paging slices client-side after an optional name filter.
    pub async fn artists(
        &self,
        browse: &ArtistBrowse,
    ) -> Result<RemotePage<ArtistView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_page());
        }
        let mut artists = self.artist_list().await?;
        if !browse.search.is_empty() {
            let needle = browse.search.to_lowercase();
            artists.retain(|artist| artist.name.to_lowercase().contains(&needle));
        }
        artists.sort_by(|left, right| left.name.cmp(&right.name));
        if browse.descending {
            artists.reverse();
        }
        let total = artists.len() as i64;
        let start = browse.offset.max(0) as usize;
        let end = start
            .saturating_add(browse.limit.max(0) as usize)
            .min(artists.len());
        let items = if start >= artists.len() {
            Vec::new()
        } else {
            artists[start..end].to_vec()
        };
        Ok(RemotePage { items, total })
    }

    /// Alphabetic artist index straight from `getArtists`.
    pub async fn artist_index(&self) -> Result<Vec<ArtistIndexEntry>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let value = self.request("/rest/getArtists", &[]).await?;
        let buckets = value
            .get("artists")
            .and_then(|artists| artists.get("index"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(buckets
            .iter()
            .map(|bucket| {
                let artists = bucket
                    .get("artist")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                ArtistIndexEntry {
                    name: str_field(bucket, "name").unwrap_or("").to_owned(),
                    artists: artists
                        .iter()
                        .map(|artist| self.artist_view(artist))
                        .collect(),
                }
            })
            .collect())
    }

    /// One artist by id. Out-of-scope ids read as absent.
    pub async fn artist_detail(&self, id: &str) -> Result<Option<ArtistView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(None);
        }
        if self.folder_ids.is_some() {
            let artists = self.artist_list().await?;
            if !artists.iter().any(|artist| artist.id == id) {
                return Ok(None);
            }
        }
        let value = self
            .unscoped_request("/rest/getArtist", &[("id".to_owned(), id.to_owned())])
            .await?;
        let artist = value.get("artist").cloned().unwrap_or(Value::Null);
        if artist.is_null() || str_field(&artist, "id").unwrap_or("").is_empty() {
            return Ok(None);
        }
        Ok(Some(self.artist_view(&artist)))
    }

    /// Track browse via `search3`. An empty query is spelled `""`, and the
    /// total follows the same stats-or-heuristic rule as albums.
    pub async fn tracks(
        &self,
        browse: &TrackBrowse,
    ) -> Result<RemotePage<TrackView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_page());
        }
        let query = if browse.search.is_empty() {
            "\"\"".to_owned()
        } else {
            browse.search.clone()
        };
        let params = vec![
            ("query".to_owned(), query),
            ("artistCount".to_owned(), "0".to_owned()),
            ("albumCount".to_owned(), "0".to_owned()),
            ("songCount".to_owned(), browse.limit.to_string()),
            ("songOffset".to_owned(), browse.offset.to_string()),
        ];
        let value = self.request("/rest/search3", &params).await?;
        let songs = value
            .get("searchResult3")
            .and_then(|result| result.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let items: Vec<TrackView> = songs.iter().map(|song| self.track_view(song)).collect();
        let total = if items.len() as i64 >= browse.limit {
            self.stats()
                .await
                .map(|stats| stats.total_tracks)
                .unwrap_or_else(|_| browse.offset + items.len() as i64 + 1)
        } else {
            browse.offset + items.len() as i64
        };
        Ok(RemotePage { items, total })
    }

    /// Unified search across the three `search3` buckets.
    pub async fn search(&self, query: &str, limit: i64) -> Result<SearchResults, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_search());
        }
        let params = vec![
            ("query".to_owned(), query.to_owned()),
            ("artistCount".to_owned(), limit.to_string()),
            ("albumCount".to_owned(), limit.to_string()),
            ("songCount".to_owned(), limit.to_string()),
        ];
        let value = self.request("/rest/search3", &params).await?;
        let result = value.get("searchResult3").cloned().unwrap_or(Value::Null);
        Ok(SearchResults {
            artists: bucket(&result, "artist", |item| self.artist_view(item)),
            albums: bucket(&result, "album", |item| self.album_view(item)),
            tracks: bucket(&result, "song", |item| self.track_view(item)),
        })
    }

    /// Recently played albums via `getAlbumList2 type=recent`.
    pub async fn recent(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let raw = self
            .album_list("recent", limit, 0, None, None, None)
            .await?;
        Ok(raw
            .iter()
            .filter(|album| known_name(album_name(album)))
            .map(|album| self.album_view(album))
            .collect())
    }

    /// Recently added albums via `getAlbumList2 type=newest`.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let raw = self
            .album_list("newest", limit, 0, None, None, None)
            .await?;
        Ok(raw
            .iter()
            .filter(|album| known_name(album_name(album)))
            .map(|album| self.album_view(album))
            .collect())
    }

    /// Starred artists, albums, and songs via `getStarred2`.
    pub async fn favorites(&self) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_favorites());
        }
        let value = self.request("/rest/getStarred2", &[]).await?;
        let starred = value.get("starred2").cloned().unwrap_or(Value::Null);
        Ok(FavoritesView {
            artists: bucket(&starred, "artist", |item| self.artist_view(item)),
            albums: bucket(&starred, "album", |item| self.album_view(item))
                .into_iter()
                .filter(|album| known_name(&album.title))
                .collect(),
            tracks: bucket(&starred, "song", |item| self.track_view(item)),
        })
    }

    /// Genre labels via `getGenres` (unscoped in v2).
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        self.require_configured()?;
        let value = self.unscoped_request("/rest/getGenres", &[]).await?;
        let raw = value
            .get("genres")
            .and_then(|genres| genres.get("genre"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw
            .iter()
            .filter_map(|genre| {
                str_field(genre, "value")
                    .or_else(|| str_field(genre, "name"))
                    .map(str::to_owned)
            })
            .filter(|name| !name.is_empty())
            .collect())
    }

    /// Songs carrying one genre via `getSongsByGenre`.
    pub async fn genre_songs(
        &self,
        genre: &str,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let params = vec![
            ("genre".to_owned(), genre.to_owned()),
            ("count".to_owned(), limit.to_string()),
            ("offset".to_owned(), offset.to_string()),
        ];
        let value = self.request("/rest/getSongsByGenre", &params).await?;
        let songs = value
            .get("songsByGenre")
            .and_then(|block| block.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs.iter().map(|song| self.track_view(song)).collect())
    }

    /// Playlists via `getPlaylists`.
    pub async fn playlists(&self) -> Result<Vec<PlaylistSummary>, AdapterError> {
        self.require_configured()?;
        let value = self.unscoped_request("/rest/getPlaylists", &[]).await?;
        let raw = value
            .get("playlists")
            .and_then(|block| block.get("playlist"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(raw.iter().map(|item| self.playlist_summary(item)).collect())
    }

    /// One playlist with entries via `getPlaylist`. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let value = self
            .unscoped_request("/rest/getPlaylist", &[("id".to_owned(), id.to_owned())])
            .await?;
        let playlist = value.get("playlist").cloned().unwrap_or(Value::Null);
        if playlist.is_null() || str_field(&playlist, "id").unwrap_or("").is_empty() {
            return Ok(None);
        }
        let entries = playlist
            .get("entry")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(Some(PlaylistDetail {
            playlist: self.playlist_summary(&playlist),
            tracks: entries.iter().map(|song| self.track_view(song)).collect(),
        }))
    }

    /// Artist info passthrough via `getArtistInfo2`. Empty when the server
    /// declines (Last.fm often unconfigured); unreachable still errors.
    pub async fn artist_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        self.require_configured()?;
        let value = match self
            .unscoped_request("/rest/getArtistInfo2", &[("id".to_owned(), id.to_owned())])
            .await
        {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => Value::Null,
            Err(other) => return Err(other),
        };
        let info = value.get("artistInfo2").cloned().unwrap_or(Value::Null);
        let similar = info
            .get("similarArtist")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(InfoView {
            source: SourceName::Navidrome,
            id: id.to_owned(),
            biography: str_field(&info, "biography").unwrap_or("").to_owned(),
            musicbrainz_id: str_field(&info, "musicBrainzId").unwrap_or("").to_owned(),
            image_url: [
                str_field(&info, "largeImageUrl").unwrap_or(""),
                str_field(&info, "mediumImageUrl").unwrap_or(""),
                str_field(&info, "smallImageUrl").unwrap_or(""),
            ]
            .into_iter()
            .find(|url| !url.is_empty())
            .unwrap_or("")
            .to_owned(),
            similar_artists: similar
                .iter()
                .map(|artist| self.artist_view(artist))
                .collect(),
        })
    }

    /// Album info passthrough via `getAlbumInfo2`, with the same
    /// decline-to-empty rule.
    pub async fn album_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        self.require_configured()?;
        let value = match self
            .unscoped_request("/rest/getAlbumInfo2", &[("id".to_owned(), id.to_owned())])
            .await
        {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => Value::Null,
            Err(other) => return Err(other),
        };
        let info = value.get("albumInfo").cloned().unwrap_or(Value::Null);
        Ok(InfoView {
            source: SourceName::Navidrome,
            id: id.to_owned(),
            biography: str_field(&info, "notes").unwrap_or("").to_owned(),
            musicbrainz_id: str_field(&info, "musicBrainzId").unwrap_or("").to_owned(),
            image_url: [
                str_field(&info, "largeImageUrl").unwrap_or(""),
                str_field(&info, "mediumImageUrl").unwrap_or(""),
                str_field(&info, "smallImageUrl").unwrap_or(""),
            ]
            .into_iter()
            .find(|url| !url.is_empty())
            .unwrap_or("")
            .to_owned(),
            similar_artists: Vec::new(),
        })
    }

    /// Lyrics: structured `getLyricsBySongId` first, classic
    /// artist/title `getLyrics` when the caller supplies both. None when
    /// the server has nothing. Declined calls read as empty, not errors.
    pub async fn lyrics(
        &self,
        id: &str,
        artist: Option<&str>,
        title: Option<&str>,
    ) -> Result<Option<LyricsView>, AdapterError> {
        self.require_configured()?;
        let structured = match self
            .unscoped_request(
                "/rest/getLyricsBySongId",
                &[("id".to_owned(), id.to_owned())],
            )
            .await
        {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => Value::Null,
            Err(other) => return Err(other),
        };
        let candidates = structured
            .get("lyricsList")
            .and_then(|list| list.get("structuredLyrics"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if let Some(best) = candidates.first() {
            let synced = best.get("synced").and_then(Value::as_bool).unwrap_or(false);
            let raw_lines = best
                .get("line")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let mut lines = Vec::new();
            for line in &raw_lines {
                lines.push(LyricLine {
                    text: str_field(line, "value").unwrap_or("").to_owned(),
                    start_ms: if synced {
                        line.get("start").and_then(value_to_i64)
                    } else {
                        None
                    },
                });
            }
            let has_text = lines.iter().any(|line| !line.text.trim().is_empty());
            let has_timing = lines.iter().any(|line| line.start_ms.is_some());
            if has_text || has_timing {
                let text = lines
                    .iter()
                    .map(|line| line.text.clone())
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(Some(LyricsView {
                    source: SourceName::Navidrome,
                    text,
                    is_synced: synced,
                    lines,
                }));
            }
        }
        let (Some(artist), Some(title)) = (artist, title) else {
            return Ok(None);
        };
        let classic = match self
            .unscoped_request(
                "/rest/getLyrics",
                &[
                    ("artist".to_owned(), artist.to_owned()),
                    ("title".to_owned(), title.to_owned()),
                ],
            )
            .await
        {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => return Ok(None),
            Err(other) => return Err(other),
        };
        let text = classic
            .get("lyrics")
            .and_then(|lyrics| str_field(lyrics, "value"))
            .unwrap_or("");
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(LyricsView {
            source: SourceName::Navidrome,
            text: text.to_owned(),
            is_synced: false,
            lines: Vec::new(),
        }))
    }

    /// Top songs for one artist via `getTopSongs`. Empty when declined.
    pub async fn top_songs(
        &self,
        artist: &str,
        limit: i64,
    ) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("artist".to_owned(), artist.to_owned()),
            ("count".to_owned(), limit.to_string()),
        ];
        let value = match self.unscoped_request("/rest/getTopSongs", &params).await {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => return Ok(Vec::new()),
            Err(other) => return Err(other),
        };
        let songs = value
            .get("topSongs")
            .and_then(|block| block.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs.iter().map(|song| self.track_view(song)).collect())
    }

    /// Random songs via `getRandomSongs`, folder-scoped like the v1
    /// route (`size` + optional `genre`). Empty when declined.
    pub async fn random(&self, limit: i64, genre: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let mut params = vec![("size".to_owned(), limit.to_string())];
        if !genre.is_empty() {
            params.push(("genre".to_owned(), genre.to_owned()));
        }
        let value = match self.request("/rest/getRandomSongs", &params).await {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => return Ok(Vec::new()),
            Err(other) => return Err(other),
        };
        let songs = value
            .get("randomSongs")
            .and_then(|block| block.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs.iter().map(|song| self.track_view(song)).collect())
    }

    /// Similar songs via `getSimilarSongs2`. Empty when declined.
    pub async fn similar(&self, id: &str, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("id".to_owned(), id.to_owned()),
            ("count".to_owned(), limit.to_string()),
        ];
        let value = match self
            .unscoped_request("/rest/getSimilarSongs2", &params)
            .await
        {
            Ok(value) => value,
            Err(AdapterError::Api(_)) => return Ok(Vec::new()),
            Err(other) => return Err(other),
        };
        let songs = value
            .get("similarSongs2")
            .and_then(|block| block.get("song"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(songs.iter().map(|song| self.track_view(song)).collect())
    }

    /// Now-playing entries via `getNowPlaying`, shaped as sessions.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let value = self.unscoped_request("/rest/getNowPlaying", &[]).await?;
        let entries = value
            .get("nowPlaying")
            .and_then(|block| block.get("entry"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(SessionsView {
            source: SourceName::Navidrome,
            sessions: entries
                .iter()
                .map(|entry| SessionView {
                    source: SourceName::Navidrome,
                    session_id: format!(
                        "{}:{}",
                        str_field(entry, "playerName").unwrap_or(""),
                        entry.get("playerId").and_then(value_to_i64).unwrap_or(0),
                    ),
                    user_name: str_field(entry, "username").unwrap_or("").to_owned(),
                    device_name: str_field(entry, "playerName").unwrap_or("").to_owned(),
                    track_title: str_field(entry, "title").unwrap_or("").to_owned(),
                    artist_name: str_field(entry, "artist").unwrap_or("").to_owned(),
                    album_name: str_field(entry, "album").unwrap_or("").to_owned(),
                    progress_ms: entry.get("minutesAgo").and_then(value_to_i64).unwrap_or(0)
                        * 60_000,
                    duration_ms: entry.get("duration").and_then(value_to_i64).unwrap_or(0) * 1000,
                    is_paused: false,
                })
                .collect(),
        })
    }

    /// Navidrome exposes no listening-history endpoint.
    pub async fn history(&self, _limit: i64, _offset: i64) -> Result<HistoryPage, AdapterError> {
        Err(AdapterError::Unsupported(
            "Navidrome has no listening-history endpoint".to_owned(),
        ))
    }

    /// Cover-art bytes for a cover-art id via `getCoverArt`.
    pub async fn image_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("id".to_owned(), id.to_owned()),
            ("size".to_owned(), size.to_string()),
        ];
        self.get_bytes("/rest/getCoverArt", &params, "image/jpeg")
            .await
    }

    /// Direct audio bytes for one song id via `/rest/stream` (v2
    /// `build_stream_url` target, fetched whole for the gateway seam).
    pub async fn audio_bytes(&self, id: &str) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        self.get_bytes(
            "/rest/stream",
            &[("id".to_owned(), id.to_owned())],
            "audio/mpeg",
        )
        .await
    }

    /// Now-playing report via `/rest/scrobble` with `submission=false`
    /// (v2 `now_playing`).
    pub async fn report_now_playing(&self, id: &str) -> Result<(), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("id".to_owned(), id.to_owned()),
            ("submission".to_owned(), "false".to_owned()),
        ];
        self.unscoped_request("/rest/scrobble", &params).await?;
        Ok(())
    }

    /// Scrobble via `/rest/scrobble` with the play time in unix millis
    /// (v2 `scrobble`).
    pub async fn scrobble(&self, id: &str, time_ms: i64) -> Result<(), AdapterError> {
        self.require_configured()?;
        let params = vec![
            ("id".to_owned(), id.to_owned()),
            ("time".to_owned(), time_ms.to_string()),
        ];
        self.unscoped_request("/rest/scrobble", &params).await?;
        Ok(())
    }

    /// Playlist cover: the playlist's own `coverArt` id resolved through
    /// `getCoverArt`.
    pub async fn playlist_cover_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        let value = self
            .unscoped_request("/rest/getPlaylist", &[("id".to_owned(), id.to_owned())])
            .await?;
        let cover_art = value
            .get("playlist")
            .and_then(|playlist| str_field(playlist, "coverArt"))
            .unwrap_or("");
        if cover_art.is_empty() {
            return Err(AdapterError::NotFound);
        }
        self.image_bytes(cover_art, size).await
    }

    /// Resolve an MBID by searching for it and comparing `musicBrainzId`,
    /// the v2 album-match rule.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(no_match());
        }
        let params = vec![
            ("query".to_owned(), mbid.to_owned()),
            ("artistCount".to_owned(), "0".to_owned()),
            ("albumCount".to_owned(), "50".to_owned()),
            ("songCount".to_owned(), "0".to_owned()),
        ];
        let value = self.request("/rest/search3", &params).await?;
        let candidates = value
            .get("searchResult3")
            .and_then(|result| result.get("album"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        for candidate in &candidates {
            if str_field(candidate, "musicBrainzId").unwrap_or("") == mbid {
                let id = str_field(candidate, "id").unwrap_or("").to_owned();
                let tracks = self.album_tracks(&id).await?;
                return Ok(MatchView {
                    source: SourceName::Navidrome,
                    found: true,
                    remote_album_id: Some(id),
                    tracks,
                });
            }
        }
        Ok(no_match())
    }

    fn require_configured(&self) -> Result<(), AdapterError> {
        if self.is_configured() {
            Ok(())
        } else {
            Err(AdapterError::NotConfigured)
        }
    }

    /// True when the caller picked folders but none survived resolution.
    /// Every scoped call fails closed then, without any request.
    fn scope_is_empty(&self) -> bool {
        matches!(&self.folder_ids, Some(ids) if ids.is_empty())
    }

    /// Scoped `getAlbumList2` with the genre/year parameter rules.
    async fn album_list(
        &self,
        list_type: &str,
        size: i64,
        offset: i64,
        genre: Option<&str>,
        from_year: Option<i32>,
        to_year: Option<i32>,
    ) -> Result<Vec<Value>, AdapterError> {
        let mut params = vec![
            ("type".to_owned(), list_type.to_owned()),
            ("size".to_owned(), size.to_string()),
            ("offset".to_owned(), offset.to_string()),
        ];
        if list_type == "byGenre"
            && let Some(genre) = genre
        {
            params.push(("genre".to_owned(), genre.to_owned()));
        }
        if list_type == "byYear" {
            params.push(("fromYear".to_owned(), from_year.unwrap_or(0).to_string()));
            params.push(("toYear".to_owned(), to_year.unwrap_or(9999).to_string()));
        }
        let value = self.request("/rest/getAlbumList2", &params).await?;
        Ok(value
            .get("albumList2")
            .and_then(|block| block.get("album"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default())
    }

    /// Full scoped artist list, flattened out of the index buckets.
    async fn artist_list(&self) -> Result<Vec<ArtistView>, AdapterError> {
        let value = self.request("/rest/getArtists", &[]).await?;
        let buckets = value
            .get("artists")
            .and_then(|artists| artists.get("index"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut artists = Vec::new();
        for bucket in &buckets {
            let raw = bucket
                .get("artist")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            for artist in &raw {
                artists.push(self.artist_view(artist));
            }
        }
        Ok(artists)
    }

    /// Scope check for album detail: the album must appear in a scoped
    /// name search, the v2 detail rule.
    async fn album_in_scope(&self, id: &str) -> Result<bool, AdapterError> {
        let detail = self
            .unscoped_request("/rest/getAlbum", &[("id".to_owned(), id.to_owned())])
            .await?;
        let name = detail
            .get("album")
            .and_then(|album| str_field(album, "name"))
            .unwrap_or("");
        if name.is_empty() {
            return Ok(false);
        }
        let params = vec![
            ("query".to_owned(), name.to_owned()),
            ("artistCount".to_owned(), "0".to_owned()),
            ("albumCount".to_owned(), "500".to_owned()),
            ("songCount".to_owned(), "0".to_owned()),
        ];
        let value = self.request("/rest/search3", &params).await?;
        let candidates = value
            .get("searchResult3")
            .and_then(|result| result.get("album"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(candidates
            .iter()
            .any(|candidate| str_field(candidate, "id").unwrap_or("") == id))
    }

    /// Scoped JSON call. Repeats one `musicFolderId` param per selected id;
    /// omits the param for the all-folders scope.
    async fn request(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<Value, AdapterError> {
        let mut full: Vec<(String, String)> = params.to_vec();
        if let Some(ids) = &self.folder_ids {
            for id in ids {
                full.push(("musicFolderId".to_owned(), id.clone()));
            }
        }
        self.unscoped_request(endpoint, &full).await
    }

    /// Unscoped JSON call: auth params plus Subsonic envelope handling.
    async fn unscoped_request(
        &self,
        endpoint: &str,
        params: &[(String, String)],
    ) -> Result<Value, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query = auth_params(&self.username, &self.password);
        query.extend(params.iter().cloned());
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
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
        let data: Value = serde_json::from_slice(&bytes).map_err(|_| {
            AdapterError::Api(format!("Navidrome returned invalid JSON for {endpoint}"))
        })?;
        parse_envelope(&data)
    }

    /// Raw-bytes call for cover art and audio. Auth rides the same token
    /// params; a missing content type falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        params: &[(String, String)],
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query = auth_params(&self.username, &self.password);
        query.extend(params.iter().cloned());
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
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

    fn album_view(&self, album: &Value) -> AlbumView {
        let id = str_field(album, "id").unwrap_or("").to_owned();
        AlbumView {
            source: SourceName::Navidrome,
            image_url: cover_url(str_field(album, "coverArt").unwrap_or("")),
            id,
            title: album_name(album).to_owned(),
            artist_name: str_field(album, "artist").unwrap_or("").to_owned(),
            artist_id: non_empty(str_field(album, "artistId")),
            year: album
                .get("year")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            genre: non_empty(str_field(album, "genre")),
            track_count: album
                .get("songCount")
                .and_then(value_to_i64)
                .map(|count| count as i32),
            release_mbid: non_empty(str_field(album, "musicBrainzId")),
            release_group_mbid: None,
            artist_mbid: None,
        }
    }

    fn artist_view(&self, artist: &Value) -> ArtistView {
        ArtistView {
            source: SourceName::Navidrome,
            id: str_field(artist, "id").unwrap_or("").to_owned(),
            name: str_field(artist, "name").unwrap_or("Unknown").to_owned(),
            album_count: artist
                .get("albumCount")
                .and_then(value_to_i64)
                .map(|count| count as i32),
            artist_mbid: non_empty(str_field(artist, "musicBrainzId")),
            image_url: cover_url(str_field(artist, "coverArt").unwrap_or("")),
        }
    }

    fn track_view(&self, song: &Value) -> TrackView {
        TrackView {
            source: SourceName::Navidrome,
            id: str_field(song, "id").unwrap_or("").to_owned(),
            title: str_field(song, "title").unwrap_or("Unknown").to_owned(),
            album_name: str_field(song, "album").unwrap_or("").to_owned(),
            album_id: non_empty(str_field(song, "albumId")),
            artist_name: str_field(song, "artist").unwrap_or("").to_owned(),
            artist_id: non_empty(str_field(song, "artistId")),
            track_number: song.get("track").and_then(value_to_i64).map(|n| n as i32),
            disc_number: song
                .get("discNumber")
                .and_then(value_to_i64)
                .map(|n| n as i32),
            duration_secs: song.get("duration").and_then(value_to_i64),
            year: song
                .get("year")
                .and_then(value_to_i64)
                .map(|year| year as i32),
            recording_mbid: non_empty(str_field(song, "musicBrainzId")),
            image_url: cover_url(str_field(song, "coverArt").unwrap_or("")),
            part_key: None,
        }
    }

    fn playlist_summary(&self, playlist: &Value) -> PlaylistSummary {
        let id = str_field(playlist, "id").unwrap_or("").to_owned();
        let cover_art = str_field(playlist, "coverArt").unwrap_or("");
        PlaylistSummary {
            source: SourceName::Navidrome,
            image_url: if cover_art.is_empty() {
                None
            } else {
                Some(format!("/api/v3/remotes/navidrome/covers/playlists/{id}"))
            },
            id,
            name: str_field(playlist, "name").unwrap_or("").to_owned(),
            track_count: playlist
                .get("songCount")
                .and_then(value_to_i64)
                .unwrap_or(0),
            duration_secs: playlist.get("duration").and_then(value_to_i64).unwrap_or(0),
        }
    }
}

/// Subsonic token auth params with a fresh random salt per call.
fn auth_params(username: &str, password: &str) -> Vec<(String, String)> {
    let mut salt_bytes = [0u8; 3];
    if getrandom::fill(&mut salt_bytes).is_err() {
        salt_bytes = [0x4e, 0x44, 0x21];
    }
    let salt = hex_bytes(&salt_bytes);
    let digest = md5::compute(format!("{password}{salt}"));
    vec![
        ("u".to_owned(), username.to_owned()),
        ("t".to_owned(), format!("{digest:x}")),
        ("s".to_owned(), salt),
        ("v".to_owned(), SUBSONIC_VERSION.to_owned()),
        ("c".to_owned(), CLIENT_NAME.to_owned()),
        ("f".to_owned(), "json".to_owned()),
    ]
}

/// Unwrap the `subsonic-response` envelope. Codes 40/41 are auth failures.
fn parse_envelope(data: &Value) -> Result<Value, AdapterError> {
    let response = data
        .get("subsonic-response")
        .ok_or_else(|| AdapterError::Api("Missing subsonic-response envelope".to_owned()))?;
    if str_field(response, "status").unwrap_or("") != "ok" {
        let error = response.get("error").cloned().unwrap_or(Value::Null);
        let code = error.get("code").and_then(value_to_i64).unwrap_or(0);
        let message = str_field(&error, "message")
            .unwrap_or("Unknown Subsonic API error")
            .to_owned();
        if code == 40 || code == 41 {
            return Err(AdapterError::Auth);
        }
        return Err(AdapterError::Api(format!(
            "Subsonic error {code}: {message}"
        )));
    }
    Ok(response.clone())
}

/// Client-side reversal rule for album sorts, ported from the v2 route
/// map: descending name and non-descending recency both reverse.
fn needs_reverse(sort_by: &str, descending: bool, has_genre: bool) -> bool {
    if has_genre {
        return false;
    }
    matches!(
        (sort_by, descending),
        ("name", true) | ("date_added", false)
    )
}

fn album_name(album: &Value) -> &str {
    str_field(album, "name")
        .or_else(|| str_field(album, "title"))
        .unwrap_or("Unknown")
}

fn known_name(name: &str) -> bool {
    !name.is_empty() && name != "Unknown"
}

fn cover_url(cover_art: &str) -> Option<String> {
    if cover_art.is_empty() {
        None
    } else {
        Some(format!("/api/v3/remotes/navidrome/images/{cover_art}"))
    }
}

fn bucket<T>(result: &Value, key: &str, map: impl Fn(&Value) -> T) -> Vec<T> {
    result
        .get(key)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(map)
        .collect()
}

fn empty_page<T>() -> RemotePage<T> {
    RemotePage {
        items: Vec::new(),
        total: 0,
    }
}

fn empty_search() -> SearchResults {
    SearchResults {
        artists: Vec::new(),
        albums: Vec::new(),
        tracks: Vec::new(),
    }
}

fn empty_favorites() -> FavoritesView {
    FavoritesView {
        artists: Vec::new(),
        albums: Vec::new(),
        tracks: Vec::new(),
    }
}

fn empty_hub() -> HubView {
    HubView {
        source: SourceName::Navidrome,
        stats: Some(zero_stats()),
        recently_played: Vec::new(),
        recently_added: Vec::new(),
        favorites: Vec::new(),
        favorite_artists: Vec::new(),
        most_played_artists: Vec::new(),
        all_albums_preview: Vec::new(),
        genres: Vec::new(),
    }
}

fn zero_stats() -> StatsView {
    StatsView {
        total_albums: 0,
        total_artists: 0,
        total_tracks: 0,
    }
}

fn no_match() -> MatchView {
    MatchView {
        source: SourceName::Navidrome,
        found: false,
        remote_album_id: None,
        tracks: Vec::new(),
    }
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
    value.as_u64().and_then(|number| i64::try_from(number).ok())
}

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn trim_cause(cause: &reqwest::Error) -> String {
    let text = cause.to_string();
    if text.len() > 200 {
        text[..200].to_owned()
    } else {
        text
    }
}
