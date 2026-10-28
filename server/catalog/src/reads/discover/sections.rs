//! The one list of Discover and home sections.
//!
//! Each entry names a section's page, key, zone, settings title and
//! description, the service it needs, the shared reads a discover build
//! makes for it, and how it takes part in the page's dedupe. The builds,
//! the section settings, ownership marking, dedupe and the "has content"
//! test all go through this list; nothing else lists sections.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use utoipa::ToSchema;

/// A page with sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    /// The home page.
    Home,
    /// The discover page.
    Discover,
}

impl Page {
    /// The page's name in the section settings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Discover => "discover",
        }
    }
}

/// One section of the home or discover page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionKey {
    /// Home: albums popular this week.
    PopularAlbums,
    /// Home: artists trending this week.
    TrendingArtists,
    /// The ListenBrainz weekly exploration playlist.
    WeeklyExploration,
    /// Home: the user's top albums this month.
    YourTopAlbums,
    /// Home: the user's latest plays.
    RecentlyPlayed,
    /// Home: the newest library albums.
    RecentlyAdded,
    /// Genre tiles.
    GenreList,
    /// Home: artists from the user's loved tracks.
    FavoriteArtists,
    /// Home: library artists.
    LibraryArtists,
    /// Home: library albums.
    LibraryAlbums,
    /// Discover: the queue deck.
    DiscoverQueue,
    /// Discover: scored album picks.
    TopPicks,
    /// Discover: what similar listeners play.
    ListenersLikeYou,
    /// Discover: album suggestions from a playlist.
    PlaylistDiscovery,
    /// Discover: genre-clustered mixes.
    DailyMixes,
    /// Discover: album radios per seed artist.
    RadioSections,
    /// Discover: similar artists per seed artist.
    BecauseYouListenTo,
    /// Discover: more similar artists.
    ArtistsYouMightLike,
    /// Discover: artists popular in the user's genres.
    PopularInYourGenres,
    /// Discover: recent releases picked for the user.
    FreshReleases,
    /// Discover: releases from followed artists.
    NewFromFollowed,
    /// Discover: albums missing from artists the library collects.
    MissingEssentials,
    /// Discover: artists played a lot, but not lately.
    Rediscover,
    /// Discover: library albums with a round birthday this year.
    Anniversaries,
    /// Discover: the user's recent Last.fm scrobbles.
    LastfmRecentScrobbles,
    /// Discover: genres the library barely has.
    UnexploredGenres,
    /// Discover: artists trending worldwide.
    GloballyTrending,
    /// Discover: the user's Last.fm weekly artist chart.
    LastfmWeeklyArtistChart,
    /// Discover: the user's Last.fm weekly album chart.
    LastfmWeeklyAlbumChart,
}

impl SectionKey {
    /// The key's stored and wire spelling.
    pub fn name(self) -> String {
        match serde_json::to_value(self) {
            Ok(serde_json::Value::String(name)) => name,
            _ => String::new(),
        }
    }

    fn named(name: &str) -> Option<Self> {
        serde_json::from_value(serde_json::Value::String(name.to_owned())).ok()
    }
}

/// A group of sections on a page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SectionZone {
    /// Home: charts.
    WhatsHot,
    /// Home: personal shelves.
    ForYou,
    /// Home: the library.
    YourLibrary,
    /// Genre tiles.
    BrowseGenres,
    /// Discover: the queue, Top Picks, the lounge, the weekly playlist.
    Essentials,
    /// Discover: Daily Mixes and radio.
    MadeForYou,
    /// Discover: similar artists.
    BecauseYouListened,
    /// Discover: new releases.
    NewAndFresh,
    /// Discover: the user's own library and history.
    FromYourLibrary,
    /// Discover: charts.
    TrendingNow,
}

impl SectionZone {
    /// The zone's heading.
    pub fn label(self) -> &'static str {
        match self {
            Self::WhatsHot => "What's Hot",
            Self::ForYou => "For You",
            Self::YourLibrary => "Your Library",
            Self::BrowseGenres => "Browse Genres",
            Self::Essentials => "Essentials",
            Self::MadeForYou => "Made For You",
            Self::BecauseYouListened => "Because You Listened",
            Self::NewAndFresh => "New & Fresh",
            Self::FromYourLibrary => "From Your Library",
            Self::TrendingNow => "Trending Now",
        }
    }
}

/// A service a section needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requires {
    /// A linked ListenBrainz account.
    Listenbrainz,
    /// A linked Last.fm account.
    Lastfm,
    /// The library.
    Library,
}

impl Requires {
    /// The requirement's name in the section settings.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Listenbrainz => "listenbrainz",
            Self::Lastfm => "lastfm",
            Self::Library => "library",
        }
    }
}

/// A read a discover build shares between sections. It is made only
/// when a section that is switched on needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The user's top artists.
    Seeds,
    /// Artists similar to each seed.
    Similar,
    /// The worldwide artist chart.
    Trending,
    /// The user's Last.fm weekly artist chart.
    LastfmWeeklyArtists,
    /// The user's Last.fm weekly album chart.
    LastfmWeeklyAlbums,
    /// The user's recent Last.fm scrobbles.
    LastfmRecent,
    /// ListenBrainz fresh releases for the user.
    Fresh,
    /// The user's ListenBrainz genres.
    Genres,
    /// The user's Last.fm top artists, whose tags stand for genres.
    LastfmGenreArtists,
    /// The user's Jellyfin play counts.
    Jellyfin,
    /// The library's identified albums.
    LibraryAlbums,
}

/// How a section takes part in the discover page's dedupe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dedupe {
    /// It keeps every row.
    None,
    /// Built one after another in page order, it skips artists an
    /// earlier artist section already showed.
    Artists,
    /// It drops albums a section of a lower rank already showed (Top
    /// Picks, rank 0, only claims).
    Albums(u8),
}

/// One section.
#[derive(Debug)]
pub struct SectionDef {
    /// The page it is on.
    pub page: Page,
    /// Its key.
    pub key: SectionKey,
    /// The zone it is shown in.
    pub zone: SectionZone,
    /// Its title in the section settings.
    pub title: &'static str,
    /// Its description in the section settings.
    pub description: &'static str,
    /// The service it needs, when any.
    pub requires: Option<Requires>,
    /// The shared reads a discover build makes for it.
    pub inputs: &'static [Input],
    /// Its part in the dedupe.
    pub dedupe: Dedupe,
    /// Whether albums the library holds are left out of it when served.
    pub hides_owned: bool,
}

const fn home(
    key: SectionKey,
    zone: SectionZone,
    title: &'static str,
    description: &'static str,
    requires: Option<Requires>,
) -> SectionDef {
    SectionDef {
        page: Page::Home,
        key,
        zone,
        title,
        description,
        requires,
        inputs: &[],
        dedupe: Dedupe::None,
        hides_owned: false,
    }
}

const fn discover(
    key: SectionKey,
    zone: SectionZone,
    title: &'static str,
    description: &'static str,
    requires: Option<Requires>,
    inputs: &'static [Input],
    dedupe: Dedupe,
) -> SectionDef {
    SectionDef {
        page: Page::Discover,
        key,
        zone,
        title,
        description,
        requires,
        inputs,
        dedupe,
        hides_owned: false,
    }
}

use Input as I;
use SectionKey as K;
use SectionZone as Z;

/// Every section, each page's in page order.
pub const SECTIONS: &[SectionDef] = &[
    home(
        K::PopularAlbums,
        Z::WhatsHot,
        "Popular Now",
        "Albums popular across your music source this week.",
        None,
    ),
    home(
        K::TrendingArtists,
        Z::WhatsHot,
        "Trending Artists",
        "Artists trending across your music source right now.",
        None,
    ),
    home(
        K::WeeklyExploration,
        Z::ForYou,
        "Weekly Exploration",
        "Your ListenBrainz weekly exploration playlist.",
        Some(Requires::Listenbrainz),
    ),
    home(
        K::YourTopAlbums,
        Z::ForYou,
        "Your Top Albums",
        "What you personally played most this month.",
        None,
    ),
    home(
        K::RecentlyPlayed,
        Z::ForYou,
        "Recently Played",
        "Your latest plays in DroppedNeedle.",
        None,
    ),
    home(
        K::RecentlyAdded,
        Z::ForYou,
        "Recently Added",
        "The newest imports in your library.",
        Some(Requires::Library),
    ),
    home(
        K::GenreList,
        Z::BrowseGenres,
        "Browse Genres",
        "Genre tiles built from your library.",
        Some(Requires::Library),
    ),
    home(
        K::FavoriteArtists,
        Z::YourLibrary,
        "Favorite Artists",
        "Artists from your loved tracks.",
        None,
    ),
    home(
        K::LibraryArtists,
        Z::YourLibrary,
        "Library Artists",
        "A shelf of artists from your library.",
        Some(Requires::Library),
    ),
    home(
        K::LibraryAlbums,
        Z::YourLibrary,
        "Library Albums",
        "A shelf of albums from your library.",
        Some(Requires::Library),
    ),
    discover(
        K::DiscoverQueue,
        Z::Essentials,
        "Discover Queue",
        "A personalised album-by-album discovery deck.",
        None,
        &[],
        Dedupe::None,
    ),
    discover(
        K::TopPicks,
        Z::Essentials,
        "Top Picks for You",
        "Albums we think you'd like, scored against your taste.",
        None,
        &[I::Seeds, I::Similar, I::Genres],
        Dedupe::Albums(0),
    ),
    discover(
        K::ListenersLikeYou,
        Z::Essentials,
        "Listening Lounge",
        "Browse albums picked for you by ear - tap a cover to hear it.",
        None,
        &[],
        Dedupe::Albums(4),
    ),
    discover(
        K::WeeklyExploration,
        Z::Essentials,
        "Weekly Exploration",
        "Your ListenBrainz weekly exploration playlist.",
        Some(Requires::Listenbrainz),
        &[],
        Dedupe::None,
    ),
    discover(
        K::PlaylistDiscovery,
        Z::Essentials,
        "Discover for a Playlist",
        "Album suggestions seeded from any playlist.",
        None,
        &[],
        Dedupe::None,
    ),
    discover(
        K::DailyMixes,
        Z::MadeForYou,
        "Daily Mixes",
        "Genre-clustered mixes of new and familiar albums.",
        Some(Requires::Library),
        &[I::Seeds, I::Genres, I::LibraryAlbums],
        Dedupe::Albums(5),
    ),
    discover(
        K::RadioSections,
        Z::MadeForYou,
        "Radio Stations",
        "Album radios seeded by your top artists.",
        None,
        &[I::Seeds, I::LibraryAlbums],
        Dedupe::Albums(6),
    ),
    discover(
        K::BecauseYouListenTo,
        Z::BecauseYouListened,
        "Because You Listened",
        "Artists similar to the ones you play most.",
        None,
        &[I::Seeds, I::Similar],
        Dedupe::Artists,
    ),
    discover(
        K::ArtistsYouMightLike,
        Z::BecauseYouListened,
        "Artists You Might Like",
        "A wider net of similar artists.",
        None,
        &[I::Seeds, I::Similar],
        Dedupe::Artists,
    ),
    discover(
        K::PopularInYourGenres,
        Z::BecauseYouListened,
        "Popular in Your Genres",
        "Big names in the genres you listen to.",
        None,
        &[I::Genres, I::LastfmGenreArtists],
        Dedupe::Artists,
    ),
    discover(
        K::FreshReleases,
        Z::NewAndFresh,
        "Fresh Releases",
        "New releases picked for you by ListenBrainz.",
        Some(Requires::Listenbrainz),
        &[I::Fresh],
        Dedupe::Albums(1),
    ),
    discover(
        K::NewFromFollowed,
        Z::NewAndFresh,
        "New From Artists You Follow",
        "Fresh releases from your followed artists.",
        None,
        &[],
        Dedupe::Albums(2),
    ),
    SectionDef {
        hides_owned: true,
        ..discover(
            K::MissingEssentials,
            Z::NewAndFresh,
            "Missing Essentials",
            "Celebrated albums missing from artists you collect.",
            Some(Requires::Library),
            &[I::LibraryAlbums],
            Dedupe::Albums(3),
        )
    },
    discover(
        K::Rediscover,
        Z::FromYourLibrary,
        "Rediscover",
        "Library albums you haven't played in a while.",
        None,
        &[I::Jellyfin],
        Dedupe::None,
    ),
    discover(
        K::Anniversaries,
        Z::FromYourLibrary,
        "Milestone Anniversaries",
        "Library albums turning 10, 20, 30… this year.",
        Some(Requires::Library),
        &[],
        Dedupe::None,
    ),
    discover(
        K::LastfmRecentScrobbles,
        Z::FromYourLibrary,
        "Recent Scrobbles",
        "Your latest Last.fm scrobbles.",
        Some(Requires::Lastfm),
        &[I::LastfmRecent],
        Dedupe::Albums(8),
    ),
    discover(
        K::UnexploredGenres,
        Z::BrowseGenres,
        "Unexplored Genres",
        "Genres adjacent to your taste you haven't dug into.",
        None,
        &[I::Seeds, I::Similar],
        Dedupe::None,
    ),
    discover(
        K::GenreList,
        Z::BrowseGenres,
        "Browse Genres",
        "Genre tiles built from your library.",
        Some(Requires::Library),
        &[I::Genres],
        Dedupe::None,
    ),
    discover(
        K::GloballyTrending,
        Z::TrendingNow,
        "Globally Trending",
        "Artists trending worldwide.",
        None,
        &[I::Trending],
        Dedupe::Artists,
    ),
    discover(
        K::LastfmWeeklyArtistChart,
        Z::TrendingNow,
        "Last.fm Weekly Artists",
        "Your Last.fm weekly artist chart.",
        Some(Requires::Lastfm),
        &[I::LastfmWeeklyArtists],
        Dedupe::Artists,
    ),
    discover(
        K::LastfmWeeklyAlbumChart,
        Z::TrendingNow,
        "Last.fm Weekly Albums",
        "Your Last.fm weekly album chart.",
        Some(Requires::Lastfm),
        &[I::LastfmWeeklyAlbums],
        Dedupe::Albums(7),
    ),
];

/// A page's sections in page order.
pub fn of(page: Page) -> impl Iterator<Item = &'static SectionDef> {
    SECTIONS.iter().filter(move |def| def.page == page)
}

/// One section of a page.
pub fn find(page: Page, key: SectionKey) -> Option<&'static SectionDef> {
    of(page).find(|def| def.key == key)
}

/// The sections of `page` the user switched off. A failed read shows
/// every section, with a log line.
pub async fn switched_off(pool: &SqlitePool, user_id: &str, page: Page) -> HashSet<SectionKey> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT section_key FROM user_section_prefs \
         WHERE user_id = ?1 AND page = ?2 AND enabled = 0",
    )
    .bind(user_id)
    .bind(page.as_str())
    .fetch_all(pool)
    .await
    .unwrap_or_else(|error| {
        tracing::warn!(%error, page = page.as_str(), "section settings unreadable; showing every section");
        Vec::new()
    });
    rows.iter()
        .filter_map(|name| SectionKey::named(name))
        .filter(|key| find(page, *key).is_some())
        .collect()
}

/// The keys in `off`, sorted, as a saved build lists them.
pub fn sorted(off: &HashSet<SectionKey>) -> Vec<SectionKey> {
    let mut keys: Vec<SectionKey> = off.iter().copied().collect();
    keys.sort_by_key(|key| key.name());
    keys
}

/// The sections of `page` a build makes for a user who switched off
/// `off`, in page order.
pub fn wanted(page: Page, off: &HashSet<SectionKey>) -> Vec<&'static SectionDef> {
    of(page).filter(|def| !off.contains(&def.key)).collect()
}
