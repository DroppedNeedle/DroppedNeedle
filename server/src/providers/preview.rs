//! 30-second track and album previews via Deezer (primary) and iTunes (fallback).
//!
//! A Rust port of v2's `backend/repositories/preview_repository.py` with the
//! wire shapes from `backend/repositories/deezer_models.py` (live-verified
//! 2026-07-03). Both APIs are keyless. Deezer matches better (field-scoped
//! query syntax, ordered album track lists); iTunes results are verified
//! against the requested artist because its top hit can be a cover.
//!
//! Expiry warning, carried over from the v2 module docs: Deezer preview URLs
//! carry an `hdnea` expiry token. Callers must resolve them just-in-time and
//! never cache the URL long-term; only the s5-core enrichment layer decides
//! what, if anything, is cached, and it must treat these URLs as short-lived.
//!
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! Rate limiting (Deezer 5/s, iTunes 0.3/s), retries, and
//! degradation recording stay with wiring/enrichment. The composed lookups
//! below are infallible exactly like v2 (a dead preview source is absence,
//! not failure); the per-leg functions stay public so wiring can account
//! retries and degradation per upstream.

use serde::{Deserialize, Serialize};

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "preview";
/// Live Deezer API origin.
pub const DEEZER_API_BASE: &str = "https://api.deezer.com";
/// Live iTunes Search API endpoint.
pub const ITUNES_SEARCH_URL: &str = "https://itunes.apple.com/search";
/// Preview length v2 stamps on every result.
pub const PREVIEW_DURATION_S: u32 = 30;

/// What can go wrong on one preview leg.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed or the upstream answered an error status. 404 never
    /// lands here: callers read it as absence (v2 raises its retriable
    /// error for any status >= 400, but a 404 names nothing to retry).
    Transport,
    /// The upstream answered 429; the caller backs off this long: the
    /// honored `Retry-After`, or the leg default (5s Deezer, 60s iTunes, as
    /// in v2) when the response gave none.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// The body did not decode.
    Unusable,
}

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// A Deezer artist stub on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerArtist {
    /// Deezer artist id.
    #[serde(default)]
    pub id: Option<i64>,
    /// Artist name.
    #[serde(default)]
    pub name: String,
}

/// A Deezer track on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerTrack {
    /// Deezer track id.
    #[serde(default)]
    pub id: Option<i64>,
    /// Full title.
    #[serde(default)]
    pub title: String,
    /// Short title, preferred for display.
    #[serde(default)]
    pub title_short: String,
    /// Full-track duration in seconds.
    #[serde(default)]
    pub duration: Option<i64>,
    /// Position within the album.
    #[serde(default)]
    pub track_position: Option<i64>,
    /// 30s MP3 URL (empty when Deezer has no preview).
    #[serde(default)]
    pub preview: String,
    /// Track artist stub.
    #[serde(default)]
    pub artist: Option<DeezerArtist>,
}

/// A Deezer track-search page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerTrackSearchResponse {
    /// Hits.
    #[serde(default)]
    pub data: Vec<DeezerTrack>,
}

/// A Deezer album on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerAlbum {
    /// Deezer album id.
    #[serde(default)]
    pub id: Option<i64>,
    /// Album title.
    #[serde(default)]
    pub title: String,
    /// Album artist stub.
    #[serde(default)]
    pub artist: Option<DeezerArtist>,
}

/// A Deezer album-search page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerAlbumSearchResponse {
    /// Hits.
    #[serde(default)]
    pub data: Vec<DeezerAlbum>,
}

/// A Deezer album-tracks page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DeezerAlbumTracksResponse {
    /// Ordered album tracks.
    #[serde(default)]
    pub data: Vec<DeezerTrack>,
}

/// An iTunes song hit on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ITunesTrack {
    /// Song artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Song title.
    #[serde(default, rename = "trackName")]
    pub track_name: String,
    /// Album title.
    #[serde(default, rename = "collectionName")]
    pub collection_name: String,
    /// 30s AAC preview URL.
    #[serde(default, rename = "previewUrl")]
    pub preview_url: String,
    /// Full-track duration in milliseconds.
    #[serde(default, rename = "trackTimeMillis")]
    pub track_time_millis: Option<i64>,
    /// Position within the album.
    #[serde(default, rename = "trackNumber")]
    pub track_number: Option<i64>,
}

/// An iTunes song-search page on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ITunesSearchResponse {
    /// Hit count.
    #[serde(default, rename = "resultCount")]
    pub result_count: i64,
    /// Raw hits.
    #[serde(default)]
    pub results: Vec<ITunesTrack>,
}

// ---------------------------------------------------------------------------
// Normalized model
// ---------------------------------------------------------------------------

/// One provider-neutral 30s preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewTrack {
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist_name: String,
    /// Short-lived preview URL: resolve just-in-time, never cache long-term.
    pub preview_url: String,
    /// Preview length in seconds (always 30 from these legs).
    pub duration_s: Option<u32>,
    /// Position within the album, when known.
    pub position: Option<i64>,
}

// ---------------------------------------------------------------------------
// Pure matching (ports of the v2 module-level helpers)
// ---------------------------------------------------------------------------

/// Loose comparison form: lowercase, ASCII alphanumerics only (v2 `_norm`:
/// casefold, then strip everything outside `a-z0-9`).
pub fn norm(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Loose name match (v2 `_names_match`): both names must be non-empty, and
/// either normalized form must contain the other. Note the faithful edge:
/// when the wanted name normalizes to something non-empty but the got name
/// normalizes to nothing, the empty string is "contained" and the match
/// succeeds, exactly as the v2 substring check behaves.
pub fn names_match(wanted: &str, got: &str) -> bool {
    if wanted.is_empty() || got.is_empty() {
        return false;
    }
    let a = norm(wanted);
    let b = norm(got);
    !a.is_empty() && (a.contains(&b) || b.contains(&a))
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Preview client across the Deezer and iTunes legs.
pub struct PreviewClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// Deezer API origin; defaults to the live base.
    pub deezer_base: String,
    /// iTunes Search endpoint; defaults to the live URL.
    pub itunes_url: String,
}

impl<'h, H: HttpPort> PreviewClient<'h, H> {
    /// Build a client against the live origins.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            deezer_base: DEEZER_API_BASE.to_owned(),
            itunes_url: ITUNES_SEARCH_URL.to_owned(),
        }
    }

    /// Build a client against scripted or mirrored origins.
    pub fn with_bases(http: &'h H, deezer_base: &str, itunes_url: &str) -> Self {
        Self {
            http,
            deezer_base: deezer_base.to_owned(),
            itunes_url: itunes_url.to_owned(),
        }
    }

    /// Classify a non-200, non-404 status: 429 stays actionable with the
    /// honored `Retry-After` (falling back to `fallback_secs`), anything
    /// else 4xx/5xx is `Transport`. 404 never reaches here: callers read it
    /// as absence.
    fn classify(status: u16, retry_after: Option<&str>, fallback_secs: f64) -> Option<FetchError> {
        if status == 429 {
            return Some(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(retry_after).unwrap_or(fallback_secs),
            });
        }
        if status >= 400 {
            return Some(FetchError::Transport);
        }
        None
    }

    /// One track preview via Deezer's field-scoped search (v2
    /// `_deezer_track_preview`). The first hit carrying a preview URL wins.
    pub async fn deezer_track_preview(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<Option<PreviewTrack>, FetchError> {
        let query = format!(r#"artist:"{artist}" track:"{track}""#);
        let reply = self
            .http
            .get(
                &format!("{}/search", self.deezer_base),
                &[("q", query.as_str()), ("limit", "3")],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 5.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: DeezerTrackSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        for hit in &page.data {
            if hit.preview.is_empty() {
                continue;
            }
            return Ok(Some(PreviewTrack {
                title: if hit.title_short.is_empty() {
                    hit.title.clone()
                } else {
                    hit.title_short.clone()
                },
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: hit.preview.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: hit.track_position,
            }));
        }
        Ok(None)
    }

    /// Ordered album previews via Deezer (v2 `_deezer_album_tracks`). The
    /// album whose artist loosely matches wins; when no artist matches but
    /// hits exist, the top hit is used (the artist field is sometimes
    /// missing or odd). Tracks without previews are skipped, positions fall
    /// back to list order, and the tracks page over-fetches
    /// (`max(limit, 4)`) so skipping preview-less rows still fills `limit`.
    pub async fn deezer_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
    ) -> Result<Vec<PreviewTrack>, FetchError> {
        let query = format!(r#"artist:"{artist}" album:"{album}""#);
        let reply = self
            .http
            .get(
                &format!("{}/search/album", self.deezer_base),
                &[("q", query.as_str()), ("limit", "3")],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 5.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let albums: DeezerAlbumSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        let mut album_id = None;
        for hit in &albums.data {
            let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
            if hit.id.is_some() && names_match(artist, hit_artist) {
                album_id = hit.id;
                break;
            }
        }
        if album_id.is_none() {
            album_id = albums.data.first().and_then(|hit| hit.id);
        }
        let Some(album_id) = album_id else {
            return Ok(Vec::new());
        };
        let rows = limit.max(4).to_string();
        let reply = self
            .http
            .get(
                &format!("{}/album/{album_id}/tracks", self.deezer_base),
                &[("limit", rows.as_str())],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 5.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let tracks: DeezerAlbumTracksResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        let mut results = Vec::new();
        for hit in &tracks.data {
            if hit.preview.is_empty() {
                continue;
            }
            let position = hit.track_position.or(Some(results.len() as i64 + 1));
            results.push(PreviewTrack {
                title: if hit.title_short.is_empty() {
                    hit.title.clone()
                } else {
                    hit.title_short.clone()
                },
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: hit.preview.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// One track preview via iTunes (v2 `_itunes_track_preview`). iTunes'
    /// top hit can be a cover, so the artist is verified with the loose
    /// name match before a hit is trusted.
    pub async fn itunes_track_preview(
        &self,
        artist: &str,
        track: &str,
    ) -> Result<Option<PreviewTrack>, FetchError> {
        let term = format!("{artist} {track}");
        let reply = self
            .http
            .get(
                &self.itunes_url,
                &[("term", term.as_str()), ("entity", "song"), ("limit", "5")],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(None);
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 60.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: ITunesSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        for hit in &page.results {
            if hit.preview_url.is_empty() {
                continue;
            }
            if !names_match(artist, &hit.artist_name) {
                continue;
            }
            return Ok(Some(PreviewTrack {
                title: hit.track_name.clone(),
                artist_name: hit.artist_name.clone(),
                preview_url: hit.preview_url.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            }));
        }
        Ok(None)
    }

    /// Ordered album previews via iTunes (v2 `_itunes_album_tracks`). Both
    /// the artist and the collection must loosely match, positions fall
    /// back to list order, and the results sort by position.
    pub async fn itunes_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
    ) -> Result<Vec<PreviewTrack>, FetchError> {
        let term = format!("{artist} {album}");
        let reply = self
            .http
            .get(
                &self.itunes_url,
                &[("term", term.as_str()), ("entity", "song"), ("limit", "25")],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 60.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: ITunesSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        let mut results = Vec::new();
        for hit in &page.results {
            if hit.preview_url.is_empty() {
                continue;
            }
            if !names_match(artist, &hit.artist_name) {
                continue;
            }
            if !names_match(album, &hit.collection_name) {
                continue;
            }
            let position = hit.track_number.or(Some(results.len() as i64 + 1));
            results.push(PreviewTrack {
                title: hit.track_name.clone(),
                artist_name: hit.artist_name.clone(),
                preview_url: hit.preview_url.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position,
            });
            if results.len() >= limit {
                break;
            }
        }
        results.sort_by_key(|track| track.position.unwrap_or(0));
        Ok(results)
    }

    /// An artist's popular tracks via Deezer search (v2
    /// `get_artist_top_tracks`). The search over-fetches (`max(limit * 2,
    /// 10)`), non-matching artists are filtered out, and titles dedupe
    /// case-insensitively. Any failure is absence.
    pub async fn get_artist_top_tracks(&self, artist: &str, limit: usize) -> Vec<PreviewTrack> {
        let found = self.artist_top_tracks_leg(artist, limit).await;
        found.unwrap_or_default()
    }

    /// Fallible leg behind `get_artist_top_tracks`, kept public so wiring
    /// can tell a failed fetch from an artist with no previews.
    pub async fn artist_top_tracks_leg(
        &self,
        artist: &str,
        limit: usize,
    ) -> Result<Vec<PreviewTrack>, FetchError> {
        let query = format!(r#"artist:"{artist}""#);
        let rows = (limit * 2).max(10).to_string();
        let reply = self
            .http
            .get(
                &format!("{}/search", self.deezer_base),
                &[("q", query.as_str()), ("limit", rows.as_str())],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(Vec::new());
        }
        if let Some(error) = Self::classify(reply.status, reply.retry_after.as_deref(), 5.0) {
            return Err(error);
        }
        if reply.status != 200 {
            return Err(FetchError::Transport);
        }
        let page: DeezerTrackSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        let mut results = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for hit in &page.data {
            let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
            if !names_match(artist, hit_artist) {
                continue;
            }
            let title = if hit.title_short.is_empty() {
                hit.title.clone()
            } else {
                hit.title_short.clone()
            };
            let key = title.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            results.push(PreviewTrack {
                title,
                artist_name: if hit_artist.is_empty() {
                    artist.to_owned()
                } else {
                    hit_artist.to_owned()
                },
                preview_url: hit.preview.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// A single 30s preview with Deezer-to-iTunes fallback (v2
    /// `get_track_preview`). Any failure on a leg falls through to the next;
    /// when both legs fail the lookup is absence, and enrichment records
    /// the degradation from the legs it can still call directly.
    pub async fn get_track_preview(
        &self,
        artist: &str,
        track: &str,
    ) -> (Option<PreviewTrack>, Option<&'static str>) {
        match self.deezer_track_preview(artist, track).await {
            Ok(Some(found)) => return (Some(found), Some("deezer")),
            Ok(None) => {}
            Err(_) => {}
        }
        match self.itunes_track_preview(artist, track).await {
            Ok(Some(found)) => (Some(found), Some("itunes")),
            _ => (None, None),
        }
    }

    /// Ordered 30s samples of an album's first tracks with Deezer-to-iTunes
    /// fallback (v2 `get_album_preview_tracks`).
    pub async fn get_album_preview_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
    ) -> (Vec<PreviewTrack>, Option<&'static str>) {
        match self.deezer_album_tracks(artist, album, limit).await {
            Ok(tracks) if !tracks.is_empty() => return (tracks, Some("deezer")),
            _ => {}
        }
        match self.itunes_album_tracks(artist, album, limit).await {
            Ok(tracks) if !tracks.is_empty() => (tracks, Some("itunes")),
            _ => (Vec::new(), None),
        }
    }
}

/// Honor a positive `Retry-After` delay; missing, unparsable, or
/// non-positive values yield `None` so the caller falls back to the leg
/// default.
fn retry_after_secs(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
}
