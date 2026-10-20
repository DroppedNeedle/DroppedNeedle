//! Typed LRCLIB reads for optional lyrics.
//!
//! A Rust port of v2's LRCLIB repository, with its API notes verified live
//! on 2026-07-22 and reverified on 2026-08-05. Only the exact `/api/get`
//! lookup feeds projections; `/api/search` results are never promoted
//! automatically, because a common recording returns several plausible
//! candidates.
//!
//! Requests go through the executor at the `lrclib` row, one a second: a
//! volunteer service with no published limit. Response caching (7d
//! positive, 6h negative) stays with wiring. A 404 is absence; every
//! failure is a typed [`ProviderError`].

use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RequestPriority;
use crate::client::{parse_base, payload, unusable_base};
use crate::error::{PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Request, Spec, Url, standard_classify};
use crate::limiter::RateLimiter;

/// Live API origin (lrclib_API_NOTES.md).
pub const API_BASE: &str = "https://lrclib.net";
/// The provider name errors carry, and its rate row.
pub const SOURCE: &str = "lrclib";
/// Largest response body accepted before the lookup is rejected.
pub const MAX_LYRICS_BYTES: usize = 2 * 1024 * 1024;
/// Largest lyrics text accepted per field.
pub const MAX_LYRICS_CHARACTERS: usize = 1_000_000;

/// LRCLIB on the wire: the standard rules. A 429 that names no wait rests
/// two seconds, and no wait past thirty seconds is honoured.
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_LYRICS_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(2)),
    retry_after_cap: Duration::from_secs(30),
    classify: standard_classify,
};

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

/// Fold one exact-signature field, as reverified on 2026-08-05: NFKC,
/// typographic apostrophes to ASCII `'`, collapse whitespace, then casefold.
/// Only the apostrophe forms are relaxed; every other text and duration
/// gate stays strict.
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

/// SHA-256 hex of the raw body, the provider revision.
pub fn provider_revision(body: &[u8]) -> String {
    format!("{:x}", Sha256::digest(body))
}

/// Normalize one exact-lookup body: reject oversized
/// bodies, decode, enforce the completeness gate (positive id, non-blank
/// track/artist/album, positive duration), reject oversized lyrics text,
/// and treat "no lyrics and not instrumental" as not found rather than an
/// empty candidate.
///
/// # Errors
///
/// [`ProviderError::Payload`] for a body that is too large, does not
/// decode, or fails the gates.
pub fn normalize_exact(body: &[u8]) -> Result<LyricsLookup, ProviderError> {
    if body.len() > MAX_LYRICS_BYTES {
        return Err(payload(
            SOURCE,
            PayloadKind::TooLarge,
            "lyrics answer over the size limit",
        ));
    }
    let raw: WireLyrics = serde_json::from_slice(body)
        .map_err(|error| payload(SOURCE, PayloadKind::Decode, &error.to_string()))?;
    if raw.id <= 0
        || raw.track_name.trim().is_empty()
        || raw.artist_name.trim().is_empty()
        || raw.album_name.trim().is_empty()
        || raw.duration <= 0.0
    {
        return Err(payload(
            SOURCE,
            PayloadKind::Contract,
            "lyrics answer without a complete identity",
        ));
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
        return Err(payload(
            SOURCE,
            PayloadKind::Contract,
            "lyrics text over the size limit",
        ));
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
///
/// Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct LrclibClient {
    executor: Executor,
    base: Option<Url>,
}

impl LrclibClient {
    /// The client sending through `executor` to the origin at `base_url`.
    #[must_use]
    pub fn new(executor: Executor, base_url: &str) -> Self {
        Self {
            executor,
            base: parse_base(base_url, SOURCE),
        }
    }

    /// The limiter the client paces through, for a caller that will not
    /// wait out a rest.
    #[must_use]
    pub fn limiter(&self) -> &RateLimiter {
        self.executor.limiter()
    }

    /// Exact lyrics lookup at `priority`. A 404 is absence.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn get_exact_lyrics(
        &self,
        track_name: &str,
        artist_name: &str,
        album_name: &str,
        duration_secs: u32,
        priority: RequestPriority,
    ) -> Result<LyricsLookup, ProviderError> {
        let base = self.base.as_ref().ok_or_else(|| unusable_base(SOURCE))?;
        let duration = duration_secs.to_string();
        let request = Request::get(
            base,
            &["api", "get"],
            &[
                ("track_name", track_name),
                ("artist_name", artist_name),
                ("album_name", album_name),
                ("duration", duration.as_str()),
            ],
        )
        .map_err(|_| unusable_base(SOURCE))?;
        match self.executor.send(request, priority).await {
            Ok(reply) => normalize_exact(reply.bytes()),
            Err(ProviderError::NotFound { .. }) => Ok(LyricsLookup::not_found()),
            Err(error) => Err(error),
        }
    }
}
