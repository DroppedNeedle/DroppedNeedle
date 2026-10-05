//! Typed LRCLIB reads for optional lyrics.
//!
//! A Rust port of v2's LRCLIB repository, with its API notes verified live
//! on 2026-07-22 and reverified on 2026-08-05. Only the exact `/api/get`
//! lookup feeds projections; `/api/search` results are never promoted
//! automatically, because a common recording returns several plausible
//! candidates.
//!
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! The conservative 1 req/s local ceiling, retries, response
//! caching (7d positive, 6h negative), and request dedup stay with wiring;
//! degradation recording stays with enrichment, which treats "not found"
//! as absence and `Err` as a degraded source.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "lrclib";
/// Live API origin (lrclib_API_NOTES.md).
pub const API_BASE: &str = "https://lrclib.net";
/// Largest response body accepted before the lookup is rejected.
pub const MAX_LYRICS_BYTES: usize = 2 * 1024 * 1024;
/// Largest lyrics text accepted per field.
pub const MAX_LYRICS_CHARACTERS: usize = 1_000_000;

/// What can go wrong on an LRCLIB fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed (retryable, degraded).
    Transport,
    /// The upstream answered 429; the caller backs off this long.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// Any other status, an oversized or undecodable body, or a payload
    /// that fails the completeness gate.
    Unusable,
}

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// An exact-lookup answer on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireLyrics {
    /// LRCLIB lyric id; 0 means the payload has no identity.
    #[serde(default)]
    pub id: i64,
    /// Track title.
    #[serde(default, rename = "trackName")]
    pub track_name: String,
    /// Track artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Album title.
    #[serde(default, rename = "albumName")]
    pub album_name: String,
    /// Track duration in seconds.
    #[serde(default)]
    pub duration: f64,
    /// True when the track has no lyrics at all.
    #[serde(default)]
    pub instrumental: bool,
    /// Plain lyrics text.
    #[serde(default, rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    /// Synced (LRC) lyrics text.
    #[serde(default, rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
}

// ---------------------------------------------------------------------------
// Normalized models
// ---------------------------------------------------------------------------

/// One usable lyrics candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsCandidate {
    /// LRCLIB lyric id.
    pub provider_id: i64,
    /// Track title.
    pub track_name: String,
    /// Track artist.
    pub artist_name: String,
    /// Album title.
    pub album_name: String,
    /// Track duration in seconds.
    pub duration_seconds: f64,
    /// True when the track has no lyrics at all.
    pub instrumental: bool,
    /// Plain lyrics text.
    pub plain_lyrics: Option<String>,
    /// Synced (LRC) lyrics text.
    pub synced_lyrics: Option<String>,
    /// SHA-256 hex of the raw response body, for change detection.
    pub provider_revision: String,
}

/// The outcome of an exact lookup.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LyricsLookup {
    /// Whether usable lyrics (or a confirmed instrumental) came back.
    pub found: bool,
    /// The candidate, present exactly when `found`.
    pub candidate: Option<LyricsCandidate>,
}

impl LyricsLookup {
    /// An empty miss.
    pub fn not_found() -> Self {
        Self {
            found: false,
            candidate: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Pure normalization
// ---------------------------------------------------------------------------

/// Fold one exact-signature field (v2 `lyrics_projection_service._normalized`,
/// honoring the 2026-08-05 reverify note): NFKC, typographic apostrophes to
/// ASCII `'`, collapse whitespace, then casefold. Only the apostrophe forms
/// are relaxed; every other text and duration gate stays strict.
pub fn signature_text(value: &str) -> String {
    use caseless::default_case_fold_str;
    use unicode_normalization::UnicodeNormalization;
    let nfkc: String = value.nfkc().collect();
    let apostrophes: String = nfkc
        .chars()
        .map(|c| match c {
            '\u{2bc}' | '\u{2018}' | '\u{2019}' | '\u{201b}' => '\'',
            _ => c,
        })
        .collect();
    default_case_fold_str(&apostrophes.split_whitespace().collect::<Vec<_>>().join(" "))
}

/// True when two exact-signature fields name the same text.
pub fn signature_matches(left: &str, right: &str) -> bool {
    signature_text(left) == signature_text(right)
}

/// Parse `Retry-After` the way v2 does: a positive value caps at 30s, while
/// a missing, unparsable, or non-positive value waits 2s (v2 `_retry_after`).
pub fn retry_after_secs(value: Option<&str>) -> f64 {
    if let Some(raw) = value
        && let Ok(secs) = raw.parse::<f64>()
        && secs > 0.0
    {
        return secs.min(30.0);
    }
    2.0
}

/// SHA-256 hex of the raw body, the provider revision (v2 `_request_exact`).
pub fn provider_revision(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}

/// Normalize one exact-lookup body (v2 `_request_exact`): reject oversized
/// bodies, decode, enforce the completeness gate (positive id, non-blank
/// track/artist/album, positive duration), reject oversized lyrics text,
/// and treat "no lyrics and not instrumental" as not found rather than an
/// empty candidate.
pub fn normalize_exact(body: &[u8]) -> Result<LyricsLookup, FetchError> {
    if body.len() > MAX_LYRICS_BYTES {
        return Err(FetchError::Unusable);
    }
    let raw: WireLyrics = serde_json::from_slice(body).map_err(|_| FetchError::Unusable)?;
    if raw.id <= 0
        || raw.track_name.trim().is_empty()
        || raw.artist_name.trim().is_empty()
        || raw.album_name.trim().is_empty()
        || raw.duration <= 0.0
    {
        return Err(FetchError::Unusable);
    }
    let plain = raw
        .plain_lyrics
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    let synced = raw
        .synced_lyrics
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned);
    if [plain.as_ref(), synced.as_ref()]
        .into_iter()
        .flatten()
        .any(|text| text.chars().count() > MAX_LYRICS_CHARACTERS)
    {
        return Err(FetchError::Unusable);
    }
    if plain.is_none() && synced.is_none() && !raw.instrumental {
        return Ok(LyricsLookup::not_found());
    }
    Ok(LyricsLookup {
        found: true,
        candidate: Some(LyricsCandidate {
            provider_id: raw.id,
            track_name: raw.track_name.trim().to_owned(),
            artist_name: raw.artist_name.trim().to_owned(),
            album_name: raw.album_name.trim().to_owned(),
            duration_seconds: raw.duration,
            instrumental: raw.instrumental,
            plain_lyrics: plain,
            synced_lyrics: synced,
            provider_revision: provider_revision(body),
        }),
    })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// LRCLIB client. The caller passes the integer duration (v2 rounds the
/// source duration before calling); the exact call never takes a MusicBrainz
/// id, which the endpoint does not accept (lrclib_API_NOTES.md).
pub struct LrclibClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// API origin; defaults to the live base.
    pub base_url: String,
}

impl<'h, H: HttpPort> LrclibClient<'h, H> {
    /// Build a client against the live origin.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            base_url: API_BASE.to_owned(),
        }
    }

    /// Build a client against a scripted or mirrored origin.
    pub fn with_base(http: &'h H, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.to_owned(),
        }
    }

    /// Exact lyrics lookup. 404 is absence; 429 stays actionable with the
    /// capped backoff; 5xx is `Transport` (retriable, like v2's three
    /// attempts); anything else unusual is `Unusable`.
    pub async fn get_exact_lyrics(
        &self,
        track_name: &str,
        artist_name: &str,
        album_name: &str,
        duration_secs: u32,
    ) -> Result<LyricsLookup, FetchError> {
        let duration = duration_secs.to_string();
        let reply = self
            .http
            .get(
                &format!("{}/api/get", self.base_url),
                &[
                    ("track_name", track_name),
                    ("artist_name", artist_name),
                    ("album_name", album_name),
                    ("duration", duration.as_str()),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 404 {
            return Ok(LyricsLookup::not_found());
        }
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()),
            });
        }
        if reply.status >= 500 {
            // Retriable, matching v2's three attempts (max_attempts=3). The
            // retry driver around `ProviderLyrics` is follow-up work: this
            // client only labels the failure, wiring decides the attempts.
            return Err(FetchError::Transport);
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        normalize_exact(&reply.body)
    }
}
