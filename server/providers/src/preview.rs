//! 30-second track and album previews via Deezer (primary) and iTunes (fallback),
//! and Deezer artist pictures by Deezer artist id.
//!
//! A Rust port of v2's preview repository. Both APIs are keyless. Deezer
//! matches better (field-scoped query syntax, ordered album track lists);
//! iTunes results are verified against the requested artist because its top
//! hit can be a cover. The Deezer wire shapes, and the dates they were
//! verified on, are in `preview/models.rs`; the iTunes ones are in
//! `itunes/models.rs`.
//!
//! Expiry warning, carried over from the v2 module docs: Deezer preview URLs
//! carry an `hdnea` expiry token. Callers must resolve them just-in-time and
//! never cache the URL long-term; only the enrichment layer decides
//! what, if anything, is cached, and it must treat these URLs as short-lived.
//!
//! Deezer requests go through the executor at the `deezer` row, iTunes
//! requests through the [`ITunesClient`] at the `itunes` row. A 404 on
//! either leg is absence. The composed lookups below are infallible
//! exactly like v2 (a dead preview source is absence, not failure); the
//! per-leg functions stay public and return typed failures.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{Answer, Backoff, Executor, Reply, Request, Spec, Url, standard_classify};
use crate::itunes::ITunesClient;
use crate::itunes::models::SongSearch;

mod models;

use self::models::{AlbumSearch, AlbumTracks, DeezerArtist, DeezerFault, Track, TrackSearch};

/// Live Deezer API origin.
pub const DEEZER_API_BASE: &str = "https://api.deezer.com";
/// The provider name Deezer errors carry, and its rate row.
pub const DEEZER: &str = "deezer";
/// Preview length v2 stamps on every result.
pub const PREVIEW_DURATION_S: u32 = 30;
/// Largest Deezer page read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Deezer on the wire: an error object inside a 200 is read by its code (see
/// `classify`), and a 429 that names no wait rests five seconds (v2's
/// default).
pub static DEEZER_SPEC: Spec = Spec {
    source: DEEZER,
    health_key: DEEZER,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify,
};

/// The Deezer hook over the standard rules. Deezer answers an error with a
/// 200 and an error object, so that body is read by its code first:
///
/// - 800, no data: [`ProviderError::NotFound`], which the lookups read as
///   absence. Seen for an unknown album id on 2026-10-20.
/// - any other code: [`ProviderError::BadRequest`] with the status, never
///   retried. Only 800 has been seen on the wire, so the other codes are
///   refused rather than mapped to a guessed meaning.
///
/// A 429 or a 5xx is standard, whatever its body says.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let status = answer.reply.status();
    if status == 429 || status >= 500 {
        return standard_classify(answer);
    }
    let Ok(envelope) = answer.reply.json::<DeezerFault>() else {
        return standard_classify(answer);
    };
    let Some(error) = envelope.error else {
        return standard_classify(answer);
    };
    Err(match error.code {
        800 => ProviderError::NotFound { provider: DEEZER },
        _ => ProviderError::BadRequest {
            provider: DEEZER,
            status,
        },
    })
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

/// Loose comparison form: lowercase, ASCII alphanumerics only (casefold,
/// then strip everything outside `a-z0-9`).
pub fn norm(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Loose name match: both names must be non-empty, and
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

/// Preview client across the Deezer and iTunes legs. Built once with the
/// providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct PreviewClient {
    deezer: Executor,
    deezer_base: Option<Url>,
    itunes: ITunesClient,
}

/// False for a picture URL with an empty image hash
/// (`/images/artist//1000x1000-...`), the form Deezer's default silhouette is
/// reported to take. Not verified: no artist without a picture was found to
/// record.
fn has_picture(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| {
        url.path_segments().is_some_and(|mut segments| {
            segments.next() == Some("images")
                && segments.next() == Some("artist")
                && segments.next().is_some_and(|hash| !hash.is_empty())
        })
    })
}

/// The title a Deezer track shows: the short title when it has one.
fn deezer_title(hit: &Track) -> String {
    if hit.title_short.is_empty() {
        hit.title.clone()
    } else {
        hit.title_short.clone()
    }
}

/// The preview URL of a Deezer track, when Deezer has one.
fn deezer_preview(hit: &Track) -> Option<&str> {
    hit.preview.as_deref().filter(|url| !url.is_empty())
}

impl PreviewClient {
    /// The client sending Deezer requests through `deezer` to the origin
    /// at `deezer_base`, and iTunes requests through `itunes`.
    #[must_use]
    pub fn new(deezer: Executor, deezer_base: &str, itunes: ITunesClient) -> Self {
        Self {
            deezer,
            deezer_base: parse_base(deezer_base, DEEZER),
            itunes,
        }
    }

    /// One Deezer GET of `segments` under the origin, or `None` on a 404.
    async fn deezer_get(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = self
            .deezer_base
            .as_ref()
            .ok_or_else(|| unusable_base(DEEZER))?;
        let request = Request::get(base, segments, query).map_err(|_| unusable_base(DEEZER))?;
        match self.deezer.send(request, priority).await {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The picture URL of Deezer artist `artist_id`: its 1000 px picture,
    /// `None` when Deezer has no such artist, answers another id, or has no
    /// picture for it.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_artist_picture(
        &self,
        artist_id: u64,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let id = artist_id.to_string();
        let Some(reply) = self
            .deezer_get(&["artist", id.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let artist: DeezerArtist = decode(&reply)?;
        if u64::try_from(artist.id).ok() != Some(artist_id) {
            return Ok(None);
        }
        Ok(artist.picture_xl.filter(|url| has_picture(url)))
    }

    /// One track preview via Deezer's field-scoped search.
    /// The first hit carrying a preview URL wins.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> Result<Option<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}" track:"{track}""#);
        let Some(reply) = self
            .deezer_get(
                &["search"],
                &[("q", query.as_str()), ("limit", "3")],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: TrackSearch = decode(&reply)?;
        Ok(page.data.iter().find_map(|hit| {
            deezer_preview(hit).map(|preview_url| PreviewTrack {
                title: deezer_title(hit),
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: preview_url.to_owned(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: hit.track_position,
            })
        }))
    }

    /// Ordered album previews via Deezer. The
    /// album whose artist loosely matches wins; when no artist matches but
    /// hits exist, the top hit is used (the artist field is sometimes
    /// missing or odd). Tracks without previews are skipped, positions fall
    /// back to list order, and the tracks page over-fetches
    /// (`max(limit, 4)`) so skipping preview-less rows still fills `limit`.
    ///
    /// # Errors
    ///
    /// The typed failure of either call or of its answer.
    pub async fn deezer_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}" album:"{album}""#);
        let Some(reply) = self
            .deezer_get(
                &["search", "album"],
                &[("q", query.as_str()), ("limit", "3")],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let albums: AlbumSearch = decode(&reply)?;
        let album_id = albums
            .data
            .iter()
            .find(|hit| {
                let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
                names_match(artist, hit_artist)
            })
            .or_else(|| albums.data.first())
            .map(|hit| hit.id);
        let Some(album_id) = album_id else {
            return Ok(Vec::new());
        };
        let album_id = album_id.to_string();
        let rows = limit.max(4).to_string();
        let Some(reply) = self
            .deezer_get(
                &["album", album_id.as_str(), "tracks"],
                &[("limit", rows.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let tracks: AlbumTracks = decode(&reply)?;
        let mut results = Vec::new();
        for hit in &tracks.data {
            let Some(preview_url) = deezer_preview(hit) else {
                continue;
            };
            let position = hit.track_position.or(Some(results.len() as i64 + 1));
            results.push(PreviewTrack {
                title: deezer_title(hit),
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: preview_url.to_owned(),
                duration_s: Some(PREVIEW_DURATION_S),
                position,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// One iTunes song search, or `None` on a 404.
    async fn itunes_songs(
        &self,
        term: &str,
        limit: &str,
        priority: RequestPriority,
    ) -> Result<Option<SongSearch>, ProviderError> {
        let Some(reply) = self
            .itunes
            .search(
                &[("term", term), ("entity", "song"), ("limit", limit)],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        decode(&reply).map(Some)
    }

    /// One track preview via iTunes. iTunes'
    /// top hit can be a cover, so the artist is verified with the loose
    /// name match before a hit is trusted.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn itunes_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> Result<Option<PreviewTrack>, ProviderError> {
        let term = format!("{artist} {track}");
        let Some(page) = self.itunes_songs(&term, "5", priority).await? else {
            return Ok(None);
        };
        Ok(page
            .results
            .iter()
            .find(|hit| !hit.preview_url.is_empty() && names_match(artist, &hit.artist_name))
            .map(|hit| PreviewTrack {
                title: hit.track_name.clone(),
                artist_name: hit.artist_name.clone(),
                preview_url: hit.preview_url.clone(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            }))
    }

    /// Ordered album previews via iTunes. Both
    /// the artist and the collection must loosely match, positions fall
    /// back to list order, and the results sort by position.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn itunes_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let term = format!("{artist} {album}");
        let Some(page) = self.itunes_songs(&term, "25", priority).await? else {
            return Ok(Vec::new());
        };
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

    /// An artist's popular tracks via Deezer search.
    /// The search over-fetches (`max(limit * 2,
    /// 10)`), non-matching artists are filtered out, and titles dedupe
    /// case-insensitively. Any failure is absence.
    pub async fn get_artist_top_tracks(
        &self,
        artist: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Vec<PreviewTrack> {
        let found = self.artist_top_tracks_leg(artist, limit, priority).await;
        found.unwrap_or_default()
    }

    /// Fallible leg behind `get_artist_top_tracks`, kept public so wiring
    /// can tell a failed fetch from an artist with no previews.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn artist_top_tracks_leg(
        &self,
        artist: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}""#);
        let rows = (limit * 2).max(10).to_string();
        let Some(reply) = self
            .deezer_get(
                &["search"],
                &[("q", query.as_str()), ("limit", rows.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let page: TrackSearch = decode(&reply)?;
        let mut results = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for hit in &page.data {
            let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
            if !names_match(artist, hit_artist) {
                continue;
            }
            let title = deezer_title(hit);
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
                preview_url: hit.preview.clone().unwrap_or_default(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// A single 30s preview with Deezer-to-iTunes fallback.
    /// Any failure on a leg falls through to the next;
    /// when both legs fail the lookup is absence. Each leg's failure is
    /// recorded by the call that met it.
    pub async fn get_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> (Option<PreviewTrack>, Option<&'static str>) {
        if let Ok(Some(found)) = self.deezer_track_preview(artist, track, priority).await {
            return (Some(found), Some("deezer"));
        }
        match self.itunes_track_preview(artist, track, priority).await {
            Ok(Some(found)) => (Some(found), Some("itunes")),
            _ => (None, None),
        }
    }

    /// Ordered 30s samples of an album's first tracks with Deezer-to-iTunes
    /// fallback.
    pub async fn get_album_preview_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> (Vec<PreviewTrack>, Option<&'static str>) {
        match self
            .deezer_album_tracks(artist, album, limit, priority)
            .await
        {
            Ok(tracks) if !tracks.is_empty() => return (tracks, Some("deezer")),
            _ => {}
        }
        match self
            .itunes_album_tracks(artist, album, limit, priority)
            .await
        {
            Ok(tracks) if !tracks.is_empty() => (tracks, Some("itunes")),
            _ => (Vec::new(), None),
        }
    }
}
