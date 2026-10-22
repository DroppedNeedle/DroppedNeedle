//! MusicBrainz provider client: search, browse, lookups, and the BrainzMash
//! mirror.
//!
//! Every behavior first observed against the live service keeps its
//! citation inline. Tolerant where the wire is sparse (unknown fields
//! ignored, display fields optional), strict where identity is at stake
//! (missing entity ids fail decoding; a retired MBID counts as its target
//! only after a lookup proves the redirect).
//!
//! Layout:
//! - [`client`]: the [`MusicBrainzClient`] with its three executors
//!   (official, BrainzMash, a self-hosted mirror) and the
//!   [`MusicBrainzView`] callers read through (status semantics,
//!   criticality routing, redirect walking).
//! - [`redirects`]: redirect-hop and BrainzMash URL validation.
//! - [`models`]: the wire shapes.
//!
//! Every request goes through the provider executor, which paces, retries,
//! cools down and records health. Redirect hops are returned to the
//! caller, never persisted here.

pub mod client;
pub mod models;
pub mod redirects;

pub use client::{Lookup, MusicBrainzClient, MusicBrainzView};
pub use models::*;
pub use redirects::{
    BRAINZMASH_HOST, RedirectHop, brainzmash_redirect_path, lookup_redirect_pair,
    official_redirect_hop, validate_brainzmash_path, validate_brainzmash_request_url,
};

/// The provider name MusicBrainz failures carry.
pub const SOURCE: &str = "musicbrainz";

/// Official MusicBrainz web-service root.
pub const MB_API_BASE: &str = "https://musicbrainz.org/ws/2";
/// The one server-owned BrainzMash origin.
pub const BRAINZMASH_API_BASE: &str = "https://api.brainzmash.cc/ws/2";
/// Redirect follows per logical lookup.
pub const MAX_REDIRECT_HOPS: usize = 2;
/// BrainzMash cooldown ceiling.
pub const BRAINZMASH_MAX_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);
/// Largest MusicBrainz answer read. A box-set release with every medium,
/// track and credit comes back whole and can pass 4 MiB.
pub const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
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
        /// API root; production uses [`BRAINZMASH_API_BASE`].
        base_url: String,
        /// False until the active binding validates; requests fail closed.
        binding_valid: bool,
    },
    /// The saved MusicBrainz settings do not read. Never a default source:
    /// every request fails as misconfigured and nothing is sent.
    Unreadable,
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
            Self::Official { base_url }
            | Self::Mirror { base_url, .. }
            | Self::BrainzMash { base_url, .. } => base_url,
            Self::Unreadable => "",
        }
    }

    pub(crate) fn is_brainzmash(&self) -> bool {
        matches!(self, Self::BrainzMash { .. })
    }

    /// The source the saved settings select. A "mirror" on a public
    /// MusicBrainz host still obeys the official 1 req/s rule, whatever
    /// rate was typed in. `official` and `brainzmash` are the roots of the
    /// two built-in sources.
    pub fn from_settings(
        settings: &droppedneedle_kernel::runtime_config::sections::MusicBrainzSettings,
        official: &str,
        brainzmash: &str,
    ) -> Self {
        use droppedneedle_kernel::runtime_config::sections::{
            MbSourceMode, is_mb_rate_policy_public_host,
        };
        match settings.source_mode {
            MbSourceMode::Brainzmash => Self::BrainzMash {
                base_url: brainzmash.to_owned(),
                binding_valid: settings.brainzmash_binding_valid(),
            },
            MbSourceMode::Official => Self::Official {
                base_url: official.to_owned(),
            },
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

/// The source the saved settings select, read at each request so a source
/// switch reaches long-lived clients without a restart.
#[derive(Clone)]
pub struct LiveSource {
    settings: droppedneedle_kernel::runtime_config::Live<
        droppedneedle_kernel::runtime_config::sections::MusicBrainzSettings,
    >,
    official: String,
    brainzmash: String,
}

impl LiveSource {
    /// The source `settings` selects, with `official` and `brainzmash` as
    /// the roots of the two built-in sources.
    pub fn new(
        settings: droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::MusicBrainzSettings,
        >,
        official: &str,
        brainzmash: &str,
    ) -> Self {
        Self {
            settings,
            official: official.to_owned(),
            brainzmash: brainzmash.to_owned(),
        }
    }

    /// The source now; [`MbSource::Unreadable`] while the settings do not
    /// read.
    pub fn current(&self) -> MbSource {
        self.settings
            .read()
            .map_or(MbSource::Unreadable, |settings| {
                MbSource::from_settings(&settings, &self.official, &self.brainzmash)
            })
    }

    /// What the cache key of an answer from this source carries: the
    /// saved source and its generation. `None` while the settings do not
    /// read.
    pub fn cache_namespace(&self) -> Option<String> {
        self.settings
            .read()
            .map(|settings| format!("{}:g{}", settings.source_id, settings.generation))
    }
}

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

/// True for the `8-4-4-4-12` hex MBID shape. `unknown_*` sentinels are not
/// this shape, so they are not valid.
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

/// Lowercase-and-trim identity key for MBIDs.
pub fn normalize_mb_id(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

/// Escape user text before placing it inside a Lucene field phrase.
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

/// Release query, live-verified against MusicBrainz WS/2 on 2026-08-13.
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

/// Release query for identifying an album: [`build_release_search_query`]
/// plus optional boosts for the local track count and an artist id (the
/// Various Artists id for compilations). The boosts only reorder hits:
/// each sits in an OR with the required title clause, so a release with
/// bonus tracks or another credit still comes back.
pub fn build_release_match_query(
    title: &str,
    artist: &str,
    track_count: Option<usize>,
    artist_id: Option<&str>,
) -> String {
    let clause = format!(r#"release:"{}""#, escape_lucene_phrase(title));
    match_query(&clause, artist, track_count, artist_id)
}

/// [`build_release_match_query`] with the title as loose words rather
/// than a phrase: the index ORs words by default, so a release whose
/// title lacks a word or orders them differently still ranks. The words
/// "and", "or" and "not" are left out in any case, so none reads as an
/// operator. `None` when no word is left.
pub fn build_release_words_query(
    title: &str,
    artist: &str,
    track_count: Option<usize>,
    artist_id: Option<&str>,
) -> Option<String> {
    let words: Vec<String> = title
        .split_whitespace()
        .filter(|word| !matches!(word.to_lowercase().as_str(), "and" | "or" | "not"))
        .map(escape_lucene_phrase)
        .collect();
    if words.is_empty() {
        return None;
    }
    let clause = format!("release:({})", words.join(" "));
    Some(match_query(&clause, artist, track_count, artist_id))
}

/// The title clause, the artist phrase when there is one, and the boosts
/// ORed with the title clause.
fn match_query(
    title_clause: &str,
    artist: &str,
    track_count: Option<usize>,
    artist_id: Option<&str>,
) -> String {
    let mut query = title_clause.to_owned();
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    let mut boosts = Vec::new();
    if let Some(count) = track_count.filter(|count| *count > 0) {
        boosts.push(format!("tracks:{count}^2"));
    }
    if let Some(id) = artist_id.map(str::trim).filter(|id| !id.is_empty()) {
        boosts.push(format!(r#"arid:"{}"^2"#, escape_lucene_phrase(id)));
    }
    if boosts.is_empty() {
        return query;
    }
    format!("{query} AND ({} OR {title_clause})", boosts.join(" OR "))
}

/// Recording query: the recording title as a phrase and, unless blank,
/// the artist as a phrase.
pub fn build_recording_query(title: &str, artist: &str) -> String {
    let mut query = format!(r#"recording:"{}""#, escape_lucene_phrase(title));
    if !artist.is_empty() {
        query.push_str(&format!(
            r#" AND artist:"{}""#,
            escape_lucene_phrase(artist)
        ));
    }
    query
}

/// Release-group tag query: the tag as
/// written, boosted, OR'd with its common spelling variants (hyphen and
/// space swapped, `&` and `and` swapped), each a quoted `tag:` phrase.
pub fn build_tag_query(tag: &str) -> String {
    let normalize = |value: &str| {
        value
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let base = normalize(tag);
    if base.is_empty() {
        return r#"tag:""^3"#.to_owned();
    }
    let mut variants = vec![base.clone()];
    let mut add = |value: String| {
        let value = normalize(&value);
        if !value.is_empty() && !variants.contains(&value) {
            variants.push(value);
        }
    };
    add(base.replace('-', " "));
    add(base.replace(' ', "-"));
    if base.contains('&') {
        add(base.replace('&', " and "));
        add(base.replace('&', " "));
    }
    if base.contains(" and ") {
        add(base.replace(" and ", " & "));
        add(base.replace(" and ", " "));
    }
    variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            let escaped = variant.replace('\\', "\\\\").replace('"', "\\\"");
            let boost = if index == 0 { "^3" } else { "^2" };
            format!(r#"tag:"{escaped}"{boost}"#)
        })
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Search-result score from either observed key:
/// `score` first, then `ext:score`, defaulting to zero.
pub fn hit_score(score: Option<i64>, ext_score: Option<i64>) -> i64 {
    score.or(ext_score).unwrap_or(0)
}

/// First artist-credit display name: credited name, then the artist's
/// canonical name.
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

/// Leading year of an MB date, tolerating partial dates.
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

/// Secondary types that change the version enough to deprioritize.
const VERSION_CHANGING_TYPES: [&str; 5] = ["live", "remix", "dj-mix", "mixtape/street", "demo"];

/// Same-recording release-group rank, lower winning: Official flag,
/// version-change flag, primary-type order, secondary-type penalty, date,
/// then MBID for determinism.
pub type ReleaseGroupRank = (u8, u8, u8, u8, String, String);

/// Stable same-recording rank where lower values win:
/// Official first, then version-keeping,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_query_boosts_without_filtering() {
        assert_eq!(
            build_release_match_query("Hits", "", None, None),
            r#"release:"Hits""#
        );
        assert_eq!(
            build_release_match_query("Hits", "", Some(12), Some("va-id")),
            r#"release:"Hits" AND (tracks:12^2 OR arid:"va\-id"^2 OR release:"Hits")"#
        );
        assert_eq!(
            build_release_words_query("Court  AND (Crimson)", "King Crimson", Some(5), None)
                .as_deref(),
            Some(
                r#"release:(Court \(Crimson\)) AND artist:"King Crimson" AND (tracks:5^2 OR release:(Court \(Crimson\)))"#
            )
        );
        assert_eq!(build_release_words_query(" and ", "", None, None), None);
        assert_eq!(
            build_recording_query("Tuesday Morning", "Paul Kossoff"),
            r#"recording:"Tuesday Morning" AND artist:"Paul Kossoff""#
        );
    }
}
