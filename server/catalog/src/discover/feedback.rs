//! What a person told Discover (`catalog_discover_feedback`), and the one
//! filter every recommending surface reads it through.
//!
//! An answer is about an album or an artist, one row per person and
//! target, so a second answer about the same album replaces the first.
//! "No" and "Know it already" hide the album until undone, "Not now"
//! for three months, and "Not this artist" every album and tile filed
//! under that artist (the row's own artist credit) until undone. A row
//! is in effect while it has no expiry or its expiry is still ahead.
//!
//! [`Feedback`] holds one person's answers in effect, read once, and
//! answers [`Feedback::hides_album`] and [`Feedback::hides_artist`]. Every
//! build that makes album or artist rows for a person filters through it,
//! and so does every serve of a saved page or teaser, so an answer
//! takes effect at once. Only the sections that recommend to the person
//! ([`applies`]) are filtered: charts, the library, the person's own
//! history, search and an artist's own lists are not.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use time::{Date, OffsetDateTime};
use utoipa::ToSchema;

use crate::discover::models::{
    AlbumRef, ArtistRef, DiscoverFeedbackEntry, DiscoverFeedbackRequest, PageSection,
    RadioPlanTrack, RadioSeedKind, SectionContent, SectionItem, SectionKey,
};
use crate::discover::queue::QueueSettings;
use crate::discover::sections::{self, Basis, Page};
use crate::discover::{read_all, sql};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;

/// Whether an answer is about an album or an artist.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackTarget {
    /// An album, by its release group.
    Album,
    /// An artist.
    Artist,
}

droppedneedle_kernel::spelled!(FeedbackTarget {
    Album => "album",
    Artist => "artist",
});

/// What a person said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedbackKind {
    /// Not for them: the album is hidden until undone.
    No,
    /// They know it already: the album is hidden until undone, and counts
    /// in their favour.
    KnowIt,
    /// Not now: the album is hidden for three months.
    NotNow,
    /// Not this artist: every album filed under them is hidden until
    /// undone.
    NotArtist,
}

droppedneedle_kernel::spelled!(FeedbackKind {
    No => "no",
    KnowIt => "know_it",
    NotNow => "not_now",
    NotArtist => "not_artist",
});

impl FeedbackKind {
    /// The target an answer of this kind is about.
    pub fn target(self) -> FeedbackTarget {
        match self {
            Self::NotArtist => FeedbackTarget::Artist,
            Self::No | Self::KnowIt | Self::NotNow => FeedbackTarget::Album,
        }
    }
}

/// A day, ms.
const DAY_MS: i64 = 86_400_000;

/// Three calendar months after `now_ms`, at the same time of day; the last
/// day of the month when that month is shorter.
fn three_months_on(now_ms: i64) -> i64 {
    let fallback = now_ms + 92 * DAY_MS;
    let Ok(at) = OffsetDateTime::from_unix_timestamp_nanos(i128::from(now_ms) * 1_000_000) else {
        return fallback;
    };
    let month = at.month().nth_next(3);
    let year = if (at.month() as u8) > 9 {
        at.year() + 1
    } else {
        at.year()
    };
    let mut day = at.day();
    let date = loop {
        match Date::from_calendar_date(year, month, day) {
            Ok(date) => break date,
            Err(_) if day > 28 => day -= 1,
            Err(_) => return fallback,
        }
    };
    i64::try_from(at.replace_date(date).unix_timestamp_nanos() / 1_000_000).unwrap_or(fallback)
}

/// Why a saved answer was refused.
#[derive(Debug)]
pub enum FeedbackError {
    /// The request does not say what it is about.
    Invalid(String),
    /// The database failed.
    Store(DbError),
}

impl From<DbError> for FeedbackError {
    fn from(error: DbError) -> Self {
        Self::Store(error)
    }
}

/// A MusicBrainz id from a request, trimmed and lowercased; `None` when
/// blank, an error when it is not an id.
fn checked_mbid(value: Option<&str>, what: &str) -> Result<Option<String>, FeedbackError> {
    let Some(trimmed) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(trimmed) {
        return Err(FeedbackError::Invalid(format!(
            "{what} must be a MusicBrainz id"
        )));
    }
    Ok(Some(trimmed.to_ascii_lowercase()))
}

/// The parsed target of an undo: its kind from the path, and its id.
pub fn checked_target(target: &str, mbid: &str) -> Result<(FeedbackTarget, String), String> {
    let target = FeedbackTarget::parse(target.trim())
        .ok_or_else(|| "target must be album or artist".to_owned())?;
    let mbid = mbid.trim();
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(mbid) {
        return Err("mbid must be a MusicBrainz id".to_owned());
    }
    Ok((target, mbid.to_ascii_lowercase()))
}

fn entry(row: sql::FeedbackColumns) -> DiscoverFeedbackEntry {
    let Millis(created_at) = row.created_at;
    DiscoverFeedbackEntry {
        target: row.target,
        target_mbid: row.target_mbid_lower,
        kind: row.kind,
        release_group_mbid: row.release_group_mbid,
        artist_mbid: row.artist_mbid,
        release_name: row.release_name,
        artist_name: row.artist_name,
        created_at: created_at / 1000,
        expires_at: row.expires_at.map(|Millis(at)| at / 1000),
    }
}

/// Record one answer at `now_ms` and return it as stored. A later answer
/// about the same target replaces the earlier one. Nothing is recorded
/// for a person who no longer exists.
pub async fn save(
    db: &Db,
    user_id: &str,
    request: &DiscoverFeedbackRequest,
    now_ms: i64,
) -> Result<DiscoverFeedbackEntry, FeedbackError> {
    let release_group = checked_mbid(request.release_group_mbid.as_deref(), "release_group_mbid")?;
    let target = request.kind.target();
    // An album's artist is only kept for the list, so one that is not an id
    // is left out rather than refusing the answer.
    let artist = match checked_mbid(request.artist_mbid.as_deref(), "artist_mbid") {
        Err(_) if target == FeedbackTarget::Album => None,
        checked => checked?,
    };
    let target_mbid = match target {
        FeedbackTarget::Album => release_group.clone().ok_or_else(|| {
            FeedbackError::Invalid("release_group_mbid is needed for an album".to_owned())
        })?,
        FeedbackTarget::Artist => artist.clone().ok_or_else(|| {
            FeedbackError::Invalid("artist_mbid is needed for an artist".to_owned())
        })?,
    };
    let expires_at = (request.kind == FeedbackKind::NotNow).then(|| three_months_on(now_ms));
    let stored = DiscoverFeedbackEntry {
        target,
        target_mbid,
        kind: request.kind,
        release_group_mbid: release_group.unwrap_or_default(),
        artist_mbid: artist.unwrap_or_default(),
        release_name: request.release_name.trim().to_owned(),
        artist_name: request.artist_name.trim().to_owned(),
        created_at: now_ms / 1000,
        expires_at: expires_at.map(|at| at / 1000),
    };
    let user_id = user_id.to_owned();
    let row = stored.clone();
    let made_from = request.made_from;
    db.write(Lane::Foreground, "discover.feedback.save", move |tx| {
        tx.execute(
            sql::SAVE_FEEDBACK.text(),
            rusqlite::params![
                user_id,
                row.target,
                row.target_mbid,
                row.kind,
                row.release_group_mbid,
                row.artist_mbid,
                row.release_name,
                row.artist_name,
                Millis(now_ms),
                expires_at.map(Millis),
                made_from,
            ],
        )?;
        Ok::<_, DbError>(())
    })
    .await?;
    Ok(stored)
}

/// Take back one of a person's answers. Nothing happens when they gave no
/// such answer.
pub async fn undo(
    db: &Db,
    user_id: &str,
    target: FeedbackTarget,
    mbid_lower: &str,
) -> Result<(), DbError> {
    let user_id = user_id.to_owned();
    let mbid_lower = mbid_lower.to_owned();
    db.write(Lane::Foreground, "discover.feedback.undo", move |tx| {
        tx.execute(
            sql::UNDO_FEEDBACK.text(),
            rusqlite::params![user_id, target, mbid_lower],
        )?;
        Ok(())
    })
    .await
}

/// A person's answers in effect at `now_ms`, newest first.
pub async fn list(
    db: &Db,
    user_id: &str,
    now_ms: i64,
) -> Result<Vec<DiscoverFeedbackEntry>, DbError> {
    let rows = read_all(
        db,
        "discover.feedback.list",
        sql::FEEDBACK_LIST.text(),
        (user_id.to_owned(), Millis(now_ms)),
        sql::FeedbackColumns::read,
    )
    .await?;
    Ok(rows.into_iter().map(entry).collect())
}

/// Drop every answer past its expiry at `now_ms`, and the hides carried
/// from before typed answers (a `no` with an expiry) made before
/// `carried_before`, so a retention shorter than their year still holds.
/// Returns how many went.
pub async fn prune(db: &Db, now_ms: i64, carried_before: i64) -> Result<usize, DbError> {
    db.write(Lane::Background, "discover.feedback.prune", move |tx| {
        Ok(tx.execute(
            sql::PRUNE_FEEDBACK.text(),
            rusqlite::params![Millis(now_ms), Millis(carried_before), FeedbackKind::No],
        )?)
    })
    .await
}

/// What people told Discover, as its routes reach it. The weekly build
/// drops the lapsed ones, at most once per prune interval, so a save never
/// waits on it.
pub struct Answers {
    db: Db,
    clock: Arc<dyn Clock>,
    settings: Live<AdvancedSettings>,
    last_prune: Mutex<i64>,
}

impl Answers {
    /// The answers in `db`, dated by `clock`, pruned as `settings` say.
    pub fn new(db: Db, clock: Arc<dyn Clock>, settings: Live<AdvancedSettings>) -> Self {
        Self {
            db,
            clock,
            settings,
            last_prune: Mutex::new(0),
        }
    }

    /// Record one answer for the user and return it as stored.
    ///
    /// # Errors
    ///
    /// A request that says nothing it can be about, or the store's failure.
    pub async fn save(
        &self,
        user_id: &str,
        request: &DiscoverFeedbackRequest,
    ) -> Result<DiscoverFeedbackEntry, FeedbackError> {
        save(&self.db, user_id, request, self.clock.now_ms()).await
    }

    /// Take back one of the user's answers.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn undo(
        &self,
        user_id: &str,
        target: FeedbackTarget,
        mbid_lower: &str,
    ) -> Result<(), DbError> {
        undo(&self.db, user_id, target, mbid_lower).await
    }

    /// The user's answers in effect, newest first.
    ///
    /// # Errors
    ///
    /// The store's failure.
    pub async fn list(&self, user_id: &str) -> Result<Vec<DiscoverFeedbackEntry>, DbError> {
        list(&self.db, user_id, self.clock.now_ms()).await
    }

    /// Drop answers past their expiry, and carried hides past their
    /// retention, when the prune interval has passed. While the settings
    /// cannot be read nothing is dropped.
    pub async fn prune_if_due(&self) {
        let Some(settings) = QueueSettings::read(&self.settings) else {
            return;
        };
        let now = self.clock.now_ms();
        {
            let mut last = self
                .last_prune
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if now - *last < settings.prune_interval_ms {
                return;
            }
            *last = now;
        }
        let cutoff = now - settings.ignored_retention_days * 86_400_000;
        match prune(&self.db, now, cutoff).await {
            Ok(0) => {}
            Ok(dropped) => tracing::info!(dropped, "lapsed discover answers removed"),
            Err(cause) => tracing::warn!(%cause, "discover answers not pruned; trying again later"),
        }
    }
}

/// Whether the filter applies to section `key` of `page`: a section about
/// the person, never a chart, the library or their own history.
pub fn applies(page: Page, key: SectionKey) -> bool {
    sections::find(page, key).is_some_and(|def| def.basis == Basis::Personal)
}

/// One person's answers in effect, read once: what they hide.
#[derive(Debug, Clone, Default)]
pub struct Feedback {
    albums: HashSet<String>,
    artists: HashSet<String>,
}

impl Feedback {
    /// The person's answers in effect at `now_ms`.
    pub async fn load(db: &Db, user_id: &str, now_ms: i64) -> Result<Self, DbError> {
        let rows = read_all(
            db,
            "discover.feedback.in_effect",
            sql::FEEDBACK_IN_EFFECT.text(),
            (user_id.to_owned(), Millis(now_ms)),
            sql::FeedbackTargetColumns::read,
        )
        .await?;
        let mut feedback = Self::default();
        for row in rows {
            let id = row.target_mbid_lower.trim().to_ascii_lowercase();
            if id.is_empty() {
                continue;
            }
            match row.target {
                FeedbackTarget::Album => feedback.albums.insert(id),
                FeedbackTarget::Artist => feedback.artists.insert(id),
            };
        }
        Ok(feedback)
    }

    /// [`Self::load`] for a page, set or list: when the rows cannot be
    /// read it logs and hides nothing, so the surface is served as built.
    pub async fn load_or_none(db: &Db, user_id: &str, now_ms: i64, surface: &str) -> Self {
        Self::load(db, user_id, now_ms)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, surface, "discover feedback unreadable; served as built");
                Self::default()
            })
    }

    /// The hidden albums' release groups, lowercased, for the builds that
    /// skip ids before they pick.
    pub fn albums(&self) -> &HashSet<String> {
        &self.albums
    }

    /// Whether an album row, by its release group and the artist it is
    /// filed under, is hidden.
    pub fn hides_album(&self, release_group: Option<&str>, artist: Option<&str>) -> bool {
        release_group.is_some_and(|id| self.albums.contains(&id.trim().to_ascii_lowercase()))
            || self.hides_artist(artist)
    }

    /// Whether an artist is hidden.
    pub fn hides_artist(&self, artist: Option<&str>) -> bool {
        artist.is_some_and(|id| self.artists.contains(&id.trim().to_ascii_lowercase()))
    }

    /// Whether a shelf row is hidden. Genre rows never are.
    pub fn hides_row(&self, item: &SectionItem) -> bool {
        match item {
            SectionItem::Album(album) => self.hides_album_row(album),
            SectionItem::Artist(artist) => self.hides_artist_row(artist),
            SectionItem::Track(track) => self.hides_artist(track.artist_id.as_deref()),
            SectionItem::Genre(_) => false,
        }
    }

    /// Whether an album row is hidden.
    pub fn hides_album_row(&self, album: &AlbumRef) -> bool {
        self.hides_album(album.musicbrainz_id.as_deref(), album.artist_id.as_deref())
    }

    /// Whether an artist row is hidden.
    pub fn hides_artist_row(&self, artist: &ArtistRef) -> bool {
        self.hides_artist(artist.musicbrainz_id.as_deref())
    }

    /// A section without the rows this person hid. A shelf of several left
    /// with no rows goes, as does a radio shelf seeded by an artist they
    /// hid.
    pub fn filter_content(&self, content: &mut SectionContent) {
        if self.albums.is_empty() && self.artists.is_empty() {
            return;
        }
        match content {
            SectionContent::Weekly { weekly } => weekly.tracks.retain(|track| {
                !self.hides_album(
                    track.release_group_mbid.as_deref(),
                    track.artist_mbid.as_deref(),
                )
            }),
            SectionContent::Shelves { shelves } => {
                shelves.retain(|shelf| {
                    !(shelf.radio_seed_type == Some(RadioSeedKind::Artist)
                        && self.hides_artist(shelf.radio_seed_id.as_deref()))
                });
                // A shelf this filter empties goes; one that came without
                // rows keeps its message.
                shelves.retain_mut(|shelf| {
                    let had = shelf.items.len();
                    shelf.items.retain(|item| !self.hides_row(item));
                    had == 0 || !shelf.items.is_empty()
                });
            }
            SectionContent::Shelf { shelf } => shelf.items.retain(|item| !self.hides_row(item)),
            SectionContent::NewReleases { new_releases } => {
                let before = new_releases.releases.len();
                new_releases
                    .releases
                    .retain(|row| !self.hides_album_row(&row.album));
                let hidden = before - new_releases.releases.len();
                new_releases.total = new_releases
                    .total
                    .saturating_sub(u64::try_from(hidden).unwrap_or(0));
            }
            SectionContent::Gaps { gaps } => gaps.retain(|gap| !self.hides_album_row(&gap.album)),
            // The person's own albums: nothing there is a recommendation.
            SectionContent::OwnShelves { .. } => {}
            // The person's own tracks, picked by sound when they tune.
            SectionContent::SoundBench { .. } => {}
        }
    }

    /// [`Self::filter_content`] on each section of `page` it applies to
    /// ([`applies`]), leaving out those it empties.
    pub fn filter_sections(&self, page: Page, sections: &mut Vec<PageSection>) {
        if self.albums.is_empty() && self.artists.is_empty() {
            return;
        }
        sections.retain_mut(|section| {
            if !applies(page, section.key) {
                return true;
            }
            self.filter_content(&mut section.content);
            !section.content.is_empty()
        });
    }

    /// Shelf rows without the hidden ones.
    pub fn keep_rows(&self, items: &mut Vec<SectionItem>) {
        items.retain(|item| !self.hides_row(item));
    }

    /// Album rows without the hidden ones.
    pub fn keep_albums(&self, albums: &mut Vec<AlbumRef>) {
        albums.retain(|album| !self.hides_album_row(album));
    }

    /// Plan tracks without those on an album the person hid.
    pub fn keep_tracks(&self, tracks: &mut Vec<RadioPlanTrack>) {
        tracks.retain(|track| !self.hides_album(track.album_mbid.as_deref(), None));
    }
}
