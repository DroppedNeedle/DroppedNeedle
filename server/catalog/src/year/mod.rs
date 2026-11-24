//! A person's year in review: their top albums, artists and songs of a
//! calendar year, the artists new to them, and when they played, worked
//! out each time it is asked for. Nothing is stored.
//!
//! - **Whose.** Only the caller's own year. Both routes take the signed-in
//!   person and no user id; no admin route, job or other area reads a
//!   person's year, and nothing in it compares them with anyone else.
//! - **What counts.** Their plays here (`library_play_history`), the web
//!   player's and every connected app's, in the year: one list of plays,
//!   [`store`]'s `year_plays`, over one rule, `COUNTED` in [`sql`]. An
//!   imported ListenBrainz listen, how a playback ended and a play mark
//!   never add a play, an hour or a place in a list.
//! - **The calendar.** The year runs from the start of 1 January to the
//!   start of the next in the time zone the request names, else the
//!   server's `TZ`, else UTC. The bounds, months, days and streak are
//!   worked out in Rust from the kernel clock; SQLite does no dates.
//! - **Keys.** An artist is the play's library artist, shown by its name
//!   now; placeholder rows (Various Artists, unknown) are left out. A play
//!   no artist row holds counts under the name key of its artist name, never
//!   joined to a library artist by name, and is never new. An album is the
//!   one its track is on now, else the one the play names, and only an
//!   owned album is listed. A song is its recording, else its track, else
//!   its artist and title, so one song on an album and a best-of counts
//!   once.
//! - **New to you.** An artist is new when its first play here is in the
//!   year and, when it has a MusicBrainz id, no imported listen before the
//!   year names it. In a person's first year (no play and no imported
//!   listen before it) every artist would be new, so none is shown. A
//!   listen naming an artist id MusicBrainz later merged away matches no
//!   library artist, so that artist may show as new.
//! - **Too few.** Under 30 plays or 5 artists the year is `too_few_plays`
//!   and its lists stay empty; the totals still show.

pub mod handlers;
pub mod models;
pub(crate) mod review;
pub(crate) mod sql;
pub(crate) mod store;

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use crate::local::LibraryDeps;
use crate::local::services::{LibraryFailure, albums_without_art, artists_missing_art};
use crate::local::stores::{ArtistRecord, StoreError};
use models::{
    YearReview, YearReviewAlbum, YearReviewArtist, YearReviewDay, YearReviewFirstArtist,
    YearReviewFirstPlay, YearReviewMonth, YearReviewNewArtists, YearReviewSong, YearReviewState,
    YearReviewStreak, YearReviewYear, YearReviewYears, ZoneName,
};
use review::{PlayedArtist, local_date};
use store::Span;

pub use handlers::routes;

/// The first and last years a review can name.
const YEARS: std::ops::RangeInclusive<i32> = 1..=9999;

/// Why a year in review could not be answered.
#[derive(Debug)]
pub enum YearError {
    /// No review for that year: outside 1 to 9999, after the current one,
    /// or an earlier year with no play.
    NotFound,
    /// The database was too busy. Names the operation.
    Busy(String),
    /// A store fault. For the log only.
    Internal(String),
}

impl From<LibraryFailure> for YearError {
    fn from(failure: LibraryFailure) -> Self {
        match failure {
            LibraryFailure::Busy(operation) => Self::Busy(operation),
            other => Self::Internal(format!("year review: {other:?}")),
        }
    }
}

impl From<StoreError> for YearError {
    fn from(error: StoreError) -> Self {
        LibraryFailure::from(error).into()
    }
}

/// The server's own time zone, resolved once at start, and the name it
/// answers under.
#[derive(Debug, Clone)]
pub struct ServerZone {
    zone: TimeZone,
    name: String,
}

impl ServerZone {
    /// `zone`, answered as `name`.
    pub fn new(zone: TimeZone, name: String) -> Self {
        Self { zone, name }
    }
}

/// The year in review over the plays, built once.
#[derive(Clone)]
pub struct YearReviews {
    clock: Arc<dyn Clock>,
    zone: ServerZone,
    library: LibraryDeps,
}

/// The first millisecond of `year` in `zone`.
fn year_start(zone: &TimeZone, year: i32) -> Option<i64> {
    let date = Date::new(i16::try_from(year).ok()?, 1, 1).ok()?;
    zone.to_timestamp(date.into())
        .ok()
        .map(Timestamp::as_millisecond)
}

impl YearReviews {
    /// Reviews at `clock`'s time, in `zone` when a request names none it
    /// can use, over the plays the library catalog of `library` reads; it
    /// also says which covers are missing.
    pub fn new(clock: Arc<dyn Clock>, zone: ServerZone, library: LibraryDeps) -> Self {
        Self {
            clock,
            zone,
            library,
        }
    }

    /// The zone a request counts in: the one it names when it exists, else
    /// the server's, with the name the answer gives. jiff answers
    /// `Etc/Unknown` with its own unknown zone, which is no calendar.
    fn zone(&self, asked: Option<&ZoneName>) -> (TimeZone, String) {
        asked
            .and_then(|name| {
                let found = TimeZone::get(name.as_str()).ok();
                found.filter(|zone| !zone.is_unknown()).map(|zone| {
                    let named = zone.iana_name().unwrap_or(name.as_str()).to_owned();
                    (zone, named)
                })
            })
            .unwrap_or_else(|| (self.zone.zone.clone(), self.zone.name.clone()))
    }

    /// The years `user_id` can look back on in the zone `asked`: the
    /// current year, then each earlier one with a play, newest first.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn years(
        &self,
        user_id: &str,
        asked: Option<&ZoneName>,
    ) -> Result<YearReviewYears, YearError> {
        let (zone, name) = self.zone(asked);
        let current = i32::from(local_date(&zone, self.clock.now_ms()).year());
        let end = year_start(&zone, current + 1).unwrap_or(i64::MAX);
        let (user, counting) = (user_id.to_owned(), zone.clone());
        let counted = self
            .library
            .catalog
            .read_long("year.list", move |conn| {
                store::read_years(conn, &user, end, current, |year| {
                    year_start(&counting, year)
                })
            })
            .await?;
        let mut years: Vec<YearReviewYear> = counted
            .into_iter()
            .map(|(year, plays, artists)| YearReviewYear {
                year,
                plays,
                state: review::state(plays, artists, year == current),
            })
            .collect();
        if years.first().is_none_or(|first| first.year != current) {
            years.insert(
                0,
                YearReviewYear {
                    year: current,
                    plays: 0,
                    state: YearReviewState::TooFewPlays,
                },
            );
        }
        Ok(YearReviewYears { zone: name, years })
    }

    /// `user_id`'s review of `year` (as the path names it) in the zone
    /// `asked`.
    ///
    /// # Errors
    ///
    /// [`YearError::NotFound`] for a year that is not a number from 1 to
    /// 9999, is after the current one, or is an earlier year with no play;
    /// else the read's failure.
    pub async fn review(
        &self,
        user_id: &str,
        year: &str,
        asked: Option<&ZoneName>,
    ) -> Result<YearReview, YearError> {
        let year: i32 = year.parse().map_err(|_| YearError::NotFound)?;
        if !YEARS.contains(&year) {
            return Err(YearError::NotFound);
        }
        let (zone, name) = self.zone(asked);
        let today = local_date(&zone, self.clock.now_ms());
        let current = i32::from(today.year());
        if year > current {
            return Err(YearError::NotFound);
        }
        let (Some(start), Some(end)) = (year_start(&zone, year), year_start(&zone, year + 1))
        else {
            return Err(YearError::NotFound);
        };
        let in_progress = year == current;
        let (user, counting) = (user_id.to_owned(), zone.clone());
        let read = self
            .library
            .catalog
            .read_long("year.review", move |conn| {
                store::read_year(conn, &user, Span { start, end }, &counting, in_progress)
            })
            .await?;
        let totals = &read.worked.totals;
        if !in_progress && totals.plays == 0 {
            return Err(YearError::NotFound);
        }
        let state = review::state(totals.plays, totals.artists, in_progress);
        let iso = |date: Date| date.to_string();
        let first_day = Date::new(i16::try_from(year).unwrap_or(1), 1, 1).unwrap_or(today);
        let last_day = if in_progress {
            today
        } else {
            Date::new(i16::try_from(year).unwrap_or(1), 12, 31).unwrap_or(today)
        };
        let mut review = YearReview {
            year,
            zone: name,
            state,
            first_day: iso(first_day),
            last_day: iso(last_day),
            plays: totals.plays,
            artists: totals.artists,
            days_played: totals.days_played,
            listened_ms: totals.listened_ms,
            plays_without_length: totals.plays_without_length,
            top_albums: Vec::new(),
            top_artists: Vec::new(),
            top_songs: Vec::new(),
            months: totals.months.to_vec(),
            busiest_month: None,
            first_play: None,
            busiest_day: None,
            longest_streak: None,
            new_artists: None,
        };
        let Some(lists) = &read.worked.lists else {
            return Ok(review);
        };

        let albums_missing = albums_without_art(
            &self.library,
            read.albums.iter().map(|(id, album)| {
                (
                    id.as_str(),
                    album.cover_available,
                    album.release_group_mbid.as_deref(),
                )
            }),
        )
        .await?;
        let mut shown: Vec<&PlayedArtist> = lists.artists.iter().collect();
        if let Some(new) = &read.new_artists {
            shown.extend(&new.top);
            shown.extend(new.first.iter().map(|(artist, _)| artist));
        }
        let records: Vec<ArtistRecord> = shown
            .iter()
            .filter_map(|artist| match &artist.tally.key {
                review::ArtistKey::Id(id) => Some(artist_record(id, artist)),
                review::ArtistKey::Name(_) => None,
            })
            .collect();
        let artists_missing = artists_missing_art(&self.library, &records).await?;

        let album_group = |id: &str| {
            read.albums
                .get(id)
                .and_then(|album| album.release_group_mbid.clone())
        };
        let album_cover = |id: &str| {
            read.albums
                .get(id)
                .is_some_and(|album| album.cover_available)
        };
        let album = |tally: &review::Tally<String>| YearReviewAlbum {
            id: tally.key.clone(),
            name: tally.name.clone(),
            artist_name: read
                .albums
                .get(&tally.key)
                .map(|album| album.artist_name.clone())
                .unwrap_or_default(),
            plays: tally.plays,
            release_group_mbid: album_group(&tally.key),
            cover_available: album_cover(&tally.key),
            art_missing: albums_missing.contains(&tally.key)
                || !read.albums.contains_key(&tally.key),
        };
        let artist = |played: &PlayedArtist| {
            let id = match &played.tally.key {
                review::ArtistKey::Id(id) => Some(id.clone()),
                review::ArtistKey::Name(_) => None,
            };
            YearReviewArtist {
                art_missing: id.as_ref().is_none_or(|id| artists_missing.contains(id)),
                id,
                mbid: played.mbid.clone(),
                name: played.tally.name.clone(),
                plays: played.tally.plays,
            }
        };
        review.top_albums = lists.albums.iter().map(album).collect();
        review.top_artists = lists.artists.iter().map(artist).collect();
        review.top_songs = lists
            .songs
            .iter()
            .map(|song| YearReviewSong {
                id: song.track_id.clone(),
                album_id: song.album_id.clone(),
                album_name: song.album_name.clone(),
                release_group_mbid: song.album_id.as_deref().and_then(album_group),
                cover_available: song.album_id.as_deref().is_some_and(album_cover),
                name: song.tally.name.clone(),
                artist_name: song.artist_name.clone(),
                plays: song.tally.plays,
                art_missing: song
                    .album_id
                    .as_ref()
                    .is_none_or(|id| albums_missing.contains(id) || !read.albums.contains_key(id)),
            })
            .collect();
        let (month, month_plays, month_album) = &lists.busiest_month;
        review.busiest_month = Some(YearReviewMonth {
            month: *month,
            plays: *month_plays,
            album: month_album.as_ref().map(album),
        });
        let (played_at_ms, song, by) = &lists.first_play;
        review.first_play = Some(YearReviewFirstPlay {
            played_at_ms: *played_at_ms,
            song: song.clone(),
            artist: by.clone(),
        });
        review.busiest_day = Some(YearReviewDay {
            date: iso(lists.busiest_day.0),
            plays: lists.busiest_day.1,
        });
        let (days, streak_first, streak_last) = lists.streak;
        review.longest_streak = Some(YearReviewStreak {
            days,
            first_day: iso(streak_first),
            last_day: iso(streak_last),
        });
        review.new_artists = read.new_artists.as_ref().map(|new| YearReviewNewArtists {
            count: new.count,
            artists: new.top.iter().map(artist).collect(),
            first: new.first.as_ref().map(|(first, at)| YearReviewFirstArtist {
                artist: artist(first),
                date: iso(local_date(&zone, *at)),
            }),
        });
        Ok(review)
    }
}

/// The record the library's picture check reads for artist `id`: its id
/// and MusicBrainz link.
fn artist_record(id: &str, artist: &PlayedArtist) -> ArtistRecord {
    ArtistRecord {
        id: id.to_owned(),
        name: artist.tally.name.clone(),
        artist_mbid: artist.mbid.clone(),
        linked: artist.mbid.is_some(),
        album_count: 0,
        track_count: 0,
        led_track_count: 0,
        appearance_album_count: 0,
        appearance_track_count: 0,
        date_added: None,
        disambiguation: None,
    }
}
