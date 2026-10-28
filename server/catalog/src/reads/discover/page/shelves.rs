//! Pure shaping for the home and discover pages: shelf rows, a section's
//! rows, the cross-shelf album dedupe and the connect-a-service cards.

use std::collections::HashSet;

use super::add_section;
use crate::reads::discover::models::{
    AlbumRef, ArtistRef, ChartSection, GenreRef, PageSection, PromptService, SectionContent,
    SectionItem, SectionKey, SectionKind, ServicePrompt, ShelfSource,
};
use crate::reads::discover::queue::build::cover_url;
use crate::reads::discover::sections::{self, Dedupe, Page};
use crate::reads::platform::artwork::warm::WarmTargets;

/// Rows a shelf shows at most.
pub const SHELF_SIZE: usize = 15;

/// An artist row.
pub fn artist(mbid: Option<&str>, name: &str, listen_count: Option<i64>) -> SectionItem {
    SectionItem::Artist(ArtistRef {
        musicbrainz_id: mbid.map(str::to_owned),
        listen_count,
        ..ArtistRef::named(name)
    })
}

/// An album row; the cover comes from the release group when known.
pub fn album(
    mbid: Option<&str>,
    title: &str,
    artist_name: Option<&str>,
    artist_mbid: Option<&str>,
    listen_count: Option<i64>,
) -> AlbumRef {
    AlbumRef {
        musicbrainz_id: mbid.map(str::to_owned),
        artist_name: artist_name
            .filter(|name| !name.is_empty())
            .map(str::to_owned),
        artist_id: artist_mbid
            .filter(|mbid| !mbid.is_empty())
            .map(str::to_owned),
        cover_url: mbid.map(cover_url),
        listen_count,
        ..AlbumRef::titled(title)
    }
}

/// A genre row.
pub fn genre(name: &str, listen_count: Option<i64>, artist_count: Option<i64>) -> SectionItem {
    SectionItem::Genre(GenreRef {
        name: name.to_owned(),
        listen_count,
        artist_count,
        artist_id: None,
    })
}

/// A shelf, `None` when it has no rows.
pub fn shelf(
    title: &str,
    kind: SectionKind,
    items: Vec<SectionItem>,
    source: Option<ShelfSource>,
) -> Option<ChartSection> {
    if items.is_empty() {
        return None;
    }
    Some(ChartSection {
        title: title.to_owned(),
        kind,
        items,
        source,
        fallback_message: None,
        connect_service: None,
        radio_seed_type: None,
        radio_seed_id: None,
    })
}

/// A row ownership is marked on.
pub enum RowMut<'a> {
    /// An album row.
    Album(&'a mut AlbumRef),
    /// An artist row.
    Artist(&'a mut ArtistRef),
}

impl SectionContent {
    /// The section's shelves; none for Top Picks and the weekly playlist.
    pub fn shelves(&self) -> Vec<&ChartSection> {
        match self {
            Self::Shelf { shelf } => vec![shelf],
            Self::Shelves { shelves } => shelves.iter().collect(),
            Self::Because { entries } => entries.iter().map(|entry| &entry.section).collect(),
            Self::Picks { .. } | Self::Weekly { .. } => Vec::new(),
        }
    }

    /// The section's shelves, to change.
    pub fn shelves_mut(&mut self) -> Vec<&mut ChartSection> {
        match self {
            Self::Shelf { shelf } => vec![shelf],
            Self::Shelves { shelves } => shelves.iter_mut().collect(),
            Self::Because { entries } => {
                entries.iter_mut().map(|entry| &mut entry.section).collect()
            }
            Self::Picks { .. } | Self::Weekly { .. } => Vec::new(),
        }
    }

    /// Every album and artist row, Top Picks' albums included.
    pub fn rows_mut(&mut self) -> Vec<RowMut<'_>> {
        if let Self::Picks { picks } = self {
            return picks
                .items
                .iter_mut()
                .map(|pick| RowMut::Album(&mut pick.album))
                .collect();
        }
        self.shelves_mut()
            .into_iter()
            .flat_map(|shelf| shelf.items.iter_mut())
            .filter_map(|item| match item {
                SectionItem::Album(row) => Some(RowMut::Album(row)),
                SectionItem::Artist(row) => Some(RowMut::Artist(row)),
                SectionItem::Track(_) | SectionItem::Genre(_) => None,
            })
            .collect()
    }

    /// Whether it has no rows.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Picks { picks } => picks.items.is_empty(),
            Self::Weekly { weekly } => weekly.tracks.is_empty(),
            _ => self.shelves().iter().all(|shelf| shelf.items.is_empty()),
        }
    }

    /// Queue the covers and artist images it shows.
    pub fn warm(&self, targets: &mut WarmTargets) {
        match self {
            Self::Picks { picks } => {
                for pick in &picks.items {
                    targets.cover(pick.album.musicbrainz_id.as_deref());
                }
            }
            Self::Weekly { weekly } => {
                for track in &weekly.tracks {
                    targets.cover(track.release_group_mbid.as_deref());
                }
            }
            _ => {
                for shelf in self.shelves() {
                    add_section(targets, shelf);
                }
            }
        }
    }
}

/// One shelf as section content.
pub fn one(shelf: Option<ChartSection>) -> Option<SectionContent> {
    shelf.map(|shelf| SectionContent::Shelf { shelf })
}

/// A section of `page`, in the zone the list gives it. `None` when it
/// has no rows.
pub fn section(page: Page, key: SectionKey, content: SectionContent) -> Option<PageSection> {
    if content.is_empty() {
        return None;
    }
    let def = sections::find(page, key)?;
    Some(PageSection {
        key,
        zone: def.zone,
        content,
    })
}

/// The shelf of section `key`, when it is a one-shelf section.
pub fn shelf_of(sections: &[PageSection], key: SectionKey) -> Option<&ChartSection> {
    sections.iter().find_map(|section| match &section.content {
        SectionContent::Shelf { shelf } if section.key == key => Some(shelf),
        _ => None,
    })
}

/// Prefer variety across album shelves: in rank order, drop albums a
/// section of a lower rank (or Top Picks) already shows, unless that
/// would leave a shelf with fewer than three of its albums, in which case
/// the shelf stays as it was.
pub fn dedupe_albums(page: &mut [PageSection]) {
    let mut ranked: Vec<(u8, usize)> = page
        .iter()
        .enumerate()
        .filter_map(
            |(at, section)| match sections::find(Page::Discover, section.key)?.dedupe {
                Dedupe::Albums(rank) => Some((rank, at)),
                Dedupe::None | Dedupe::Artists => None,
            },
        )
        .collect();
    ranked.sort_unstable();
    let mut seen: HashSet<String> = HashSet::new();
    for (_, at) in ranked {
        let content = &mut page[at].content;
        if let SectionContent::Picks { picks } = content {
            seen.extend(
                picks
                    .items
                    .iter()
                    .filter_map(|pick| pick.album.musicbrainz_id.as_deref())
                    .map(str::to_lowercase),
            );
            continue;
        }
        for shelf in content.shelves_mut() {
            dedupe(shelf, &mut seen);
        }
    }
}

fn dedupe(section: &mut ChartSection, seen: &mut HashSet<String>) {
    let album_key = |item: &SectionItem| match item {
        SectionItem::Album(album) => Some(album.musicbrainz_id.as_deref().map(str::to_lowercase)),
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
}

fn prompt(
    service: PromptService,
    title: &str,
    description: &str,
    icon: &str,
    color: &str,
    features: [&str; 4],
) -> ServicePrompt {
    ServicePrompt {
        service,
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
            PromptService::Listenbrainz,
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
            PromptService::Jellyfin,
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
            PromptService::DownloadClient,
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
            PromptService::Lastfm,
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
            kind: SectionKind::Albums,
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
                SectionItem::Album(album) => album.musicbrainz_id.clone(),
                _ => None,
            })
            .collect()
    }

    fn shelf(key: SectionKey, mbids: &[&str]) -> PageSection {
        PageSection {
            key,
            zone: sections::SectionZone::NewAndFresh,
            content: SectionContent::Shelf {
                shelf: albums(mbids),
            },
        }
    }

    fn shelf_ids(page: &[PageSection], key: SectionKey) -> Vec<String> {
        shelf_of(page, key).map(ids).unwrap_or_default()
    }

    #[test]
    fn dedupe_drops_repeats_but_keeps_a_shelf_that_would_shrink_below_three() {
        let mut page = vec![
            shelf(SectionKey::LastfmWeeklyAlbumChart, &["a", "b", "h"]),
            shelf(SectionKey::MissingEssentials, &["a", "e", "f", "g"]),
            shelf(SectionKey::FreshReleases, &["a", "b", "c", "d"]),
        ];
        dedupe_albums(&mut page);
        assert_eq!(
            shelf_ids(&page, SectionKey::MissingEssentials),
            ["e", "f", "g"]
        );
        assert_eq!(
            shelf_ids(&page, SectionKey::LastfmWeeklyAlbumChart),
            ["a", "b", "h"]
        );
    }
}
