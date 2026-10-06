//! MusicBrainz provider client: search, browse, lookups, and the BrainzMash
//! mirror.
//!
//! This module ports the v2 `musicbrainz_*` repositories plus the BrainzMash
//! transport. Every behavior first observed against the live service keeps
//! its citation inline. Tolerant where the wire is sparse (unknown fields
//! ignored, display fields optional), strict where identity is at stake
//! (missing entity ids fail decoding; a retired MBID counts as its target
//! only after a lookup proves the redirect).
//!
//! Layout:
//! - [`client`]: the [`MusicBrainzClient`] (status semantics, criticality
//!   routing, redirect walking).
//! - [`transport`]: the [`MbTransport`] port and its reqwest adapter.
//! - [`pacing`]: process-wide pacing through the shared provider limiters
//!   plus the BrainzMash cooldown.
//! - [`redirects`]: redirect-hop and BrainzMash URL validation.
//! - [`models`]: the wire shapes.
//!
//! Degradation rides the shared [`DegradationSink`](super::degradation::DegradationSink)
//! under the `musicbrainz` source key. Redirect hops are returned to the
//! caller, never persisted here. Retries live above this client (typed
//! errors carry retry hints); the BrainzMash cooldown is the one exception.

pub mod client;
pub mod models;
pub mod pacing;
pub mod redirects;
pub mod transport;

pub use client::{Lookup, MusicBrainzClient};
pub use models::*;
pub use pacing::{BrainzMashCooldown, MbPacing, MbPacingState};
pub use redirects::{
    BRAINZMASH_HOST, RedirectHop, brainzmash_redirect_path, lookup_redirect_pair,
    official_redirect_hop, validate_brainzmash_path, validate_brainzmash_request_url,
    validate_brainzmash_url,
};
pub use transport::{MbRequest, MbTransport, RawResponse, ReqwestMbTransport, TransportError};

use thiserror::Error;

/// Official MusicBrainz web-service root.
pub const MB_API_BASE: &str = "https://musicbrainz.org/ws/2";
/// The one server-owned BrainzMash origin (v2 `BRAINZMASH_ENDPOINT`).
pub const BRAINZMASH_API_BASE: &str = "https://api.brainzmash.cc/ws/2";
/// Redirect follows per logical lookup (v2 `_BRAINZMASH_MAX_REDIRECTS`).
pub const MAX_REDIRECT_HOPS: usize = 2;
/// BrainzMash cooldown ceiling (v2 `_BRAINZMASH_MAX_COOLDOWN_SECONDS`).
pub const BRAINZMASH_MAX_COOLDOWN_SECS: f64 = 60.0;
/// Largest page MusicBrainz serves for search and browse requests.
pub const MAX_PAGE_LIMIT: u32 = 100;

/// Which upstream answers: official MusicBrainz or a BrainzMash binding.
#[derive(Debug, Clone)]
pub enum MbSource {
    /// Official service (paced at its 1 req/s rule). A base other than
    /// [`MB_API_BASE`] serves public-host aliases and test instances.
    Official {
        /// API root; production uses [`MB_API_BASE`].
        base_url: String,
    },
    /// A self-hosted or community mirror, paced at the rate its owner set.
    Mirror {
        /// API root, ending in `/ws/2`.
        base_url: String,
        /// Requests per second; zero or less means unpaced.
        rate_per_sec: f64,
    },
    /// Community mirror behind the pinned endpoint and lifecycle gate.
    BrainzMash {
        /// False until the active binding validates; requests fail closed.
        binding_valid: bool,
    },
}

impl MbSource {
    /// The official production source.
    pub fn official() -> Self {
        Self::Official {
            base_url: MB_API_BASE.to_owned(),
        }
    }

    /// API root for request building.
    pub fn base_url(&self) -> &str {
        match self {
            Self::Official { base_url } | Self::Mirror { base_url, .. } => base_url,
            Self::BrainzMash { .. } => BRAINZMASH_API_BASE,
        }
    }

    pub(crate) fn is_brainzmash(&self) -> bool {
        matches!(self, Self::BrainzMash { .. })
    }

    /// The source the saved settings select. A "mirror" on a public
    /// MusicBrainz host still obeys the official 1 req/s rule, whatever
    /// rate was typed in.
    pub fn from_settings(settings: &crate::runtime_config::sections::MusicBrainzSettings) -> Self {
        use crate::runtime_config::sections::{MbSourceMode, is_mb_rate_policy_public_host};
        match settings.source_mode {
            MbSourceMode::Brainzmash => Self::BrainzMash {
                binding_valid: crate::settings::musicbrainz::is_brainzmash_active_binding_valid(
                    settings,
                ),
            },
            MbSourceMode::Official => Self::official(),
            MbSourceMode::Mirror | MbSourceMode::Community
                if is_mb_rate_policy_public_host(&settings.api_url) =>
            {
                Self::Official {
                    base_url: settings.api_url.clone(),
                }
            }
            MbSourceMode::Mirror | MbSourceMode::Community => Self::Mirror {
                base_url: settings.api_url.clone(),
                rate_per_sec: settings.rate_limit,
            },
        }
    }
}

/// Reads the configured source at each request, so a source switch in
/// settings reaches long-lived clients without a restart.
pub type SourceFn = std::sync::Arc<dyn Fn() -> MbSource + Send + Sync>;

/// Operation class from the degradation matrix. A dead MusicBrainz fails
/// identity-critical work with a typed error and degrades everything else
/// to a recorded absence (a dead MusicBrainz fails identity-critical work
/// only; v2 grouped search kept the same bucket isolation by returning
/// empty results plus a failure record per dead bucket).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Criticality {
    /// Identity proof: provider death is a typed failure, never silence.
    IdentityCritical,
    /// Display, search acceleration, optional enrichment: provider death
    /// records a degradation and resolves to absence.
    BestEffort,
}

/// Typed MusicBrainz failures. `None`/empty still means absence; these
/// variants mean the provider could not answer or answered badly.
#[derive(Debug, Error, PartialEq)]
pub enum MbError {
    /// Transport dead or 5xx (other than the rate-limit 503): retriable.
    #[error("musicbrainz unavailable: {0}")]
    Unavailable(String),
    /// 429, or 503 which MusicBrainz also uses for rate limiting (v2
    /// `_mb_api_get_attempt` labels 503 "rate limited"). Carries the
    /// honored Retry-After delay when the response gave one.
    #[error("musicbrainz rate limited")]
    RateLimited {
        /// Seconds to wait before retrying, when the response said.
        retry_after_secs: Option<f64>,
    },
    /// Syntactically valid MBID the service rejects, e.g. the all-zero
    /// UUID answered 400 `{"error":"Invalid mbid."}` (live 2026-07-21,
    /// `musicbrainz_MANAGEMENT_API_NOTES.md`).
    #[error("musicbrainz rejected the request as invalid: {0}")]
    InvalidMbid(String),
    /// Other 4xx: the request is wrong, retrying will not help.
    #[error("musicbrainz rejected the request (HTTP {0})")]
    Rejected(u16),
    /// A redirect the client refuses to follow: foreign host, scheme or
    /// port shift, off-`/ws/2` path, or a non-lookup shape such as a
    /// browse URL (v2 `_lookup_redirect_pair` persists lookup-shaped hops
    /// only, since a 301 on `/release-group?artist=` names an artist).
    #[error("musicbrainz redirect rejected: {0}")]
    RedirectRejected(String),
    /// The wire answered 200 but the payload breaks the provider contract
    /// (unparseable body or a missing required identity field). Never
    /// masked as absence: a contract break is a signal, not a miss.
    #[error("musicbrainz contract break: {0}")]
    Contract(String),
    /// Local configuration refuses the request (BrainzMash binding not
    /// valid, unapproved endpoint, bad path). Fail fast, never send.
    #[error("musicbrainz misconfigured: {0}")]
    Misconfigured(String),
}

/// Parse Retry-After in either legal shape: delay seconds or an HTTP date.
/// Delegates to the core [`parse_retry_after`](super::error::parse_retry_after),
/// which covers all three HTTP date shapes where the old local parser read
/// IMF-fixdate only. Mirrors v2 `_parse_retry_after_seconds`: unparseable,
/// non-finite, and negative values yield `None`; valid values clamp to 60
/// seconds.
pub fn parse_retry_after_secs(value: Option<&str>) -> Option<f64> {
    super::error::parse_retry_after(value).map(|delay| delay.as_secs_f64())
}

/// True for the `8-4-4-4-12` hex MBID shape (v2 `is_valid_mbid` rejects
/// `unknown_*` sentinels the same way: not this shape, not valid).
pub fn is_valid_mbid(value: &str) -> bool {
    let trimmed = value.trim();
    if trimmed.len() != 36 {
        return false;
    }
    for (index, byte) in trimmed.bytes().enumerate() {
        let is_dash_slot = matches!(index, 8 | 13 | 18 | 23);
        if is_dash_slot != (byte == b'-') {
            return false;
        }
        if !is_dash_slot && !byte.is_ascii_hexdigit() {
            return false;
        }
    }
    true
}

/// Lowercase-and-trim identity key for MBIDs (v2 `normalize_mb_id`).
pub fn normalize_mb_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Escape user text before placing it inside a Lucene field phrase (v2
/// `escape_lucene_phrase`).
pub fn escape_lucene_phrase(value: &str) -> String {
    const RESERVED: [char; 19] = [
        '+', '-', '&', '|', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\',
        '/',
    ];
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if RESERVED.contains(&character) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Release query, live-verified against MusicBrainz WS/2 on 2026-08-13 (v2
/// `build_release_search_query`).
pub fn build_release_search_query(title: &str, artist: &str) -> String {
    let mut query = format!(r#"release:"{}""#, escape_lucene_phrase(title));
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    query
}

/// Release-group query, live-verified against MusicBrainz WS/2 on 2026-08-13
/// (v2 `build_release_group_search_query`).
pub fn build_release_group_search_query(title: &str, artist: &str) -> String {
    let escaped = escape_lucene_phrase(title);
    let mut query = format!(r#"(releasegroup:"{escaped}" OR release:"{escaped}")"#);
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    query
}

/// Recording query using the same verified Lucene field escaping (v2
/// `build_recording_search_query`).
pub fn build_recording_search_query(title: &str, artist: &str) -> String {
    format!(
        r#"recording:"{}" AND artist:"{}""#,
        escape_lucene_phrase(title),
        escape_lucene_phrase(artist)
    )
}

/// Search-result score from either observed key (v2 `get_score`):
/// `score` first, then `ext:score`, defaulting to zero.
pub fn hit_score(score: Option<i64>, ext_score: Option<i64>) -> i64 {
    score.or(ext_score).unwrap_or(0)
}

/// First artist-credit display name: credited name, then the artist's
/// canonical name (v2 `extract_artist_name`).
pub fn credit_display_name(credit: &[ArtistCreditName]) -> Option<&str> {
    credit.first().and_then(|entry| {
        if !entry.name.is_empty() {
            Some(entry.name.as_str())
        } else if !entry.artist.name.is_empty() {
            Some(entry.artist.name.as_str())
        } else {
            None
        }
    })
}

/// Leading year of an MB date, tolerating partial dates (v2 `parse_year`).
pub fn parse_year(date: Option<&str>) -> Option<i32> {
    let year = date?.split('-').next()?;
    if !year.is_empty() && year.bytes().all(|byte| byte.is_ascii_digit()) {
        year.parse().ok()
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Ranking: same-recording release-group choice. Ranking never substitutes
// one recording MBID for another; it only orders the release groups already
// attached to that recording (live 2026-07-20, `musicbrainz_API_NOTES.md`).
// ---------------------------------------------------------------------------

/// Secondary types that change the version enough to deprioritize (v2
/// `_VERSION_CHANGING_TYPES`).
const VERSION_CHANGING_TYPES: [&str; 5] = ["live", "remix", "dj-mix", "mixtape/street", "demo"];

/// Same-recording release-group rank, lower winning: Official flag,
/// version-change flag, primary-type order, secondary-type penalty, date,
/// then MBID for determinism.
pub type ReleaseGroupRank = (u8, u8, u8, u8, String, String);

/// Stable same-recording rank where lower values win (v2
/// `recording_release_group_rank`): Official first, then version-keeping,
/// then Album > EP > Single > other, then type-less, then earliest date,
/// then MBID for determinism.
pub fn recording_release_group_rank(
    release_status: Option<&str>,
    secondary_types: &[String],
    primary_type: Option<&str>,
    release_date: Option<&str>,
    release_group_mbid: &str,
) -> ReleaseGroupRank {
    let official =
        u8::from(!release_status.is_some_and(|status| status.eq_ignore_ascii_case("official")));
    let lowered: Vec<String> = secondary_types
        .iter()
        .map(|entry| entry.to_ascii_lowercase())
        .collect();
    let version_changed = u8::from(
        lowered
            .iter()
            .any(|entry| VERSION_CHANGING_TYPES.contains(&entry.as_str())),
    );
    let primary = match primary_type.unwrap_or("").to_ascii_lowercase().as_str() {
        "album" => 0,
        "ep" => 1,
        "single" => 2,
        _ => 3,
    };
    let secondary_penalty = u8::from(!secondary_types.is_empty());
    (
        official,
        version_changed,
        primary,
        secondary_penalty,
        release_date.unwrap_or("9999-99-99").to_owned(),
        release_group_mbid.to_owned(),
    )
}

/// Best release group attached to one recording, plus its rank. Returns
/// `None` when the recording carries no usable group. Never looks beyond
/// this recording's own releases: cross-recording substitution is the
/// caller's bug, and this signature makes it unwritable.
pub fn best_release_group_for_recording(
    releases: &[RecordingRelease],
) -> Option<(&ReleaseGroupRef, ReleaseGroupRank)> {
    releases
        .iter()
        .filter_map(|release| {
            let group = release.release_group.as_ref()?;
            if group.id.is_empty() {
                return None;
            }
            let rank = recording_release_group_rank(
                release.status.as_deref(),
                &group.secondary_types,
                group.primary_type.as_deref(),
                release.date.as_deref(),
                &group.id,
            );
            Some((group, rank))
        })
        .min_by(|left, right| left.1.cmp(&right.1))
}
