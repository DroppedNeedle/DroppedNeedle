//! Health numbers for each discover page build: how much of the page is
//! about the person, how much repeats.
//!
//! Each build counts its page with [`page_health`], logs the numbers and
//! keeps them with the saved row;
//! [`from_stored`] turns every user's saved rows into the admin's read.
//! Nothing here reads or writes the database. They are counts only: no
//! album, artist or seed is named, and shares are left for the reader to
//! work out.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::discover::cache::{PageKind, StoredPage};
use crate::discover::models::{
    DiscoverHealthResponse, DiscoverResponse, PageHealthRow, SectionContent, SectionItem,
};
use crate::discover::sections::{self, Basis, Page, Requires, SectionDef, SectionKey};

/// The numbers of one discover page build.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PageHealth {
    /// Sections on the page.
    pub sections: usize,
    /// Sections switched on that came back with nothing to show.
    pub wanted_empty: usize,
    /// Rows with a MusicBrainz id.
    pub cards: usize,
    /// Distinct albums among them.
    pub distinct_albums: usize,
    /// Distinct artists among them.
    pub distinct_artists: usize,
    /// Cards in sections that claim to be about the person.
    pub personal_cards: usize,
    /// Ids shown in more than one section.
    pub duplicates: usize,
    /// Albums the page it replaced showed too.
    pub repeats: usize,
    /// Seed artists the build grew from.
    pub seeds_used: usize,
    /// Seed artists the person has, the ones used included, at most 30.
    /// Pages saved before it was counted read 0.
    #[serde(default)]
    #[schema(required = true)]
    pub seeds_found: usize,
    /// The most cards any one artist has.
    pub max_cards_one_artist: usize,
    /// Whether a read failed or a shelf ran out of time.
    pub incomplete: bool,
}

/// What a page build knows beyond the page itself.
pub struct BuildFacts<'a> {
    /// The sections switched on for it.
    pub wanted: &'a [&'static SectionDef],
    /// Seed artists it grew from.
    pub seeds_used: usize,
    /// Seed artists the person has.
    pub seeds_found: usize,
    /// Whether a read failed or a shelf ran out of time.
    pub incomplete: bool,
    /// Whether ListenBrainz sections could fill for the person.
    pub listenbrainz: bool,
    /// Whether Last.fm sections could fill for the person.
    pub lastfm: bool,
}

impl BuildFacts<'_> {
    /// Whether the person can get `def` at all: a section needing a
    /// service they have not linked never counts as empty.
    fn reachable(&self, def: &SectionDef) -> bool {
        match def.requires {
            Some(Requires::Listenbrainz) => self.listenbrainz,
            Some(Requires::Lastfm) => self.lastfm,
            Some(Requires::Library) | None => true,
        }
    }
}

/// One row with a MusicBrainz id: its own id, and the album and artist it
/// belongs to when known.
struct Card<'a> {
    id: &'a str,
    album: Option<&'a str>,
    artist: Option<&'a str>,
}

fn known(id: Option<&String>) -> Option<&str> {
    id.map(String::as_str).filter(|id| !id.is_empty())
}

/// Every card of one section.
fn cards(content: &SectionContent) -> Vec<Card<'_>> {
    match content {
        SectionContent::Weekly { weekly } => weekly
            .tracks
            .iter()
            .filter_map(|track| {
                let album = known(track.release_group_mbid.as_ref());
                Some(Card {
                    id: known(track.recording_mbid.as_ref()).or(album)?,
                    album,
                    artist: known(track.artist_mbid.as_ref()),
                })
            })
            .collect(),
        SectionContent::Gaps { gaps } => gaps
            .iter()
            .filter_map(|gap| {
                let id = known(gap.album.musicbrainz_id.as_ref())?;
                Some(Card {
                    id,
                    album: Some(id),
                    artist: known(gap.album.artist_id.as_ref()),
                })
            })
            .collect(),
        SectionContent::OwnShelves { own_shelves } => own_shelves
            .played_a_lot
            .iter()
            .chain(&own_shelves.never_played)
            .filter_map(|row| {
                let id = known(row.album.musicbrainz_id.as_ref())?;
                Some(Card {
                    id,
                    album: Some(id),
                    artist: known(row.album.artist_id.as_ref()),
                })
            })
            .collect(),
        SectionContent::NewReleases { new_releases } => new_releases
            .releases
            .iter()
            .filter_map(|row| {
                let id = known(row.album.musicbrainz_id.as_ref())?;
                Some(Card {
                    id,
                    album: Some(id),
                    artist: known(row.album.artist_id.as_ref()),
                })
            })
            .collect(),
        _ => content
            .shelves()
            .into_iter()
            .flat_map(|shelf| shelf.items.iter())
            .filter_map(|item| match item {
                SectionItem::Album(row) => {
                    let id = known(row.musicbrainz_id.as_ref())?;
                    Some(Card {
                        id,
                        album: Some(id),
                        artist: known(row.artist_id.as_ref()),
                    })
                }
                SectionItem::Artist(row) => {
                    let id = known(row.musicbrainz_id.as_ref())?;
                    Some(Card {
                        id,
                        album: None,
                        artist: Some(id),
                    })
                }
                SectionItem::Track(row) => Some(Card {
                    id: known(row.musicbrainz_id.as_ref())?,
                    album: None,
                    artist: known(row.artist_id.as_ref()),
                }),
                SectionItem::Genre(_) => None,
            })
            .collect(),
    }
}

/// Whether a section's rows are on the page: the weekly set and the
/// playlist tool are served by routes of their own.
fn on_page(key: SectionKey) -> bool {
    !matches!(key, SectionKey::WeeklySet | SectionKey::PlaylistDiscovery)
}

/// The albums a page shows.
fn albums(page: &DiscoverResponse) -> HashSet<&str> {
    page.sections
        .iter()
        .flat_map(|section| cards(&section.content))
        .filter_map(|card| card.album)
        .collect()
}

/// The most cards any one artist has.
fn max_per_artist<'a>(artists: impl Iterator<Item = &'a str>) -> usize {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for artist in artists {
        *counts.entry(artist).or_default() += 1;
    }
    counts.into_values().max().unwrap_or(0)
}

/// Count a built page, with ownership marked, against the page it
/// replaces.
pub fn page_health(
    page: &DiscoverResponse,
    previous: Option<&DiscoverResponse>,
    build: &BuildFacts<'_>,
) -> PageHealth {
    let mut health = PageHealth {
        sections: page.sections.len(),
        seeds_used: build.seeds_used,
        seeds_found: build.seeds_found,
        incomplete: build.incomplete,
        ..PageHealth::default()
    };
    let shown: HashSet<SectionKey> = page.sections.iter().map(|section| section.key).collect();
    health.wanted_empty = build
        .wanted
        .iter()
        .filter(|def| on_page(def.key) && build.reachable(def) && !shown.contains(&def.key))
        .count();

    let mut sections_by_id: HashMap<&str, HashSet<SectionKey>> = HashMap::new();
    let mut album_ids: HashSet<&str> = HashSet::new();
    let mut artist_ids: HashSet<&str> = HashSet::new();
    let mut artist_cards: Vec<&str> = Vec::new();
    for section in &page.sections {
        let personal = sections::find(Page::Discover, section.key)
            .is_some_and(|def| def.basis == Basis::Personal);
        let found = cards(&section.content);
        health.cards += found.len();
        if personal {
            health.personal_cards += found.len();
        }
        for card in found {
            sections_by_id
                .entry(card.id)
                .or_default()
                .insert(section.key);
            album_ids.extend(card.album);
            artist_ids.extend(card.artist);
            artist_cards.extend(card.artist);
        }
    }
    health.distinct_albums = album_ids.len();
    health.distinct_artists = artist_ids.len();
    health.duplicates = sections_by_id
        .values()
        .filter(|sections| sections.len() > 1)
        .count();
    health.max_cards_one_artist = max_per_artist(artist_cards.into_iter());
    if let Some(previous) = previous {
        // `previous` is the raw saved page, this one is counted after
        // ownership marking, so an owned album that moved shelves can count.
        let before = albums(previous);
        health.repeats = album_ids.intersection(&before).count();
    }
    health
}

/// The one log line of a discover page build.
pub fn log_page_health(user_id: &str, seconds: f64, health: &PageHealth) {
    tracing::info!(
        user = %user_id,
        seconds,
        sections = health.sections,
        wanted_empty = health.wanted_empty,
        cards = health.cards,
        distinct_albums = health.distinct_albums,
        distinct_artists = health.distinct_artists,
        personal_cards = health.personal_cards,
        duplicates = health.duplicates,
        repeats = health.repeats,
        seeds_used = health.seeds_used,
        seeds_found = health.seeds_found,
        max_cards_one_artist = health.max_cards_one_artist,
        incomplete = health.incomplete,
        "discover page built"
    );
}

/// The numbers a saved page carries; the rest of the payload is
/// skipped.
#[derive(Deserialize)]
struct Kept<H> {
    health: Option<H>,
}

/// The numbers in one saved payload. `None` for a row saved before builds
/// kept them, and, with a log line, for one that does not decode.
fn kept<H: serde::de::DeserializeOwned>(payload: &[u8], page: &str) -> Option<H> {
    match serde_json::from_slice::<Kept<H>>(payload) {
        Ok(kept) => kept.health,
        Err(error) => {
            tracing::warn!(%error, page, "saved page health does not decode; leaving it out");
            None
        }
    }
}

/// Every user's latest discover page numbers, from their saved Discover
/// rows. A row without them is left out until it is built again.
pub fn from_stored(rows: Vec<StoredPage>) -> DiscoverHealthResponse {
    let page = PageKind::Discover.as_str();
    let pages = rows
        .into_iter()
        .filter_map(|row| {
            kept(&row.payload, page).map(|health| PageHealthRow {
                user_id: row.user_id,
                built_at: row.built_at / 1000,
                health,
            })
        })
        .collect();
    DiscoverHealthResponse { pages }
}
