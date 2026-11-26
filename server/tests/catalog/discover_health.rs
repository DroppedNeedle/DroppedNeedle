//! The health numbers of a discover build, on a hand-built page: an album
//! in two sections and one the page before showed are each counted.

use droppedneedle_catalog::discover::health::{BuildFacts, PageHealth, page_health};
use droppedneedle_catalog::discover::models::{
    AlbumRef, ArtistRef, ChartSection, DiscoverResponse, NewRelease, NewReleaseSheet, PageSection,
    SectionContent, SectionItem, SectionKey, SectionKind, SectionZone,
};
use droppedneedle_catalog::discover::page::empty_page;
use droppedneedle_catalog::discover::sections::{self, Page};

fn album(id: &str, artist: &str) -> AlbumRef {
    AlbumRef {
        musicbrainz_id: Some(id.to_owned()),
        artist_id: Some(artist.to_owned()),
        ..AlbumRef::titled(id)
    }
}

fn chart(kind: SectionKind, items: Vec<SectionItem>) -> ChartSection {
    ChartSection {
        title: String::new(),
        kind,
        items,
        source: None,
        fallback_message: None,
        connect_service: None,
        radio_seed_type: None,
        radio_seed_id: None,
    }
}

fn shelf(key: SectionKey, kind: SectionKind, items: Vec<SectionItem>) -> PageSection {
    PageSection {
        key,
        zone: SectionZone::NewAndFresh,
        content: SectionContent::Shelf {
            shelf: chart(kind, items),
        },
    }
}

/// New from your artists holding `albums`.
fn new_releases(albums: Vec<AlbumRef>) -> PageSection {
    PageSection {
        key: SectionKey::NewFromYourArtists,
        zone: SectionZone::NewAndFresh,
        content: SectionContent::NewReleases {
            new_releases: NewReleaseSheet {
                total: albums.len() as u64,
                releases: albums
                    .into_iter()
                    .map(|album| NewRelease {
                        album,
                        reason: None,
                        secondary_kind: None,
                    })
                    .collect(),
                updated_at: None,
            },
        },
    }
}

fn page(sections: Vec<PageSection>) -> DiscoverResponse {
    DiscoverResponse {
        sections,
        ..empty_page()
    }
}

#[test]
fn a_page_counts_duplicates_and_repeats() {
    let built = page(vec![
        PageSection {
            key: SectionKey::RadioSections,
            zone: SectionZone::MadeForYou,
            content: SectionContent::Shelves {
                shelves: vec![chart(
                    SectionKind::Albums,
                    ["a1", "a2", "a3"]
                        .into_iter()
                        .zip(["x", "y", "x"])
                        .map(|(id, artist)| SectionItem::Album(album(id, artist)))
                        .collect(),
                )],
            },
        },
        new_releases(vec![album("a1", "x")]),
        shelf(
            SectionKey::FromYourShelves,
            SectionKind::Artists,
            vec![SectionItem::Artist(ArtistRef {
                musicbrainz_id: Some("y".to_owned()),
                ..ArtistRef::named("Y")
            })],
        ),
    ]);
    let before = page(vec![new_releases(vec![album("a2", "y")])]);
    let wanted: Vec<_> = [
        SectionKey::WeeklySet,
        SectionKey::RadioSections,
        SectionKey::NewFromYourArtists,
        SectionKey::GapsInYourArtists,
        SectionKey::FromYourShelves,
        SectionKey::WeeklyExploration,
    ]
    .into_iter()
    .filter_map(|key| sections::find(Page::Discover, key))
    .collect();
    let facts = |listenbrainz| BuildFacts {
        wanted: &wanted,
        seeds_used: 2,
        seeds_found: 5,
        incomplete: false,
        listenbrainz,
        lastfm: false,
    };

    // Linked to ListenBrainz, its empty weekly playlist counts as empty.
    let linked = page_health(&built, Some(&before), &facts(true));
    assert_eq!(linked.wanted_empty, 2);

    let health = page_health(&built, Some(&before), &facts(false));
    assert_eq!(
        health,
        PageHealth {
            sections: 3,
            // Gaps in artists you collect: the weekly set is never a page section,
            // and sections of services the person has not linked never
            // count.
            wanted_empty: 1,
            cards: 5,
            distinct_albums: 3,
            distinct_artists: 2,
            personal_cards: 4,
            duplicates: 1,
            repeats: 1,
            seeds_used: 2,
            seeds_found: 5,
            max_cards_one_artist: 3,
            incomplete: false,
        }
    );
}
