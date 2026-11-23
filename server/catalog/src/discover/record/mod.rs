//! The pick record (`catalog_discover_picks`): what Discover offered each
//! person and what came of it, one row per person and release group.
//!
//! Three writers fill a row, each its own columns:
//!
//! - [`offered`]: a saved deck or freshly built Top Picks
//!   stamp when each card was offered, on which surface, and the evidence
//!   it carried.
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

use crate::discover::models::{QueueItemLight, ShelfSource, TopPicksSection};
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
    /// The Later pile. Not written yet: the Later pile writes it.
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

/// The evidence an offered card carried, as stored. The version names this
/// shape, so a later shape can be told apart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    /// The shape's version.
    pub version: u32,
    /// The reason lines the card showed.
    #[serde(default)]
    pub reasons: Vec<String>,
    /// The seed artist's name, when the card names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_artist: Option<String>,
    /// Whether it was a chart card rather than one of the person's own.
    #[serde(default)]
    pub chart: bool,
    /// The listening service behind it, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<ShelfSource>,
}

/// The version [`Evidence`] is written at.
pub const EVIDENCE_VERSION: u32 = 1;

/// One card a build offered.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// Release-group MBID, as the card has it.
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
    /// A deck card from a deck built on `service`. A wildcard is a
    /// trending card, so it counts as a chart card.
    pub fn from_card(card: &QueueItemLight, service: Option<ShelfSource>) -> Self {
        Self {
            release_group_mbid: card.musicbrainz_id.clone(),
            artist_mbid: card.artist_id.clone(),
            album_name: card.title.clone(),
            artist_name: card.artist_name.clone(),
            evidence: Evidence {
                version: EVIDENCE_VERSION,
                reasons: Some(card.recommendation_reason.clone())
                    .filter(|reason| !reason.trim().is_empty())
                    .into_iter()
                    .collect(),
                seed_artist: None,
                chart: card.is_wildcard,
                service,
            },
        }
    }

    /// Every Top Pick of a built section. A pick without a release group
    /// is left out.
    pub fn from_top_picks(section: &TopPicksSection) -> Vec<Self> {
        section
            .items
            .iter()
            .filter_map(|pick| {
                Some(Self {
                    release_group_mbid: pick.album.musicbrainz_id.clone()?,
                    artist_mbid: pick.album.artist_id.clone().unwrap_or_default(),
                    album_name: pick.album.title.clone(),
                    artist_name: pick.album.artist_name.clone().unwrap_or_default(),
                    evidence: Evidence {
                        version: EVIDENCE_VERSION,
                        reasons: pick.reasons.clone(),
                        seed_artist: pick.seed_artist.clone(),
                        chart: false,
                        service: section.source,
                    },
                })
            })
            .collect()
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

/// Record that `offers` were offered to `user_id` on `surface` at `now_ms`,
/// in one background write. A failed write is logged and dropped: a build
/// never fails because of it, and the next build offers again.
pub async fn offered(db: &Db, user_id: &str, surface: MadeFrom, offers: Vec<Offer>, now_ms: i64) {
    if offers.is_empty() {
        return;
    }
    let count = offers.len();
    if let Err(error) = store::offered(db, user_id, surface, offers, now_ms).await {
        tracing::warn!(%error, surface = surface.as_str(), count, "picks offered not recorded");
    }
}

/// Remove every row of the pick record that is `user_id`'s, in one write.
pub async fn forget(db: &Db, user_id: &str) -> Result<usize, DbError> {
    store::forget(db, user_id).await
}
