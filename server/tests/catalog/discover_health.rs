//! The health numbers of a discover build, on a hand-built page and deck:
//! a chart pick under a personal heading, an owned pick, an album in two
//! sections and one the page before showed are each counted, and so are
//! a deck's chart cards.

use droppedneedle_catalog::discover::health::{
    BuildFacts, DeckHealth, PageHealth, deck_health, page_health,
};
use droppedneedle_catalog::discover::models::{
    AlbumRef, ArtistRef, ChartSection, DiscoverResponse, PageSection, QueueItemLight,
    SectionContent, SectionItem, SectionKey, SectionKind, SectionZone, TopPickItem,
    TopPicksSection,
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

fn pick(album: AlbumRef, seed: Option<&str>) -> TopPickItem {
    TopPickItem {
        album,
        match_pct: 50,
        reasons: Vec::new(),
        seed_artist: seed.map(str::to_owned),
    }
}

fn shelf(key: SectionKey, kind: SectionKind, items: Vec<SectionItem>) -> PageSection {
    PageSection {
        key,
        zone: SectionZone::NewAndFresh,
        content: SectionContent::Shelf {
            shelf: ChartSection {
                title: String::new(),
                kind,
                items,
                source: None,
                fallback_message: None,
                connect_service: None,
                radio_seed_type: None,
                radio_seed_id: None,
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
fn a_page_counts_chart_fill_owned_picks_duplicates_and_repeats() {
    let owned = AlbumRef {
        in_library: true,
        ..album("a3", "x")
    };
    let built = page(vec![
        PageSection {
            key: SectionKey::TopPicks,
            zone: SectionZone::Essentials,
            content: SectionContent::Picks {
                picks: TopPicksSection {
                    title: String::new(),
                    items: vec![
                        pick(album("a1", "x"), Some("Seed")),
                        pick(album("a2", "y"), None),
                        pick(owned, Some("Seed")),
                    ],
                    source: None,
                    personalizing: false,
                },
            },
        },
        shelf(
            SectionKey::FreshReleases,
            SectionKind::Albums,
            vec![SectionItem::Album(album("a1", "x"))],
        ),
        shelf(
            SectionKey::GloballyTrending,
            SectionKind::Artists,
            vec![SectionItem::Artist(ArtistRef {
                musicbrainz_id: Some("y".to_owned()),
                ..ArtistRef::named("Y")
            })],
        ),
    ]);
    let before = page(vec![shelf(
        SectionKey::FreshReleases,
        SectionKind::Albums,
        vec![SectionItem::Album(album("a2", "y"))],
    )]);
    let wanted: Vec<_> = [
        SectionKey::DiscoverQueue,
        SectionKey::TopPicks,
        SectionKey::FreshReleases,
        SectionKey::MissingEssentials,
        SectionKey::GloballyTrending,
        SectionKey::WeeklyExploration,
        SectionKey::LastfmWeeklyAlbumChart,
    ]
    .into_iter()
    .filter_map(|key| sections::find(Page::Discover, key))
    .collect();
    let facts = |listenbrainz| BuildFacts {
        wanted: &wanted,
        seeds_used: 2,
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
            // Missing Essentials: the deck is never a page section, and
            // sections of services the person has not linked never count.
            wanted_empty: 1,
            cards: 5,
            distinct_albums: 3,
            distinct_artists: 2,
            personal_cards: 4,
            chart_under_personal: 1,
            owned_picks: 1,
            duplicates: 1,
            repeats: 1,
            seeds_used: 2,
            max_cards_one_artist: 3,
            incomplete: false,
        }
    );
}

#[test]
fn a_deck_counts_its_chart_cards() {
    let card = |id: &str, artist: &str, wildcard: bool| QueueItemLight {
        musicbrainz_id: id.to_owned(),
        title: id.to_owned(),
        artist_name: artist.to_owned(),
        artist_id: artist.to_owned(),
        recommendation_reason: String::new(),
        cover_url: None,
        is_wildcard: wildcard,
        in_library: false,
    };
    let deck = [
        card("a1", "x", false),
        card("a2", "x", false),
        card("a3", "y", true),
    ];
    assert_eq!(
        deck_health(&deck),
        DeckHealth {
            cards: 3,
            chart_cards: 1,
            distinct_artists: 2,
            max_cards_one_artist: 2,
        }
    );
}
