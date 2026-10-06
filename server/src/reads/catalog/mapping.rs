//! Pure rules ported from v2's artist, album and purchase helpers: release
//! type filtering, external links, edition ranking, tracklist extraction,
//! and the store table behind "where to buy".

use std::collections::{BTreeMap, HashSet};

use crate::providers::degradation::{DegradationContext, IntegrationStatus};
use crate::providers::musicbrainz::{ArtistCreditName, MbRelease, Relation};

use super::models::{
    AlbumTrack, ExternalLink, PurchaseKind, PurchaseLink, ServiceStatus, SourceStatus,
};

/// Artists that never headline search or pages: Various Artists, `/v/`
/// and `[unknown]` (v2 `FILTERED_ARTIST_MBIDS`).
pub const FILTERED_ARTIST_MBIDS: [&str; 3] = [
    "89ad4ac3-39f7-470e-963a-56509c546377",
    "41ece0f7-91f6-4c87-982c-3a39c5a02586",
    "125ec42a-7229-4250-afc5-e057484327fe",
];

/// Names filtered the same way (v2 `FILTERED_ARTIST_NAMES`).
const FILTERED_ARTIST_NAMES: [&str; 3] = ["various artists", "[unknown]", "/v/"];

/// Secondary types hidden when no secondary filter applies (v2's default
/// exclusions).
const DEFAULT_EXCLUDED_SECONDARY: [&str; 7] = [
    "compilation",
    "live",
    "remix",
    "soundtrack",
    "dj-mix",
    "mixtape/street",
    "demo",
];

/// Whether an artist search hit is a placeholder artist.
pub fn is_filtered_artist(mbid: &str, name: &str) -> bool {
    FILTERED_ARTIST_MBIDS.contains(&mbid.trim().to_ascii_lowercase().as_str())
        || FILTERED_ARTIST_NAMES.contains(&name.trim().to_lowercase().as_str())
}

/// Lowercase, trimmed set of preference values.
pub fn type_set(values: &[String]) -> HashSet<String> {
    values
        .iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect()
}

/// The release-type policy (v2 `should_include_release`). A primary filter
/// keeps only listed primary types. A secondary filter keeps release groups
/// with a listed secondary type, and groups with none only when `studio`
/// is listed. Without a secondary filter, the default exclusions apply
/// unless the caller turned them off.
pub fn should_include_release(
    primary_type: Option<&str>,
    secondary_types: &[String],
    included_primary: Option<&HashSet<String>>,
    included_secondary: Option<&HashSet<String>>,
    default_exclusions: bool,
) -> bool {
    if let Some(primary_filter) = included_primary {
        let primary = primary_type.unwrap_or("").trim().to_lowercase();
        if !primary_filter.contains(&primary) {
            return false;
        }
    }
    let secondary: HashSet<String> = secondary_types
        .iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| !value.is_empty())
        .collect();
    match included_secondary {
        None => {
            !default_exclusions
                || secondary
                    .iter()
                    .all(|value| !DEFAULT_EXCLUDED_SECONDARY.contains(&value.as_str()))
        }
        Some(filter) if secondary.is_empty() => filter.contains("studio"),
        Some(filter) => secondary.iter().any(|value| filter.contains(value)),
    }
}

/// `Album + Live, Compilation` style label for search hits.
pub fn type_info(primary_type: Option<&str>, secondary_types: &[String]) -> Option<String> {
    let primary = primary_type.unwrap_or("");
    if secondary_types.is_empty() {
        return (!primary.is_empty()).then(|| primary.to_owned());
    }
    Some(format!("{primary} + {}", secondary_types.join(", ")))
}

/// Leading year of a MusicBrainz date.
pub fn year_of(date: Option<&str>) -> Option<i32> {
    crate::providers::musicbrainz::parse_year(date)
}

/// First credited artist: credited name (canonical name as fallback) and
/// MBID (v2 `extract_artist_info`).
pub fn first_credit(credit: &[ArtistCreditName]) -> (String, String) {
    match credit.first() {
        Some(entry) => {
            let name = if entry.name.is_empty() {
                entry.artist.name.clone()
            } else {
                entry.name.clone()
            };
            let name = if name.is_empty() {
                "Unknown Artist".to_owned()
            } else {
                name
            };
            (name, entry.artist.id.clone())
        }
        None => ("Unknown Artist".to_owned(), String::new()),
    }
}

/// The whole credit as printed: each name plus its join phrase.
pub fn full_credit(credit: &[ArtistCreditName]) -> String {
    let mut text = String::new();
    for entry in credit {
        let name = if entry.name.is_empty() {
            &entry.artist.name
        } else {
            &entry.name
        };
        text.push_str(name);
        text.push_str(&entry.joinphrase);
    }
    text.trim().to_owned()
}

/// URL host patterns and the label and category they map to.
const PLATFORM_PATTERNS: [(&str, &str, &str); 17] = [
    ("instagram.com", "Instagram", "social"),
    ("twitter.com", "Twitter", "social"),
    ("x.com", "Twitter", "social"),
    ("facebook.com", "Facebook", "social"),
    ("youtube.com", "YouTube", "music"),
    ("youtu.be", "YouTube", "music"),
    ("spotify.com", "Spotify", "music"),
    ("deezer.com", "Deezer", "music"),
    ("apple.com/music", "Apple Music", "music"),
    ("music.apple.com", "Apple Music", "music"),
    ("tidal.com", "Tidal", "music"),
    ("amazon.com", "Amazon", "music"),
    ("bandcamp.com", "Bandcamp", "music"),
    ("soundcloud.com", "SoundCloud", "music"),
    ("last.fm", "Last.fm", "info"),
    ("lastfm.", "Last.fm", "info"),
    ("wikipedia.org", "Wikipedia", "info"),
];

/// Relationship types and the label and category they map to when no URL
/// pattern matched.
const LINK_TYPE_LABELS: [(&str, &str, &str); 9] = [
    ("official homepage", "Official Website", "info"),
    ("wikipedia", "Wikipedia", "info"),
    ("last.fm", "Last.fm", "info"),
    ("bandcamp", "Bandcamp", "music"),
    ("youtube", "YouTube", "music"),
    ("soundcloud", "SoundCloud", "music"),
    ("instagram", "Instagram", "social"),
    ("twitter", "Twitter", "social"),
    ("facebook", "Facebook", "social"),
];

/// Labels shown on artist pages (v2 `_ALLOWED_LABELS`).
const ALLOWED_LABELS: [&str; 14] = [
    "Spotify",
    "Apple Music",
    "YouTube",
    "Bandcamp",
    "SoundCloud",
    "Deezer",
    "Tidal",
    "Amazon",
    "Instagram",
    "Twitter",
    "Facebook",
    "Official Website",
    "Wikipedia",
    "Last.fm",
];

/// Known links from an artist's URL relationships, one per label, in
/// MusicBrainz order (v2 `extract_external_links`).
pub fn external_links(relations: &[Relation]) -> Vec<ExternalLink> {
    let mut links = Vec::new();
    let mut seen = HashSet::new();
    for relation in relations {
        let Some(url) = relation.url.as_ref().map(|url| url.resource.trim()) else {
            continue;
        };
        if url.is_empty() {
            continue;
        }
        let lowered = url.to_lowercase();
        let (label, category) = PLATFORM_PATTERNS
            .iter()
            .find(|(pattern, _, _)| lowered.contains(pattern))
            .map(|(_, label, category)| (*label, *category))
            .or_else(|| {
                LINK_TYPE_LABELS
                    .iter()
                    .find(|(rel_type, _, _)| *rel_type == relation.rel_type)
                    .map(|(_, label, category)| (*label, *category))
            })
            .unwrap_or(("", "other"));
        if !ALLOWED_LABELS.contains(&label) || !seen.insert(label) {
            continue;
        }
        links.push(ExternalLink {
            link_type: relation.rel_type.clone(),
            url: url.to_owned(),
            label: label.to_owned(),
            category: category.to_owned(),
        });
    }
    links
}

/// The Wikidata id and the first Wikipedia-or-Wikidata URL from an
/// artist's URL relationships (v2 `extract_wiki_info`).
pub fn wiki_info(relations: &[Relation]) -> (Option<String>, Option<String>) {
    let mut wikidata_id = None;
    let mut wiki_url = None;
    for relation in relations {
        let Some(url) = relation.url.as_ref().map(|url| url.resource.as_str()) else {
            continue;
        };
        if relation.rel_type == "wikidata" && wikidata_id.is_none() {
            wikidata_id = crate::providers::wikidata::extract_wikidata_id(url);
        }
        if (relation.rel_type == "wikipedia" || relation.rel_type == "wikidata")
            && wiki_url.is_none()
        {
            wiki_url = Some(url.to_owned());
        }
    }
    (wikidata_id, wiki_url)
}

/// A release group's releases ranked the way v2 picks a display edition:
/// official releases (or all, when none are official), worldwide releases
/// first, physical packaging last, then by id for stability.
pub fn ranked_releases(releases: &[MbRelease]) -> Vec<&MbRelease> {
    let official: Vec<&MbRelease> = releases
        .iter()
        .filter(|release| release.status.as_deref() == Some("Official"))
        .collect();
    let mut ranked = if official.is_empty() {
        releases.iter().collect()
    } else {
        official
    };
    ranked.sort_by(|left, right| {
        release_rank(left)
            .cmp(&release_rank(right))
            .then_with(|| left.id.cmp(&right.id))
    });
    ranked
}

fn release_rank(release: &MbRelease) -> u8 {
    let packaging = release.packaging.as_deref().unwrap_or("").to_lowercase();
    if release
        .country
        .as_deref()
        .unwrap_or("")
        .eq_ignore_ascii_case("XW")
    {
        0
    } else if ["vinyl", "cassette", "gatefold"]
        .iter()
        .any(|keyword| packaging.contains(keyword))
    {
        2
    } else {
        1
    }
}

/// Tracks across all media of one release.
pub fn media_track_count(release: &MbRelease) -> u32 {
    release
        .media
        .iter()
        .map(|medium| medium.track_count.unwrap_or(0))
        .sum()
}

/// The ranked release whose track count is closest to the library's file
/// count, keeping rank order on ties (v2 `_closest_release_id`).
pub fn closest_release(ranked: &[&MbRelease], file_count: u32) -> Option<String> {
    ranked
        .iter()
        .enumerate()
        .filter_map(|(rank, release)| {
            let count = media_track_count(release);
            (count > 0).then(|| (count.abs_diff(file_count), rank, release.id.clone()))
        })
        .min()
        .map(|(_, _, id)| id)
}

/// The tracklist of one release and its total length in milliseconds
/// (v2 `extract_tracks`).
pub fn extract_tracks(release: &MbRelease) -> (Vec<AlbumTrack>, u64) {
    let mut tracks = Vec::new();
    let mut total = 0u64;
    for (index, medium) in release.media.iter().enumerate() {
        let disc_number = medium
            .position
            .unwrap_or(u32::try_from(index + 1).unwrap_or(1));
        for track in &medium.tracks {
            let length = track.length_ms();
            total += length.unwrap_or(0);
            let position = track
                .position
                .or_else(|| {
                    track
                        .number
                        .as_deref()
                        .and_then(|number| number.parse().ok())
                })
                .unwrap_or(0);
            tracks.push(AlbumTrack {
                position,
                title: track.display_title().unwrap_or("Unknown").to_owned(),
                disc_number,
                length,
                recording_id: track
                    .recording
                    .as_ref()
                    .map(|recording| recording.id.clone()),
                release_track_id: Some(track.id.clone()),
                media_format: medium.format.clone(),
            });
        }
    }
    (tracks, total)
}

/// The first label name on a release.
pub fn first_label(release: &MbRelease) -> Option<String> {
    release
        .label_info
        .iter()
        .find_map(|info| info.label.as_ref()?.name.clone())
}

/// Strip HTML tags and the trailing "Read more on Last.fm" link from a
/// Last.fm biography (v2 `clean_lastfm_bio`). Empty text reads as `None`.
pub fn clean_lastfm_bio(text: &str) -> Option<String> {
    let mut plain = String::with_capacity(text.len());
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => plain.push(character),
            _ => {}
        }
    }
    let plain = plain
        .replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">");
    let mut trimmed = plain.trim_end().to_owned();
    for suffix in ["read more on last.fm.", "read more on last.fm"] {
        if trimmed.to_lowercase().ends_with(suffix) {
            let cut = trimmed.len() - suffix.len();
            trimmed.truncate(cut);
            trimmed = trimmed.trim_end().to_owned();
            break;
        }
    }
    (!trimmed.is_empty()).then_some(trimmed)
}

/// The degraded sources of one page build, for `service_status`.
pub fn service_status(context: &DegradationContext) -> ServiceStatus {
    let summary: BTreeMap<String, SourceStatus> = context
        .degraded_summary()
        .into_iter()
        .filter_map(|(source, status)| match status {
            IntegrationStatus::Ok => None,
            IntegrationStatus::Degraded => Some((source.to_owned(), SourceStatus::Degraded)),
            IntegrationStatus::Error => Some((source.to_owned(), SourceStatus::Error)),
        })
        .collect();
    (!summary.is_empty()).then_some(summary)
}

// ---------------------------------------------------------------------------
// Purchase links (v2 `get_it_service`)
// ---------------------------------------------------------------------------

/// Release relationship types that are purchase or download links.
pub const RELEASE_STORE_RELATIONS: [&str; 4] = [
    "purchase for download",
    "purchase for mail-order",
    "download for free",
    "amazon asin",
];

/// Artist relationship types that are the artist's own store pages.
pub const ARTIST_STORE_RELATIONS: [&str; 3] = [
    "bandcamp",
    "purchase for download",
    "purchase for mail-order",
];

/// Bandcamp first, specialist stores next, large storefronts last.
const STORE_ORDER: [&str; 8] = [
    "bandcamp",
    "qobuz",
    "beatport",
    "hdtracks",
    "junodownload",
    "7digital",
    "itunes",
    "amazon",
];

/// Display name for a known store key.
pub fn store_label(store: &str) -> Option<&'static str> {
    Some(match store {
        "bandcamp" => "Bandcamp",
        "qobuz" => "Qobuz",
        "beatport" => "Beatport",
        "hdtracks" => "HDtracks",
        "junodownload" => "Juno Download",
        "7digital" => "7digital",
        "itunes" => "iTunes / Apple Music",
        "amazon" => "Amazon",
        _ => return None,
    })
}

/// Host of a URL, lowercase, without credentials or port.
fn host_of(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit('@').next().unwrap_or("");
    host.split(':').next().unwrap_or("").to_lowercase()
}

/// Store key for a URL (v2 `_store_for`).
pub fn store_for(url: &str) -> &'static str {
    let host = host_of(url);
    if host.ends_with("bandcamp.com") {
        "bandcamp"
    } else if host.contains("qobuz.com") {
        "qobuz"
    } else if host.contains("beatport.com") {
        "beatport"
    } else if host.contains("hdtracks.com") {
        "hdtracks"
    } else if host.contains("junodownload.com") {
        "junodownload"
    } else if host.contains("7digital.com") {
        "7digital"
    } else if matches!(
        host.as_str(),
        "music.apple.com" | "itunes.apple.com" | "geo.music.apple.com"
    ) {
        "itunes"
    } else if format!(".{host}").contains(".amazon.") || host.starts_with("amazon.") {
        "amazon"
    } else {
        "other"
    }
}

/// One link from a URL relationship, when it is an allowed store type,
/// still current, and an http(s) URL.
pub fn purchase_link(relation: &Relation, allowed: &[&str]) -> Option<PurchaseLink> {
    if !allowed.contains(&relation.rel_type.as_str()) || relation.ended {
        return None;
    }
    let url = relation.url.as_ref()?.resource.trim();
    if !url.starts_with("http") {
        return None;
    }
    let kind = match relation.rel_type.as_str() {
        "purchase for mail-order" | "amazon asin" => PurchaseKind::Physical,
        "download for free" => PurchaseKind::Free,
        _ => PurchaseKind::Digital,
    };
    let store = store_for(url);
    let label = store_label(store).map_or_else(|| host_of(url), str::to_owned);
    Some(PurchaseLink {
        store: store.to_owned(),
        label: if label.is_empty() {
            "Store".to_owned()
        } else {
            label
        },
        url: url.to_owned(),
        kind,
    })
}

/// Sort links into store order, then by label.
pub fn sort_links(links: &mut [PurchaseLink]) {
    links.sort_by_key(|link| {
        let rank = STORE_ORDER
            .iter()
            .position(|store| *store == link.store)
            .unwrap_or(STORE_ORDER.len());
        (rank, link.label.to_lowercase())
    });
}

/// Percent-encode a search term the way `quote_plus` does.
pub fn quote_plus(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b' ' => encoded.push('+'),
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

/// The covers route for a release group (v2 `release_group_cover_url`),
/// or `None` when the id is not an MBID.
pub fn release_group_cover_url(release_group_mbid: &str) -> Option<String> {
    crate::providers::musicbrainz::is_valid_mbid(release_group_mbid).then(|| {
        format!(
            "/api/v3/covers/release-group/{}?size=500",
            release_group_mbid.trim().to_ascii_lowercase()
        )
    })
}
