//! Pure shaping for the discover page: shelf rows, the per-zone status the
//! page renders while it updates, the "is there anything to show" test,
//! the cross-shelf album dedupe and the connect-a-service cards.

use std::collections::{HashMap, HashSet};

use crate::reads::discover::adapters::queue::build::cover_url;
use crate::reads::discover::models::{
    ChartAlbum, ChartArtist, ChartGenre, ChartSection, DiscoverResponse, SectionItem, ServicePrompt,
};

/// Rows a shelf shows at most.
pub const SHELF_SIZE: usize = 15;

/// The zones the page groups its shelves into, in page order.
pub const ZONES: [&str; 9] = [
    "picks", "lounge", "weekly", "made", "because", "fresh", "library", "genres", "trending",
];

/// An artist row.
pub fn artist(mbid: Option<&str>, name: &str, listen_count: Option<i64>) -> SectionItem {
    SectionItem::Artist(ChartArtist {
        name: name.to_owned(),
        mbid: mbid.map(str::to_owned),
        local_id: None,
        image_url: None,
        listen_count,
        in_library: false,
        source: None,
    })
}

/// An album row; the cover comes from the release group when known.
pub fn album(
    mbid: Option<&str>,
    name: &str,
    artist_name: Option<&str>,
    artist_mbid: Option<&str>,
    listen_count: Option<i64>,
) -> ChartAlbum {
    ChartAlbum {
        name: name.to_owned(),
        mbid: mbid.map(str::to_owned),
        local_id: None,
        artist_name: artist_name
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        artist_mbid: artist_mbid
            .filter(|mbid| !mbid.is_empty())
            .map(str::to_owned),
        image_url: mbid.map(cover_url),
        release_date: None,
        listen_count,
        in_library: false,
        requested: false,
        source: None,
    }
}

/// A genre row.
pub fn genre(name: &str, listen_count: Option<i64>, artist_count: Option<i64>) -> SectionItem {
    SectionItem::Genre(ChartGenre {
        name: name.to_owned(),
        listen_count,
        artist_count,
        artist_mbid: None,
    })
}

/// A shelf, `None` when it has no rows.
pub fn shelf(
    title: &str,
    kind: &str,
    items: Vec<SectionItem>,
    source: Option<&str>,
) -> Option<ChartSection> {
    if items.is_empty() {
        return None;
    }
    Some(ChartSection {
        title: title.to_owned(),
        section_type: kind.to_owned(),
        items,
        source: source.map(str::to_owned),
        fallback_message: None,
        connect_service: None,
        radio_seed_type: None,
        radio_seed_id: None,
    })
}

fn has_items(section: Option<&ChartSection>) -> bool {
    section.is_some_and(|section| !section.items.is_empty())
}

/// Whether a build found anything worth showing. An empty build never
/// replaces a good saved page.
pub fn has_content(page: &DiscoverResponse) -> bool {
    zone_presence(page).values().any(|present| *present)
}

fn zone_presence(page: &DiscoverResponse) -> HashMap<&'static str, bool> {
    let any = |sections: &[Option<&ChartSection>]| sections.iter().any(|s| has_items(*s));
    HashMap::from([
        (
            "picks",
            page.top_picks
                .as_ref()
                .is_some_and(|picks| !picks.items.is_empty()),
        ),
        ("lounge", has_items(page.listeners_like_you.as_ref())),
        (
            "weekly",
            page.weekly_exploration
                .as_ref()
                .is_some_and(|weekly| !weekly.tracks.is_empty()),
        ),
        (
            "made",
            page.daily_mixes.iter().any(|mix| !mix.items.is_empty())
                || page
                    .radio_sections
                    .iter()
                    .any(|radio| !radio.items.is_empty()),
        ),
        (
            "because",
            page.because_you_listen_to
                .iter()
                .any(|entry| !entry.section.items.is_empty())
                || any(&[
                    page.artists_you_might_like.as_ref(),
                    page.popular_in_your_genres.as_ref(),
                ]),
        ),
        (
            "fresh",
            any(&[
                page.fresh_releases.as_ref(),
                page.new_from_followed.as_ref(),
                page.missing_essentials.as_ref(),
            ]),
        ),
        (
            "library",
            any(&[
                page.rediscover.as_ref(),
                page.anniversaries.as_ref(),
                page.lastfm_recent_scrobbles.as_ref(),
            ]),
        ),
        (
            "genres",
            any(&[page.unexplored_genres.as_ref(), page.genre_list.as_ref()]),
        ),
        (
            "trending",
            any(&[
                page.globally_trending.as_ref(),
                page.lastfm_weekly_artist_chart.as_ref(),
                page.lastfm_weekly_album_chart.as_ref(),
            ]),
        ),
    ])
}

/// Status per zone: `loading` with no page yet, `updating` while a build
/// runs, else `ready` or `empty`.
pub fn section_status(page: Option<&DiscoverResponse>, updating: bool) -> HashMap<String, String> {
    let Some(page) = page else {
        return ZONES
            .iter()
            .map(|zone| ((*zone).to_owned(), "loading".to_owned()))
            .collect();
    };
    zone_presence(page)
        .into_iter()
        .map(|(zone, present)| {
            let state = match (updating, present) {
                (true, _) => "updating",
                (false, true) => "ready",
                (false, false) => "empty",
            };
            (zone.to_owned(), state.to_owned())
        })
        .collect()
}

/// Prefer variety across album shelves: drop albums an earlier shelf (or
/// Top Picks) already shows, unless that would leave a shelf with fewer
/// than three of its albums, in which case the shelf stays as it was.
pub fn dedupe_albums(page: &mut DiscoverResponse) {
    let mut seen: HashSet<String> = page
        .top_picks
        .iter()
        .flat_map(|picks| picks.items.iter())
        .filter_map(|pick| pick.album.mbid.as_deref())
        .map(str::to_lowercase)
        .collect();
    let mut dedupe = |section: &mut ChartSection| {
        let album_key = |item: &SectionItem| match item {
            SectionItem::Album(album) => Some(album.mbid.as_deref().map(str::to_lowercase)),
            _ => None,
        };
        let albums = section
            .items
            .iter()
            .filter(|item| album_key(item).is_some())
            .count();
        let filtered: Vec<SectionItem> = section
            .items
            .iter()
            .filter(|item| match album_key(item) {
                Some(Some(mbid)) => !seen.contains(&mbid),
                _ => true,
            })
            .cloned()
            .collect();
        let kept_albums = filtered
            .iter()
            .filter(|item| album_key(item).is_some())
            .count();
        if kept_albums >= albums.min(3) {
            section.items = filtered;
        }
        for item in &section.items {
            if let Some(Some(mbid)) = album_key(item) {
                seen.insert(mbid);
            }
        }
    };
    for section in [
        page.fresh_releases.as_mut(),
        page.new_from_followed.as_mut(),
        page.missing_essentials.as_mut(),
        page.listeners_like_you.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        dedupe(section);
    }
    for section in page
        .daily_mixes
        .iter_mut()
        .chain(page.radio_sections.iter_mut())
    {
        dedupe(section);
    }
    for section in [
        page.globally_trending.as_mut(),
        page.lastfm_weekly_album_chart.as_mut(),
        page.lastfm_recent_scrobbles.as_mut(),
    ]
    .into_iter()
    .flatten()
    {
        dedupe(section);
    }
}

fn prompt(
    service: &str,
    title: &str,
    description: &str,
    icon: &str,
    color: &str,
    features: [&str; 4],
) -> ServicePrompt {
    ServicePrompt {
        service: service.to_owned(),
        title: title.to_owned(),
        description: description.to_owned(),
        icon: icon.to_owned(),
        color: color.to_owned(),
        features: features
            .iter()
            .map(|feature| (*feature).to_owned())
            .collect(),
    }
}

/// The connect-a-service cards for what the user has not linked yet.
pub fn service_prompts(
    listenbrainz: bool,
    jellyfin: bool,
    download_client: bool,
    lastfm: bool,
) -> Vec<ServicePrompt> {
    let mut prompts = Vec::new();
    if !listenbrainz {
        prompts.push(prompt(
            "listenbrainz",
            "Connect ListenBrainz",
            "Pulls recommendations from your listening history, finds similar artists, and tracks your top genres. Add Last.fm for global listener stats.",
            "LB",
            "primary",
            [
                "Personalized recommendations",
                "Similar artists",
                "Listening stats",
                "Genre insights",
            ],
        ));
    }
    if !jellyfin {
        prompts.push(prompt(
            "jellyfin",
            "Connect Jellyfin",
            "Uses your play history to bring back old favorites and improve recommendations.",
            "JF",
            "secondary",
            [
                "Rediscover favorites",
                "Play statistics",
                "Listening history",
                "Better recommendations",
            ],
        ));
    }
    if !download_client {
        prompts.push(prompt(
            "download-client",
            "Connect Download Client",
            "Lets you request and download albums and tracks straight into your library.",
            "DL",
            "accent",
            [
                "Album requests",
                "Track requests",
                "Automatic import",
                "Library management",
            ],
        ));
    }
    if !lastfm {
        prompts.push(prompt(
            "lastfm",
            "Connect Last.fm",
            "Tracks what you listen to, shows your stats, and suggests music based on your taste.",
            "FM",
            "primary",
            [
                "Scrobbling",
                "Global listener stats",
                "Artist recommendations",
                "Play history",
            ],
        ));
    }
    prompts
}

/// The genres Browse by Genre shows when ListenBrainz has none for the
/// user (v2's default list).
pub const DEFAULT_GENRES: [&str; 20] = [
    "Rock",
    "Pop",
    "Hip Hop",
    "Electronic",
    "Jazz",
    "Classical",
    "R&B",
    "Country",
    "Metal",
    "Folk",
    "Blues",
    "Reggae",
    "Soul",
    "Punk",
    "Indie",
    "Alternative",
    "Dance",
    "Soundtrack",
    "World",
    "Latin",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn albums(mbids: &[&str]) -> ChartSection {
        ChartSection {
            title: "s".to_owned(),
            section_type: "albums".to_owned(),
            items: mbids
                .iter()
                .map(|mbid| SectionItem::Album(album(Some(mbid), mbid, None, None, None)))
                .collect(),
            source: None,
            fallback_message: None,
            connect_service: None,
            radio_seed_type: None,
            radio_seed_id: None,
        }
    }

    fn ids(section: &ChartSection) -> Vec<String> {
        section
            .items
            .iter()
            .filter_map(|item| match item {
                SectionItem::Album(album) => album.mbid.clone(),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn dedupe_drops_repeats_but_keeps_a_shelf_that_would_shrink_below_three() {
        let mut page: DiscoverResponse = serde_json::from_value(serde_json::json!({
            "discover_queue_enabled": true,
            "genre_artwork_schema_version": "v2"
        }))
        .unwrap_or_else(|error| panic!("page: {error}"));
        page.fresh_releases = Some(albums(&["a", "b", "c", "d"]));
        page.missing_essentials = Some(albums(&["a", "e", "f", "g"]));
        page.globally_trending = Some(albums(&["a", "b", "h"]));
        dedupe_albums(&mut page);
        let missing = page
            .missing_essentials
            .as_ref()
            .map(ids)
            .unwrap_or_default();
        assert_eq!(missing, ["e", "f", "g"]);
        let trending = page.globally_trending.as_ref().map(ids).unwrap_or_default();
        assert_eq!(trending, ["a", "b", "h"]);
    }
}
