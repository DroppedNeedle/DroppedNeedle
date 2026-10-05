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
//!
//! Payloads decode into the typed shapes in [`super::navidrome_models`]; a
//! payload missing an item id is an upstream error, not an empty item.

use std::time::Duration;

use super::adapter::{AdapterError, AlbumBrowse, ArtistBrowse, RemotePage, TrackBrowse};
use super::models::{
    AlbumView, ArtistIndexEntry, ArtistView, FavoritesView, HistoryPage, HubView, InfoView,
    LyricLine, LyricsView, MatchView, PlaylistDetail, PlaylistSummary, SearchResults, SessionView,
    SessionsView, SourceName, StatsView, TrackView,
};
use super::navidrome_models::{Album, Artist, Body, Envelope, Playlist, Song};

/// Request timeout per upstream call, matching the v2 repository.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Subsonic API version pinned by the v2 repository.
const SUBSONIC_VERSION: &str = "1.16.1";

/// Client name sent on every Navidrome call.
const CLIENT_NAME: &str = "droppedneedle";

/// Stats album scan batch size, matching the v2 service.
const STATS_BATCH: i64 = 500;

/// Query pairs, owned.
type Params = Vec<(String, String)>;

fn params(pairs: &[(&str, &str)]) -> Params {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
        .collect()
}

/// Navidrome browse client. Built per request from the caller's resolved
/// connection plus their folder scope; holds no cache.
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
        let body = self.request("/rest/ping", &[]).await?;
        Ok(format!(
            "Connected to Navidrome (API v{})",
            body.version.as_deref().unwrap_or("unknown")
        ))
    }

    /// Music folders exposed by the server. Unscoped by design: the folder
    /// preference UI lists everything before the user picks.
    pub async fn music_folders(&self) -> Result<Vec<(String, String)>, AdapterError> {
        self.require_configured()?;
        let body = self.unscoped_request("/rest/getMusicFolders", &[]).await?;
        Ok(body
            .music_folders
            .unwrap_or_default()
            .music_folder
            .into_iter()
            .map(|folder| (folder.id, folder.name.unwrap_or_default()))
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
            self.favorites(50),
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
                .album_list("alphabeticalByName", STATS_BATCH, offset, None, None)
                .await?;
            if batch.is_empty() {
                break;
            }
            total_albums += batch.len() as i64;
            total_tracks += batch
                .iter()
                .map(|album| album.song_count.unwrap_or(0))
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
        let years = (list_type == "byYear").then(|| {
            if browse.descending {
                (9999, 0)
            } else {
                (0, 9999)
            }
        });
        let genre = (!browse.genre.is_empty()).then_some(browse.genre.as_str());
        let mut raw = self
            .album_list(list_type, browse.limit, browse.offset, genre, years)
            .await?;
        if needs_reverse(&browse.sort_by, browse.descending, genre.is_some()) {
            raw.reverse();
        }
        let items: Vec<AlbumView> = raw
            .iter()
            .filter(|album| known_name(album.display_name()))
            .map(album_view)
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

    /// One album by id. Out-of-scope ids read as absent, and an empty scope
    /// reads every detail as absent.
    pub async fn album_detail(&self, id: &str) -> Result<Option<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(None);
        }
        let Some(album) = self.get_album(id).await? else {
            return Ok(None);
        };
        if self.folder_ids.is_some() && !self.album_in_scope(&album).await? {
            return Ok(None);
        }
        Ok(Some(album_view(&album)))
    }

    /// Album tracks in track order.
    pub async fn album_tracks(&self, id: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        Ok(self
            .get_album(id)
            .await?
            .map(|album| album.song.iter().map(track_view).collect())
            .unwrap_or_default())
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
        let items = artists
            .into_iter()
            .skip(browse.offset.max(0) as usize)
            .take(browse.limit.max(0) as usize)
            .collect();
        Ok(RemotePage { items, total })
    }

    /// Alphabetic artist index straight from `getArtists`.
    pub async fn artist_index(&self) -> Result<Vec<ArtistIndexEntry>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let body = self.request("/rest/getArtists", &[]).await?;
        Ok(body
            .artists
            .unwrap_or_default()
            .index
            .into_iter()
            .map(|bucket| ArtistIndexEntry {
                name: bucket.name,
                artists: bucket.artist.iter().map(artist_view).collect(),
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
        let body = self
            .unscoped_request("/rest/getArtist", &params(&[("id", id)]))
            .await?;
        Ok(body.artist.as_ref().map(artist_view))
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
            "\"\""
        } else {
            browse.search.as_str()
        };
        let limit = browse.limit.to_string();
        let offset = browse.offset.to_string();
        let pairs = params(&[
            ("query", query),
            ("artistCount", "0"),
            ("albumCount", "0"),
            ("songCount", &limit),
            ("songOffset", &offset),
        ]);
        let body = self.request("/rest/search3", &pairs).await?;
        let items: Vec<TrackView> = body
            .search_result3
            .unwrap_or_default()
            .song
            .iter()
            .map(track_view)
            .collect();
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
            return Ok(SearchResults {
                artists: Vec::new(),
                albums: Vec::new(),
                tracks: Vec::new(),
            });
        }
        let limit = limit.to_string();
        let pairs = params(&[
            ("query", query),
            ("artistCount", &limit),
            ("albumCount", &limit),
            ("songCount", &limit),
        ]);
        let result = self
            .request("/rest/search3", &pairs)
            .await?
            .search_result3
            .unwrap_or_default();
        Ok(SearchResults {
            artists: result.artist.iter().map(artist_view).collect(),
            albums: result.album.iter().map(album_view).collect(),
            tracks: result.song.iter().map(track_view).collect(),
        })
    }

    /// Recently played albums via `getAlbumList2 type=recent`.
    pub async fn recent(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.album_feed("recent", limit).await
    }

    /// Recently added albums via `getAlbumList2 type=newest`.
    pub async fn recently_added(&self, limit: i64) -> Result<Vec<AlbumView>, AdapterError> {
        self.album_feed("newest", limit).await
    }

    /// Starred artists, albums, and songs via `getStarred2`, up to `limit`
    /// of each (the endpoint answers everything at once).
    pub async fn favorites(&self, limit: i64) -> Result<FavoritesView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(empty_favorites());
        }
        let limit = limit.max(0) as usize;
        let starred = self
            .request("/rest/getStarred2", &[])
            .await?
            .starred2
            .unwrap_or_default();
        Ok(FavoritesView {
            artists: starred.artist.iter().take(limit).map(artist_view).collect(),
            albums: starred
                .album
                .iter()
                .filter(|album| known_name(album.display_name()))
                .take(limit)
                .map(album_view)
                .collect(),
            tracks: starred.song.iter().take(limit).map(track_view).collect(),
        })
    }

    /// Genre labels via `getGenres` (unscoped in v2).
    pub async fn genres(&self) -> Result<Vec<String>, AdapterError> {
        self.require_configured()?;
        let body = self.unscoped_request("/rest/getGenres", &[]).await?;
        Ok(body
            .genres
            .unwrap_or_default()
            .genre
            .into_iter()
            .filter_map(|genre| genre.value.or(genre.name))
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
        let (limit, offset) = (limit.to_string(), offset.to_string());
        let pairs = params(&[("genre", genre), ("count", &limit), ("offset", &offset)]);
        let body = self.request("/rest/getSongsByGenre", &pairs).await?;
        Ok(songs(body.songs_by_genre))
    }

    /// Playlists via `getPlaylists`.
    pub async fn playlists(&self) -> Result<Vec<PlaylistSummary>, AdapterError> {
        self.require_configured()?;
        let body = self.unscoped_request("/rest/getPlaylists", &[]).await?;
        Ok(body
            .playlists
            .unwrap_or_default()
            .playlist
            .iter()
            .map(playlist_summary)
            .collect())
    }

    /// One playlist with entries via `getPlaylist`. None is absence.
    pub async fn playlist_detail(&self, id: &str) -> Result<Option<PlaylistDetail>, AdapterError> {
        self.require_configured()?;
        let body = self
            .unscoped_request("/rest/getPlaylist", &params(&[("id", id)]))
            .await?;
        Ok(body.playlist.map(|playlist| PlaylistDetail {
            tracks: playlist.entry.iter().map(track_view).collect(),
            playlist: playlist_summary(&playlist),
        }))
    }

    /// Artist info passthrough via `getArtistInfo2`. Empty when the server
    /// declines (Last.fm often unconfigured); unreachable still errors.
    pub async fn artist_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        self.require_configured()?;
        let info = self
            .declinable("/rest/getArtistInfo2", &params(&[("id", id)]))
            .await?
            .and_then(|body| body.artist_info2)
            .unwrap_or_default();
        Ok(InfoView {
            source: SourceName::Navidrome,
            id: id.to_owned(),
            biography: info.biography.clone().unwrap_or_default(),
            musicbrainz_id: info.music_brainz_id.clone().unwrap_or_default(),
            image_url: info.best_image(),
            similar_artists: info.similar_artist.iter().map(artist_view).collect(),
        })
    }

    /// Album info passthrough via `getAlbumInfo2`, with the same
    /// decline-to-empty rule.
    pub async fn album_info(&self, id: &str) -> Result<InfoView, AdapterError> {
        self.require_configured()?;
        let info = self
            .declinable("/rest/getAlbumInfo2", &params(&[("id", id)]))
            .await?
            .and_then(|body| body.album_info)
            .unwrap_or_default();
        Ok(InfoView {
            source: SourceName::Navidrome,
            id: id.to_owned(),
            biography: info.notes.clone().unwrap_or_default(),
            musicbrainz_id: info.music_brainz_id.clone().unwrap_or_default(),
            image_url: info.best_image(),
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
        let structured = self
            .declinable("/rest/getLyricsBySongId", &params(&[("id", id)]))
            .await?
            .and_then(|body| body.lyrics_list)
            .unwrap_or_default();
        if let Some(best) = structured.structured_lyrics.first() {
            let lines: Vec<LyricLine> = best
                .line
                .iter()
                .map(|line| LyricLine {
                    text: line.value.clone().unwrap_or_default(),
                    start_ms: if best.synced { line.start } else { None },
                })
                .collect();
            let has_text = lines.iter().any(|line| !line.text.trim().is_empty());
            let has_timing = lines.iter().any(|line| line.start_ms.is_some());
            if has_text || has_timing {
                let text = lines
                    .iter()
                    .map(|line| line.text.as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                return Ok(Some(LyricsView {
                    source: SourceName::Navidrome,
                    text,
                    is_synced: best.synced,
                    lines,
                }));
            }
        }
        let (Some(artist), Some(title)) = (artist, title) else {
            return Ok(None);
        };
        let text = self
            .declinable(
                "/rest/getLyrics",
                &params(&[("artist", artist), ("title", title)]),
            )
            .await?
            .and_then(|body| body.lyrics)
            .and_then(|lyrics| lyrics.value)
            .unwrap_or_default();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some(LyricsView {
            source: SourceName::Navidrome,
            text,
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
        let limit = limit.to_string();
        let body = self
            .declinable(
                "/rest/getTopSongs",
                &params(&[("artist", artist), ("count", &limit)]),
            )
            .await?;
        Ok(songs(body.and_then(|body| body.top_songs)))
    }

    /// Random songs via `getRandomSongs`, folder-scoped like the v2
    /// route (`size` + optional `genre`). Empty when declined.
    pub async fn random(&self, limit: i64, genre: &str) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let mut pairs = vec![("size".to_owned(), limit.to_string())];
        if !genre.is_empty() {
            pairs.push(("genre".to_owned(), genre.to_owned()));
        }
        let body = match self.request("/rest/getRandomSongs", &pairs).await {
            Ok(body) => Some(body),
            Err(AdapterError::Api(detail)) => {
                tracing::debug!(%detail, "navidrome declined getRandomSongs");
                None
            }
            Err(other) => return Err(other),
        };
        Ok(songs(body.and_then(|body| body.random_songs)))
    }

    /// Similar songs via `getSimilarSongs2`. Empty when declined.
    pub async fn similar(&self, id: &str, limit: i64) -> Result<Vec<TrackView>, AdapterError> {
        self.require_configured()?;
        let limit = limit.to_string();
        let body = self
            .declinable(
                "/rest/getSimilarSongs2",
                &params(&[("id", id), ("count", &limit)]),
            )
            .await?;
        Ok(songs(body.and_then(|body| body.similar_songs2)))
    }

    /// Now-playing entries via `getNowPlaying`, shaped as sessions.
    pub async fn sessions(&self) -> Result<SessionsView, AdapterError> {
        self.require_configured()?;
        let body = self.unscoped_request("/rest/getNowPlaying", &[]).await?;
        let sessions = body
            .now_playing
            .unwrap_or_default()
            .entry
            .into_iter()
            .map(|entry| {
                let duration_ms = entry.duration.unwrap_or(0) * 1000;
                let minutes_ago = entry.minutes_ago.unwrap_or(0);
                let text = |value: &Option<String>| value.clone().unwrap_or_default();
                SessionView {
                    source: SourceName::Navidrome,
                    session_id: format!(
                        "{}:{}:{}:{}",
                        text(&entry.username),
                        text(&entry.player_name),
                        text(&entry.album_id),
                        text(&entry.title),
                    ),
                    user_name: text(&entry.username),
                    device_name: text(&entry.player_name),
                    track_title: text(&entry.title),
                    artist_name: text(&entry.artist),
                    album_name: text(&entry.album),
                    // getNowPlaying reports only minutes since the play
                    // started; v2 estimates the position from it.
                    progress_ms: if minutes_ago > 0 {
                        (duration_ms - minutes_ago * 60_000).max(0)
                    } else {
                        0
                    },
                    duration_ms,
                    // getNowPlaying carries no play/pause state.
                    is_paused: false,
                    image_url: cover_url(entry.cover_art.as_deref()),
                }
            })
            .collect();
        Ok(SessionsView {
            source: SourceName::Navidrome,
            sessions,
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
        let size = size.to_string();
        self.get_bytes(
            "/rest/getCoverArt",
            &params(&[("id", id), ("size", &size)]),
            "image/jpeg",
        )
        .await
    }

    /// Direct audio bytes for one song id via `/rest/stream` (v2
    /// `build_stream_url` target, fetched whole for the gateway seam).
    pub async fn audio_bytes(&self, id: &str) -> Result<(Vec<u8>, String), AdapterError> {
        self.require_configured()?;
        self.get_bytes("/rest/stream", &params(&[("id", id)]), "audio/mpeg")
            .await
    }

    /// Now-playing report via `/rest/scrobble` with `submission=false`
    /// (v2 `now_playing`).
    pub async fn report_now_playing(&self, id: &str) -> Result<(), AdapterError> {
        self.require_configured()?;
        self.unscoped_request(
            "/rest/scrobble",
            &params(&[("id", id), ("submission", "false")]),
        )
        .await?;
        Ok(())
    }

    /// Scrobble via `/rest/scrobble` with the play time in unix millis
    /// (v2 `scrobble`).
    pub async fn scrobble(&self, id: &str, time_ms: i64) -> Result<(), AdapterError> {
        self.require_configured()?;
        let time = time_ms.to_string();
        self.unscoped_request("/rest/scrobble", &params(&[("id", id), ("time", &time)]))
            .await?;
        Ok(())
    }

    /// Playlist cover: the playlist's own `coverArt` id resolved through
    /// `getCoverArt`.
    pub async fn playlist_cover_bytes(
        &self,
        id: &str,
        size: i64,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        let body = self
            .unscoped_request("/rest/getPlaylist", &params(&[("id", id)]))
            .await?;
        let cover_art = body
            .playlist
            .and_then(|playlist| playlist.cover_art)
            .filter(|cover| !cover.is_empty())
            .ok_or(AdapterError::NotFound)?;
        self.image_bytes(&cover_art, size).await
    }

    /// Resolve an MBID by searching for it and comparing `musicBrainzId`,
    /// the v2 album-match rule.
    pub async fn match_album(&self, mbid: &str) -> Result<MatchView, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(no_match());
        }
        let pairs = params(&[
            ("query", mbid),
            ("artistCount", "0"),
            ("albumCount", "50"),
            ("songCount", "0"),
        ]);
        let result = self
            .request("/rest/search3", &pairs)
            .await?
            .search_result3
            .unwrap_or_default();
        let Some(album) = result
            .album
            .iter()
            .find(|album| album.music_brainz_id.as_deref() == Some(mbid))
        else {
            return Ok(no_match());
        };
        let tracks = self.album_tracks(&album.id).await?;
        Ok(MatchView {
            source: SourceName::Navidrome,
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

    /// True when the caller picked folders but none survived resolution.
    /// Every scoped call fails closed then, without any request.
    fn scope_is_empty(&self) -> bool {
        matches!(&self.folder_ids, Some(ids) if ids.is_empty())
    }

    /// One album feed (`recent`, `newest`), named albums only.
    async fn album_feed(
        &self,
        list_type: &str,
        limit: i64,
    ) -> Result<Vec<AlbumView>, AdapterError> {
        self.require_configured()?;
        if self.scope_is_empty() {
            return Ok(Vec::new());
        }
        let raw = self.album_list(list_type, limit, 0, None, None).await?;
        Ok(raw
            .iter()
            .filter(|album| known_name(album.display_name()))
            .map(album_view)
            .collect())
    }

    /// `getAlbum`, unscoped. None is absence.
    async fn get_album(&self, id: &str) -> Result<Option<Album>, AdapterError> {
        let body = self
            .unscoped_request("/rest/getAlbum", &params(&[("id", id)]))
            .await?;
        Ok(body.album)
    }

    /// Scoped `getAlbumList2` with the genre/year parameter rules.
    async fn album_list(
        &self,
        list_type: &str,
        size: i64,
        offset: i64,
        genre: Option<&str>,
        years: Option<(i32, i32)>,
    ) -> Result<Vec<Album>, AdapterError> {
        let mut pairs = vec![
            ("type".to_owned(), list_type.to_owned()),
            ("size".to_owned(), size.to_string()),
            ("offset".to_owned(), offset.to_string()),
        ];
        if list_type == "byGenre"
            && let Some(genre) = genre
        {
            pairs.push(("genre".to_owned(), genre.to_owned()));
        }
        if list_type == "byYear" {
            let (from, to) = years.unwrap_or((0, 9999));
            pairs.push(("fromYear".to_owned(), from.to_string()));
            pairs.push(("toYear".to_owned(), to.to_string()));
        }
        let body = self.request("/rest/getAlbumList2", &pairs).await?;
        Ok(body.album_list2.unwrap_or_default().album)
    }

    /// Full scoped artist list, flattened out of the index buckets.
    async fn artist_list(&self) -> Result<Vec<ArtistView>, AdapterError> {
        let body = self.request("/rest/getArtists", &[]).await?;
        Ok(body
            .artists
            .unwrap_or_default()
            .index
            .iter()
            .flat_map(|bucket| bucket.artist.iter().map(artist_view))
            .collect())
    }

    /// Scope check for album detail: the album must appear in a scoped
    /// name search, the v2 detail rule.
    async fn album_in_scope(&self, album: &Album) -> Result<bool, AdapterError> {
        let name = album.display_name();
        if !known_name(name) {
            return Ok(false);
        }
        let pairs = params(&[
            ("query", name),
            ("artistCount", "0"),
            ("albumCount", "500"),
            ("songCount", "0"),
        ]);
        let result = self
            .request("/rest/search3", &pairs)
            .await?
            .search_result3
            .unwrap_or_default();
        Ok(result
            .album
            .iter()
            .any(|candidate| candidate.id == album.id))
    }

    /// An unscoped call the server may decline (no Last.fm, no lyrics):
    /// a Subsonic error reads as `None`, everything else propagates.
    async fn declinable(
        &self,
        endpoint: &str,
        pairs: &[(String, String)],
    ) -> Result<Option<Body>, AdapterError> {
        match self.unscoped_request(endpoint, pairs).await {
            Ok(body) => Ok(Some(body)),
            Err(AdapterError::Api(detail)) => {
                tracing::debug!(endpoint, %detail, "navidrome declined the call");
                Ok(None)
            }
            Err(other) => Err(other),
        }
    }

    /// Scoped call. Repeats one `musicFolderId` param per selected id;
    /// omits the param for the all-folders scope.
    async fn request(
        &self,
        endpoint: &str,
        pairs: &[(String, String)],
    ) -> Result<Body, AdapterError> {
        let mut full: Params = pairs.to_vec();
        if let Some(ids) = &self.folder_ids {
            for id in ids {
                full.push(("musicFolderId".to_owned(), id.clone()));
            }
        }
        self.unscoped_request(endpoint, &full).await
    }

    /// Unscoped call: auth params plus Subsonic envelope handling.
    async fn unscoped_request(
        &self,
        endpoint: &str,
        pairs: &[(String, String)],
    ) -> Result<Body, AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query = auth_params(&self.username, &self.password);
        query.extend(pairs.iter().cloned());
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .query(&query)
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
        let bytes = response
            .bytes()
            .await
            .map_err(|cause| AdapterError::Transport(trim_cause(&cause)))?;
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(|error| {
            AdapterError::Api(format!(
                "Navidrome returned an unreadable {endpoint} payload: {error}"
            ))
        })?;
        let body = envelope.response;
        if body.status != "ok" {
            let error = body.error.unwrap_or_default();
            if error.code == 40 || error.code == 41 {
                return Err(AdapterError::Auth);
            }
            return Err(AdapterError::Api(format!(
                "Subsonic error {}: {}",
                error.code,
                error
                    .message
                    .as_deref()
                    .unwrap_or("Unknown Subsonic API error")
            )));
        }
        Ok(body)
    }

    /// Raw-bytes call for cover art and audio. Auth rides the same token
    /// params; a missing content type falls back to `fallback_content_type`.
    async fn get_bytes(
        &self,
        endpoint: &str,
        pairs: &[(String, String)],
        fallback_content_type: &str,
    ) -> Result<(Vec<u8>, String), AdapterError> {
        if !self.is_configured() {
            return Err(AdapterError::NotConfigured);
        }
        let mut query = auth_params(&self.username, &self.password);
        query.extend(pairs.iter().cloned());
        let response = self
            .client
            .get(format!("{}{endpoint}", self.base_url))
            .query(&query)
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
}

/// Subsonic token auth params with a fresh random salt per call.
fn auth_params(username: &str, password: &str) -> Params {
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

fn known_name(name: &str) -> bool {
    !name.is_empty() && name != "Unknown"
}

fn cover_url(cover_art: Option<&str>) -> Option<String> {
    cover_art
        .filter(|cover| !cover.is_empty())
        .map(|cover| format!("/api/v3/remotes/navidrome/images/{cover}"))
}

fn songs(list: Option<super::navidrome_models::Songs>) -> Vec<TrackView> {
    list.unwrap_or_default()
        .song
        .iter()
        .map(track_view)
        .collect()
}

fn album_view(album: &Album) -> AlbumView {
    AlbumView {
        source: SourceName::Navidrome,
        image_url: cover_url(album.cover_art.as_deref()),
        id: album.id.clone(),
        title: album.display_name().to_owned(),
        artist_name: album.artist.clone().unwrap_or_default(),
        artist_id: non_empty(album.artist_id.as_deref()),
        year: album.year.and_then(|year| i32::try_from(year).ok()),
        genre: non_empty(album.genre.as_deref()),
        track_count: album.song_count.and_then(|count| i32::try_from(count).ok()),
        release_mbid: non_empty(album.music_brainz_id.as_deref()),
        release_group_mbid: None,
        artist_mbid: None,
    }
}

fn artist_view(artist: &Artist) -> ArtistView {
    ArtistView {
        source: SourceName::Navidrome,
        id: artist.id.clone(),
        name: artist.name.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_count: artist
            .album_count
            .and_then(|count| i32::try_from(count).ok()),
        artist_mbid: non_empty(artist.music_brainz_id.as_deref()),
        image_url: cover_url(artist.cover_art.as_deref()),
    }
}

fn track_view(song: &Song) -> TrackView {
    TrackView {
        source: SourceName::Navidrome,
        id: song.id.clone(),
        title: song.title.clone().unwrap_or_else(|| "Unknown".to_owned()),
        album_name: song.album.clone().unwrap_or_default(),
        album_id: non_empty(song.album_id.as_deref()),
        artist_name: song.artist.clone().unwrap_or_default(),
        artist_id: non_empty(song.artist_id.as_deref()),
        track_number: song.track.and_then(|number| i32::try_from(number).ok()),
        disc_number: song
            .disc_number
            .and_then(|number| i32::try_from(number).ok()),
        duration_secs: song.duration,
        year: song.year.and_then(|year| i32::try_from(year).ok()),
        recording_mbid: non_empty(song.music_brainz_id.as_deref()),
        image_url: cover_url(song.cover_art.as_deref()),
        part_key: None,
    }
}

fn playlist_summary(playlist: &Playlist) -> PlaylistSummary {
    let has_cover = playlist
        .cover_art
        .as_deref()
        .is_some_and(|cover| !cover.is_empty());
    PlaylistSummary {
        source: SourceName::Navidrome,
        image_url: has_cover
            .then(|| format!("/api/v3/remotes/navidrome/covers/playlists/{}", playlist.id)),
        id: playlist.id.clone(),
        name: playlist.name.clone().unwrap_or_default(),
        track_count: playlist.song_count.unwrap_or(0),
        duration_secs: playlist.duration.unwrap_or(0),
    }
}

fn empty_page<T>() -> RemotePage<T> {
    RemotePage {
        items: Vec::new(),
        total: 0,
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

fn hex_bytes(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn trim_cause(cause: &reqwest::Error) -> String {
    cause.to_string().chars().take(200).collect()
}
