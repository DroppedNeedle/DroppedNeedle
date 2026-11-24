//! The year in review's wire shapes. Every name starts `YearReview`, apart
//! from the keyed ListenBrainz `Wrapped*` shapes.

use serde::{Deserialize, Deserializer, Serialize};
use utoipa::ToSchema;

/// The longest time zone name a request may send.
pub const ZONE_NAME_MAX: usize = 64;

/// How far a year's review has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum YearReviewState {
    /// The current year, counted so far.
    InProgress,
    /// A year that has ended.
    Complete,
    /// Under 30 plays or under 5 artists: the lists stay empty.
    TooFewPlays,
}

/// The years a person can look back on, newest first.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewYears {
    /// The IANA name of the time zone the years were counted in.
    pub zone: String,
    /// The current year, then each earlier year with a play.
    pub years: Vec<YearReviewYear>,
}

/// One year in the list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewYear {
    /// The calendar year.
    pub year: i32,
    /// Plays made here that year.
    pub plays: u64,
    /// How far its review has come.
    pub state: YearReviewState,
}

/// A person's year of plays made here.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReview {
    /// The calendar year.
    pub year: i32,
    /// The IANA name of the time zone the year was counted in.
    pub zone: String,
    /// How far the review has come.
    pub state: YearReviewState,
    /// 1 January, as an ISO date.
    pub first_day: String,
    /// 31 December, or today while the year is in progress, as an ISO date.
    pub last_day: String,
    /// Plays made here that year.
    pub plays: u64,
    /// Distinct artists played.
    pub artists: u64,
    /// Days with at least one play.
    pub days_played: u64,
    /// The lengths of the songs played, in milliseconds.
    pub listened_ms: u64,
    /// Plays with no known length, left out of `listened_ms`.
    pub plays_without_length: u64,
    /// Up to ten albums, most played first.
    pub top_albums: Vec<YearReviewAlbum>,
    /// Up to ten artists, most played first.
    pub top_artists: Vec<YearReviewArtist>,
    /// Up to ten songs, most played first.
    pub top_songs: Vec<YearReviewSong>,
    /// Plays in each month, January first.
    pub months: Vec<u64>,
    /// The month with most plays; null under too few plays.
    pub busiest_month: Option<YearReviewMonth>,
    /// The year's first play; null under too few plays.
    pub first_play: Option<YearReviewFirstPlay>,
    /// The day with most plays; null under too few plays.
    pub busiest_day: Option<YearReviewDay>,
    /// The most days in a row with a play; null under too few plays.
    pub longest_streak: Option<YearReviewStreak>,
    /// Artists first played this year. Null in a person's first year (no
    /// play here and no imported listen before it), and under too few plays.
    pub new_artists: Option<YearReviewNewArtists>,
}

/// An album in a list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewAlbum {
    /// The library album.
    pub id: String,
    /// Its title.
    pub name: String,
    /// Its album artist.
    pub artist_name: String,
    /// Plays that year (or that month, for the busiest month's album).
    pub plays: u64,
    /// Its release group, when it is matched: the cover the web UI falls
    /// back to when the album has no art of its own.
    pub release_group_mbid: Option<String>,
    /// True when the album has art of its own (a folder or embedded
    /// image), which the library cover route serves.
    pub cover_available: bool,
    /// True when the library has no cover to show for it.
    pub art_missing: bool,
}

/// An artist in a list.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewArtist {
    /// The library artist; null for plays filed by name alone.
    pub id: Option<String>,
    /// Its MusicBrainz id, for the artist's photo; null when it has none
    /// or for plays filed by name alone.
    pub mbid: Option<String>,
    /// Its name.
    pub name: String,
    /// Plays that year.
    pub plays: u64,
    /// True when the library has no picture to show for it.
    pub art_missing: bool,
}

/// A song in a list: one recording, however many copies of it were played.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewSong {
    /// The library track played most for it; null when none was.
    pub id: Option<String>,
    /// That track's album; null when it has none.
    pub album_id: Option<String>,
    /// That album's title; null when `album_id` is.
    pub album_name: Option<String>,
    /// That album's release group, when it is matched: the cover the web
    /// UI falls back to when the album has no art of its own.
    pub release_group_mbid: Option<String>,
    /// True when that album has art of its own (a folder or embedded
    /// image), which the library cover route serves.
    pub cover_available: bool,
    /// The song's title.
    pub name: String,
    /// Its artist.
    pub artist_name: String,
    /// Plays that year.
    pub plays: u64,
    /// True when the library has no cover to show for its album.
    pub art_missing: bool,
}

/// The month with most plays.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewMonth {
    /// The month, 1 for January.
    pub month: u8,
    /// Plays that month.
    pub plays: u64,
    /// The album played most that month, with its plays that month; null
    /// when no album has 5.
    pub album: Option<YearReviewAlbum>,
}

/// The year's first play.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewFirstPlay {
    /// When it was played, unix milliseconds.
    pub played_at_ms: i64,
    /// The song's title.
    pub song: String,
    /// Its artist.
    pub artist: String,
}

/// The day with most plays.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewDay {
    /// The day, as an ISO date.
    pub date: String,
    /// Plays that day.
    pub plays: u64,
}

/// The most days in a row with a play.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewStreak {
    /// How many days.
    pub days: u64,
    /// Its first day, as an ISO date.
    pub first_day: String,
    /// Its last day, as an ISO date.
    pub last_day: String,
}

/// Artists first played this year.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewNewArtists {
    /// How many.
    pub count: u64,
    /// Up to eight of them, most played first.
    pub artists: Vec<YearReviewArtist>,
    /// The first of them met; null when there is none.
    pub first: Option<YearReviewFirstArtist>,
}

/// The first new artist met.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearReviewFirstArtist {
    /// The artist.
    pub artist: YearReviewArtist,
    /// The day of its first play, as an ISO date.
    pub date: String,
}

/// The query both routes take.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct YearReviewQuery {
    /// The IANA time zone to count in.
    pub zone: Option<ZoneName>,
}

/// A time zone name as a request sends it: at most [`ZONE_NAME_MAX`]
/// bytes of printable ASCII. Whether a zone of that name exists is decided
/// later; this only refuses what cannot be a name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneName(String);

impl ZoneName {
    /// The name as sent.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for ZoneName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        if name.len() > ZONE_NAME_MAX || !name.bytes().all(|byte| (0x20..0x7f).contains(&byte)) {
            return Err(serde::de::Error::custom("zone must be a time zone name"));
        }
        Ok(Self(name))
    }
}
