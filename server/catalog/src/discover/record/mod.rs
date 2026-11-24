//! The pick record (`catalog_discover_picks`): what Discover offered each
//! person and what came of it, one row per person and release group.
//!
//! Three writers fill a row, each its own columns:
//!
//! - [`offered`]: a saved deck or freshly built Top Picks
//!   stamp when each pick was offered, on which surface, and the evidence
//!   it carried: its reason and the paths that found it.
//! - [`PickRecord::event`]: the web UI says a card was shown (counted once
//!   per UTC day), sampled or opened. An album never offered to the caller
//!   writes nothing.
//! - [`outcomes`]: the daily job works out requested, arrived, played and
//!   what the person said from the request, library, play and feedback
//!   rows, and applies the retention.
//!
//! [`MadeFrom`] names where something was made. Requests, discovery batches
//! and answers to Discover carry it, so the job can tell which surface a
//! request came from.

pub mod outcomes;
pub mod store;

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::discover::cache::{ForgetCounts, Generation};
use crate::discover::engine::candidate::{Candidate, Path};
use crate::discover::models::PickReason;
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError};

/// Where a request, a batch, an answer or a pick was made. Absent means not
/// known. Declared whole: a stored CHECK lists every value, the ones later
/// work first writes included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MadeFrom {
    /// The Discover queue deck.
    Deck,
    /// Top Picks on the Discover page.
    TopPicks,
    /// Any other shelf on the Discover page.
    DiscoverShelf,
    /// A shelf on the home page.
    HomeShelf,
    /// The playlist suggestions tool.
    PlaylistSuggestions,
    /// An album's page.
    AlbumPage,
    /// An artist's page.
    ArtistPage,
    /// The Weekly Mix's standing auto-request.
    WeeklyMix,
    /// Search results.
    Search,
    /// The new releases of followed artists.
    NewReleases,
    /// The weekly set. Not written yet: the weekly set writes it.
    WeeklySet,
    /// Dig further, under the weekly set. Not written yet: the weekly set
    /// writes it.
    DigFurther,
    /// The Later pile.
    Later,
    /// A thread's page. Not written yet: the thread pages write it.
    ThreadPage,
    /// Radio. Not written yet: radio writes it once it can ask for an
    /// album.
    Radio,
    /// A mix. Not written yet: the mixes write it.
    Mix,
    /// Listen first. Not written yet: listening before owning writes it.
    ListenFirst,
    /// The albums passed on. Not written yet: the passed-on list writes it.
    PassedOn,
}

droppedneedle_kernel::spelled!(MadeFrom {
    Deck => "deck",
    TopPicks => "top_picks",
    DiscoverShelf => "discover_shelf",
    HomeShelf => "home_shelf",
    PlaylistSuggestions => "playlist_suggestions",
    AlbumPage => "album_page",
    ArtistPage => "artist_page",
    WeeklyMix => "weekly_mix",
    Search => "search",
    NewReleases => "new_releases",
    WeeklySet => "weekly_set",
    DigFurther => "dig_further",
    Later => "later",
    ThreadPage => "thread_page",
    Radio => "radio",
    Mix => "mix",
    ListenFirst => "listen_first",
    PassedOn => "passed_on",
});

/// What the web UI saw happen to a pick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PickEvent {
    /// It became the card on screen: the deck's current card, or the
    /// featured Top Pick.
    Shown,
    /// Its sample started.
    Sampled,
    /// Its album page was opened from the card.
    Opened,
}

/// The evidence an offered pick carried, as stored: the reason it showed
/// and every path that found it. The version names this shape, so a later
/// shape can be told apart; rows of version 1 (a card's reason lines, its
/// seed's name and whether it was a chart card) are left as they were.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// The shape's version.
    pub version: u32,
    /// How the candidate came into the pool.
    pub kind: String,
    /// The reason the pick showed.
    pub reason: PickReason,
    /// Every path that found it, strongest first.
    pub paths: Vec<EvidencePath>,
}

/// One path behind an offered pick, as stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidencePath {
    /// What it is.
    pub kind: String,
    /// Where its fact came from.
    pub source: String,
    /// When that fact was fetched, unix ms, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetched_at: Option<i64>,
    /// The person's artist it runs through, for a seed path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<EvidenceArtist>,
    /// The link, for a seed path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<EvidenceLink>,
    /// A known artist credited on the album, lowercased.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub known_credit: Option<String>,
}

/// The person's artist a path runs through, as it stood when offered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceArtist {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// The person's plays and imported listens of it.
    pub plays: i64,
    /// Its weight in their taste.
    pub weight: f64,
    /// How many of its albums the library held.
    pub albums_owned: i64,
}

/// A path's link from the person's artist to the pick's, as it stood.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceLink {
    /// Its strength.
    pub strength: f64,
    /// The pick's artist's place in that artist's list, from 1.
    pub rank: i64,
    /// How long that list was.
    pub of: i64,
    /// Whether ListenBrainz labs' list named it.
    pub labs: bool,
    /// Whether Last.fm's list named it.
    pub lastfm: bool,
    /// Whether it was the closest artist in that list the person did not
    /// know.
    pub closest_new: bool,
}

/// The version [`Evidence`] is written at.
pub const EVIDENCE_VERSION: u32 = 2;

impl EvidencePath {
    fn of(path: &Path) -> Self {
        Self {
            kind: path.kind.as_str().to_owned(),
            source: path.source.as_str().to_owned(),
            fetched_at: path.fetched_at,
            artist: path.artist.as_ref().map(|artist| EvidenceArtist {
                mbid: artist.mbid.clone(),
                name: artist.name.clone(),
                plays: artist.plays,
                weight: artist.weight,
                albums_owned: artist.albums_owned,
            }),
            link: path.link.as_ref().map(|link| EvidenceLink {
                strength: link.strength,
                rank: link.rank,
                of: link.of,
                labs: link.labs,
                lastfm: link.lastfm,
                closest_new: link.closest_new,
            }),
            known_credit: path.known_credit.clone(),
        }
    }
}

/// One pick a build offered.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// Release-group MBID, as the pick has it.
    pub release_group_mbid: String,
    /// The artist's MBID, or empty.
    pub artist_mbid: String,
    /// Album title.
    pub album_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Why it was offered.
    pub evidence: Evidence,
}

impl Offer {
    /// A pick from the pool, offered with `reason`, with the candidate's
    /// paths as its evidence.
    pub fn from_pick(candidate: &Candidate, reason: &PickReason) -> Self {
        Self {
            release_group_mbid: candidate.release_group_mbid.clone(),
            artist_mbid: candidate.artist_mbid.clone(),
            album_name: candidate.title.clone(),
            artist_name: candidate.artist_name.clone(),
            evidence: Evidence {
                version: EVIDENCE_VERSION,
                kind: candidate.kind.as_str().to_owned(),
                reason: reason.clone(),
                paths: candidate.paths.iter().map(EvidencePath::of).collect(),
            },
        }
    }
}

/// Why an event was refused.
#[derive(Debug)]
pub enum EventError {
    /// The release group is not a MusicBrainz id.
    Invalid(String),
    /// The database failed.
    Store(DbError),
}

impl From<DbError> for EventError {
    fn from(error: DbError) -> Self {
        Self::Store(error)
    }
}

/// The pick record's writes from builds and the web UI.
#[derive(Clone)]
pub struct PickRecord {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl PickRecord {
    /// The record on `db`, dated by `clock`.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// Stamp `event` on the caller's row for `release_group_mbid` now.
    /// An album never offered to them writes nothing.
    pub async fn event(
        &self,
        user_id: &str,
        release_group_mbid: &str,
        event: PickEvent,
    ) -> Result<(), EventError> {
        let group = release_group_mbid.trim().to_ascii_lowercase();
        if !droppedneedle_providers::musicbrainz::is_valid_mbid(&group) {
            return Err(EventError::Invalid(
                "release_group_mbid must be a MusicBrainz id".to_owned(),
            ));
        }
        store::stamp(&self.db, user_id, &group, event, self.clock.now_ms()).await?;
        Ok(())
    }
}

/// What a build that offers picks was started under: its generation and
/// the forget counts it is held to.
#[derive(Clone, Copy)]
pub struct Built<'a> {
    /// The generation the build was claimed under.
    pub generation: Generation,
    /// Each person's forget count.
    pub forgets: &'a ForgetCounts,
}

/// Record that `offers` were offered to `user_id` on `surface` at `now_ms`,
/// in one background write. A build the person's "Forget my history" came
/// after records nothing. A failed write is logged and dropped: a build
/// never fails because of it, and the next build offers again.
pub async fn offered(
    db: &Db,
    user_id: &str,
    surface: MadeFrom,
    offers: Vec<Offer>,
    now_ms: i64,
    built: Built<'_>,
) {
    if offers.is_empty() {
        return;
    }
    let count = offers.len();
    if let Err(error) = store::offered(db, user_id, surface, offers, now_ms, built).await {
        tracing::warn!(%error, surface = surface.as_str(), count, "picks offered not recorded");
    }
}

/// Remove every row of the pick record that is `user_id`'s, in one write.
pub async fn forget(db: &Db, user_id: &str) -> Result<usize, DbError> {
    store::forget(db, user_id).await
}
