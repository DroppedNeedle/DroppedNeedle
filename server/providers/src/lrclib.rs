//! Typed LRCLIB reads for optional lyrics.
//!
//! Only the exact `/api/get` lookup feeds projections; `/api/search` results
//! are never promoted automatically, because a common recording returns
//! several plausible candidates.
//!
//! A record's `lyricsfile` (YAML) is read for word timings only, under a
//! tight budget; when it cannot be used the synced text still stands.
//!
//! Requests go through the executor at the `lrclib` row, one a second: a
//! volunteer service with no published limit. Response caching (7d
//! positive, 6h negative) stays with wiring. A 404 is absence; every
//! failure is a typed [`ProviderError`].

mod models;

use std::panic::AssertUnwindSafe;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::RequestPriority;
use crate::client::{decode, parse_base, payload, unusable_base};
use crate::error::{PayloadKind, ProviderError};
use crate::executor::{Backoff, Executor, Reply, Request, Spec, Url, standard_classify};
use crate::limiter::RateLimiter;

use self::models::{Lyrics, Lyricsfile};

/// Live API origin.
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
    /// Word timings from the record's lyricsfile, when it has them. Rows
    /// cached before this field existed read as `None`.
    #[serde(default)]
    pub word_synced: Option<WordSynced>,
    /// SHA-256 hex of the raw response body, for change detection.
    pub provider_revision: String,
}

/// Lyrics timed word by word, from a record's lyricsfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordSynced {
    /// The timed lines, in the file's order.
    pub lines: Vec<WordSyncedLine>,
}

/// One timed line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordSyncedLine {
    /// The line's text, read when it has no words.
    pub text: String,
    /// Start in milliseconds.
    pub start_ms: i64,
    /// End in milliseconds, kept only on a line with words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
    /// The line's words in order; joined they give its text.
    pub words: Vec<SyncedWord>,
}

/// One timed word or syllable, with any space that follows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncedWord {
    /// The word's text.
    pub text: String,
    /// Start in milliseconds.
    pub start_ms: i64,
    /// End in milliseconds, when given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_ms: Option<i64>,
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

/// Fold one exact-signature field: NFKC, typographic apostrophes to ASCII
/// `'`, collapse whitespace, then casefold. Only the apostrophe forms are
/// relaxed; every other text and duration gate stays strict.
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

/// Normalize one exact-lookup answer: decode it, enforce the completeness
/// gate (positive id, non-blank track/artist/album, positive duration),
/// reject oversized lyrics text, and treat "no lyrics and not instrumental"
/// as not found rather than an empty candidate.
///
/// # Errors
///
/// [`ProviderError::Payload`] for an answer that does not decode, a missing
/// id included, or fails the gates.
pub fn normalize_exact(reply: &Reply) -> Result<LyricsLookup, ProviderError> {
    let raw: Lyrics = decode(reply)?;
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
    let word_synced = word_synced(&raw);
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
            word_synced,
            provider_revision: provider_revision(reply.bytes()),
        }),
    })
}

/// Largest Lyricsfile depth read: the document, its lines, a line, its
/// words, a word, with room to spare.
const LYRICSFILE_MAX_DEPTH: usize = 8;
/// Most YAML nodes read from one Lyricsfile.
const LYRICSFILE_MAX_NODES: usize = 200_000;
/// Most scalar bytes read from one Lyricsfile.
const LYRICSFILE_MAX_SCALAR_BYTES: usize = 2 * 1024 * 1024;

/// The word timings of a record's lyricsfile: only when LRCLIB says it has
/// them, the file is version 1.0, within [`MAX_LYRICS_CHARACTERS`], reads
/// under the YAML budget, and some line has words. Anything else is `None`
/// with a debug line, never a failed lookup: the synced text still stands.
/// `offset_ms` is not read (its meaning is not settled in the draft, and
/// LRCLIB ignores it too).
fn word_synced(raw: &Lyrics) -> Option<WordSynced> {
    if !raw.has_word_sync {
        return None;
    }
    let text = raw.lyricsfile.as_deref()?;
    if text.chars().count() > MAX_LYRICS_CHARACTERS {
        tracing::debug!(
            id = raw.id,
            "lyricsfile over the size limit; word timing left out"
        );
        return None;
    }
    let parsed = std::panic::catch_unwind(AssertUnwindSafe(|| {
        serde_saphyr::from_str_with_options::<Lyricsfile>(text, lyricsfile_options())
    }));
    let file = match parsed {
        Ok(Ok(file)) => file,
        Ok(Err(error)) => {
            let error = capped(&error.to_string(), 160);
            tracing::debug!(id = raw.id, %error, "lyricsfile unreadable; word timing left out");
            return None;
        }
        Err(_) => {
            tracing::debug!(
                id = raw.id,
                "lyricsfile parser panicked; word timing left out"
            );
            return None;
        }
    };
    if file.version != "1.0" {
        let version = capped(&file.version, 16);
        tracing::debug!(id = raw.id, %version, "lyricsfile version not read");
        return None;
    }
    let lines: Vec<WordSyncedLine> = file
        .lines
        .unwrap_or_default()
        .into_iter()
        .map(|line| {
            let words: Vec<SyncedWord> = line
                .words
                .unwrap_or_default()
                .into_iter()
                .map(|word| SyncedWord {
                    text: word.text,
                    start_ms: word.start_ms,
                    end_ms: word.end_ms,
                })
                .collect();
            WordSyncedLine {
                text: line.text,
                start_ms: line.start_ms,
                end_ms: line.end_ms.filter(|_| !words.is_empty()),
                words,
            }
        })
        .collect();
    lines
        .iter()
        .any(|line| !line.words.is_empty())
        .then_some(WordSynced { lines })
}

/// The first `chars` characters of `text`, for a log line.
fn capped(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

/// How a lyricsfile is read: one document, shallow, no aliases or anchors,
/// a node and scalar budget, no includes, duplicate keys refused, and no
/// source snippet in errors.
fn lyricsfile_options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_documents: 1,
            max_depth: LYRICSFILE_MAX_DEPTH,
            max_aliases: 0,
            max_anchors: 0,
            max_nodes: LYRICSFILE_MAX_NODES,
            max_total_scalar_bytes: LYRICSFILE_MAX_SCALAR_BYTES,
            max_inclusion_depth: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        with_snippet: false,
    }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// LRCLIB client. The caller passes the duration rounded to whole seconds.
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
    /// Verified against lrclib.net/api/get on 2026-10-20: tests/fixtures/lrclib/get_not_found.json.
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
            Ok(reply) => normalize_exact(&reply),
            Err(ProviderError::NotFound { .. }) => Ok(LyricsLookup::not_found()),
            Err(error) => Err(error),
        }
    }
}
